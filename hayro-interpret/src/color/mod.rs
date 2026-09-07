//! PDF colors and color spaces.

mod cal_gray;
mod cal_rgb;
mod device_cmyk;
mod device_gray;
mod device_n;
mod device_rgb;
mod icc;
mod image;
mod indexed;
mod intent;
mod lab;
mod pattern;
mod separation;

use self::cal_gray::CalGray;
use self::cal_rgb::CalRgb;
use self::device_cmyk::DeviceCmyk;
use self::device_gray::DeviceGray;
use self::device_n::DeviceN;
use self::device_rgb::DeviceRgb;
use self::icc::ICCProfile;
use self::indexed::Indexed;
use self::lab::Lab;
use self::pattern::Pattern;
use self::separation::Separation;
use crate::cache::{Cache, CacheKey};
use hayro_syntax::object::Dict;
use hayro_syntax::object::Name;
use hayro_syntax::object::Object;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::*;
pub use image::ImageColorSpaceError;
pub use intent::{ColorConversionError, RenderingIntent};
use smallvec::{SmallVec, smallvec};
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

/// A storage for the components of colors.
pub type ColorComponents = SmallVec<[f32; 4]>;

/// The semantic family of a PDF color space.
///
/// This deliberately exposes only stable classification data. It lets retained
/// renderers choose a spec-defined conversion without borrowing Hayro's
/// internal color-space implementation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ColorSpaceKind {
    /// The device-dependent single-component gray space.
    DeviceGray,
    /// The device-dependent three-component RGB space.
    DeviceRgb,
    /// The device-dependent four-component CMYK space.
    DeviceCmyk,
    /// A calibrated single-component gray space.
    CalGray,
    /// A calibrated three-component RGB space.
    CalRgb,
    /// A CIE L*a*b* space.
    Lab,
    /// An ICC profile-backed space.
    IccBased,
    /// A palette-indexed space.
    Indexed,
    /// A single-colorant special space.
    Separation,
    /// A multi-colorant special space.
    DeviceN,
    /// A colored or uncolored pattern space.
    Pattern,
}

#[derive(Clone)]
pub(super) struct U8Lookup<T>(Arc<OnceLock<Option<Box<[T; 256]>>>>);

impl<T> Default for U8Lookup<T> {
    fn default() -> Self {
        Self(Arc::new(OnceLock::new()))
    }
}

impl<T> std::fmt::Debug for U8Lookup<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("U8Lookup")
    }
}

impl<T> U8Lookup<T> {
    pub(super) fn get_or_init_with(
        &self,
        init: impl FnOnce(&[u8; 256]) -> Option<Box<[T; 256]>>,
    ) -> Option<&[T; 256]> {
        self.0
            .get_or_init(|| {
                let input: [u8; 256] = core::array::from_fn(|index| index as u8);
                init(&input)
            })
            .as_deref()
    }
}

impl U8Lookup<[u8; 3]> {
    pub(super) fn get_or_init(
        &self,
        convert: impl FnOnce(&[u8], &mut [u8]) -> Option<()>,
    ) -> Option<&[[u8; 3]; 256]> {
        self.get_or_init_with(|input| {
            let mut output = Box::new([[0; 3]; 256]);
            convert(input, output.as_flattened_mut())?;
            Some(output)
        })
    }
}

/// An RGB color with an alpha channel.
#[derive(Debug, Copy, Clone)]
pub struct AlphaColor {
    components: [f32; 4],
}

impl AlphaColor {
    /// A black color.
    pub const BLACK: Self = Self::new([0., 0., 0., 1.]);

    /// A transparent color.
    pub const TRANSPARENT: Self = Self::new([0., 0., 0., 0.]);

    /// A white color.
    pub const WHITE: Self = Self::new([1., 1., 1., 1.]);

    /// Create a new color from the given components.
    pub const fn new(components: [f32; 4]) -> Self {
        Self { components }
    }

    /// Create a new color from RGB8 values.
    pub const fn from_rgb8(r: u8, g: u8, b: u8) -> Self {
        let components = [u8_to_f32(r), u8_to_f32(g), u8_to_f32(b), 1.];
        Self::new(components)
    }

    /// Return the color as premulitplied RGBF32.
    pub fn premultiplied(&self) -> [f32; 4] {
        [
            self.components[0] * self.components[3],
            self.components[1] * self.components[3],
            self.components[2] * self.components[3],
            self.components[3],
        ]
    }

