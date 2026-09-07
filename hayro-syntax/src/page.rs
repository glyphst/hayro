//! Reading the pages of a PDF document.

use crate::content::{TypedIter, UntypedIter};
use crate::object::Array;
use crate::object::Dict;
use crate::object::Name;
use crate::object::Rect;
use crate::object::Stream;
use crate::object::dict::keys::*;
use crate::object::stream::DecodeFailure;
use crate::object::{MaybeRef, Null, Number, ObjRef, Object, ObjectLike};
use crate::reader::{Readable, Reader, ReaderContext, ReaderExt, Skippable};
use crate::sync::OnceLock;
use crate::transform::Transform;
use crate::util::FloatExt;
use crate::xref::XRef;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Deref;

#[cfg(test)]
mod tests;

/// Attributes that can be inherited.
#[derive(Debug, Clone)]
struct PagesContext {
    media_box: Option<Rect>,
    crop_box: Option<Rect>,
    rotate: Option<i32>,
}

impl PagesContext {
    fn new() -> Self {
        Self {
            media_box: None,
            crop_box: None,
            rotate: None,
        }
    }
}

/// A structure holding the pages of a PDF document.
pub struct Pages<'a> {
    pages: Vec<Page<'a>>,
    xref: &'a XRef,
}

impl<'a> Pages<'a> {
    /// Create a new `Pages` object.
    pub(crate) fn new(
        pages_dict: &Dict<'a>,
        ctx: &ReaderContext<'a>,
        xref: &'a XRef,
    ) -> Option<Self> {
        let mut pages = vec![];
        let pages_ctx = PagesContext::new();
        resolve_pages(
            pages_dict,
            &mut pages,
            pages_ctx,
            Resources::new(Dict::empty(), None, ctx),
        )?;

        Some(Self { pages, xref })
    }

    /// Create a new `Pages` object by bruteforce-searching.
    ///
    /// Of course this could result in the order of pages being messed up, but
    /// this is still better than nothing.
    pub(crate) fn new_brute_force(ctx: &ReaderContext<'a>, xref: &'a XRef) -> Option<Self> {
        let mut pages = vec![];

        for object in xref.objects() {
            if let Some(dict) = object.into_dict()
                && let Some(page) = Page::new(
                    &dict,
                    &PagesContext::new(),
                    Resources::new(Dict::empty(), None, ctx),
                    true,
                )
            {
                pages.push(page);
            }
        }

        if pages.is_empty() {
            return None;
        }

        Some(Self { pages, xref })
    }

    /// Return the xref table (of the document the pages belong to).   
    pub fn xref(&self) -> &'a XRef {
        self.xref
    }
}

impl<'a> Deref for Pages<'a> {
    type Target = [Page<'a>];

    fn deref(&self) -> &Self::Target {
        &self.pages
    }
}

fn resolve_pages<'a>(
    pages_dict: &Dict<'a>,
    entries: &mut Vec<Page<'a>>,
    mut ctx: PagesContext,
    resources: Resources<'a>,
) -> Option<()> {
    if let Some(media_box) = pages_dict.get::<Rect>(MEDIA_BOX) {
        ctx.media_box = Some(media_box);
    }

    if let Some(crop_box) = pages_dict.get::<Rect>(CROP_BOX) {
        ctx.crop_box = Some(crop_box);
    }

    if let Some(rotate) = pages_dict.get::<i32>(ROTATE) {
        ctx.rotate = Some(rotate);
    }

    let resources = Resources::from_parent(
        pages_dict.get::<Dict<'_>>(RESOURCES).unwrap_or_default(),
        resources.clone(),
    );

    let kids = pages_dict.get::<Array<'a>>(KIDS)?;

    for dict in kids.iter::<Dict<'_>>() {
        match dict.get::<Name<'_>>(TYPE).as_deref() {
            Some(PAGES) => {
                resolve_pages(&dict, entries, ctx.clone(), resources.clone());
            }
            // Let's be lenient and assume it's a `Page` in case it's `None` or something else
            // (see corpus test case 0083781).
            _ => {
                if let Some(page) = Page::new(&dict, &ctx, resources.clone(), false) {
                    entries.push(page);
                }
            }
        }
    }

    Some(())
}

