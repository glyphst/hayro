use super::ToRgb;
use super::cie::{CieRgb, numbers};
use hayro_syntax::object::Dict;
use hayro_syntax::object::dict::keys::{BLACK_POINT, GAMMA, MATRIX, WHITE_POINT};
use moxcms::Matrix3d;

#[derive(Debug, Clone)]
pub(crate) struct CalRgb {
    transform: CieRgb,
    gamma: [f32; 3],
}

impl CalRgb {
    pub(super) fn new(dict: &Dict<'_>) -> Option<Self> {
        Self::from_dict(dict, false)
    }

    pub(super) fn for_gray(dict: &Dict<'_>) -> Option<Self> {
        Self::from_dict(dict, true)
    }

    fn from_dict(dict: &Dict<'_>, gray: bool) -> Option<Self> {
        let white = numbers(dict, WHITE_POINT, None)?;
        let black = numbers(dict, BLACK_POINT, Some([0.0; 3]))?;
        let gamma = if gray {
            [if dict.contains_key(GAMMA) {
                dict.get::<f32>(GAMMA)?
            } else {
                1.0
            }; 3]
        } else {
            numbers(dict, GAMMA, Some([1.0; 3]))?
        };
        if gamma
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return None;
        }
        let matrix = if gray {
            // Repeated A components give XYZ = WhitePoint * A^Gamma.
            [white[0], 0.0, 0.0, 0.0, white[1], 0.0, 0.0, 0.0, white[2]]
        } else {
            numbers(
                dict,
                MATRIX,
                Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            )?
        };
        // PDF 32000-1, 8.6.5.3: Matrix contains XYZ columns, without
        // renormalizing their magnitudes to the declared white point.
        let pdf_matrix = Matrix3d {
            v: core::array::from_fn(|row| {
                core::array::from_fn(|column| f64::from(matrix[column * 3 + row]))
            }),
        };
        let transform = CieRgb::new(white, black)?.compose(pdf_matrix)?;
        Some(Self { transform, gamma })
    }

    pub(super) fn convert_pixel(&self, input: [u8; 3]) -> [u8; 3] {
        self.convert_components(input.map(|value| f64::from(value) / 255.0))
    }

    pub(super) fn convert_components(&self, input: [f64; 3]) -> [u8; 3] {
        self.convert_real(input)
            .map(|encoded| (encoded * 255.0).round() as u8)
    }

    pub(super) fn convert_real(&self, input: [f64; 3]) -> [f64; 3] {
        let decoded =
            core::array::from_fn(|i| input[i].clamp(0.0, 1.0).powf(f64::from(self.gamma[i])));
        self.transform.convert(decoded)
    }
}

