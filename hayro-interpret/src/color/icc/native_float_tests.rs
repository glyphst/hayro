use super::*;
use crate::FloatImageError as Error;
use crate::x_object::decode_rgb_f64;

use crate::jpx_fixtures as jpx;

fn floats(bytes: &[u8]) -> impl Iterator<Item = f64> + '_ {
    bytes
        .chunks_exact(8)
        .map(|v| f64::from_le_bytes(v.try_into().unwrap()))
}

#[test]
fn native_float_conversion_keeps_output_precision_before_strong_transfer() {
    let input: Vec<_> = (0..=65535_u16).map(|v| f64::from(v) / 65535.0).collect();
    for lab in [false, true] {
        for gamma in [4, 8] {
            let source = ICCProfile::new(&profile_bytes(lab, gamma), 1).unwrap();
            for intent in INTENTS {
                let source = source.with_intent(intent).unwrap();
                let mut actual = vec![0.0; input.len() * 3];
                source.convert_native_f64(&input, &mut actual).unwrap();
                let mut early_rounding_error = 0;
                for (&sample, pixel) in input.iter().zip(actual.chunks_exact(3)) {
                    let expected = expected_float(lab, gamma, sample);
                    for &value in pixel {
                        assert!(
                            (value - expected).abs() <= 0.00002,
                            "lab={lab} gamma={gamma} intent={intent:?} sample={sample}: {value} != {expected}"
                        );
                        let wanted = encode_component(expected.powi(16)).unwrap();
                        let actual = encode_component(value.powi(16)).unwrap();
                        assert!(actual.abs_diff(wanted) <= 1);
                        let rounded = f64::from(encode_component(value).unwrap()) / 255.0;
                        early_rounding_error = early_rounding_error
                            .max(encode_component(rounded.powi(16)).unwrap().abs_diff(wanted));
                    }
                }
                assert!(early_rounding_error >= 7);
            }
        }
    }
}

#[test]
fn icc_float_images_preserve_decode_defaults_palette_and_tint_alternates() {
    let samples: Vec<_> = (0..=65535_u16).flat_map(u16::to_be_bytes).collect();
    for (space, decode, low, high, palette, tint) in [
        ("[/ICCBased 5 0 R]", "", 0.0, 1.0, false, false),
        ("/DeviceGray", "/Decode[1 0]", 1.0, 0.0, false, false),
        (
            "[/ICCBased 5 0 R]",
            "/Decode[.125 .875]",
            0.125,
            0.875,
            false,
            false,
        ),
        (
            "[/Indexed[/ICCBased 5 0 R]3<0055aaff>]",
            "/Decode[0 3]",
            0.0,
            1.0,
            true,
            false,
        ),
        (
            "[/Separation/GrayInk[/ICCBased 5 0 R]<</FunctionType 2/Domain[0 1]/C0[.25]/C1[.75]/N 1>>]",
            "",
            0.25,
            0.75,
            false,
            true,
        ),
        (
            "[/DeviceN[/GrayInk][/ICCBased 5 0 R]<</FunctionType 2/Domain[0 1]/C0[.25]/C1[.75]/N 1>>]",
            "",
            0.25,
            0.75,
            false,
            true,
        ),
    ] {
        let header = format!("/BitsPerComponent 16/ColorSpace{space}{decode}");
        image::with_image(
            &header,
            &samples,
            None,
            (256, 256),
            &profile_bytes(false, 4),
            &Cache::new(),
            |image| {
                let output = decode_rgb_f64(image, 65536 * 24, || true).unwrap();
                assert_eq!(output.data.len(), 65536 * 24);
                for (i, value) in floats(&output.data).enumerate() {
                    let mut input = (i / 3) as f64 / 65535.0;
                    if palette {
                        input = (input * 3.0).round() / 3.0;
                    }
                    if tint {
                        input = f64::from(input as f32);
                    }
                    let mut input = low + (high - low) * input;
                    if tint {
                        input = f64::from(input as f32);
                    }
                    let expected = expected_float(false, 4, input);
                    assert!(
                        (value - expected).abs() <= 0.00002,
                        "{space} pixel {}: {value} != {expected}",
                        i / 3
                    );
                }
            },
        );
    }
}

