use crate::cache::{Cache, CacheKey};
use crate::color::{Color, ColorSpace};
use crate::convert::convert_transform;
use crate::font::outline::OutlineFont;
use crate::font::{Font, PositionedGlyph, StandardFont};
use crate::interpret::state::{ClipType, State, TextStateFont};
use crate::ocg::OcgState;
use crate::util::{BezPathExt, Float64Ext};
use crate::{ClipPath, Device, DrawProps, FillRule, InterpreterSettings, Paint, StrokeProps};
use hayro_syntax::content::ops::Transform;
use hayro_syntax::object::Dict;
use hayro_syntax::object::Name;
use hayro_syntax::object::dict::keys::{SUBTYPE, TYPE3};
use hayro_syntax::page::Resources;
use hayro_syntax::xref::XRef;
use kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape};
use rustc_hash::FxHashMap;
use smallvec::smallvec;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Maximum nesting depth for interpreting `XObject`'s/patterns/streams.
pub(crate) const MAX_NESTED_INTERPRETATION_DEPTH: u32 = 50;

/// Resource bounds for a document's shared interpreter cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterpreterCacheLimits {
    /// Maximum number of parsed outline fonts retained by a document.
    pub max_outline_fonts: usize,
    /// Maximum estimated bytes retained by parsed outline fonts.
    pub max_outline_font_bytes: u64,
    /// Maximum number of decoded or resolved interpreter objects retained by a document.
    pub max_objects: usize,
}

impl Default for InterpreterCacheLimits {
    fn default() -> Self {
        Self {
            max_outline_fonts: 512,
            max_outline_font_bytes: 256 * 1024 * 1024,
            max_objects: 8_192,
        }
    }
}

/// A point-in-time snapshot of a document's shared interpreter cache.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InterpreterCacheStats {
    /// Number of parsed outline-font entries currently retained.
    pub outline_fonts: usize,
    /// Estimated bytes currently retained by parsed outline fonts.
    pub outline_font_bytes: u64,
    /// Number of outline-font cache hits since construction.
    pub outline_font_hits: u64,
    /// Number of outline-font cache misses since construction.
    pub outline_font_misses: u64,
    /// Number of outline-font entries evicted since construction.
    pub outline_font_evictions: u64,
    /// Number of decoded or resolved object entries currently retained.
    pub objects: usize,
    /// Number of decoded or resolved object cache hits since construction.
    pub object_hits: u64,
    /// Number of decoded or resolved object cache misses since construction.
    pub object_misses: u64,
    /// Number of decoded or resolved object entries evicted since construction.
    pub object_evictions: u64,
}

struct OutlineFontSlot {
    value: OnceLock<Option<OutlineFont>>,
    accounted: AtomicBool,
}

struct OutlineFontEntry {
    slot: Arc<OutlineFontSlot>,
    estimated_bytes: u64,
    last_used: u64,
}

struct OutlineFontCache {
    entries: FxHashMap<u128, OutlineFontEntry>,
    recency: VecDeque<(u128, u64)>,
    clock: u64,
    estimated_bytes: u64,
    limits: InterpreterCacheLimits,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl OutlineFontCache {
    fn touch(&mut self, cache_key: u128) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&cache_key) {
            entry.last_used = self.clock;
            self.recency.push_back((cache_key, self.clock));
        }

        if self.recency.len() > self.entries.len().saturating_mul(4).saturating_add(32) {
            let mut current = self
                .entries
                .iter()
                .map(|(key, entry)| (*key, entry.last_used))
                .collect::<Vec<_>>();
            current.sort_unstable_by_key(|(_, last_used)| *last_used);
            self.recency = current.into();
        }
    }

    fn evict_to_limits(&mut self) {
        while self.entries.len() > self.limits.max_outline_fonts
            || self.estimated_bytes > self.limits.max_outline_font_bytes
        {
            let Some((cache_key, last_used)) = self.recency.pop_front() else {
                break;
            };
            let is_current = self
                .entries
                .get(&cache_key)
                .is_some_and(|entry| entry.last_used == last_used);
            if !is_current {
                continue;
            }
            if let Some(entry) = self.entries.remove(&cache_key) {
                self.estimated_bytes = self.estimated_bytes.saturating_sub(entry.estimated_bytes);
                self.evictions = self.evictions.saturating_add(1);
            }
        }
    }
}

