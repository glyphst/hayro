use crate::CacheKey;
use crate::color::{Color, ColorSpaceKind};
use crate::pattern::Pattern;
use crate::util::hash128;
use crate::x_object::ImageXObject;
use crate::x_object::soft_mask::SoftMask;
use hayro_syntax::object::Stream;
use kurbo::{Affine, BezPath, Cap, Join};
use smallvec::{SmallVec, smallvec};

/// A clip path.
#[derive(Debug, Clone)]
pub struct ClipPath {
    /// The clipping path.
    pub path: BezPath,
    /// The fill rule.
    pub fill: FillRule,
}

impl CacheKey for ClipPath {
    fn cache_key(&self) -> u128 {
        hash128(&(&self.path.to_svg(), &self.fill))
    }
}

/// A stencil image.
pub struct StencilImage<'a, 'b> {
    pub(crate) paint: Paint<'a>,
    pub(crate) image_xobject: ImageXObject<'b>,
}

impl<'a, 'b> StencilImage<'a, 'b> {
    /// Perform some operation with the stencil data of the image.
    ///
    /// The second argument allows you to give the image decoder a hint for
    /// what resolution of the image you want to have. Note that this does not
    /// mean that the resulting image will have that dimension. Instead, it allows
    /// the image decoder to extract a lower-resolution version of the image in
    /// certain cases.
    pub fn with_stencil(
        &self,
        func: impl FnOnce(LumaData, &Paint<'a>),
        target_dimension: Option<(u32, u32)>,
    ) {
        if let Some(decoded) = self.image_xobject.decoded_mask(target_dimension) {
            func(decoded.luma, &self.paint);
        }
    }

    // These are hidden since clients are supposed to call get the
    // width/height from `LumaData` instead.
    #[doc(hidden)]
    pub fn width(&self) -> u32 {
        self.image_xobject.width()
    }

    #[doc(hidden)]
    pub fn height(&self) -> u32 {
        self.image_xobject.height()
    }
}

impl CacheKey for StencilImage<'_, '_> {
    fn cache_key(&self) -> u128 {
        self.image_xobject.cache_key()
    }
}

/// A raster image.
pub struct RasterImage<'a>(pub(crate) ImageXObject<'a>);

impl RasterImage<'_> {
    /// Return how a JPEG 2000 opacity channel is associated with its color samples.
    ///
    /// `None` means that the image does not have an active embedded opacity
    /// channel. In particular, `/SMaskInData` is ignored for non-JPX images as
    /// required by PDF.
    pub fn embedded_alpha_mode(&self) -> Option<EmbeddedImageAlphaMode> {
        self.0.embedded_alpha_mode()
    }

    /// Return typed source/effective color-space properties for this image.
    ///
    /// The decoded pixels returned by [`Self::with_rgba`] already reflect the
    /// effective space. Retained consumers can use this value to prove that a
    /// `DefaultGray`, `DefaultRGB`, or `DefaultCMYK` resource was applied
    /// instead of silently treating the declared device space as final.
    pub fn color_space_properties(&self) -> ImageColorSpaceProperties {
        self.0.color_space_properties
    }

    /// Perform some operation with the RGB and alpha channel of the image.
    ///
    /// The second argument allows you to give the image decoder a hint for
    /// what resolution of the image you want to have. Note that this does not
    /// mean that the resulting image will have that dimension. Instead, it allows
    /// the image decoder to extract a lower-resolution version of the image in
    /// certain cases.
    pub fn with_rgba(
        &self,
        func: impl FnOnce(ImageData, Option<LumaData>),
        target_dimension: Option<(u32, u32)>,
    ) {
        if let Some(decoded) = self.0.decoded_image(target_dimension) {
            func(decoded.image, decoded.alpha);
        }
    }

    /// Perform an operation with decoded direct `DeviceCMYK` samples and alpha.
    ///
    /// The callback is invoked only when the image resolves directly to
    /// `DeviceCMYK` and neither a transfer function nor soft-mask matte requires
    /// modifying the source components. Decode arrays and encoded-image color
    /// transforms are applied before the normalized eight-bit samples are
    /// returned. Other image color spaces continue through [`Self::with_rgba`].
    pub fn with_device_cmyk(
        &self,
        func: impl FnOnce(CmykData, Option<LumaData>),
        target_dimension: Option<(u32, u32)>,
    ) {
        if let Some(decoded) = self.0.decoded_device_cmyk_image(target_dimension) {
            func(decoded.image, decoded.alpha);
        }
    }

    /// Return the underlying stream object.
    ///
    /// This allows you to get access to the raw encoded image data, without doing any decoding.
    pub fn stream(&self) -> &Stream<'_> {
        self.0.stream()
    }

    // These are hidden since clients are supposed to call get the
    // width/height from `LumaData` instead.
    #[doc(hidden)]
    pub fn width(&self) -> u32 {
        self.0.width()
    }

    #[doc(hidden)]
    pub fn height(&self) -> u32 {
        self.0.height()
    }
}