    /// Create a new color from RGBA8 values.
    pub const fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        let components = [u8_to_f32(r), u8_to_f32(g), u8_to_f32(b), u8_to_f32(a)];
        Self::new(components)
    }

    /// Return the color as RGBA8.
    pub fn to_rgba8(&self) -> [u8; 4] {
        [
            (self.components[0] * 255.0 + 0.5) as u8,
            (self.components[1] * 255.0 + 0.5) as u8,
            (self.components[2] * 255.0 + 0.5) as u8,
            (self.components[3] * 255.0 + 0.5) as u8,
        ]
    }

    /// Return the components of the color as RGBF32.
    pub fn components(&self) -> [f32; 4] {
        self.components
    }
}

const fn u8_to_f32(x: u8) -> f32 {
    x as f32 * (1.0 / 255.0)
}

#[derive(Debug, Clone)]
pub(crate) enum ColorSpaceType {
    DeviceCmyk(DeviceCmyk),
    DeviceGray(DeviceGray),
    DeviceRgb(DeviceRgb),
    Pattern(Pattern),
    Indexed(Indexed),
    ICCBased(ICCProfile),
    CalGray(CalGray),
    CalRgb(CalRgb),
    Lab(Lab),
    Separation(Separation),
    DeviceN(DeviceN),
}

impl ColorSpaceType {
    fn new(object: Object<'_>, cache: &Cache) -> Option<Self> {
        Self::new_inner(object, cache, &mut |object| ColorSpace::new(object, cache))
    }

    fn new_inner<'a>(
        object: Object<'a>,
        cache: &Cache,
        resolve: &mut dyn FnMut(Object<'a>) -> Option<ColorSpace>,
    ) -> Option<Self> {
        if let Object::Name(name) = object {
            return Self::new_from_name(&name);
        } else if let Object::Array(color_array) = object {
            let mut iter = color_array.flex_iter();
            let name = iter.next::<Name<'_>>()?;

            match name.deref() {
                ICC_BASED => {
                    let icc_stream = iter.next::<Stream<'_>>()?;
                    let dict = icc_stream.dict();
                    let num_components = dict.get::<usize>(N)?;

                    let profile_cache_key = crate::util::hash128(&(
                        "icc-profile",
                        icc_stream.cache_key(),
                        icc_stream.raw_data().as_ref(),
                    ));
                    let profile = cache.get_or_insert_with(profile_cache_key, || {
                        let decoded = icc_stream.decoded().ok()?;
                        ICCProfile::new(&decoded, num_components).map(Self::ICCBased)
                    });
                    return profile;
                }
                CALCMYK => return Some(Self::DeviceCmyk(DeviceCmyk)),
                CALGRAY => {
                    if color_array.raw_iter().take(3).count() != 2 {
                        return None;
                    }
                    let cal_dict = iter.next::<Dict<'_>>()?;
                    return Some(Self::CalGray(CalGray::new(&cal_dict)?));
                }
                CALRGB => {
                    if color_array.raw_iter().take(3).count() != 2 {
                        return None;
                    }
                    let cal_dict = iter.next::<Dict<'_>>()?;
                    return Some(Self::CalRgb(CalRgb::new(&cal_dict)?));
                }
                DEVICE_RGB | RGB => return Some(Self::DeviceRgb(DeviceRgb)),
                DEVICE_GRAY | G => return Some(Self::DeviceGray(DeviceGray)),
                DEVICE_CMYK | CMYK => return Some(Self::DeviceCmyk(DeviceCmyk)),
                LAB => {
                    let lab_dict = iter.next::<Dict<'_>>()?;
                    return Some(Self::Lab(Lab::new(&lab_dict)?));
                }
                INDEXED | I => {
                    return Some(Self::Indexed(Indexed::new(&color_array, resolve)?));
                }
                SEPARATION => {
                    return Some(Self::Separation(Separation::new(&color_array, resolve)?));
                }
                DEVICE_N => {
                    return Some(Self::DeviceN(DeviceN::new(&color_array, resolve)?));
                }
                PATTERN => {
                    let cs = if color_array.raw_iter().take(2).count() == 1 {
                        ColorSpace::device_rgb()
                    } else {
                        resolve(iter.next::<Object<'_>>()?)?
                    };
                    return Some(Self::Pattern(Pattern::new(cs)));
                }
                _ => {
                    warn!("unsupported color space: {}", name.as_str());
                    return None;
                }
            }
        }

        None
    }

    fn new_from_name(name: &Name<'_>) -> Option<Self> {
        match name.deref() {
            DEVICE_RGB | RGB => Some(Self::DeviceRgb(DeviceRgb)),
            DEVICE_GRAY | G => Some(Self::DeviceGray(DeviceGray)),
            DEVICE_CMYK | CMYK => Some(Self::DeviceCmyk(DeviceCmyk)),
            CALCMYK => Some(Self::DeviceCmyk(DeviceCmyk)),
            PATTERN => Some(Self::Pattern(Pattern::new(ColorSpace::device_rgb()))),
            _ => None,
        }
    }
}

type IntentVariants = [OnceLock<Result<Arc<ColorSpaceType>, ColorConversionError>>; 4];

/// A PDF color space with lazily shared conversions for each rendering intent.
#[derive(Clone)]
pub struct ColorSpace(Arc<ColorSpaceType>, RenderingIntent, Arc<IntentVariants>);

impl std::fmt::Debug for ColorSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Derived cache population must not change retained invocation identity.
        f.debug_tuple("ColorSpace")
            .field(&self.0)
            .field(&self.1)
            .finish()
    }
}