/// The rotation of the page.
#[derive(Debug, Copy, Clone)]
pub enum Rotation {
    /// No rotation.
    None,
    /// A rotation of 90 degrees.
    Horizontal,
    /// A rotation of 180 degrees.
    Flipped,
    /// A rotation of 270 degrees.
    FlippedHorizontal,
}

/// A PDF page.
pub struct Page<'a> {
    inner: Dict<'a>,
    media_box: Rect,
    crop_box: Rect,
    rotation: Rotation,
    user_unit: Result<f64, UserUnitError>,
    page_streams: OnceLock<Result<Option<Vec<u8>>, PageStreamError>>,
    resources: Resources<'a>,
    ctx: ReaderContext<'a>,
}

/// A failure that prevents complete page-content interpretation.
#[derive(Debug, Copy, Clone)]
pub enum PageStreamError {
    /// Contents or an array member is not a resolvable stream.
    InvalidContents,
    /// A content stream references an unsupported external file.
    ExternalStream,
    /// A content stream failed to decode.
    Decode(DecodeFailure),
}

/// A page `UserUnit` declaration that cannot define supported physical geometry.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum UserUnitError {
    /// The entry is not a resolvable finite positive number or null.
    Invalid,
    /// The value exceeds the supported maximum of 75,000 points per user unit.
    OutOfRange,
    /// The scaled page dimensions overflow or underflow the rendering API.
    GeometryRange,
}

fn read_user_unit(dict: &Dict<'_>, xref: &XRef) -> Result<f64, UserUnitError> {
    // UserUnit is page-local, not an inheritable page-tree attribute (table 30).
    // Null dictionary values and undefined references mean absence (§7.3.9).
    if !dict.contains_key(USER_UNIT)
        || dict
            .get_ref(USER_UNIT)
            .is_some_and(|id| !xref.contains_object(id.into()))
        || dict.get::<Null>(USER_UNIT).is_some()
    {
        return Ok(1.0);
    }
    let unit = dict
        .get::<UserUnitNumber>(USER_UNIT)
        .ok_or(UserUnitError::Invalid)?
        .0;
    if !unit.is_finite() || unit <= 0.0 {
        return Err(UserUnitError::Invalid);
    }
    if unit > 75_000.0 {
        return Err(UserUnitError::OutOfRange);
    }
    Ok(unit)
}

// An indirect object containing `5 0 R` is not the numeric value 5. Keep the
// generic number reader's recovery unchanged while rejecting such aliases and
// malformed numeric token suffixes at this physical-geometry boundary.
#[derive(Debug, Clone)]
struct UserUnitNumber(f64);

impl<'a> Readable<'a> for UserUnitNumber {
    fn read(r: &mut Reader<'a>, ctx: &ReaderContext<'a>) -> Option<Self> {
        if r.clone().read_with_context::<ObjRef>(ctx).is_some() {
            return None;
        }
        Number::skip(&mut r.clone(), ctx.in_content_stream())?;
        Number::read(r, ctx).map(|number| Self(number.as_f64()))
    }
}

impl<'a> TryFrom<Object<'a>> for UserUnitNumber {
    type Error = ();

    fn try_from(value: Object<'a>) -> Result<Self, Self::Error> {
        match value {
            Object::Number(number) => Ok(Self(number.as_f64())),
            _ => Err(()),
        }
    }
}

impl<'a> ObjectLike<'a> for UserUnitNumber {}

