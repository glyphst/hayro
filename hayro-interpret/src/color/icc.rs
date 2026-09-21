use super::{ColorConversionError, RenderingIntent, ToLuma, ToRgb};
use crate::cache::{IccMemory, MemoryReservation};
use hayro_syntax::object::{
    Array, Dict, Name, Stream,
    dict::keys::{ICC_BASED, N, RANGE},
};
use moxcms::{
    ColorProfile, DataColorSpace, Layout, LutWarehouse, PcsXyzAdjustment, ProfileClass,
    Transform8BitExecutor, TransformOptions,
};
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(test)]
mod channel_tests;
#[cfg(test)]
mod declaration_tests;
mod equivalence;
#[cfg(test)]
mod gray_lab_trc_tests;
#[cfg(test)]
mod gray_lut_tests;
#[cfg(test)]
mod lut_tests;
mod memory;
#[cfg(test)]
mod memory_tests;
pub use equivalence::{IccDeviceRgbBounds, IccEquivalenceError};

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
// ICC.1:2004-10, 6.3.4.3/Table 12: perceptual reference-medium black.
const PERCEPTUAL_BLACK: [f64; 3] = [0.003357, 0.003479, 0.002869];
const TRANSFORM_BATCH_PIXELS: usize = 1024;

fn source_lut(profile: &ColorProfile, intent: RenderingIntent) -> Option<&LutWarehouse> {
    match intent {
        RenderingIntent::Perceptual => profile.lut_a_to_b_perceptual.as_ref(),
        RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => profile
            .lut_a_to_b_colorimetric
            .as_ref()
            .or(profile.lut_a_to_b_perceptual.as_ref()),
        RenderingIntent::Saturation => profile
            .lut_a_to_b_saturation
            .as_ref()
            .or(profile.lut_a_to_b_perceptual.as_ref()),
    }
}

pub(super) fn declaration<'a>(array: &Array<'a>) -> Option<(Stream<'a>, usize)> {
    if array.raw_iter().take(3).count() != 2 {
        return None;
    }
    let mut iter = array.flex_iter();
    if iter.next::<Name<'_>>()?.as_ref() != ICC_BASED {
        return None;
    }
    let stream = iter.next::<Stream<'_>>()?;
    let components = stream.dict().get::<usize>(N)?;
    ICCProfile::validate_range(stream.dict(), components)?;
    Some((stream, components))
}

struct ICCColorRepr {
    number_components: usize,
    src_profile: ColorProfile,
    matrix_tags_only: bool,
    // Shared source data has only four possible derived transforms. Failed
    // construction is cached too; no mutable global rendering intent exists.
    transforms: [OnceLock<Option<OwnedTransform>>; 4],
    equivalence: [OnceLock<Result<IccDeviceRgbBounds, IccEquivalenceError>>; 4],
    transform_build: Mutex<()>,
    memory: Option<IccMemory>,
    _source_reservation: Option<MemoryReservation>,
}

struct OwnedTransform {
    value: IccTransform,
    _reservation: Option<MemoryReservation>,
}

enum IccTransform {
    Rgb(Arc<Transform8BitExecutor>),
    Gray {
        rgb: Box<[[u8; 3]; 256]>,
        neutral: bool,
    },
}

#[derive(Clone)]
pub(crate) struct ICCProfile {
    data: Arc<ICCColorRepr>,
    intent: RenderingIntent,
}

impl Debug for ICCProfile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ICCColor")
            .field("intent", &self.intent)
            .finish_non_exhaustive()
    }
}

