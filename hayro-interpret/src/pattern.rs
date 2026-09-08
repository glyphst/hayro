//! PDF patterns.

use crate::cache::Cache;
use crate::color::{Color, ColorSpace};
use crate::context::{Context, InterpreterCache};
use crate::device::Device;
use crate::font::GlyphRun;
use crate::interpret::state::{ActiveTransferFunction, State};
use crate::shading::Shading;
use crate::util::{Float32Ext, RectExt, hash128};
use crate::x_object::soft_mask::SoftMask;
use crate::{BlendMode, CacheKey, ClipPath, DrawMode, DrawProps, Image, ImageDrawProps};
use crate::{FillRule, InterpreterSettings, Paint, interpret};
use hayro_syntax::content::TypedIter;
use hayro_syntax::object::Dict;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::{
    BBOX, EXT_G_STATE, MATRIX, PAINT_TYPE, RESOURCES, SHADING, TILING_TYPE, X_STEP, Y_STEP,
};
use hayro_syntax::object::{Object, dict_or_stream};
use hayro_syntax::page::Resources;
use hayro_syntax::xref::XRef;
use kurbo::{Affine, BezPath, Rect, Shape};
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

/// A PDF pattern.
#[derive(Debug, Clone)]
pub enum Pattern<'a> {
    /// A shading pattern.
    Shading(ShadingPattern),
    /// A tiling pattern.
    Tiling(Box<TilingPattern<'a>>),
}

impl<'a> Pattern<'a> {
    pub(crate) fn new(
        object: Object<'a>,
        ctx: &Context<'a>,
        resources: &Resources<'a>,
    ) -> Option<Self> {
        match object {
            Object::Dict(dict) => {
                let mut pattern = ShadingPattern::new(
                    &dict,
                    &ctx.interpreter_cache.object_cache,
                    ctx.get().graphics_state.non_stroke_alpha,
                    ctx.initial_transfer_function.clone(),
                    &ctx.settings.warning_sink,
                )?;
                pattern.defer_transfer_function = ctx.settings.defer_transfer_functions;
                Some(Self::Shading(pattern))
            }
            Object::Stream(stream) => Some(Self::Tiling(Box::new(TilingPattern::new(
                stream, ctx, resources,
            )?))),
            _ => None,
        }
    }