/// The association between JPEG 2000 color samples and an embedded opacity channel.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EmbeddedImageAlphaMode {
    /// Color samples are independent of the opacity samples (`/SMaskInData 1`).
    Unassociated,
    /// Color samples are preblended with a matte (`/SMaskInData 2`).
    Premultiplied,
}

/// Typed source/effective color-space properties for a raster image.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ImageColorSpaceProperties {
    pub(crate) has_color_space: bool,
    pub(crate) declared_color_space_kind: Option<ColorSpaceKind>,
    pub(crate) color_space_kind: Option<ColorSpaceKind>,
    pub(crate) color_space_components: Option<usize>,
    pub(crate) color_space_default_overridden: bool,
}

impl ImageColorSpaceProperties {
    /// Whether the image explicitly declares `ColorSpace`/`CS`.
    pub fn has_color_space(self) -> bool {
        self.has_color_space
    }

    /// Parsed family named by the image before default-device remapping.
    pub fn declared_color_space_kind(self) -> Option<ColorSpaceKind> {
        self.declared_color_space_kind
    }

    /// Parsed family actually used to decode the image.
    pub fn color_space_kind(self) -> Option<ColorSpaceKind> {
        self.color_space_kind
    }

    /// Native component count accepted by the effective color space.
    pub fn color_space_components(self) -> Option<usize> {
        self.color_space_components
    }

    /// Whether a default device-space resource replaces the declared space or
    /// an underlying Indexed base or selected Separation/DeviceN alternative.
    pub fn color_space_is_default_overridden(self) -> bool {
        self.color_space_default_overridden
    }
}

impl CacheKey for RasterImage<'_> {
    fn cache_key(&self) -> u128 {
        self.0.cache_key()
    }
}

/// A type of image.
pub enum Image<'a, 'b> {
    /// A stencil image.
    Stencil(StencilImage<'a, 'b>),
    /// A normal raster image.
    Raster(RasterImage<'b>),
}

impl Image<'_, '_> {
    // These are hidden since clients are supposed to call get the
    // width/height from `LumaData/RgbData` instead.
    #[doc(hidden)]
    pub fn width(&self) -> u32 {
        match self {
            Image::Stencil(s) => s.width(),
            Image::Raster(r) => r.width(),
        }
    }