fn content_stream(object: Object<'_>) -> Result<Stream<'_>, PageStreamError> {
    match object {
        Object::Stream(stream) => Ok(stream),
        Object::Dict(dict) if dict.contains_key(F) => Err(PageStreamError::ExternalStream),
        _ => Err(PageStreamError::InvalidContents),
    }
}

fn resolve_content_object<'a>(
    object: MaybeRef<Object<'a>>,
    ctx: &ReaderContext<'a>,
) -> Result<Object<'a>, PageStreamError> {
    let reference = object.as_obj_ref();
    object.resolve(ctx).ok_or_else(|| {
        // The stream parser deliberately refuses external streams. Read only
        // the dictionary after that failure so callers can diagnose the boundary.
        if reference
            .and_then(|reference| ctx.xref().get_with::<Dict<'_>>(reference.into(), ctx))
            .is_some_and(|dict| dict.contains_key(F))
        {
            PageStreamError::ExternalStream
        } else {
            PageStreamError::InvalidContents
        }
    })
}

impl<'a> Page<'a> {
    fn new(
        dict: &Dict<'a>,
        ctx: &PagesContext,
        resources: Resources<'a>,
        brute_force: bool,
    ) -> Option<Self> {
        // In general, pages without content are allowed, but in case we are brute-forcing
        // we ignore them.
        if brute_force && !dict.contains_key(CONTENTS) {
            return None;
        }

        let media_box = dict.get::<Rect>(MEDIA_BOX).or(ctx.media_box).unwrap_or(A4);

        let crop_box = dict
            .get::<Rect>(CROP_BOX)
            .or(ctx.crop_box)
            .unwrap_or(media_box);

        let rotation = match dict
            .get::<i32>(ROTATE)
            .or(ctx.rotate)
            .unwrap_or(0)
            .rem_euclid(360)
        {
            0 => Rotation::None,
            90 => Rotation::Horizontal,
            180 => Rotation::Flipped,
            270 => Rotation::FlippedHorizontal,
            _ => Rotation::None,
        };

        let ctx = resources.ctx.clone();
        let resources = Resources::from_parent(
            dict.get::<Dict<'_>>(RESOURCES).unwrap_or_default(),
            resources,
        );

        let mut page = Self {
            inner: dict.clone(),
            media_box,
            crop_box,
            rotation,
            user_unit: read_user_unit(dict, ctx.xref()),
            page_streams: OnceLock::new(),
            resources,
            ctx,
        };
        if let Ok(unit) = page.user_unit {
            let (width, height) = page.base_dimensions();
            if [width, height].into_iter().any(|dimension| {
                let scaled = (f64::from(dimension) * unit) as f32;
                !scaled.is_finite() || scaled <= 0.0
            }) {
                page.user_unit = Err(UserUnitError::GeometryRange);
            }
        }
        Some(page)
    }

    fn operations_impl(&self) -> Option<UntypedIter<'_>> {
        let stream = self.page_stream()?;
        let iter = UntypedIter::new(stream);