/// A bounded, thread-safe interpreter cache shared by all pages of one document.
///
/// Outline fonts and owned decoded objects are reusable across concurrent page
/// interpretations. Type 3 fonts remain in the page-local [`InterpreterCache`]
/// because their programs borrow page resources.
#[derive(Clone)]
pub struct SharedInterpreterCache {
    outline_fonts: Arc<Mutex<OutlineFontCache>>,
    object_cache: Cache,
}

impl Default for SharedInterpreterCache {
    fn default() -> Self {
        Self::new(InterpreterCacheLimits::default())
    }
}

impl SharedInterpreterCache {
    /// Construct an empty document cache with explicit resource limits.
    pub fn new(limits: InterpreterCacheLimits) -> Self {
        Self {
            outline_fonts: Arc::new(Mutex::new(OutlineFontCache {
                entries: FxHashMap::default(),
                recency: VecDeque::new(),
                clock: 0,
                estimated_bytes: 0,
                limits,
                hits: 0,
                misses: 0,
                evictions: 0,
            })),
            object_cache: Cache::with_max_entries(limits.max_objects),
        }
    }

    /// Return the configured cache resource limits.
    pub fn limits(&self) -> InterpreterCacheLimits {
        self.outline_fonts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .limits
    }

    /// Return current residency and cumulative hit/miss statistics.
    pub fn stats(&self) -> InterpreterCacheStats {
        let fonts = self
            .outline_fonts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let objects = self.object_cache.stats();
        InterpreterCacheStats {
            outline_fonts: fonts.entries.len(),
            outline_font_bytes: fonts.estimated_bytes,
            outline_font_hits: fonts.hits,
            outline_font_misses: fonts.misses,
            outline_font_evictions: fonts.evictions,
            objects: objects.entries,
            object_hits: objects.hits,
            object_misses: objects.misses,
            object_evictions: objects.evictions,
        }
    }

    /// Drop all currently retained entries while preserving cumulative statistics.
    pub fn clear(&self) {
        let mut fonts = self
            .outline_fonts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        fonts.entries.clear();
        fonts.recency.clear();
        fonts.estimated_bytes = 0;
        drop(fonts);
        self.object_cache.clear();
    }

    fn outline_font(
        &self,
        cache_key: u128,
        build: impl FnOnce() -> Option<OutlineFont>,
    ) -> Option<OutlineFont> {
        let slot = {
            let mut cache = self
                .outline_fonts
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if cache.entries.contains_key(&cache_key) {
                cache.hits = cache.hits.saturating_add(1);
                cache.touch(cache_key);
                cache
                    .entries
                    .get(&cache_key)
                    .map(|entry| entry.slot.clone())
                    .expect("an outline-font entry checked above must remain present")
            } else {
                cache.misses = cache.misses.saturating_add(1);
                cache.clock = cache.clock.wrapping_add(1);
                let last_used = cache.clock;
                let slot = Arc::new(OutlineFontSlot {
                    value: OnceLock::new(),
                    accounted: AtomicBool::new(false),
                });
                cache.entries.insert(
                    cache_key,
                    OutlineFontEntry {
                        slot: slot.clone(),
                        estimated_bytes: 0,
                        last_used,
                    },
                );
                cache.recency.push_back((cache_key, last_used));
                cache.evict_to_limits();
                slot
            }
        };

        let font = slot.value.get_or_init(build).clone();
        if !slot.accounted.swap(true, Ordering::AcqRel) {
            let estimated_bytes = font
                .as_ref()
                .map(OutlineFont::estimated_cache_bytes)
                .unwrap_or(64);
            let mut cache = self
                .outline_fonts
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let is_current = cache
                .entries
                .get(&cache_key)
                .is_some_and(|entry| Arc::ptr_eq(&entry.slot, &slot));
            if is_current {
                if let Some(entry) = cache.entries.get_mut(&cache_key) {
                    entry.estimated_bytes = estimated_bytes;
                }
                cache.estimated_bytes = cache.estimated_bytes.saturating_add(estimated_bytes);
                cache.evict_to_limits();
            }
        }
        font
    }
}