    pub(crate) fn set_rendering_intent(
        &mut self,
        intent: crate::color::RenderingIntent,
    ) -> Result<(), crate::color::ColorConversionError> {
        match self {
            Self::Shading(pattern) => {
                let space = pattern.shading.color_space.with_rendering_intent(intent)?;
                Arc::make_mut(&mut pattern.shading).color_space = space;
            }
            Self::Tiling(pattern) => {
                pattern.rendering_intent = intent;
                if !pattern.is_color {
                    pattern.stroke_paint = pattern.stroke_paint.with_rendering_intent(intent)?;
                    pattern.non_stroking_paint =
                        pattern.non_stroking_paint.with_rendering_intent(intent)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn pre_concat_transform(&mut self, transform: Affine) {
        match self {
            Self::Shading(p) => {
                p.matrix = transform * p.matrix;
                let transformed_clip_path = p.shading.clip_path.clone().map(|r| p.matrix * r);
                Arc::make_mut(&mut p.shading).clip_path = transformed_clip_path;
            }
            Self::Tiling(p) => p.matrix = transform * p.matrix,
        }
    }

    pub(crate) fn set_selecting_transfer_function(
        &mut self,
        tf: Option<ActiveTransferFunction>,
    ) -> Option<()> {
        match self {
            // Colored patterns use their parent stream's initial state.
            Self::Shading(_) => {}
            Self::Tiling(pattern) => {
                if pattern.settings.defer_transfer_functions {
                    pattern.deferred_base_transfer_function =
                        (!pattern.is_color).then_some(tf).flatten();
                    return Some(());
                }
                if !pattern.is_color
                    && let Some(tf) = &tf
                {
                    pattern.stroke_paint =
                        Color::from_rgba(tf.apply(&pattern.stroke_paint.to_rgba())?);
                    pattern.non_stroking_paint =
                        Color::from_rgba(tf.apply(&pattern.non_stroking_paint.to_rgba())?);
                }
            }
        }
        Some(())
    }
}

impl CacheKey for Pattern<'_> {
    fn cache_key(&self) -> u128 {
        match self {
            Self::Shading(p) => p.cache_key(),
            Self::Tiling(p) => p.cache_key(),
        }
    }
}

/// A shading pattern.
#[derive(Clone, Debug)]
pub struct ShadingPattern {
    /// The underlying shading of the pattern.
    pub shading: Arc<Shading>,
    /// A transformation matrix to apply prior to rendering.
    pub matrix: Affine,
    /// An additional opacity to apply to the shading pattern.
    pub opacity: f32,
    /// An optional transfer function to apply to the shading's output colors.
    pub transfer_function: Option<ActiveTransferFunction>,
    /// Whether sampling/encoding must leave this function unapplied so a
    /// retaining device can select it in the final page region.
    pub defer_transfer_function: bool,
    /// Whether the shading dictionary's `/Background` entry applies.
    ///
    /// PDF applies the entry only when a shading is selected through a
    /// Pattern color space. A direct `sh` operator paints only the shading
    /// itself, even though both operations otherwise share this retained
    /// representation.
    pub background_applies: bool,
}

impl ShadingPattern {
    pub(crate) fn new(
        dict: &Dict<'_>,
        cache: &Cache,
        opacity: f32,
        mut transfer_function: Option<ActiveTransferFunction>,
        warning_sink: &interpret::WarningSinkFn,
    ) -> Option<Self> {
        let shading = dict.get::<Object<'_>>(SHADING).and_then(|o| {
            let (dict, stream) = dict_or_stream(&o)?;

            Shading::new(dict, stream, cache, warning_sink)
        })?;
        let matrix = dict
            .get::<[f64; 6]>(MATRIX)
            .map(Affine::new)
            .unwrap_or_default();

        if let Some(state) = dict.get::<Dict<'_>>(EXT_G_STATE)
            && let Some((key, object)) = crate::selected_transfer_function(&state)
        {
            transfer_function = match ActiveTransferFunction::from_object(&object, key == b"TR2") {
                Ok(function) => function,
                Err(_) => {
                    warning_sink(crate::InterpreterWarning::TransferFunctionFailure);
                    return None;
                }
            };
        }

        Some(Self {
            shading: Arc::new(shading),
            opacity,
            matrix,
            transfer_function,
            defer_transfer_function: false,
            background_applies: true,
        })
    }
}

impl CacheKey for ShadingPattern {
    fn cache_key(&self) -> u128 {
        hash128(&(
            self.shading.cache_key(),
            self.matrix.cache_key(),
            self.background_applies,
            self.opacity.to_bits(),
            self.transfer_function.as_ref().map(CacheKey::cache_key),
            self.defer_transfer_function,
        ))
    }
}

/// A tiling pattern.
#[derive(Clone)]
pub struct TilingPattern<'a> {
    cache_key: u128,
    ctx_bbox: Rect,
    /// The bbox of the tiling pattern.
    pub bbox: Rect,
    /// The step in the x direction.
    pub x_step: f32,
    /// The step in the y direction.
    pub y_step: f32,
    /// A transformation to apply prior to rendering.
    pub matrix: Affine,
    stream: Stream<'a>,
    is_color: bool,
    tiling_type: u8,
    pub(crate) stroke_paint: Color,
    pub(crate) non_stroking_paint: Color,
    pub(crate) parent_resources: Resources<'a>,
    pub(crate) cache: InterpreterCache<'a>,
    pub(crate) settings: InterpreterSettings,
    pub(crate) xref: &'a XRef,
    nesting_depth: u32,
    rendering_intent: crate::color::RenderingIntent,
    transfer_function: Option<ActiveTransferFunction>,
    deferred_base_transfer_function: Option<ActiveTransferFunction>,
}

impl Debug for TilingPattern<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("TilingPattern")
    }
}

