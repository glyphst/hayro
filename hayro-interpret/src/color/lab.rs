use super::ToRgb;
use super::cie::{CieRgb, numbers};
use hayro_syntax::object::Dict;
use hayro_syntax::object::dict::keys::{BLACK_POINT, RANGE, WHITE_POINT};
use moxcms::Matrix3d;

#[derive(Debug, Clone)]
pub(crate) struct Lab {
    pub(super) range: [f32; 4],
    transform: CieRgb,
}

impl Lab {
    pub(super) fn new(dict: &Dict<'_>) -> Option<Self> {
        let white = numbers(dict, WHITE_POINT, None)?;
        let black = numbers(dict, BLACK_POINT, Some([0.0; 3]))?;
        let range = numbers(dict, RANGE, Some([-100.0, 100.0, -100.0, 100.0]))?;
        if range[0] > range[1] || range[2] > range[3] {
            return None;
        }
        let transform = CieRgb::new(white, black)?.compose(Matrix3d {
            v: [
                [f64::from(white[0]), 0.0, 0.0],
                [0.0, f64::from(white[1]), 0.0],
                [0.0, 0.0, f64::from(white[2])],
            ],
        })?;
        Some(Self { range, transform })
    }

    pub(super) fn convert_real(&self, input: [f64; 3]) -> [f64; 3] {
        // PDF 32000-1, 8.6.5.4: clamp L*, a*, b* to their source ranges,
        // then apply the two implicit transformation stages. No byte encoding
        // or synthetic ICC profile lies between Lab and XYZ.
        let lightness = input[0].clamp(0.0, 100.0);
        let a = input[1].clamp(f64::from(self.range[0]), f64::from(self.range[1]));
        let b = input[2].clamp(f64::from(self.range[2]), f64::from(self.range[3]));
        let m = (lightness + 16.0) / 116.0;
        let lmn = [m + a / 500.0, m, m - b / 200.0];
        self.transform.convert(lmn.map(|value| {
            if value >= 6.0 / 29.0 {
                value * value * value
            } else {
                (108.0 / 841.0) * (value - 4.0 / 29.0)
            }
        }))
    }

    pub(super) fn convert_components(&self, input: [f64; 3]) -> [u8; 3] {
        self.convert_real(input).map(|v| (v * 255.0).round() as u8)
    }

    fn convert_pixel(&self, input: [u8; 3]) -> [u8; 3] {
        let ranges = [
            (0.0, 100.0),
            (self.range[0], self.range[1]),
            (self.range[2], self.range[3]),
        ];
        self.convert_components(core::array::from_fn(|i| {
            let (low, high) = ranges[i];
            let t = f64::from(input[i]) / 255.0;
            f64::from(low) * (1.0 - t) + f64::from(high) * t
        }))
    }
}