/// A page-local cache used by the interpreter.
///
/// Construct this from a document-owned [`SharedInterpreterCache`] to reuse parsed
/// outline fonts and decoded objects across pages. Borrowing Type 3 fonts remain
/// local to this cache.
#[derive(Clone)]
pub struct InterpreterCache<'a> {
    pub(crate) type3_font_cache: Rc<RefCell<FxHashMap<u128, Option<Font<'a>>>>>,
    shared: SharedInterpreterCache,
    pub(crate) object_cache: Cache,
}

impl<'a> Default for InterpreterCache<'a> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> InterpreterCache<'a> {
    /// Create a new interpreter cache.
    pub fn new() -> Self {
        Self::from_shared(&SharedInterpreterCache::default())
    }

    /// Create a page-local interpreter cache backed by a document cache.
    pub fn from_shared(shared: &SharedInterpreterCache) -> Self {
        Self {
            type3_font_cache: Rc::new(RefCell::new(FxHashMap::default())),
            shared: shared.clone(),
            object_cache: shared.object_cache.clone(),
        }
    }
}

#[cfg(test)]
mod shared_cache_tests {
    use super::{InterpreterCacheLimits, SharedInterpreterCache};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn shared_cache_is_send_sync_and_reports_its_limits() {
        assert_send_sync::<SharedInterpreterCache>();
        let limits = InterpreterCacheLimits {
            max_outline_fonts: 7,
            max_outline_font_bytes: 8_192,
            max_objects: 11,
        };
        let cache = SharedInterpreterCache::new(limits);
        assert_eq!(cache.limits(), limits);
        assert_eq!(cache.stats(), Default::default());
    }
}

/// A per-page interpretation context that borrows shared data from an [`InterpreterCache`].
pub struct Context<'a> {
    pub(crate) initial_transfer_function: Option<crate::ActiveTransferFunction>,
    states: Vec<State<'a>>,
    path: BezPath,
    sub_path_start: Point,
    last_point: Point,
    clip: Option<FillRule>,
    root_transforms: Vec<Affine>,
    bbox: Vec<Rect>,
    pub(crate) settings: InterpreterSettings,
    pub(crate) interpreter_cache: InterpreterCache<'a>,
    pub(crate) xref: &'a XRef,
    pub(crate) ocg_state: OcgState,
    nesting_depth: u32,
    pub(crate) glyph_scratch: Vec<PositionedGlyph<'a>>,
}

impl<'a> Context<'a> {
    /// Create a new context.
    pub fn new(
        initial_transform: Affine,
        bbox: Rect,
        cache: &InterpreterCache<'a>,
        xref: &'a XRef,
        settings: InterpreterSettings,
    ) -> Self {
        let state = State::new(initial_transform);

        Self::new_with(initial_transform, bbox, cache, xref, settings, state, 0)
    }