impl ToRgb for CalRgb {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        for (input, output) in input.chunks_exact(3).zip(output.chunks_exact_mut(3)) {
            output.copy_from_slice(&self.convert_pixel([input[0], input[1], input[2]]));
        }
        Some(())
    }

    fn convert_in_place(&self, input: &mut [u8]) -> Option<()> {
        for input in input.chunks_exact_mut(3) {
            input.copy_from_slice(&self.convert_pixel([input[0], input[1], input[2]]));
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{ColorSpace, ToLuma};
    use hayro_syntax::object::{FromBytes, Object};
    use moxcms::{ColorProfile, Layout, TransformOptions};

    fn space(definition: &str) -> ColorSpace {
        ColorSpace::from_pdf_object(Object::from_bytes(definition.as_bytes()).unwrap())
            .expect("valid calibrated space")
    }

    fn rgb(space: &ColorSpace, input: &[u8]) -> Vec<u8> {
        let mut output = vec![0; input.len() / space.component_count() * 3];
        space.convert(input, &mut output).unwrap();
        output
    }

    #[test]
    fn calibrated_neutral_ramps_match_independent_gray_cms() {
        let ramp: Vec<u8> = (0..=255).collect();
        for gamma in [0.5, 1.0, 2.0, 2.222, 4.0] {
            let mut reference = vec![0; 256 * 3];
            ColorProfile::new_gray_with_gamma(gamma)
                .create_transform_8bit(
                    Layout::Gray,
                    &ColorProfile::new_srgb(),
                    Layout::Rgb,
                    TransformOptions::default(),
                )
                .unwrap()
                .transform(&ramp, &mut reference)
                .unwrap();
            for white in ["1 1 1", "0.96422 1 0.82521", "0.95047 1 1.08883"] {
                let gray = space(&format!(
                    "[/CalGray <</WhitePoint[{white}]/Gamma {gamma}>>]"
                ));
                let output = rgb(&gray, &ramp);
                assert!(
                    output
                        .iter()
                        .zip(&reference)
                        .all(|(a, b)| a.abs_diff(*b) <= 1),
                    "{white}, gamma {gamma}"
                );
                assert_eq!(&output[..3], &[0; 3]);
                assert_eq!(&output[765..], &[255; 3]);
                assert!(output.chunks_exact(3).all(|p| p[0] == p[1] && p[0] == p[2]));
                assert!(
                    output
                        .chunks_exact(3)
                        .map(|p| p[0])
                        .collect::<Vec<_>>()
                        .windows(2)
                        .all(|w| w[0] <= w[1])
                );
                let mut luma = ramp.clone();
                gray.to_luma(&mut luma).unwrap();
                assert!(
                    luma.iter()
                        .zip(output.chunks_exact(3))
                        .all(|(a, b)| *a == b[0])
                );

                let wp: Vec<&str> = white.split_whitespace().collect();
                let cal_rgb = space(&format!(
                    "[/CalRGB <</WhitePoint[{white}]/Gamma[{gamma} {gamma} {gamma}]/Matrix[{} 0 0 0 1 0 0 0 {}]>>]",
                    wp[0], wp[2]
                ));
                assert_eq!(
                    rgb(
                        &cal_rgb,
                        &ramp.iter().flat_map(|v| [*v; 3]).collect::<Vec<_>>()
                    ),
                    output
                );
            }
        }
        let gray = space("[/CalGray <</WhitePoint[1 1 1]/Gamma 2>>]");
        assert_eq!(rgb(&gray, &[128]), [137; 3]);
    }

    #[test]
    fn calibrated_rgb_preserves_matrix_columns_gamma_and_clipping() {
        let matrix = "0.4124564 0.2126729 0.0193339 0.3575761 0.7151522 0.1191920 0.1804375 0.0721750 0.9503041";
        let calibrated = space(&format!(
            "[/CalRGB <</WhitePoint[0.95047 1 1.08883]/Matrix[{matrix}]/Gamma[1 2 3]>>]"
        ));
        assert_eq!(
            rgb(&calibrated, &[255, 0, 0, 0, 255, 0, 0, 0, 255]),
            [255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
        assert_eq!(rgb(&calibrated, &[128; 3]), [188, 137, 100]);
        let d50 = space(
            "[/CalRGB <</WhitePoint[0.96422 1 0.82521]/Matrix[0.96422 0 0 0 1 0 0 0 0.82521]>>]",
        );
        assert_eq!(rgb(&d50, &[128; 3]), [188; 3]);
        // Literal identity means XYZ, not rescaled RGB primaries.
        let identity = space("[/CalRGB <</WhitePoint[0.95047 1 1.08883]>>]");
        assert_eq!(rgb(&identity, &[255, 0, 0]), [255, 0, 67]);
        assert_ne!(rgb(&identity, &[128; 3]), [188; 3]);
        let singular = space("[/CalRGB <</WhitePoint[1 1 1]/Matrix[-1 0 0 0 0 0 0 0 0]>>]");
        assert_eq!(rgb(&singular, &[0; 3]), [0; 3]);
        assert_eq!(rgb(&singular, &[255, 0, 0])[0], 0);
        let mut inplace = vec![128; 3];
        calibrated.convert_in_place(&mut inplace).unwrap();
        assert_eq!(inplace, rgb(&calibrated, &[128; 3]));
    }

    #[test]
    fn calibrated_paint_components_are_not_quantized_before_gamma() {
        for definition in [
            "[/CalGray <</WhitePoint[1 1 1]>>]",
            "[/CalRGB <</WhitePoint[1 1 1]>>]",
        ] {
            let calibrated = space(definition);
            for (value, expected) in [(0.001, 3), (0.002, 7), (0.003, 10)] {
                let mut output = [0; 3];
                calibrated
                    .convert_values(&vec![value; calibrated.component_count()], &mut output)
                    .unwrap();
                assert_eq!(output, [expected; 3]);
            }
        }
    }

    #[test]
    fn calibrated_black_is_xyz_and_chromatic_gray_keeps_rgb() {
        let gray = space("[/CalGray <</WhitePoint[1 1 1]/BlackPoint[0.1 0.1 0.1]>>]");
        assert_eq!(
            rgb(&gray, &[0, 51, 128, 255]),
            [0, 0, 0, 94, 94, 94, 178, 178, 178, 255, 255, 255]
        );
        let gray = space("[/CalGray <</WhitePoint[1 1 1]/BlackPoint[0.05 0.1 0.15]>>]");
        let calibrated = space("[/CalRGB <</WhitePoint[1 1 1]/BlackPoint[0.05 0.1 0.15]>>]");
        assert_eq!(rgb(&gray, &[128]), rgb(&calibrated, &[128; 3]));
        assert_ne!(rgb(&gray, &[128]), [178; 3]);
        let mut samples = [0, 64, 128, 255];
        assert!(gray.to_luma(&mut samples).is_none());
        assert_eq!(samples, [0, 64, 128, 255]);
        assert!(gray.to_luma(&mut samples).is_none());
    }

    #[test]
    fn calibrated_parameters_fail_closed_including_typed_iterator_tails() {
        for definition in [
            "[/CalGray <<>>]",
            "[/CalGray <</WhitePoint[0 1 1]>>]",
            "[/CalGray <</WhitePoint[1 2 1]>>]",
            "[/CalGray <</WhitePoint[1 1 -1]>>]",
            "[/CalGray <</WhitePoint[1 1 1 /tail]>>]",
            "[/CalGray <</WhitePoint[1 /bad 1]>>]",
            "[/CalGray <</WhitePoint[1 1 1]/Gamma 0>>]",
            "[/CalGray <</WhitePoint[1 1 1]/Gamma -1>>]",
            "[/CalGray <</WhitePoint[1 1 1]/Gamma /bad>>]",
            "[/CalGray <</WhitePoint[1 1 1]/BlackPoint[-1 0 0]>>]",
            "[/CalGray <</WhitePoint[1 1 1]/BlackPoint[1 1 1]>>]",
            "[/CalRGB <</WhitePoint[1 1 1]/BlackPoint[2 2 2]>>]",
            "[/CalGray <</WhitePoint[1 1 1]/BlackPoint[0 0 0 null]>>]",
            "[/CalRGB <</WhitePoint[1 1 1]/Gamma 2>>]",
            "[/CalRGB <</WhitePoint[1 1 1]/Gamma[1 0 1]>>]",
            "[/CalRGB <</WhitePoint[1 1 1]/Gamma[1 1 1 false]>>]",
            "[/CalRGB <</WhitePoint[1 1 1]/Matrix[1 0 0 0 1 0 0 0]>>]",
            "[/CalRGB <</WhitePoint[1 1 1]/Matrix[1 0 0 0 1 0 0 0 1 null]>>]",
            "[/CalRGB <</WhitePoint[1 1 1]>> null]",
            "[/Pattern [/CalGray <<>>]]",
            "[/Indexed [/CalGray <<>>] 0 <00>]",
            "[/Separation /Spot [/CalRGB <<>>] <</FunctionType 2/Domain[0 1]/C0[0 0 0]/C1[1 1 1]/N 1>>]",
        ] {
            assert!(
                ColorSpace::from_pdf_object(Object::from_bytes(definition.as_bytes()).unwrap())
                    .is_none(),
                "{definition}"
            );
        }
        let overflow = format!(
            "[/CalGray <</WhitePoint[1 1 1]/Gamma {}.0>>]",
            "9".repeat(60)
        );
        assert!(
            ColorSpace::from_pdf_object(Object::from_bytes(overflow.as_bytes()).unwrap()).is_none()
        );
        let explicit = space("[/CalGray <</WhitePoint[1 1 1]/Gamma 1/BlackPoint[0 0 0]>>]");
        let defaults = space("[/CalGray <</WhitePoint[1 1 1]>>]");
        assert_eq!(
            rgb(&explicit, &[0, 128, 255]),
            rgb(&defaults, &[0, 128, 255])
        );
    }
}
