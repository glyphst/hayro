use super::*;
use moxcms::{ToneReprCurve, Xyzd};

#[test]
fn unequal_channels_match_independent_gamma_controls_across_intents_and_consumers() {
    let input: Vec<u8> = (0..=255).flat_map(|v| [v, 255 - v, v / 2]).collect();
    for version in [2, 4] {
        for gammas in [[1, 1, 2], [2, 1, 1], [1, 2, 1], [1, 2, 3], [2, 2, 2]] {
            let mut source = ColorProfile::new_srgb();
            source.cicp = None;
            source.media_white_point = Some(Xyzd::new(D50[0], D50[1], D50[2]));
            [source.red_trc, source.green_trc, source.blue_trc] = gammas.map(|gamma| {
                Some(if version == 2 {
                    ToneReprCurve::Lut(vec![gamma * 256])
                } else {
                    ToneReprCurve::Parametric(vec![f32::from(gamma)])
                })
            });
            let mut bytes = source.encode().unwrap();
            bytes[8..12].copy_from_slice(&[version, 0x20, 0, 0]);
            let profile = ICCProfile::new(&bytes, 3).unwrap();
            drop(bytes);
            for intent in [
                RenderingIntent::Perceptual,
                RenderingIntent::RelativeColorimetric,
                RenderingIntent::Saturation,
                RenderingIntent::AbsoluteColorimetric,
            ] {
                let bound = profile.with_intent(intent).unwrap();
                let mut output = vec![0; input.len()];
                bound.convert(&input, &mut output).unwrap();
                for (i, (&value, &actual)) in input.iter().zip(&output).enumerate() {
                    let linear = (f64::from(value) / 255.0).powi(i32::from(gammas[i % 3]));
                    let srgb = if linear <= 0.0031308 {
                        linear * 12.92
                    } else {
                        1.055 * linear.powf(1.0 / 2.4) - 0.055
                    };
                    let expected = (srgb * 255.0).round() as u8;
                    assert!(
                        actual.abs_diff(expected) <= 1,
                        "v{version} {gammas:?} {intent:?} channel={} {value} -> {actual}, expected {expected}",
                        i % 3
                    );
                }
                let mut in_place = input.clone();
                bound.convert_in_place(&mut in_place).unwrap();
                assert_eq!(in_place, output);
                for (pixel, expected) in input.chunks_exact(3).zip(output.chunks_exact(3)) {
                    let mut scalar = [0; 3];
                    bound.convert(pixel, &mut scalar).unwrap();
                    assert!(
                        scalar
                            .iter()
                            .zip(expected)
                            .all(|(a, b)| a.abs_diff(*b) <= 1)
                    );
                }
            }
        }
    }
}