    pub(crate) fn new_with(
        initial_transform: Affine,
        bbox: Rect,
        cache: &InterpreterCache<'a>,
        xref: &'a XRef,
        settings: InterpreterSettings,
        state: State<'a>,
        nesting_depth: u32,
    ) -> Self {
        let ocg_state = {
            let root_ref = xref.root_id();
            xref.get::<Dict<'_>>(root_ref)
                .map(|catalog| OcgState::from_catalog(&catalog, settings.preserve_optional_content))
                .unwrap_or_default()
        };

        Self {
            initial_transfer_function: state.graphics_state.transfer_function.clone(),
            states: vec![state],
            settings,
            xref,
            root_transforms: vec![initial_transform],
            last_point: Point::default(),
            sub_path_start: Point::default(),
            clip: None,
            bbox: vec![bbox],
            path: BezPath::new(),
            interpreter_cache: cache.clone(),
            ocg_state,
            nesting_depth,
            glyph_scratch: Vec::new(),
        }
    }

    pub(crate) fn save_state(&mut self) {
        let Some(cur) = self.states.last().cloned() else {
            warn!("attempted to save state without existing state");
            return;
        };

        self.states.push(cur);
    }

    pub(crate) fn bbox(&self) -> Rect {
        self.bbox.last().copied().unwrap_or_else(|| {
            warn!("failed to get a bbox");

            Rect::new(0.0, 0.0, 1.0, 1.0)
        })
    }

    fn push_bbox(&mut self, bbox: Rect) {
        let new = self.bbox().intersect(bbox);
        self.bbox.push(new);
    }

    pub(crate) fn push_clip_path(
        &mut self,
        clip_path: BezPath,
        fill: FillRule,
        device: &mut impl Device<'a>,
    ) {
        if let Some(clip_rect) = path_as_rect(&clip_path) {
            let cur_bbox = self.bbox();

            // If the clip path is a rect and completely covers the current bbox, don't emit it.
            if cur_bbox
                .min_x()
                .is_nearly_greater_or_equal(clip_rect.min_x())
                && cur_bbox
                    .min_y()
                    .is_nearly_greater_or_equal(clip_rect.min_y())
                && cur_bbox.max_x().is_nearly_less_or_equal(clip_rect.max_x())
                && cur_bbox.max_y().is_nearly_less_or_equal(clip_rect.max_y())
            {
                self.get_mut().clips.push(ClipType::Dummy);
                return;
            }

            device.push_clip_rect(&clip_rect);
            self.push_bbox(clip_rect);
            self.get_mut().clips.push(ClipType::Real);
            return;
        }

        let bbox = clip_path.bounding_box();
        device.push_clip_path(&ClipPath {
            path: clip_path,
            fill,
        });
        self.push_bbox(bbox);
        self.get_mut().clips.push(ClipType::Real);
    }

    pub(crate) fn pop_clip(&mut self, device: &mut impl Device<'a>) {
        if let Some(ClipType::Real) = self.get_mut().clips.pop() {
            device.pop_clip();
            self.pop_bbox();
        }
    }

    fn pop_bbox(&mut self) {
        self.bbox.pop();
    }

    pub(crate) fn push_root_transform(&mut self) {
        self.root_transforms.push(self.get().ctm);
    }

    pub(crate) fn pop_root_transform(&mut self) {
        self.root_transforms.pop();
    }

    pub(crate) fn root_transform(&self) -> Affine {
        self.root_transforms
            .last()
            .copied()
            .unwrap_or(Affine::IDENTITY)
    }

    pub(crate) fn restore_state(&mut self, device: &mut impl Device<'a>) {
        let Some(target_clips) = self
            .states
            .get(self.states.len().saturating_sub(2))
            .map(|s| s.clips.len())
        else {
            warn!("underflowed graphics state");
            return;
        };

        while self.get().clips.len() > target_clips {
            self.pop_clip(device);
        }

        // The first state should never be popped.
        if self.states.len() > 1 {
            self.states.pop();
        }
    }