    // These are hidden since clients are supposed to call get the
    // width/height from `LumaData/RgbData` instead.
    #[doc(hidden)]
    pub fn height(&self) -> u32 {
        match self {
            Image::Stencil(s) => s.height(),
            Image::Raster(r) => r.height(),
        }
    }
}

impl CacheKey for Image<'_, '_> {
    fn cache_key(&self) -> u128 {
        match self {
            Image::Stencil(i) => i.cache_key(),
            Image::Raster(i) => i.cache_key(),
        }
    }
}

/// A structure holding 3-channel RGB data.
#[derive(Clone)]
pub struct RgbData {
    /// The actual data. It is guaranteed to have the length width * height * 3.
    pub data: Vec<u8>,
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
    /// Whether the image should be interpolated.
    pub interpolate: bool,
    /// Additional scaling factors to apply to the image.
    ///
    /// In most cases, those factors will just be 1.0, and you can
    /// ignore them. There are two situations in which they will not be equal
    /// to 1:
    /// 1) The PDF provided wrong metadata about the width/height of the image,
    ///    which needs to be corrected
    /// 2) A lower resolution of the image was requested, in which case it needs
    ///    to be scaled up so that it still covers the same area.
    ///
    /// The first number indicates the x scaling factor, the second number the
    /// y scaling factor.
    pub scale_factors: (f32, f32),
}

/// A structure holding normalized four-channel direct `DeviceCMYK` data.
#[derive(Clone)]
pub struct CmykData {
    /// Interleaved C, M, Y, and K bytes. Its length is width * height * 4.
    pub data: Vec<u8>,
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
    /// Whether the image should be interpolated.
    pub interpolate: bool,
    /// Additional scaling factors to apply to the image.
    ///
    /// These have the same correction and reduced-resolution meaning as
    /// [`RgbData::scale_factors`].
    pub scale_factors: (f32, f32),
}

/// A structure holding 1-channel luma data.
#[derive(Clone)]
pub struct LumaData {
    /// The actual data. It is guaranteed to have the length width * height.
    pub data: Vec<u8>,
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
    /// Whether the image should be interpolated.
    pub interpolate: bool,
    /// Additional scaling factors to apply to the image.
    ///
    /// In most cases, those factors will just be 1.0, and you can
    /// ignore them. There are two situations in which they will not be equal
    /// to 1:
    /// 1) The PDF provided wrong metadata about the width/height of the image,
    ///    which needs to be corrected
    /// 2) A lower resolution of the image was requested, in which case it needs
    ///    to be scaled up so that it still covers the same area.
    ///
    /// The first number indicates the x scaling factor, the second number the
    /// y scaling factor.
    pub scale_factors: (f32, f32),
}

/// The color data of a raster image, either 3-channel RGB or 1-channel luma.
#[derive(Clone)]
pub enum ImageData {
    /// 3-channel RGB data.
    Rgb(RgbData),
    /// 1-channel grayscale data.
    Luma(LumaData),
}

impl ImageData {
    /// The width of the image.
    pub fn width(&self) -> u32 {
        match self {
            Self::Rgb(d) => d.width,
            Self::Luma(d) => d.width,
        }
    }

    /// The height of the image.
    pub fn height(&self) -> u32 {
        match self {
            Self::Rgb(d) => d.height,
            Self::Luma(d) => d.height,
        }
    }

    /// Whether the image should be interpolated.
    pub fn interpolate(&self) -> bool {
        match self {
            Self::Rgb(d) => d.interpolate,
            Self::Luma(d) => d.interpolate,
        }
    }

    /// The scaling factors of the image.
    pub fn scale_factors(&self) -> (f32, f32) {
        match self {
            Self::Rgb(d) => d.scale_factors,
            Self::Luma(d) => d.scale_factors,
        }
    }
}

/// A type of paint.
#[derive(Clone, Debug)]
pub enum Paint<'a> {
    /// A solid RGBA color.
    Color(Color),
    /// A PDF pattern.
    Pattern(Box<Pattern<'a>>),
}

impl CacheKey for Paint<'_> {
    fn cache_key(&self) -> u128 {
        match self {
            Paint::Color(c) => {
                // TODO: We should actually cache the color with color space etc., not just the
                // RGBA8 version.
                hash128(&c.to_rgba().to_rgba8())
            }
            Paint::Pattern(p) => p.cache_key(),
        }
    }
}