impl ColorSpace {
    fn from_kind(kind: ColorSpaceType) -> Self {
        Self(
            Arc::new(kind),
            RenderingIntent::default(),
            Arc::new(std::array::from_fn(|_| OnceLock::new())),
        )
    }

    /// Bind this source color space and its derived palettes to a rendering intent.
    /// Invalid or unsupported source-to-output conversions return an error.
    pub fn with_rendering_intent(
        &self,
        intent: RenderingIntent,
    ) -> Result<Self, ColorConversionError> {
        let variant = self.2[intent as usize].get_or_init(|| {
            let kind = match self.0.as_ref() {
                ColorSpaceType::ICCBased(profile) => {
                    ColorSpaceType::ICCBased(profile.with_intent(intent)?)
                }
                ColorSpaceType::CalGray(_) | ColorSpaceType::CalRgb(_) | ColorSpaceType::Lab(_)
                    if intent != RenderingIntent::default() =>
                {
                    return Err(ColorConversionError::CalibratedIntent);
                }
                ColorSpaceType::Indexed(space) => {
                    ColorSpaceType::Indexed(space.with_intent(intent)?)
                }
                ColorSpaceType::Separation(space) => {
                    ColorSpaceType::Separation(space.with_intent(intent)?)
                }
                ColorSpaceType::DeviceN(space) => {
                    ColorSpaceType::DeviceN(space.with_intent(intent)?)
                }
                ColorSpaceType::Pattern(space) => ColorSpaceType::Pattern(Pattern::new(
                    space.color_space().with_rendering_intent(intent)?,
                )),
                kind => kind.clone(),
            };
            Ok(Arc::new(kind))
        });
        Ok(Self(variant.clone()?, intent, self.2.clone()))
    }

    /// The intent used for conversions and derived resource identity.
    pub fn rendering_intent(&self) -> RenderingIntent {
        self.1
    }

    /// Parse a self-contained PDF color-space object, preserving ICC identity.
    /// Resource aliases must be resolved by the caller.
    pub fn from_pdf_object(object: Object<'_>) -> Option<Self> {
        Some(Self::from_kind(ColorSpaceType::new(object, &Cache::new())?))
    }