    pub(crate) fn draw_props(&self, is_stroke: bool) -> DrawProps<'a> {
        let alpha_constant = if is_stroke {
            self.get().graphics_state.stroke_alpha
        } else {
            self.get().graphics_state.non_stroke_alpha
        };
        let paint = self.get_paint(is_stroke);
        let deferred_transfer_function = (self.settings.defer_transfer_functions
            && matches!(paint, Paint::Color(_)))
        .then(|| self.get().graphics_state.transfer_function.clone())
        .flatten();
        DrawProps {
            transform: self.get().ctm,
            paint,
            deferred_transfer_function,
            soft_mask: self.get().graphics_state.soft_mask.clone(),
            blend_mode: self.get().graphics_state.blend_mode,
            alpha_constant,
            alpha_is_shape: self.get().graphics_state.alpha_is_shape,
        }
    }

    pub(crate) fn get_paint(&self, is_stroke: bool) -> Paint<'a> {
        let data = if is_stroke {
            self.get().stroke_data()
        } else {
            self.get().non_stroke_data()
        };

        let intent = self.get().graphics_state.rendering_intent;
        let failed = |error| {
            (self.settings.warning_sink)(crate::InterpreterWarning::ColorConversion(error));
            Paint::Color(Color::from_rgba(crate::color::AlphaColor::TRANSPARENT))
        };
        if data.color_space.is_pattern() || data.pattern.is_some() {
            if let Some(mut pattern) = data.pattern {
                // Alpha belongs to the painting operation, which may occur
                // after a different ExtGState than the pattern selection.
                if let crate::pattern::Pattern::Shading(shading) = &mut pattern {
                    shading.opacity = data.alpha;
                }
                if let Err(error) = pattern.set_rendering_intent(intent) {
                    return failed(error);
                }
                if pattern
                    .set_selecting_transfer_function(data.transfer_function.clone())
                    .is_none()
                {
                    (self.settings.warning_sink)(
                        crate::InterpreterWarning::TransferFunctionFailure,
                    );
                    return Paint::Color(Color::from_rgba(crate::color::AlphaColor::TRANSPARENT));
                }

                pattern.pre_concat_transform(self.root_transform());

                Paint::Pattern(Box::new(pattern))
            } else {
                // Pattern was likely invalid, use transparent paint.
                Paint::Color(Color::new(ColorSpace::device_gray(), smallvec![0.0], 0.0))
            }
        } else {
            let color_space = match data.color_space.with_rendering_intent(intent) {
                Ok(space) => space,
                Err(error) => return failed(error),
            };
            let color = Color::new(color_space, data.color, data.alpha);

            if !self.settings.defer_transfer_functions
                && let Some(tf) = &data.transfer_function
            {
                match tf.apply(&color.to_rgba()) {
                    Some(rgba) => Paint::Color(Color::from_rgba(rgba)),
                    None => {
                        (self.settings.warning_sink)(
                            crate::InterpreterWarning::TransferFunctionFailure,
                        );
                        Paint::Color(Color::from_rgba(crate::color::AlphaColor::TRANSPARENT))
                    }
                }
            } else {
                Paint::Color(color)
            }
        }
    }

    pub(crate) fn path(&self) -> &BezPath {
        &self.path
    }

    pub(crate) fn path_mut(&mut self) -> &mut BezPath {
        &mut self.path
    }

    pub(crate) fn sub_path_start(&self) -> &Point {
        &self.sub_path_start
    }

    pub(crate) fn sub_path_start_mut(&mut self) -> &mut Point {
        &mut self.sub_path_start
    }

    pub(crate) fn last_point(&self) -> &Point {
        &self.last_point
    }

    pub(crate) fn last_point_mut(&mut self) -> &mut Point {
        &mut self.last_point
    }

    pub(crate) fn clip(&self) -> &Option<FillRule> {
        &self.clip
    }

    pub(crate) fn clip_mut(&mut self) -> &mut Option<FillRule> {
        &mut self.clip
    }

    pub(crate) fn get(&self) -> &State<'a> {
        self.states.last().unwrap()
    }

    pub(crate) fn get_mut(&mut self) -> &mut State<'a> {
        self.states.last_mut().unwrap()
    }

    pub(crate) fn pre_concat_transform(&mut self, transform: Transform) {
        self.pre_concat_affine(convert_transform(transform));
    }

    pub(crate) fn pre_concat_affine(&mut self, transform: Affine) {
        self.get_mut().ctm *= transform;
    }

    pub(crate) fn get_color_space(
        &mut self,
        resources: &Resources<'_>,
        name: &Name<'_>,
    ) -> Option<ColorSpace> {
        let color_space = resources.get_color_space(name).and_then(|cs_object| {
            self.interpreter_cache
                .object_cache
                .get_or_insert_with(cs_object.cache_key(), || {
                    ColorSpace::new(cs_object.clone(), &self.interpreter_cache.object_cache)
                })
        });
        if color_space.is_none() {
            (self.settings.warning_sink)(crate::InterpreterWarning::ColorSpaceFailure);
        }
        color_space
    }

    pub(crate) fn stroke_props(&self) -> StrokeProps {
        self.get().graphics_state.stroke_props.clone()
    }

    pub(crate) fn num_states(&self) -> usize {
        self.states.len()
    }

    pub(crate) fn nesting_depth(&self) -> u32 {
        self.nesting_depth
    }

    pub(crate) fn begin_nested_interpretation(&mut self) -> bool {
        if self.nesting_depth >= MAX_NESTED_INTERPRETATION_DEPTH {
            warn!("interpreter nesting depth exceeded");

            return false;
        }

        self.nesting_depth += 1;

        true
    }

    pub(crate) fn end_nested_interpretation(&mut self) {
        self.nesting_depth = self.nesting_depth.saturating_sub(1);
    }
    pub(crate) fn resolve_font(&mut self, font_dict: &Dict<'a>) -> Option<TextStateFont<'a>> {
        let cache_key = font_dict.cache_key();

        let is_type3 = font_dict
            .get::<Name<'_>>(SUBTYPE)
            .is_some_and(|subtype| subtype.as_ref() == TYPE3);
        let resolved = if is_type3 {
            let mut font_cache = self.interpreter_cache.type3_font_cache.borrow_mut();
            font_cache
                .entry(cache_key)
                .or_insert_with(|| Font::new_type3(font_dict, &self.settings.cmap_resolver))
                .clone()
        } else {
            self.interpreter_cache
                .shared
                .outline_font(cache_key, || {
                    Font::new_outline(
                        font_dict,
                        &self.settings.font_resolver,
                        &self.settings.cmap_resolver,
                    )
                })
                .map(|font| Font::from_outline(cache_key, font))
        };

        if let Some(resolved) = resolved {
            Some(TextStateFont::Font(resolved))
        } else {
            (self.settings.warning_sink)(crate::InterpreterWarning::UnsupportedFont);
            Font::new_standard(StandardFont::Helvetica, &self.settings.font_resolver)
                .map(TextStateFont::Fallback)
        }
    }
}

pub(crate) fn path_as_rect(path: &BezPath) -> Option<Rect> {
    let points = match path.elements() {
        [
            PathEl::MoveTo(first),
            PathEl::LineTo(second),
            PathEl::LineTo(third),
            PathEl::LineTo(fourth),
            PathEl::ClosePath,
        ] => [*first, *second, *third, *fourth, *first],
        [
            PathEl::MoveTo(first),
            PathEl::LineTo(second),
            PathEl::LineTo(third),
            PathEl::LineTo(fourth),
            PathEl::LineTo(last),
        ] if first.x.is_nearly_equal(last.x) && first.y.is_nearly_equal(last.y) => {
            [*first, *second, *third, *fourth, *last]
        }
        _ => return None,
    };

    let mut previous_axis = None;
    for edge in points.windows(2) {
        let same_x = edge[0].x.is_nearly_equal(edge[1].x);
        let same_y = edge[0].y.is_nearly_equal(edge[1].y);
        if same_x == same_y || previous_axis == Some(same_x) {
            return None;
        }
        previous_axis = Some(same_x);
    }

    Some(path.fast_bounding_box())
}
