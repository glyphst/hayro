use super::*;
use crate::color::RenderingIntent;
use crate::color::icc::lut_tests::{expected_native, profile_bytes};

const INTENTS: [RenderingIntent; 4] = [
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
    RenderingIntent::AbsoluteColorimetric,
];

#[test]
fn native_rgb_and_cmyk_luts_preserve_channels_intents_and_batch_boundaries() {
    // Original LUT16 bytes and independent affine-CMYK/power-curve equations
    // are shared with the byte-path controls, without reducing these inputs.
    for version in [2, 4] {
        for components in [3, 4] {
            for separate in [false, true] {
                let profile =
                    ICCProfile::new(&profile_bytes(version, components, separate), components)
                        .unwrap();
                for (index, intent) in INTENTS.into_iter().enumerate() {
                    let bound = profile.with_intent(intent).unwrap();
                    for length in [0, 1, 1023, 1024, 1025, 4099] {
                        let input: Vec<_> = (0..length)
                            .flat_map(|sample| {
                                (0..components).map(move |channel| {
                                    f64::from(((sample * (2 * channel + 1) * 31) % 65536) as u16)
                                        / 65535.0
                                })
                            })
                            .collect();
                        let mut actual = vec![0; length * 3];
                        bound.convert_native(&input, &mut actual).unwrap();
                        for (samples, rgb) in
                            input.chunks_exact(components).zip(actual.chunks_exact(3))
                        {
                            let expected = expected_native(samples, version, separate, index);
                            assert!(
                                rgb.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1),
                                "v{version} channels={components} separate={separate} intent={intent:?} input={samples:?} actual={rgb:?} expected={expected:?}"
                            );
                        }
                    }
                    assert!(bound.convert_native(&[0.0; 5], &mut [0; 3]).is_none());
                    assert!(
                        bound
                            .convert_native(&vec![f64::NAN; components], &mut [0; 3])
                            .is_none()
                    );
                    assert!(
                        bound
                            .convert_native(&vec![f64::INFINITY; components], &mut [0; 3])
                            .is_none()
                    );
                }
            }
        }
    }
}

#[test]
fn native_gray_luts_keep_selected_intents_instead_of_the_unselected_trc() {
    let source = ICCProfile::new(include_bytes!("gray-lut-input.bin"), 1).unwrap();
    let input: Vec<_> = (0..=65535_u16).map(|v| f64::from(v) / 65535.0).collect();
    for (index, intent) in INTENTS.into_iter().enumerate() {
        let bound = source.with_intent(intent).unwrap();
        let mut actual = vec![0; input.len() * 3];
        bound.convert_native(&input, &mut actual).unwrap();
        for (&sample, rgb) in input.iter().zip(actual.chunks_exact(3)) {
            let linear = sample.powi([1, 2, 3, 2][index]);
            let encoded = if linear <= 0.0031308 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            let expected = (encoded * 255.0).round() as u8;
            assert!(
                rgb.iter().all(|&value| value.abs_diff(expected) <= 1),
                "{intent:?} {sample}: {rgb:?}, expected {expected}"
            );
        }
    }
}