        Some(iter)
    }

    /// Return the decoded content stream of the page.
    ///
    /// Use [`Self::page_stream_checked`] to distinguish absent contents from a
    /// decoding failure. A failed array member invalidates the entire stream.
    pub fn page_stream(&self) -> Option<&[u8]> {
        self.page_stream_checked().ok().flatten()
    }

    /// Return cached decoded contents, preserving failures before operation iteration.
    ///
    /// Every array member must resolve to a decodable stream. No valid prefix or
    /// suffix is returned if a member fails. Absent or null Contents means an empty page.
    pub fn page_stream_checked(&self) -> Result<Option<&[u8]>, PageStreamError> {
        self.page_streams
            .get_or_init(|| {
                if !self.inner.contains_key(CONTENTS) {
                    return Ok(None);
                }
                let contents = resolve_content_object(
                    self.inner
                        .get_raw::<Object<'_>>(CONTENTS)
                        .ok_or(PageStreamError::InvalidContents)?,
                    self.inner.ctx(),
                )?;
                if let Object::Array(array) = contents {
                    let mut collected = vec![];
                    for member in array.raw_iter() {
                        let object = resolve_content_object(member, self.inner.ctx())?;
                        let stream = content_stream(object)?;
                        let data = stream.decoded().map_err(PageStreamError::Decode)?;
                        collected.extend_from_slice(&data);
                        // Streams must have at least one whitespace in-between.
                        collected.push(b' ');
                    }
                    Ok(Some(collected))
                } else if matches!(contents, Object::Null(_)) {
                    Ok(None)
                } else {
                    let stream = content_stream(contents)?;
                    Ok(Some(
                        stream
                            .decoded()
                            .map_err(PageStreamError::Decode)?
                            .into_owned(),
                    ))
                }
            })
            .as_ref()
            .map(|data| data.as_deref())
            .map_err(|error| *error)
    }

    /// Get the resources of the page.
    pub fn resources(&self) -> &Resources<'a> {
        &self.resources
    }

    /// Get the media box of the page.
    pub fn media_box(&self) -> Rect {
        self.media_box
    }

    /// Get the rotation of the page.
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// The size of one default user-space unit in physical 1/72-inch points.
    ///
    /// Absent/null entries default to 1. The supported range is positive finite
    /// values through 75,000 whose scaled dimensions fit the rendering API.
    /// Rendering methods use 1 for an invalid/unsupported declaration, as
    /// permitted by PDF 1.7 §8.3.2.3. Strict consumers must handle this error
    /// before admitting native rendering.
    pub fn user_unit(&self) -> Result<f64, UserUnitError> {
        self.user_unit
    }

    /// Get the crop box of the page.
    pub fn crop_box(&self) -> Rect {
        self.crop_box
    }

    /// Return the intersection of crop box and media box.
    pub fn intersected_crop_box(&self) -> Rect {
        self.crop_box().intersect(self.media_box())
    }

    /// Return the base dimensions of the page (same as `intersected_crop_box`, but with special
    /// handling applied for zero-area pages).
    pub fn base_dimensions(&self) -> (f32, f32) {
        let crop_box = self.intersected_crop_box();

        if (crop_box.width() as f32).is_nearly_zero() || (crop_box.height() as f32).is_nearly_zero()
        {
            (A4.width() as f32, A4.height() as f32)
        } else {
            (
                crop_box.width().max(1.0) as f32,
                crop_box.height().max(1.0) as f32,
            )
        }
    }

    /// Return the width and height in physical 1/72-inch points for rendering.
    ///
    /// Depending on the document, it is either based on the media box or the crop box
    /// of the page. Rotation and the supported `UserUnit` scale are applied.
    pub fn render_dimensions(&self) -> (f32, f32) {
        let (width, height) = self.unscaled_render_dimensions();
        let unit = self.user_unit.unwrap_or(1.0);
        (
            (f64::from(width) * unit) as f32,
            (f64::from(height) * unit) as f32,
        )
    }

    fn unscaled_render_dimensions(&self) -> (f32, f32) {
        let (mut base_width, mut base_height) = self.base_dimensions();

        if matches!(
            self.rotation(),
            Rotation::Horizontal | Rotation::FlippedHorizontal
        ) {
            core::mem::swap(&mut base_width, &mut base_height);
        }

        (base_width, base_height)
    }

    /// Return an untyped iterator over the operators of the page's content stream.
    pub fn operations(&self) -> UntypedIter<'_> {
        self.operations_impl().unwrap_or(UntypedIter::empty())
    }

    /// Get the raw dictionary of the page.
    pub fn raw(&self) -> &Dict<'a> {
        &self.inner
    }

    /// Get the xref table (of the document the page belongs to).
    pub fn xref(&self) -> &'a XRef {
        self.ctx.xref()
    }

    /// Return a typed iterator over the operators of the page's content stream.
    pub fn typed_operations(&self) -> TypedIter<'_> {
        TypedIter::from_untyped(self.operations())
    }

    /// Return the initial transform that should be applied when rendering.
    ///
    /// This accounts for the mismatch between PDF's y-up and most renderers'
    /// y-down coordinate system, the rotation of the page and the offset of
    /// the crop box, and scales default user space into physical points using
    /// `UserUnit`. Raw page boxes and base dimensions remain in source user units.
    pub fn initial_transform(&self, invert_y: bool) -> Transform {
        let crop_box = self.intersected_crop_box();
        let (_, base_height) = self.base_dimensions();
        let (width, height) = self.unscaled_render_dimensions();

        let horizontal_t = Transform::ROTATE_CW_90 * Transform::translate((0.0, -width as f64));
        let flipped_horizontal_t =
            Transform::translate((0.0, height as f64)) * Transform::ROTATE_CCW_90;

        let rotation_transform = match self.rotation() {
            Rotation::None => Transform::IDENTITY,
            Rotation::Horizontal => {
                if invert_y {
                    horizontal_t
                } else {
                    flipped_horizontal_t
                }
            }
            Rotation::Flipped => {
                Transform::scale(-1.0) * Transform::translate((-width as f64, -height as f64))
            }
            Rotation::FlippedHorizontal => {
                if invert_y {
                    flipped_horizontal_t
                } else {
                    horizontal_t
                }
            }
        };

        let inversion_transform = if invert_y {
            Transform::new([1.0, 0.0, 0.0, -1.0, 0.0, base_height as f64])
        } else {
            Transform::IDENTITY
        };

        Transform::scale(self.user_unit.unwrap_or(1.0))
            * rotation_transform
            * inversion_transform
            * Transform::translate((-crop_box.x0, -crop_box.y0))
    }
}

