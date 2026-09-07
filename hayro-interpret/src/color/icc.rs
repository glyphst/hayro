use super::{ColorConversionError, RenderingIntent, ToLuma, ToRgb};
use moxcms::{
    ColorProfile, DataColorSpace, Layout, ProfileClass, Transform8BitExecutor, TransformOptions,
};
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, OnceLock};

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

struct ICCColorRepr {
    number_components: usize,
    src_profile: ColorProfile,
    // Shared source data has only four possible derived transforms. Failed
    // construction is cached too; no mutable global rendering intent exists.
    transforms: [OnceLock<Option<IccTransform>>; 4],
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
    pub(super) fn new(profile: &[u8], number_components: usize) -> Option<Self> {
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
            )
        {
            return None;
        }
        let mut result = Self::new_from_src_profile(source, number_components)?;
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
                transforms: std::array::from_fn(|_| OnceLock::new()),
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
        result
            .transform()
            .ok_or(ColorConversionError::IccTransform)?;
        Ok(result)
    }

    pub(super) fn number_components(&self) -> usize {
        self.data.number_components
    }
    pub(super) fn is_lab(&self) -> bool {
        self.data.src_profile.color_space == DataColorSpace::Lab
    }

    fn transform(&self) -> Option<&IccTransform> {
        self.data.transforms[self.intent as usize]
            .get_or_init(|| self.build_transform())
            .as_ref()
    }

    fn build_transform(&self) -> Option<IccTransform> {
        let mut source = self.data.src_profile.clone();
        let mut destination = ColorProfile::new_srgb();
        let options = TransformOptions {
            rendering_intent: self.intent.cms(),
            // Embedded ICC tags, rather than optional CICP shortcuts, define this conversion.
            allow_use_cicp_transfer: false,
            ..TransformOptions::default()
        };
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
        if self.number_components() == 1 {
            if source.pcs != DataColorSpace::Xyz
                || (source.lut_a_to_b_perceptual.is_some()
                    || source.lut_a_to_b_colorimetric.is_some()
                    || source.lut_a_to_b_saturation.is_some())
            {
                return None;
            }
            // The CMS gray shortcut bypasses the destination matrix. Replicating
            // gray through a neutral RGB matrix with the original TRC preserves
            // PCS luminance and permits absolute media-white tinting.
            let trc = source.gray_trc.clone()?;
            let mut neutral = ColorProfile::new_srgb();
            neutral.red_trc = Some(trc.clone());
            neutral.green_trc = Some(trc.clone());
            neutral.blue_trc = Some(trc);
            neutral.cicp = None;
            let transform = neutral
                .create_transform_8bit(Layout::Rgb, &destination, Layout::Rgb, options)
                .ok()?;
            let input: Vec<u8> = (0..=255_u8).flat_map(|v| [v; 3]).collect();
            let mut output = [0_u8; 768];
            transform.transform(&input, &mut output).ok()?;
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
        match self.transform()? {
            IccTransform::Rgb(transform) => transform.transform(input, output).ok()?,
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
        let IccTransform::Gray { rgb, neutral: true } = self.transform()? else {
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