impl<'a> TilingPattern<'a> {
    pub(crate) fn new(
        stream: Stream<'a>,
        ctx: &Context<'a>,
        resources: &Resources<'a>,
    ) -> Option<Self> {
        let cache_key = stream.cache_key();
        let dict = stream.dict();

        let bbox = dict.get::<hayro_syntax::object::Rect>(BBOX)?.to_kurbo();
        let x_step = dict.get::<f32>(X_STEP)?;
        let y_step = dict.get::<f32>(Y_STEP)?;

        if x_step.is_nearly_zero() || y_step.is_nearly_zero() || bbox.is_zero_area() {
            return None;
        }

        let paint_type = dict.get::<u8>(PAINT_TYPE)?;
        if !matches!(paint_type, 1 | 2) {
            return None;
        }
        let is_color = paint_type == 1;
        let tiling_type = dict.get::<u8>(TILING_TYPE)?;
        if !matches!(tiling_type, 1..=3) {
            return None;
        }
        let matrix = dict
            .get::<[f64; 6]>(MATRIX)
            .map(Affine::new)
            .unwrap_or_default();

        let state = ctx.get().clone();
        let ctx_bbox = ctx.bbox();

        let fill_cs = state
            .graphics_state
            .none_stroke_cs
            .pattern_cs()
            .unwrap_or(ColorSpace::device_gray());
        let stroke_cs = state
            .graphics_state
            .stroke_cs
            .pattern_cs()
            .unwrap_or(ColorSpace::device_gray());

        let non_stroking_paint = Color::new(
            fill_cs,
            state.graphics_state.non_stroke_color.clone(),
            state.graphics_state.non_stroke_alpha,
        );
        let stroke_paint = Color::new(
            stroke_cs,
            state.graphics_state.stroke_color.clone(),
            state.graphics_state.stroke_alpha,
        );
        let nesting_depth = ctx.nesting_depth() + 1;

        Some(Self {
            cache_key,
            bbox,
            x_step,
            y_step,
            matrix,
            ctx_bbox,
            is_color,
            stream,
            stroke_paint,
            non_stroking_paint,
            tiling_type,
            settings: ctx.settings.clone(),
            parent_resources: resources.clone(),
            cache: ctx.interpreter_cache.clone(),
            xref: ctx.xref,
            nesting_depth,
            rendering_intent: state.graphics_state.rendering_intent,
            transfer_function: ctx.initial_transfer_function.clone(),
            deferred_base_transfer_function: None,
        })
    }

    /// Whether the pattern's content stream supplies its own colors
    /// (`/PaintType 1`) instead of using the selecting graphics-state paint.
    pub fn is_colored(&self) -> bool {
        self.is_color
    }

    /// Return the selecting graphics-state color for an uncolored
    /// (`/PaintType 2`) cell.
    ///
    /// A retaining device can use this value as the binding for a reusable
    /// paint-parameterized cell instead of baking the selection into every
    /// callback. Colored patterns return `None` because their content stream
    /// owns its paints.
    pub fn uncolored_base_color(&self, is_stroke: bool) -> Option<&Color> {
        if self.is_color {
            None
        } else if is_stroke {
            Some(&self.stroke_paint)
        } else {
            Some(&self.non_stroking_paint)
        }
    }

    /// Unapplied selecting transfer for a deferred uncolored base paint.
    /// Colored cells instead retain transfer in their interpreted child paints.
    pub fn uncolored_base_transfer_function(&self) -> Option<&ActiveTransferFunction> {
        self.deferred_base_transfer_function.as_ref()
    }

    /// Return the validated PDF `/TilingType` value (1, 2, or 3).
    pub fn tiling_type(&self) -> u8 {
        self.tiling_type
    }

    /// Interpret the contents of the pattern into the given device.
    pub fn interpret(
        &self,
        device: &mut impl Device<'a>,
        initial_transform: Affine,
        is_stroke: bool,
    ) -> Option<()> {
        self.interpret_with_bbox_clip(device, initial_transform, is_stroke, true)
    }

    /// Interpret one pattern cell without synthesizing the outer `/BBox`
    /// clip. Consumers may use this only when their retained output proves
    /// that every possible mark is already contained by the cell box.
    pub fn interpret_unclipped(
        &self,
        device: &mut impl Device<'a>,
        initial_transform: Affine,
        is_stroke: bool,
    ) -> Option<()> {
        self.interpret_with_bbox_clip(device, initial_transform, is_stroke, false)
    }