    /// Whether handles share the same parsed and intent-bound color space.
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) && self.1 == other.1
    }

    pub(crate) fn new(object: Object<'_>, cache: &Cache) -> Option<Self> {
        Some(Self::from_kind(ColorSpaceType::new(object, cache)?))
    }

    pub(crate) fn new_from_name(name: &Name<'_>) -> Option<Self> {
        ColorSpaceType::new_from_name(name).map(Self::from_kind)
    }

    pub(crate) fn device_gray() -> Self {
        Self::from_kind(ColorSpaceType::DeviceGray(DeviceGray))
    }
    pub(crate) fn device_rgb() -> Self {
        Self::from_kind(ColorSpaceType::DeviceRgb(DeviceRgb))
    }
    pub(crate) fn device_cmyk() -> Self {
        Self::from_kind(ColorSpaceType::DeviceCmyk(DeviceCmyk))
    }
    pub(crate) fn pattern() -> Self {
        Self::from_kind(ColorSpaceType::Pattern(Pattern::new(Self::device_gray())))
    }

    pub(crate) fn pattern_cs(&self) -> Option<Self> {
        match self.0.as_ref() {
            ColorSpaceType::Pattern(pattern) => Some(pattern.color_space()),
            _ => None,
        }
    }

    /// Return the stable semantic family of this color space.
    pub fn kind(&self) -> ColorSpaceKind {
        match self.0.as_ref() {
            ColorSpaceType::DeviceGray(_) => ColorSpaceKind::DeviceGray,
            ColorSpaceType::DeviceRgb(_) => ColorSpaceKind::DeviceRgb,
            ColorSpaceType::DeviceCmyk(_) => ColorSpaceKind::DeviceCmyk,
            ColorSpaceType::CalGray(_) => ColorSpaceKind::CalGray,
            ColorSpaceType::CalRgb(_) => ColorSpaceKind::CalRgb,
            ColorSpaceType::Lab(_) => ColorSpaceKind::Lab,
            ColorSpaceType::ICCBased(_) => ColorSpaceKind::IccBased,
            ColorSpaceType::Indexed(_) => ColorSpaceKind::Indexed,
            ColorSpaceType::Separation(_) => ColorSpaceKind::Separation,
            ColorSpaceType::DeviceN(_) => ColorSpaceKind::DeviceN,
            ColorSpaceType::Pattern(_) => ColorSpaceKind::Pattern,
        }
    }

    /// Return the number of native components accepted by this color space.
    pub fn component_count(&self) -> usize {
        usize::from(self.num_components())
    }

    /// Return `true` if the current color space is the pattern color space.
    pub(crate) fn is_pattern(&self) -> bool {
        matches!(self.0.as_ref(), ColorSpaceType::Pattern(_))
    }

    /// Return `true` if the current color space is an indexed color space.
    pub(crate) fn is_indexed(&self) -> bool {
        matches!(self.0.as_ref(), ColorSpaceType::Indexed(_))
    }

    pub(crate) fn indexed_hival(&self) -> Option<u8> {
        match self.0.as_ref() {
            ColorSpaceType::Indexed(indexed) => Some(indexed.hival()),
            ColorSpaceType::Pattern(pattern) => pattern.color_space().indexed_hival(),
            _ => None,
        }
    }

    /// Get the default decode array for the color space.
    pub(crate) fn default_decode_arr(&self, n: f32) -> SmallVec<[(f32, f32); 4]> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk(_) => {
                smallvec![(0.0, 1.0), (0.0, 1.0), (0.0, 1.0), (0.0, 1.0)]
            }
            ColorSpaceType::DeviceGray(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::DeviceRgb(_) => smallvec![(0.0, 1.0), (0.0, 1.0), (0.0, 1.0)],
            ColorSpaceType::ICCBased(i) => smallvec![(0.0, 1.0); i.number_components()],
            ColorSpaceType::CalGray(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::CalRgb(_) => smallvec![(0.0, 1.0), (0.0, 1.0), (0.0, 1.0)],
            ColorSpaceType::Lab(l) => smallvec![
                (0.0, 100.0),
                (l.range[0], l.range[1]),
                (l.range[2], l.range[3]),
            ],
            ColorSpaceType::Indexed(_) => smallvec![(0.0, 2.0_f32.powf(n) - 1.0)],
            ColorSpaceType::Separation(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::DeviceN(d) => smallvec![(0.0, 1.0); d.num_components as usize],
            // Not a valid image color space.
            ColorSpaceType::Pattern(_) => smallvec![(0.0, 1.0)],
        }
    }

    pub(crate) fn inverted_default_decode_arr(&self, n: f32) -> SmallVec<[(f32, f32); 4]> {
        self.default_decode_arr(n)
            .iter()
            .map(|(min, max)| (*max, *min))
            .collect()
    }

    pub(crate) fn component_ranges(&self) -> SmallVec<[(f32, f32); 4]> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk(_) => smallvec![(0.0, 1.0); 4],
            ColorSpaceType::DeviceGray(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::DeviceRgb(_) => smallvec![(0.0, 1.0); 3],
            ColorSpaceType::ICCBased(i) => smallvec![(0.0, 1.0); i.number_components()],
            ColorSpaceType::CalGray(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::CalRgb(_) => smallvec![(0.0, 1.0); 3],
            ColorSpaceType::Lab(_) => {
                smallvec![(0.0, 100.0), (-128.0, 127.0), (-128.0, 127.0)]
            }
            ColorSpaceType::Indexed(i) => smallvec![(0.0, i.hival() as f32)],
            ColorSpaceType::Separation(_) => smallvec![(0.0, 1.0)],
            ColorSpaceType::Pattern(pattern) => pattern.color_space().component_ranges(),
            ColorSpaceType::DeviceN(d) => smallvec![(0.0, 1.0); d.num_components as usize],
        }
    }

    pub(crate) fn convert_values(&self, input: &[f32], output: &mut [u8]) -> Option<()> {
        // Calibrated paints and sampled shadings carry real components.
        // Quantizing before gamma creates large steps in their dark colors.
        let calibrated = match self.0.as_ref() {
            ColorSpaceType::CalGray(gray) => Some(gray.convert_value(*input.first()?)),
            ColorSpaceType::CalRgb(rgb) => Some(rgb.convert_components([
                f64::from(*input.first()?),
                f64::from(*input.get(1)?),
                f64::from(*input.get(2)?),
            ])),
            ColorSpaceType::Pattern(pattern) => {
                return pattern.color_space().convert_values(input, output);
            }
            _ => None,
        };
        if let Some(calibrated) = calibrated {
            output.get_mut(..3)?.copy_from_slice(&calibrated);
            return Some(());
        }
        let converted = self.encode_values(input);
        self.convert(&converted, output)
    }

    pub(crate) fn encode_values(&self, input: &[f32]) -> SmallVec<[u8; 4]> {
        if let Some(hival) = self.indexed_hival() {
            return input
                .iter()
                .map(|value| (*value + 0.5).clamp(0.0, hival as f32) as u8)
                .collect();
        }

        let ranges = match self.0.as_ref() {
            ColorSpaceType::ICCBased(icc) if icc.is_lab() => {
                smallvec![(0.0, 100.0), (-128.0, 127.0), (-128.0, 127.0)]
            }
            _ => self.component_ranges(),
        };
        encode_components(input, &ranges)
    }

    /// Get the initial color of the color space.
    pub(crate) fn initial_color(&self) -> ColorComponents {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk(_) => smallvec![0.0, 0.0, 0.0, 1.0],
            ColorSpaceType::DeviceGray(_) => smallvec![0.0],
            ColorSpaceType::DeviceRgb(_) => smallvec![0.0, 0.0, 0.0],
            ColorSpaceType::ICCBased(icc) => match icc.number_components() {
                1 => smallvec![0.0],
                3 => smallvec![0.0, 0.0, 0.0],
                4 => smallvec![0.0, 0.0, 0.0, 1.0],
                _ => unreachable!(),
            },
            ColorSpaceType::CalGray(_) => smallvec![0.0],
            ColorSpaceType::CalRgb(_) => smallvec![0.0, 0.0, 0.0],
            ColorSpaceType::Lab(_) => smallvec![0.0, 0.0, 0.0],
            ColorSpaceType::Indexed(_) => smallvec![0.0],
            ColorSpaceType::Separation(_) => smallvec![1.0],
            ColorSpaceType::Pattern(pattern) => pattern.initial_color(),
            ColorSpaceType::DeviceN(d) => smallvec![1.0; d.num_components as usize],
        }
    }

    /// Get the number of components of the color space.
    pub(crate) fn num_components(&self) -> u8 {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk(_) => 4,
            ColorSpaceType::DeviceGray(_) => 1,
            ColorSpaceType::DeviceRgb(_) => 3,
            ColorSpaceType::ICCBased(icc) => icc.number_components() as u8,
            ColorSpaceType::CalGray(_) => 1,
            ColorSpaceType::CalRgb(_) => 3,
            ColorSpaceType::Lab(_) => 3,
            ColorSpaceType::Indexed(_) => 1,
            ColorSpaceType::Separation(_) => 1,
            ColorSpaceType::Pattern(pattern) => pattern.num_components(),
            ColorSpaceType::DeviceN(d) => d.num_components,
        }
    }

    /// Turn the given component values and opacity into an RGBA color.
    #[inline]
    pub fn to_rgba(&self, c: &[f32], opacity: f32) -> AlphaColor {
        let alpha = f32_to_u8(opacity);

        match self.0.as_ref() {
            ColorSpaceType::DeviceGray(_) => {
                let gray = c.first().copied().map(f32_to_u8).unwrap_or(0);
                AlphaColor::from_rgba8(gray, gray, gray, alpha)
            }
            ColorSpaceType::DeviceRgb(_) => AlphaColor::from_rgba8(
                c.first().copied().map(f32_to_u8).unwrap_or(0),
                c.get(1).copied().map(f32_to_u8).unwrap_or(0),
                c.get(2).copied().map(f32_to_u8).unwrap_or(0),
                alpha,
            ),
            ColorSpaceType::DeviceCmyk(device_cmyk) if c.len() == 4 => {
                let input = [
                    f32_to_u8(c[0]),
                    f32_to_u8(c[1]),
                    f32_to_u8(c[2]),
                    f32_to_u8(c[3]),
                ];
                let mut output = [0; 3];

                if device_cmyk.convert(&input, &mut output).is_some() {
                    AlphaColor::from_rgba8(output[0], output[1], output[2], alpha)
                } else {
                    AlphaColor::BLACK
                }
            }
            _ => self.to_alpha_color(c, opacity).unwrap_or(AlphaColor::BLACK),
        }
    }

    fn to_alpha_color(&self, input: &[f32], mut opacity: f32) -> Option<AlphaColor> {
        let mut output = [0; 3];
        self.convert_values(input, &mut output)?;

        // For separation color spaces:
        // "The special colourant name None shall not produce any visible output.
        // Painting operations in a Separation space with this colourant name
        // shall have no effect on the current page."
        if self.is_none() {
            opacity = 0.0;
        }

        Some(AlphaColor::from_rgba8(
            output[0],
            output[1],
            output[2],
            (opacity * 255.0 + 0.5) as u8,
        ))
    }
}