/// Properties for a painted drawing operation.
#[derive(Clone)]
pub struct DrawProps<'a> {
    /// The transform.
    pub transform: Affine,
    /// The paint.
    pub paint: Paint<'a>,
    /// Unapplied transfer for a raw color paint when the interpreter's
    /// `defer_transfer_functions` setting is enabled. Identity/Default are
    /// represented by `None`. Pattern paints retain their own transfer state.
    pub deferred_transfer_function: Option<crate::ActiveTransferFunction>,
    /// The soft mask.
    pub soft_mask: Option<SoftMask<'a>>,
    /// The blend mode.
    pub blend_mode: BlendMode,
    /// The current stroking or nonstroking alpha constant before it is folded
    /// into the retained paint alpha.
    ///
    /// Devices that preserve PDF shape separately from opacity need this
    /// scalar when [`Self::alpha_is_shape`] is true.
    pub alpha_constant: f32,
    /// Whether the current alpha constant and soft mask contribute to object
    /// shape instead of object opacity.
    ///
    /// This is the PDF graphics-state `/AIS` parameter. Keeping it separate
    /// from the paint alpha is required for exact knockout compositing.
    pub alpha_is_shape: bool,
}

/// Properties for an image drawing operation.
#[derive(Clone)]
pub struct ImageDrawProps<'a> {
    /// The transform.
    pub transform: Affine,
    /// Unapplied transfer for the raw raster or stencil paint when transfer
    /// deferral is enabled. Image alpha and masks do not affect this function's
    /// identity; they separately determine whether the image is fully opaque.
    pub deferred_transfer_function: Option<crate::ActiveTransferFunction>,
    /// The soft mask.
    pub soft_mask: Option<SoftMask<'a>>,
    /// The blend mode.
    pub blend_mode: BlendMode,
    /// Whether the current alpha constant and soft mask contribute to object
    /// shape instead of object opacity (`/AIS`).
    pub alpha_is_shape: bool,
}

/// The draw mode.
#[derive(Clone, Debug)]
pub enum DrawMode {
    /// Draw using a fill.
    Fill(FillRule),
    /// Draw using a stroke.
    Stroke(StrokeProps),
    /// Draw using both fill and stroke.
    FillAndStroke(FillRule, StrokeProps),
    /// Invisible text (for text extraction but not visual rendering).
    Invisible,
}

/// Stroke properties.
#[derive(Clone, Debug)]
pub struct StrokeProps {
    /// The line width.
    pub line_width: f32,
    /// The line cap.
    pub line_cap: Cap,
    /// The line join.
    pub line_join: Join,
    /// The miter limit.
    pub miter_limit: f32,
    /// The dash array.
    pub dash_array: SmallVec<[f32; 4]>,
    /// The dash offset.
    pub dash_offset: f32,
    /// Whether device-dependent automatic stroke adjustment is enabled.
    ///
    /// This is the PDF graphics-state `/SA` parameter. Devices must retain it
    /// because its effect depends on the final user-to-device transform.
    pub automatic_adjustment: bool,
}

impl Default for StrokeProps {
    fn default() -> Self {
        Self {
            line_width: 1.0,
            line_cap: Cap::Butt,
            line_join: Join::Miter,
            miter_limit: 10.0,
            dash_array: smallvec![],
            dash_offset: 0.0,
            automatic_adjustment: false,
        }
    }
}

/// A fill rule.
#[derive(Clone, Debug, Copy, Hash, PartialEq, Eq)]
pub enum FillRule {
    /// Non-zero filling.
    NonZero,
    /// Even-odd filling.
    EvenOdd,
}

/// A blend mode.
#[derive(Clone, Debug, Copy, Hash, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// Normal blend mode (default).
    #[default]
    Normal,
    /// Multiply blend mode.
    Multiply,
    /// Screen blend mode.
    Screen,
    /// Overlay blend mode.
    Overlay,
    /// Darken blend mode.
    Darken,
    /// Lighten blend mode.
    Lighten,
    /// `ColorDodge` blend mode.
    ColorDodge,
    /// `ColorBurn` blend mode.
    ColorBurn,
    /// `HardLight` blend mode.
    HardLight,
    /// `SoftLight` blend mode.
    SoftLight,
    /// Difference blend mode.
    Difference,
    /// Exclusion blend mode.
    Exclusion,
    /// Hue blend mode.
    Hue,
    /// Saturation blend mode.
    Saturation,
    /// Color blend mode.
    Color,
    /// Luminosity blend mode.
    Luminosity,
}