    fn interpret_with_bbox_clip(
        &self,
        device: &mut impl Device<'a>,
        initial_transform: Affine,
        is_stroke: bool,
        clip_bbox: bool,
    ) -> Option<()> {
        let mut state = State::new(initial_transform);
        state.graphics_state.rendering_intent = self.rendering_intent;
        state.graphics_state.transfer_function = self.transfer_function.clone();

        let mut context = Context::new_with(
            state.ctm,
            // TODO: bbox?
            (initial_transform * self.ctx_bbox.to_path(0.1)).bounding_box(),
            &self.cache,
            self.xref,
            self.settings.clone(),
            state,
            self.nesting_depth,
        );

        let decoded = self.stream.decoded().ok()?;
        let resources = Resources::from_parent(
            self.stream.dict().get(RESOURCES).unwrap_or_default(),
            self.parent_resources.clone(),
        );
        let iter = TypedIter::new(decoded.as_ref());

        let clip_path = ClipPath {
            path: initial_transform * self.bbox.to_path(0.1),
            fill: FillRule::NonZero,
        };
        if clip_bbox {
            device.push_clip_path(&clip_path);
        }

        if self.is_color {
            interpret(iter, &resources, &mut context, device);
        } else {
            let paint = if !is_stroke {
                Paint::Color(self.non_stroking_paint.clone())
            } else {
                Paint::Color(self.stroke_paint.clone())
            };

            let mut device = StencilPatternDevice::new(
                device,
                paint.clone(),
                self.deferred_base_transfer_function.clone(),
            );
            interpret(iter, &resources, &mut context, &mut device);
        }

        if clip_bbox {
            device.pop_clip();
        }

        Some(())
    }
}

impl CacheKey for TilingPattern<'_> {
    fn cache_key(&self) -> u128 {
        hash128(&(
            self.cache_key,
            self.rendering_intent,
            self.transfer_function.as_ref().map(CacheKey::cache_key),
            self.deferred_base_transfer_function
                .as_ref()
                .map(CacheKey::cache_key),
            self.settings.defer_transfer_functions,
        ))
    }
}

struct StencilPatternDevice<'a, 'b, T: Device<'a>> {
    inner: &'b mut T,
    paint: Paint<'a>,
    deferred_transfer_function: Option<ActiveTransferFunction>,
}

impl<'a, 'b, T: Device<'a>> StencilPatternDevice<'a, 'b, T> {
    pub(crate) fn new(
        device: &'b mut T,
        paint: Paint<'a>,
        deferred_transfer_function: Option<ActiveTransferFunction>,
    ) -> Self {
        Self {
            inner: device,
            paint,
            deferred_transfer_function,
        }
    }
}

// Only filling, stroking of paths and stencil masks are allowed.
impl<'a, T: Device<'a>> Device<'a> for StencilPatternDevice<'a, '_, T> {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        let props = DrawProps {
            paint: self.paint.clone(),
            deferred_transfer_function: self.deferred_transfer_function.clone(),
            ..props
        };
        self.inner.draw_path(path, props, draw_mode);
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        self.inner.push_clip_path(clip_path);
    }

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'_>>, _: BlendMode) {}

    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        draw_mode: &DrawMode,
    ) {
        self.inner.draw_glyph_run(glyph_run, props, draw_mode);
    }

    fn begin_text_object(&mut self, text_knockout: bool) {
        self.inner.begin_text_object(text_knockout);
    }

    fn end_text_object(&mut self) {
        self.inner.end_text_object();
    }

    fn begin_combined_fill_stroke(&mut self) {
        self.inner.begin_combined_fill_stroke();
    }

    fn end_combined_fill_stroke(&mut self) {
        self.inner.end_combined_fill_stroke();
    }

    fn draw_image(&mut self, image: Image<'a, '_>, mut props: ImageDrawProps<'a>) {
        if let Image::Stencil(mut s) = image {
            s.paint = self.paint.clone();
            props.deferred_transfer_function = self.deferred_transfer_function.clone();
            self.inner.draw_image(Image::Stencil(s), props);
        }
    }

    fn pop_clip(&mut self) {
        self.inner.pop_clip();
    }

    fn pop_transparency_group(&mut self) {}
}