/// A structure keeping track of the resources of a page.
#[derive(Clone, Debug)]
pub struct Resources<'a> {
    parent: Option<Box<Self>>,
    ctx: ReaderContext<'a>,
    /// The raw dictionary of external graphics states.
    pub ext_g_states: Dict<'a>,
    /// The raw dictionary of fonts.
    pub fonts: Dict<'a>,
    /// The raw dictionary of properties.
    pub properties: Dict<'a>,
    /// The raw dictionary of color spaces.
    pub color_spaces: Dict<'a>,
    /// The raw dictionary of x objects.
    pub x_objects: Dict<'a>,
    /// The raw dictionary of patterns.
    pub patterns: Dict<'a>,
    /// The raw dictionary of shadings.
    pub shadings: Dict<'a>,
}

impl<'a> Resources<'a> {
    /// Create a new `Resources` object from a dictionary with a parent.
    pub fn from_parent(resources: Dict<'a>, parent: Self) -> Self {
        let ctx = parent.ctx.clone();

        Self::new(resources, Some(parent), &ctx)
    }

    /// Create a new `Resources` object.
    pub(crate) fn new(resources: Dict<'a>, parent: Option<Self>, ctx: &ReaderContext<'a>) -> Self {
        let ext_g_states = resources.get::<Dict<'_>>(EXT_G_STATE).unwrap_or_default();
        let fonts = resources.get::<Dict<'_>>(FONT).unwrap_or_default();
        let color_spaces = resources.get::<Dict<'_>>(COLORSPACE).unwrap_or_default();
        let x_objects = resources.get::<Dict<'_>>(XOBJECT).unwrap_or_default();
        let patterns = resources.get::<Dict<'_>>(PATTERN).unwrap_or_default();
        let shadings = resources.get::<Dict<'_>>(SHADING).unwrap_or_default();
        let properties = resources.get::<Dict<'_>>(PROPERTIES).unwrap_or_default();

        let parent = parent.map(Box::new);

        Self {
            parent,
            ext_g_states,
            fonts,
            color_spaces,
            properties,
            x_objects,
            patterns,
            shadings,
            ctx: ctx.clone(),
        }
    }