impl ToRgb for Lab {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        for (input, output) in input.chunks_exact(3).zip(output.chunks_exact_mut(3)) {
            output.copy_from_slice(&self.convert_pixel([input[0], input[1], input[2]]));
        }
        Some(())
    }

    fn convert_in_place(&self, input: &mut [u8]) -> Option<()> {
        for pixel in input.chunks_exact_mut(3) {
            pixel.copy_from_slice(&self.convert_pixel([pixel[0], pixel[1], pixel[2]]));
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorSpace;
    use hayro_syntax::object::{FromBytes, Object};

    fn space(definition: &str) -> ColorSpace {
        ColorSpace::from_pdf_object(Object::from_bytes(definition.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn lab_equations_match_decimal_d65_reference_and_preserve_real_paints() {
        let lab = space("[/Lab <</WhitePoint[.95047 1 1.08883]>>]");
        // Independently evaluated with 50-digit Decimal arithmetic, PDF Table
        // 65 and the declared XYZ-to-sRGB matrix (before final byte storage).
        for (input, expected) in [
            ([0.0, 0.0, 0.0], [0.0; 3]),
            ([8.0, 0.0, 0.0], [0.092212279, 0.092212273, 0.092212275]),
            ([50.0, 0.0, 0.0], [0.466326622, 0.466326602, 0.466326609]),
            ([100.0, 0.0, 0.0], [1.0; 3]),
            ([50.0, 20.0, -30.0], [0.496359879, 0.429259588, 0.666814521]),
        ] {
            let actual = lab.image_rgb_f64(&input).unwrap();
            for (a, b) in actual.into_iter().zip(expected) {
                assert!((a - b).abs() < 2e-6, "{input:?}: {actual:?}");
            }
            let paint = lab.to_rgba(&input.map(|v| v as f32), 0.75);
            assert_eq!(
                paint.components,
                [actual[0] as f32, actual[1] as f32, actual[2] as f32, 0.75]
            );
        }
        let low = lab.to_rgba(&[50.0, 0.0, 0.0], 1.0);
        let high = lab.to_rgba(&[50.001, 0.0, 0.0], 1.0);
        assert_ne!(low.components, high.components);
        assert_eq!(
            (low.components[0] * 255.0).round(),
            (high.components[0] * 255.0).round()
        );
    }

    #[test]
    fn lab_range_clips_real_values_and_decodes_lookup_bytes_once() {
        let definition = "[/Lab <</WhitePoint[.95047 1 1.08883]/Range[-10 10 -20 20]>>]";
        let lab = space(definition);
        for (outside, inside) in [
            ([50.0, 50.0, 0.0], [50.0, 10.0, 0.0]),
            ([50.0, -50.0, 100.0], [50.0, -10.0, 20.0]),
            ([-50.0, 0.0, -100.0], [0.0, 0.0, -20.0]),
            ([150.0, 0.0, 0.0], [100.0, 0.0, 0.0]),
        ] {
            assert_eq!(lab.image_rgb_f64(&outside), lab.image_rgb_f64(&inside));
        }
        let indexed = space(&format!("[/Indexed {definition} 1 <80ff008000ff>]"));
        let mut output = [0; 6];
        indexed.convert(&[0, 1], &mut output).unwrap();
        let expected: Vec<u8> = [
            [128.0 / 255.0 * 100.0, 10.0, -20.0],
            [128.0 / 255.0 * 100.0, -10.0, 20.0],
        ]
        .into_iter()
        .flat_map(|v| {
            lab.image_rgb_f64(&v)
                .unwrap()
                .map(|v| (v * 255.0).round() as u8)
        })
        .collect();
        assert_eq!(output.as_slice(), expected);
        let constant = space("[/Lab <</WhitePoint[1 1 1]/Range[0 0 0 0]>>]");
        assert_eq!(
            constant.image_rgb_f64(&[50.0, -500.0, 500.0]),
            constant.image_rgb_f64(&[50.0, 0.0, 0.0])
        );
        let mut bytes = [128, 0, 255];
        constant.convert_in_place(&mut bytes).unwrap();
        assert_eq!(bytes, [119; 3]);
    }

    #[test]
    fn lab_blackpoint_and_chromatic_adaptation_follow_fixed_output_policy() {
        for white in [".95047 1 1.08883", ".96422 1 .82521", "1 1 1"] {
            let lab = space(&format!("[/Lab <</WhitePoint[{white}]>>]"));
            for lightness in [0.0, 7.999, 8.0, 8.001, 50.0, 100.0] {
                // The neutral axis has Y = L*/(24389/27) below L*=8.
                let y = if lightness <= 8.0 {
                    lightness * 27.0 / 24389.0
                } else {
                    ((lightness + 16.0) / 116.0_f64).powi(3)
                };
                let expected = if y <= 0.0031308 {
                    y * 12.92
                } else {
                    1.055 * y.powf(1.0 / 2.4) - 0.055
                };
                assert!(
                    lab.image_rgb_f64(&[lightness, 0.0, 0.0])
                        .unwrap()
                        .iter()
                        .all(|v| (v - expected).abs() < 2e-6)
                );
            }
        }
        let lab = space("[/Lab <</WhitePoint[1 1 1]/BlackPoint[.1 .1 .1]>>]");
        let y = ((50.0 + 16.0) / 116.0_f64).powi(3);
        let expected = 1.055 * ((y - 0.1) / 0.9).powf(1.0 / 2.4) - 0.055;
        assert!(
            lab.image_rgb_f64(&[50.0, 0.0, 0.0])
                .unwrap()
                .iter()
                .all(|v| (v - expected).abs() < 2e-6)
        );
    }

    #[test]
    fn lab_malformed_parameters_fail_closed() {
        for definition in [
            "[/Lab <<>>]",
            "[/Lab <</WhitePoint[0 1 1]>>]",
            "[/Lab <</WhitePoint[1 2 1]>>]",
            "[/Lab <</WhitePoint[1 1 -1]>>]",
            "[/Lab <</WhitePoint[1 1 1 null]>>]",
            "[/Lab <</WhitePoint[1 /bad 1]>>]",
            "[/Lab <</WhitePoint[1 1 1]/BlackPoint[-1 0 0]>>]",
            "[/Lab <</WhitePoint[1 1 1]/BlackPoint[0 0 0 false]>>]",
            "[/Lab <</WhitePoint[1 1 1]/BlackPoint[1 1 1]>>]",
            "[/Lab <</WhitePoint[1 1 1]/Range[-10 10 -20]>>]",
            "[/Lab <</WhitePoint[1 1 1]/Range[-10 10 -20 20 null]>>]",
            "[/Lab <</WhitePoint[1 1 1]/Range[10 -10 -20 20]>>]",
            "[/Lab <</WhitePoint[1 1 1]/Range[-10 10 20 -20]>>]",
            "[/Lab <</WhitePoint[1 1 1]/Range /bad>>]",
            "[/Lab <</WhitePoint[1 1 1]>> null]",
            "[/Indexed [/Lab<<>>] 0 <000000>]",
        ] {
            assert!(
                ColorSpace::from_pdf_object(Object::from_bytes(definition.as_bytes()).unwrap())
                    .is_none(),
                "{definition}"
            );
        }
    }
}