impl ToRgb for ColorSpace {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk(i) => i.convert(input, output),
            ColorSpaceType::DeviceGray(i) => i.convert(input, output),
            ColorSpaceType::DeviceRgb(i) => i.convert(input, output),
            ColorSpaceType::Pattern(i) => i.convert(input, output),
            ColorSpaceType::Indexed(i) => i.convert(input, output),
            ColorSpaceType::ICCBased(i) => i.convert(input, output),
            ColorSpaceType::CalGray(i) => i.convert(input, output),
            ColorSpaceType::CalRgb(i) => i.convert(input, output),
            ColorSpaceType::Lab(i) => i.convert(input, output),
            ColorSpaceType::Separation(i) => i.convert(input, output),
            ColorSpaceType::DeviceN(i) => i.convert(input, output),
        }
    }

    fn convert_in_place(&self, input: &mut [u8]) -> Option<()> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceCmyk(i) => i.convert_in_place(input),
            ColorSpaceType::DeviceGray(i) => i.convert_in_place(input),
            ColorSpaceType::DeviceRgb(i) => i.convert_in_place(input),
            ColorSpaceType::Pattern(i) => i.convert_in_place(input),
            ColorSpaceType::Indexed(i) => i.convert_in_place(input),
            ColorSpaceType::ICCBased(i) => i.convert_in_place(input),
            ColorSpaceType::CalGray(i) => i.convert_in_place(input),
            ColorSpaceType::CalRgb(i) => i.convert_in_place(input),
            ColorSpaceType::Lab(i) => i.convert_in_place(input),
            ColorSpaceType::Separation(i) => i.convert_in_place(input),
            ColorSpaceType::DeviceN(i) => i.convert_in_place(input),
        }
    }

    fn is_none(&self) -> bool {
        match self.0.as_ref() {
            ColorSpaceType::Separation(s) => s.is_none(),
            ColorSpaceType::DeviceN(d) => d.is_none(),
            _ => false,
        }
    }
}