    fn get_resource<T: ObjectLike<'a>>(&self, name: &Name<'_>, dict: &Dict<'a>) -> Option<T> {
        dict.get::<T>(name.deref())
    }

    /// Get the parent in the resource, chain, if available.
    pub fn parent(&self) -> Option<&Self> {
        self.parent.as_deref()
    }

    /// Get an external graphics state by name.
    pub fn get_ext_g_state(&self, name: &Name<'_>) -> Option<Dict<'a>> {
        self.get_resource::<Dict<'_>>(name, &self.ext_g_states)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_ext_g_state(name)))
    }

    /// Get a color space by name.
    pub fn get_color_space(&self, name: &Name<'_>) -> Option<Object<'a>> {
        self.get_resource::<Object<'_>>(name, &self.color_spaces)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_color_space(name)))
    }

    /// Get a font by name.
    pub fn get_font(&self, name: &Name<'_>) -> Option<Dict<'a>> {
        self.get_resource::<Dict<'_>>(name, &self.fonts)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_font(name)))
    }

    /// Get a pattern by name.
    pub fn get_pattern(&self, name: &Name<'_>) -> Option<Object<'a>> {
        self.get_resource::<Object<'_>>(name, &self.patterns)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_pattern(name)))
    }

    /// Get an x object by name.
    pub fn get_x_object(&self, name: &Name<'_>) -> Option<Stream<'a>> {
        self.get_resource::<Stream<'_>>(name, &self.x_objects)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_x_object(name)))
    }

    /// Get a shading by name.
    pub fn get_shading(&self, name: &Name<'_>) -> Option<Object<'a>> {
        self.get_resource::<Object<'_>>(name, &self.shadings)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get_shading(name)))
    }
}

// <https://github.com/apache/pdfbox/blob/a53a70db16ea3133994120bcf1e216b9e760c05b/pdfbox/src/main/java/org/apache/pdfbox/pdmodel/common/PDRectangle.java#L38>
const POINTS_PER_INCH: f64 = 72.0;
const POINTS_PER_MM: f64 = 1.0 / (10.0 * 2.54) * POINTS_PER_INCH;

/// The dimension of an A4 page.
pub const A4: Rect = Rect {
    x0: 0.0,
    y0: 0.0,
    x1: 210.0 * POINTS_PER_MM,
    y1: 297.0 * POINTS_PER_MM,
};

pub(crate) mod cached {
    use crate::page::Pages;
    use crate::reader::ReaderContext;
    use crate::xref::XRef;
    use core::ops::Deref;

    // Keep in sync with the implementation in `sync`. We duplicate it here
    // to make it more visible since we have unsafe code here.
    #[cfg(feature = "std")]
    pub(crate) use std::sync::Arc;

    #[cfg(not(feature = "std"))]
    pub(crate) use alloc::rc::Rc as Arc;

    pub(crate) struct CachedPages {
        pages: Pages<'static>,
        // NOTE: `pages` references the data in `xref`, so it's important that `xref`
        // appears after `pages` in the struct definition to ensure correct drop order.
        _xref: Arc<XRef>,
    }

    impl CachedPages {
        pub(crate) fn new(xref: Arc<XRef>) -> Option<Self> {
            // SAFETY:
            // - The XRef's location is stable in memory:
            //   - We wrapped it in a `Arc` (or `Rc` in `no_std`), which implements `StableDeref`.
            //   - The struct owns the `Arc`, ensuring that the inner value is not dropped during the whole
            //     duration.
            // - The internal 'static lifetime is not leaked because its rewritten
            //   to the self-lifetime in `pages()`.
            let xref_reference: &'static XRef = unsafe { core::mem::transmute(xref.deref()) };

            let ctx = ReaderContext::new(xref_reference, false);
            let pages = xref_reference
                .get_with(xref.trailer_data().pages_ref, &ctx)
                .and_then(|p| Pages::new(&p, &ctx, xref_reference))
                .or_else(|| Pages::new_brute_force(&ctx, xref_reference))?;

            Some(Self { pages, _xref: xref })
        }

        pub(crate) fn get(&self) -> &Pages<'_> {
            &self.pages
        }
    }
}