#[test]
fn icc_float_images_reject_masks_truncation_and_limits_and_release_batch_quota() {
    let profile = profile_bytes(false, 4);
    let header = "/BitsPerComponent 16/ColorSpace[/ICCBased 5 0 R]";
    let samples = vec![128; 2050];
    let cache = Cache::with_limits(4, 32 * 1024 * 1024);
    image::with_image(
        header,
        &samples,
        None,
        (1025, 1),
        &profile,
        &cache,
        |image| {
            assert!(matches!(
                decode_rgb_f64(image, 24599, || true),
                Err(Error::Limit { requested: 24600 })
            ));
            assert!(matches!(
                decode_rgb_f64(image, 24600, || false),
                Err(Error::Cancelled)
            ));
            let mut calls = 0;
            assert!(matches!(
                decode_rgb_f64(image, 24600, || {
                    calls += 1;
                    calls < 4
                }),
                Err(Error::Cancelled)
            ));
            assert_eq!(calls, 4);
            let retained = cache.stats().icc_bytes;
            assert!(retained > 0);
            let complete = decode_rgb_f64(image, 24600, || true).unwrap();
            assert_eq!(complete.data.len(), 24600);
            assert_eq!(cache.stats().icc_bytes, retained);
            let held = cache
                .icc_memory()
                .reserve(32 * 1024 * 1024 - retained - 1024)
                .unwrap();
            assert!(matches!(
                decode_rgb_f64(image, 24600, || true),
                Err(Error::Decode)
            ));
            drop(held);
            assert_eq!(
                decode_rgb_f64(image, 24600, || true).unwrap().data,
                complete.data
            );
            assert_eq!(cache.stats().icc_bytes, retained);
        },
    );
    cache.clear();
    assert_eq!(cache.stats().icc_bytes, 0);
    for extra in [
        "/Mask[0 1]",
        "/SMask null",
        "/Decode[0 false]",
        "/Decode[0 1 2]",
    ] {
        image::with_image(
            &format!("{header}{extra}"),
            &samples,
            None,
            (1025, 1),
            &profile,
            &cache,
            |image| {
                assert!(
                    matches!(
                        decode_rgb_f64(image, 24600, || true),
                        Err(Error::Unsupported)
                    ),
                    "{extra}"
                );
            },
        );
    }
    image::with_image(
        header,
        &samples[..2049],
        None,
        (1025, 1),
        &profile,
        &cache,
        |image| {
            assert!(matches!(
                decode_rgb_f64(image, 24600, || true),
                Err(Error::Decode)
            ));
        },
    );
}

#[test]
fn icc_float_jpx_preserves_native_depth_and_dictionary_precedence() {
    for (bits, data) in [
        (1, jpx::DEPTH_1),
        (2, jpx::DEPTH_2),
        (3, jpx::DEPTH_3),
        (4, jpx::DEPTH_4),
        (5, jpx::DEPTH_5),
        (6, jpx::DEPTH_6),
        (7, jpx::DEPTH_7),
        (8, jpx::DEPTH_8),
        (9, jpx::DEPTH_9),
        (10, jpx::DEPTH_10),
        (11, jpx::DEPTH_11),
        (12, jpx::DEPTH_12),
        (13, jpx::DEPTH_13),
        (14, jpx::DEPTH_14),
        (15, jpx::DEPTH_15),
        (16, jpx::DEPTH_16),
        (17, jpx::DEPTH_17),
    ] {
        for lab in [false, true] {
            let profile = profile_bytes(lab, 4);
            image::with_image(
                "/ColorSpace[/ICCBased 5 0 R]/Filter/JPXDecode/BitsPerComponent 1/Decode false",
                data,
                None,
                (3, 2),
                &profile,
                &Cache::new(),
                |image| {
                    let result = decode_rgb_f64(image, 144, || true);
                    if bits > 16 {
                        assert!(matches!(result, Err(Error::Unsupported)));
                        return;
                    }
                    let output = result.unwrap();
                    let maximum = (1_u32 << bits) - 1;
                    let samples = [0, maximum / 2, maximum, 1, maximum - 1, maximum.div_ceil(2)];
                    assert_eq!(output.data.len(), 144);
                    for (i, actual) in floats(&output.data).enumerate() {
                        let expected =
                            expected_float(lab, 4, f64::from(samples[i / 3]) / f64::from(maximum));
                        assert!(
                            (actual - expected).abs() <= 0.00002,
                            "bits={bits} lab={lab} pixel={}: {actual} != {expected}",
                            i / 3
                        );
                    }
                },
            );
        }
    }
    for extra in ["", "/SMaskInData 1", "/SMaskInData 2"] {
        image::with_image(
            &format!("/ColorSpace[/ICCBased 5 0 R]/Filter/JPXDecode {extra}"),
            jpx::GRAY_ALPHA16,
            None,
            (3, 2),
            &profile_bytes(false, 4),
            &Cache::new(),
            |image| {
                assert!(matches!(
                    decode_rgb_f64(image, 144, || true),
                    Err(Error::Unsupported)
                ));
            },
        );
    }
}