impl ICCProfile {
    pub(super) fn validate_range(dict: &Dict<'_>, components: usize) -> Option<()> {
        if !matches!(components, 1 | 3 | 4) {
            return None;
        }
        // PDF 32000-1 Tables 66/69: the supported Gray/RGB/CMYK profiles
        // have normalized component ranges. Validate the whole declaration
        // before profile-cache reuse, including indirect bounds and tails.
        if dict.is_null_or_absent(RANGE) {
            return Some(());
        }
        let valid = match components {
            1 => dict.get::<[f64; 2]>(RANGE)? == [0.0, 1.0],
            3 => dict.get::<[f64; 6]>(RANGE)? == [0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            4 => dict.get::<[f64; 8]>(RANGE)? == [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            _ => return None,
        };
        valid.then_some(())
    }

    pub(super) fn new(profile: &[u8], number_components: usize) -> Option<Self> {
        Self::new_with_memory(profile, number_components, None)
    }

    pub(super) fn new_with_memory(
        profile: &[u8],
        number_components: usize,
        memory: Option<IccMemory>,
    ) -> Option<Self> {
        let source = ColorProfile::new_from_slice(profile).ok()?;
        if !matches!(profile.get(8), Some(2 | 4))
            || !matches!(
                (source.color_space, number_components),
                (DataColorSpace::Gray, 1) | (DataColorSpace::Rgb, 3) | (DataColorSpace::Cmyk, 4)
            )
            || !matches!(
                source.profile_class,
                ProfileClass::InputDevice
                    | ProfileClass::DisplayDevice
                    | ProfileClass::OutputDevice
                    | ProfileClass::ColorSpace
            )
        {
            return None;
        }
        // ICC.1:2004-10, 8.7: ColorSpace profiles require both perceptual
        // directions. Matrix/TRC tags alone do not define this profile class.
        if source.profile_class == ProfileClass::ColorSpace
            && (source.lut_a_to_b_perceptual.is_none() || source.lut_b_to_a_perceptual.is_none())
        {
            return None;
        }
        let mut result = Self::new_from_src_profile(source, number_components)?;
        let data = Arc::get_mut(&mut result.data)?;
        if let Some(memory) = memory {
            data._source_reservation =
                Some(memory.reserve(memory::source_bytes(&data.src_profile))?);
            data.memory = Some(memory);
        }
        data.matrix_tags_only = equivalence::matrix_tags_only(profile);
        result.intent = RenderingIntent::default();
        Some(result)
    }

    // Internal DeviceCMYK/Lab policies retain their existing perceptual mapping.
    pub(super) fn new_from_src_profile(
        src_profile: ColorProfile,
        number_components: usize,
    ) -> Option<Self> {
        if !matches!(number_components, 1 | 3 | 4) {
            return None;
        }
        Some(Self {
            data: Arc::new(ICCColorRepr {
                number_components,
                src_profile,
                matrix_tags_only: true,
                transforms: std::array::from_fn(|_| OnceLock::new()),
                equivalence: std::array::from_fn(|_| OnceLock::new()),
                transform_build: Mutex::new(()),
                memory: None,
                _source_reservation: None,
            }),
            intent: RenderingIntent::Perceptual,
        })
    }

    pub(super) fn with_intent(
        &self,
        intent: RenderingIntent,
    ) -> Result<Self, ColorConversionError> {
        let result = Self {
            data: self.data.clone(),
            intent,
        };
        result.transform()?;
        Ok(result)
    }

    pub(super) fn number_components(&self) -> usize {
        self.data.number_components
    }
    pub(super) fn is_lab(&self) -> bool {
        self.data.src_profile.color_space == DataColorSpace::Lab
    }

    fn transform(&self) -> Result<&IccTransform, ColorConversionError> {
        let slot = &self.data.transforms[self.intent as usize];
        if slot.get().is_none() {
            let _guard = self
                .data
                .transform_build
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if slot.get().is_none() {
                let bytes = memory::transform_bytes(
                    &self.data.src_profile,
                    self.number_components(),
                    self.intent,
                );
                let reservation = self.reserve_memory(bytes)?;
                let _construction = self.reserve_memory(memory::construction_bytes(
                    &self.data.src_profile,
                    bytes,
                    self.intent,
                ))?;
                let value = self.build_transform().map(|value| OwnedTransform {
                    value,
                    _reservation: reservation,
                });
                // Resource refusal returns before populating the slot. Invalid
                // transforms remain cached, as before, while budget can recover.
                let _ = slot.set(value);
            }
        }
        slot.get()
            .and_then(Option::as_ref)
            .map(|value| &value.value)
            .ok_or(ColorConversionError::IccTransform)
    }

    fn reserve_memory(
        &self,
        bytes: u64,
    ) -> Result<Option<MemoryReservation>, ColorConversionError> {
        self.data
            .memory
            .as_ref()
            .map(|memory| {
                memory
                    .reserve(bytes)
                    .ok_or(ColorConversionError::IccMemoryLimit)
            })
            .transpose()
    }

    fn conversion_profiles(&self) -> Option<(ColorProfile, ColorProfile, TransformOptions)> {
        let mut source = self.data.src_profile.clone();
        let mut destination = ColorProfile::new_srgb();
        if self.intent == RenderingIntent::AbsoluteColorimetric {
            let white = if source.version() < moxcms::ProfileVersion::V4_0
                && source.profile_class == ProfileClass::DisplayDevice
            {
                D50
            } else {
                let white = source.media_white_point?;
                [white.x, white.y, white.z]
            };
            if white.iter().any(|v| !v.is_finite() || *v <= 0.0) {
                return None;
            }
            // Fully adapted ICC absolute conversion multiplies PCS XYZ by
            // source-media-white / destination-media-white. Folding its inverse
            // into the destination colorants applies it before clipping and the
            // sRGB transfer curve, including Lab-PCS and CMYK LUT transforms.
            for color in [
                &mut destination.red_colorant,
                &mut destination.green_colorant,
                &mut destination.blue_colorant,
            ] {
                color.x *= D50[0] / white[0];
                color.y *= D50[1] / white[1];
                color.z *= D50[2] / white[2];
                if [color.x, color.y, color.z].iter().any(|v| !v.is_finite()) {
                    return None;
                }
            }
            destination.cicp = None;
            destination.colorant_matrix().determinant()?;
        }
        // ICC uses the perceptual AToB0 tag when a requested AToB1/2 is absent.
        // Matrix-only profiles continue to use their matrix/TRCs for all intents.
        match self.intent {
            RenderingIntent::RelativeColorimetric | RenderingIntent::AbsoluteColorimetric => {
                if source.lut_a_to_b_colorimetric.is_none() {
                    source.lut_a_to_b_colorimetric = source.lut_a_to_b_perceptual.clone();
                }
            }
            RenderingIntent::Saturation => {
                if source.lut_a_to_b_saturation.is_none() {
                    source.lut_a_to_b_saturation = source.lut_a_to_b_perceptual.clone();
                }
            }
            RenderingIntent::Perceptual => {}
        }
        let adjust_black = source.version() >= moxcms::ProfileVersion::V4_0
            && match self.intent {
                RenderingIntent::Perceptual => source.lut_a_to_b_perceptual.is_some(),
                RenderingIntent::Saturation => source.lut_a_to_b_saturation.is_some(),
                _ => false,
            };
        let pcs_xyz_adjustment = if adjust_black {
            // The destination's matrix/TRC sRGB black is zero. Map the v4 LUT
            // reference black to it in XYZ, preserving D50 and signed values
            // until the destination matrix and final encoding have run.
            Some(
                PcsXyzAdjustment::new(
                    std::array::from_fn(|i| (D50[i] / (D50[i] - PERCEPTUAL_BLACK[i])) as f32),
                    std::array::from_fn(|i| {
                        (-PERCEPTUAL_BLACK[i] * D50[i] / (D50[i] - PERCEPTUAL_BLACK[i])) as f32
                    }),
                )
                .ok()?,
            )
        } else {
            None
        };
        let options = TransformOptions {
            rendering_intent: self.intent.cms(),
            allow_use_cicp_transfer: false,
            prefer_original_lut: true,
            pcs_xyz_adjustment,
            ..TransformOptions::default()
        };
        Some((source, destination, options))
    }

    fn build_transform(&self) -> Option<IccTransform> {
        let (mut source, destination, options) = self.conversion_profiles()?;
        if self.number_components() == 1 {
            let mut output = [0_u8; 768];
            let selected_lut = source_lut(&source, self.intent).is_some();
            if selected_lut || source.pcs == DataColorSpace::Lab {
                if !selected_lut {
                    // Unselected intent tags must not suppress the Gray TRC.
                    // Lab curves encode L*: the CMS applies Lab-to-XYZ before
                    // the destination matrix, including absolute-white tint.
                    source.lut_a_to_b_perceptual = None;
                    source.lut_a_to_b_colorimetric = None;
                    source.lut_a_to_b_saturation = None;
                }
                let transform = source
                    .create_transform_8bit(Layout::Gray, &destination, Layout::Rgb, options)
                    .ok()?;
                let input = std::array::from_fn::<_, 256, _>(|i| i as u8);
                transform.transform(&input, &mut output).ok()?;
            } else {
                if source.pcs != DataColorSpace::Xyz {
                    return None;
                }
                // The CMS gray shortcut bypasses the destination matrix.
                // Replicating gray through a neutral RGB matrix with the
                // original TRC preserves absolute media-white tinting.
                let trc = source.gray_trc.clone()?;
                let mut neutral = ColorProfile::new_srgb();
                neutral.red_trc = Some(trc.clone());
                neutral.green_trc = Some(trc.clone());
                neutral.blue_trc = Some(trc);
                neutral.cicp = None;
                let transform = neutral
                    .create_transform_8bit(Layout::Rgb, &destination, Layout::Rgb, options)
                    .ok()?;
                let input = std::array::from_fn::<_, 768, _>(|i| (i / 3) as u8);
                transform.transform(&input, &mut output).ok()?;
            }
            // Retain only the complete byte-domain table, independently of
            // profile/LUT size and subsequent image dimensions.
            let rgb = Box::new(std::array::from_fn(|i| {
                [output[i * 3], output[i * 3 + 1], output[i * 3 + 2]]
            }));
            let neutral = rgb.iter().all(|p| p[0] == p[1] && p[1] == p[2]);
            return Some(IccTransform::Gray { rgb, neutral });
        }
        let layout = if self.number_components() == 4 {
            Layout::Rgba
        } else {
            Layout::Rgb
        };
        source
            .create_transform_8bit(layout, &destination, Layout::Rgb, options)
            .ok()
            .map(IccTransform::Rgb)
    }
}

impl ToRgb for ICCProfile {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        match self.transform().ok()? {
            IccTransform::Rgb(transform) => {
                let components = self.number_components();
                if !input.len().is_multiple_of(components)
                    || output.len() != (input.len() / components).checked_mul(3)?
                {
                    return None;
                }
                // Direct CMS stages allocate working vectors for their input.
                // Keep them bounded independently of decoded image dimensions.
                for (source, destination) in input
                    .chunks(components * TRANSFORM_BATCH_PIXELS)
                    .zip(output.chunks_mut(3 * TRANSFORM_BATCH_PIXELS))
                {
                    transform.transform(source, destination).ok()?;
                }
            }
            IccTransform::Gray { rgb, .. } => {
                if output.len() != input.len().checked_mul(3)? {
                    return None;
                }
                for (value, pixel) in input.iter().zip(output.chunks_exact_mut(3)) {
                    pixel.copy_from_slice(&rgb[usize::from(*value)]);
                }
            }
        }
        Some(())
    }

    fn convert_in_place(&self, input: &mut [u8]) -> Option<()> {
        if self.number_components() != 3 || !input.len().is_multiple_of(3) {
            return None;
        }
        // Also correct for profiles with LUTs: moxcms's in-place matrix path
        // does not consult those tags. Fixed scratch avoids an image-sized copy.
        let mut scratch = [0_u8; 768];
        for chunk in input.chunks_mut(scratch.len()) {
            scratch[..chunk.len()].copy_from_slice(chunk);
            self.convert(&scratch[..chunk.len()], chunk)?;
        }
        Some(())
    }
}

impl ToLuma for ICCProfile {
    fn to_luma(&self, input: &mut [u8]) -> Option<()> {
        let IccTransform::Gray { rgb, neutral: true } = self.transform().ok()? else {
            return None;
        };
        for value in input {
            *value = rgb[usize::from(*value)][0];
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::{
        Pdf,
        object::{ObjectIdentifier, Stream},
    };

    fn cmyk_profile() -> ICCProfile {
        let pdf = Pdf::new(
            include_bytes!("../../../hayro-tests/pdfs/custom/image_cmyk_icc_jpg.pdf").to_vec(),
        )
        .unwrap();
        let stream = pdf
            .xref()
            .get::<Stream<'_>>(ObjectIdentifier::new(3, 0))
            .unwrap();
        ICCProfile::new(&stream.decoded().unwrap(), 4).unwrap()
    }

    #[test]
    fn real_cmyk_lut_intents_match_littlecms_controls() {
        // LittleCMS 2.19.1, source profile from image_cmyk_icc_jpg.pdf,
        // sRGB destination, adaptation=1, BPC disabled. Maximum 2/255 error.
        let controls = [
            (
                RenderingIntent::Perceptual,
                [
                    [255, 255, 255],
                    [197, 79, 146],
                    [103, 128, 84],
                    [0, 174, 239],
                ],
            ),
            (
                RenderingIntent::RelativeColorimetric,
                [
                    [255, 255, 255],
                    [198, 89, 150],
                    [109, 133, 93],
                    [0, 176, 240],
                ],
            ),
            (
                RenderingIntent::Saturation,
                [
                    [255, 255, 255],
                    [197, 79, 146],
                    [103, 128, 84],
                    [0, 174, 239],
                ],
            ),
            (
                RenderingIntent::AbsoluteColorimetric,
                [
                    [225, 223, 216],
                    [174, 77, 126],
                    [95, 116, 77],
                    [0, 153, 203],
                ],
            ),
        ];
        let profile = cmyk_profile();
        let input = [
            [0, 0, 0, 0],
            [26, 204, 0, 26],
            [128, 51, 178, 77],
            [255, 0, 0, 0],
        ];
        for (intent, control) in controls {
            let bound = profile.with_intent(intent).unwrap();
            let mut actual = [0; 12];
            bound.convert(input.as_flattened(), &mut actual).unwrap();
            assert!(
                actual
                    .iter()
                    .zip(control.as_flattened())
                    .all(|(a, b)| a.abs_diff(*b) <= 2),
                "{intent:?}: {actual:?}"
            );
        }
    }

    #[test]
    fn missing_lut_intents_use_perceptual_table() {
        let profile = cmyk_profile();
        let mut source = profile.data.src_profile.clone();
        source.lut_a_to_b_colorimetric = None;
        source.lut_a_to_b_saturation = None;
        let profile = ICCProfile::new_from_src_profile(source, 4).unwrap();
        let mut perceptual = [0; 3];
        profile.convert(&[26, 204, 0, 26], &mut perceptual).unwrap();
        for intent in [
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::Saturation,
        ] {
            let mut actual = [0; 3];
            profile
                .with_intent(intent)
                .unwrap()
                .convert(&[26, 204, 0, 26], &mut actual)
                .unwrap();
            assert_eq!(actual, perceptual);
        }
    }

    #[test]
    fn absolute_matrix_conversion_precedes_clipping_and_honors_v2_display_white() {
        let mut source = ColorProfile::new_srgb();
        source.profile_class = ProfileClass::OutputDevice;
        source.cicp = None;
        source.media_white_point = Some(moxcms::Xyzd::new(0.75, 0.5, 0.25));
        let mut encoded = source.encode().unwrap();
        encoded[8..12].copy_from_slice(&[4, 0x20, 0, 0]);
        let profile = ICCProfile::new(&encoded, 3).unwrap();
        let mut actual = [0; 6];
        profile
            .with_intent(RenderingIntent::AbsoluteColorimetric)
            .unwrap()
            .convert(&[64, 128, 191, 255, 255, 255], &mut actual)
            .unwrap();
        // Independent fully adapted XYZ scaling and LittleCMS agree at <=1.
        let expected = [129_u8, 67, 110, 255, 132, 147];
        assert!(
            actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
            "{actual:?}"
        );
        encoded[8] = 2;
        encoded[12..16].copy_from_slice(b"mntr");
        let display = ICCProfile::new(&encoded, 3).unwrap();
        let input = [64, 128, 191, 255, 255, 255];
        display
            .with_intent(RenderingIntent::AbsoluteColorimetric)
            .unwrap()
            .convert(&input, &mut actual)
            .unwrap();
        assert!(actual.iter().zip(input).all(|(a, b)| a.abs_diff(b) <= 1));
        assert!(ICCProfile::new(&encoded, 4).is_none());
        encoded[8] = 5;
        assert!(ICCProfile::new(&encoded, 3).is_none());
    }
}