impl ToLuma for ColorSpace {
    fn to_luma(&self, input: &mut [u8]) -> Option<()> {
        match self.0.as_ref() {
            ColorSpaceType::DeviceGray(i) => i.to_luma(input),
            ColorSpaceType::Pattern(i) => i.to_luma(input),
            ColorSpaceType::Indexed(i) => i.to_luma(input),
            ColorSpaceType::ICCBased(i) => i.to_luma(input),
            ColorSpaceType::CalGray(i) => i.to_luma(input),
            ColorSpaceType::DeviceCmyk(_)
            | ColorSpaceType::DeviceRgb(_)
            | ColorSpaceType::CalRgb(_)
            | ColorSpaceType::Lab(_)
            | ColorSpaceType::Separation(_)
            | ColorSpaceType::DeviceN(_) => None,
        }
    }
}

#[inline(always)]
fn f32_to_u8(val: f32) -> u8 {
    (val * 255.0 + 0.5) as u8
}

#[derive(Debug, Clone)]
/// A color.
pub struct Color {
    color_space: ColorSpace,
    components: ColorComponents,
    opacity: f32,
}

impl Color {
    pub(crate) fn with_rendering_intent(
        &self,
        intent: RenderingIntent,
    ) -> Result<Self, ColorConversionError> {
        let mut color = self.clone();
        color.color_space = color.color_space.with_rendering_intent(intent)?;
        Ok(color)
    }

    pub(crate) fn new(color_space: ColorSpace, components: ColorComponents, opacity: f32) -> Self {
        Self {
            color_space,
            components,
            opacity,
        }
    }

    /// Return the color as an RGBA color.
    #[inline]
    pub fn to_rgba(&self) -> AlphaColor {
        self.color_space.to_rgba(&self.components, self.opacity)
    }

    /// Return the color's original PDF color-space family.
    pub fn color_space_kind(&self) -> ColorSpaceKind {
        self.color_space.kind()
    }

    /// Return the unconverted PDF color components.
    pub fn components(&self) -> &[f32] {
        &self.components
    }

    /// Create a color from RGBA.
    #[inline]
    pub fn from_rgba(rgba: AlphaColor) -> Self {
        let c = rgba.components();
        Self {
            color_space: ColorSpace::device_rgb(),
            components: smallvec![c[0], c[1], c[2]],
            opacity: c[3],
        }
    }
}

pub(crate) trait ToRgb {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()>;
    fn convert_in_place(&self, _input: &mut [u8]) -> Option<()> {
        None
    }
    fn is_none(&self) -> bool {
        false
    }
}

pub(crate) trait ToLuma {
    fn to_luma(&self, input: &mut [u8]) -> Option<()>;
}

#[inline]
fn encode_components(input: &[f32], ranges: &[(f32, f32)]) -> SmallVec<[u8; 4]> {
    input
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let (min, max) = ranges[index % ranges.len()];
            (((*value - min) / (max - min)) * 255.0 + 0.5) as u8
        })
        .collect()
}

#[cfg(test)]
mod retained_inspection_tests {
    use super::*;
    use hayro_syntax::Pdf;

    #[test]
    fn owned_inspection_preserves_srgb_marked_icc_identity() {
        let bytes =
            include_bytes!("../../../hayro-tests/pdfs/custom/xobject_with_fill_opacity.pdf");
        let pdf = Pdf::new(bytes.to_vec()).expect("parse ICC page-group fixture");
        let group = pdf.pages()[0]
            .raw()
            .get::<Dict<'_>>(GROUP)
            .expect("page group");
        let object = group.get::<Object<'_>>(CS).expect("page group color space");
        let color_space = ColorSpace::from_pdf_object(object).expect("parse owned color space");
        assert_eq!(color_space.kind(), ColorSpaceKind::IccBased);
    }

    #[test]
    fn all_icc_handles_preserve_profile_identity() {
        let bytes =
            include_bytes!("../../../hayro-tests/pdfs/custom/xobject_with_fill_opacity.pdf");
        let pdf = Pdf::new(bytes.to_vec()).expect("parse ICC page-group fixture");
        let group = pdf.pages()[0]
            .raw()
            .get::<Dict<'_>>(GROUP)
            .expect("page group");
        let object = group.get::<Object<'_>>(CS).expect("page group color space");
        let cache = Cache::new();
        let optimized = ColorSpace::new(object.clone(), &cache).expect("optimized ICC");
        let preserving = ColorSpace::new(object, &cache).expect("identity-preserving ICC");
        assert_eq!(optimized.kind(), ColorSpaceKind::IccBased);
        assert_eq!(preserving.kind(), ColorSpaceKind::IccBased);
    }
}
