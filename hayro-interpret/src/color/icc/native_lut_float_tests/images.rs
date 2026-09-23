use super::*;
use crate::FloatImageError as Error;
use crate::jpx_fixtures as jpx;
use crate::x_object::{decode_rgb_f32, decode_rgb_f64};

// Independent original-profile equations, including the existing binary32
// tint-function boundary. Generation/provenance and full-domain controls live
// in external pdf-test-suite/reports/gray-lut16-transfer-images-2026-09-23.
const ROUTES: &[u8] = include_bytes!("image-routes.f64");
const DEPTHS: &[u8] = include_bytes!("image-depths.f64");

fn compare(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.chunks_exact(8).zip(expected.chunks_exact(8)) {
        let actual = f64::from_le_bytes(a.try_into().unwrap());
        let expected = f64::from_le_bytes(e.try_into().unwrap());
        assert!((actual - expected).abs() <= 1e-12, "{actual} != {expected}");
    }
}

#[test]
fn gray_lut_images_preserve_decode_defaults_palette_and_tint_components() {
    let samples: Vec<_> = references::SAMPLES
        .iter()
        .flat_map(|&v| (v as u16).to_be_bytes())
        .collect();
    let stride = references::SAMPLES.len() * 24;
    assert_eq!(ROUTES.len(), stride * 6);
    let tint = "<</FunctionType 2/Domain[0 1]/C0[.25]/C1[.75]/N 1>>";
    for (space, decode, route, two_tints) in [
        ("[/ICCBased 5 0 R]".to_string(), "", 0, false),
        ("/DeviceGray".to_string(), "", 0, false),
        ("[/ICCBased 5 0 R]".to_string(), "/Decode[1 0]", 1, false),
        (
            "[/ICCBased 5 0 R]".to_string(),
            "/Decode[-.25 1.25]",
            2,
            false,
        ),
        (
            "[/Indexed[/ICCBased 5 0 R]3<0055aaff>]".to_string(),
            "/Decode[0 3]",
            3,
            false,
        ),
        (
            format!("[/Separation/Ink[/ICCBased 5 0 R]{tint}]"),
            "",
            4,
            false,
        ),
        (
            format!("[/DeviceN[/Ink][/ICCBased 5 0 R]{tint}]"),
            "",
            4,
            false,
        ),
        (
            "[/DeviceN[/Ink/None][/ICCBased 5 0 R]7 0 R]".to_string(),
            "",
            5,
            true,
        ),
    ] {
        let paired: Vec<_> = references::SAMPLES
            .iter()
            .flat_map(|&v| [(v as u16), 65535 - v as u16])
            .flat_map(u16::to_be_bytes)
            .collect();
        let extra = two_tints.then(|| {
            b"<</FunctionType 4/Domain[0 1 0 1]/Range[0 1]/Length 7>>stream\n{ pop }\nendstream"
                .to_vec()
        });
        image::with_image(
            &format!("/BitsPerComponent 16/ColorSpace{space}{decode}"),
            if two_tints { &paired } else { &samples },
            extra,
            (references::SAMPLES.len() as u32, 1),
            PROFILE,
            &Cache::new(),
            |image| {
                let output = decode_rgb_f64(image, stride as u64, || true).unwrap();
                compare(&output.data, &ROUTES[route * stride..(route + 1) * stride]);
            },
        );
    }
}

#[test]
fn gray_lut_packed_and_jpx_samples_preserve_native_depth_and_storage_contracts() {
    assert_eq!(DEPTHS.len(), 16 * 6 * 24);
    for bits in [1_u32, 2, 4, 8, 16] {
        let maximum = (1_u32 << bits) - 1;
        let samples = [0, maximum / 2, maximum, 1, maximum - 1, maximum.div_ceil(2)];
        let row_bytes = (3 * bits).div_ceil(8) as usize;
        let mut packed = vec![0; row_bytes * 2];
        for (row, values) in samples.chunks_exact(3).enumerate() {
            for (pixel, &sample) in values.iter().enumerate() {
                for bit in 0..bits as usize {
                    let at = pixel * bits as usize + bit;
                    packed[row * row_bytes + at / 8] |=
                        (((sample >> (bits as usize - 1 - bit)) & 1) as u8) << (7 - at % 8);
                }
            }
        }
        image::with_image(
            &format!("/BitsPerComponent {bits}/ColorSpace[/ICCBased 5 0 R]"),
            &packed,
            None,
            (3, 2),
            PROFILE,
            &Cache::new(),
            |image| {
                let expected = &DEPTHS[(bits as usize - 1) * 144..bits as usize * 144];
                compare(&decode_rgb_f64(image, 144, || true).unwrap().data, expected);
                let output = decode_rgb_f32(image, 72, || true).unwrap();
                assert_eq!(output.data.len(), 72);
                for (a, e) in output.data.chunks_exact(4).zip(expected.chunks_exact(8)) {
                    let actual = f32::from_le_bytes(a.try_into().unwrap());
                    let expected = f64::from_le_bytes(e.try_into().unwrap()) as f32;
                    assert_eq!(actual, expected);
                }
            },
        );
    }
    for (index, data) in [
        jpx::DEPTH_1,
        jpx::DEPTH_2,
        jpx::DEPTH_3,
        jpx::DEPTH_4,
        jpx::DEPTH_5,
        jpx::DEPTH_6,
        jpx::DEPTH_7,
        jpx::DEPTH_8,
        jpx::DEPTH_9,
        jpx::DEPTH_10,
        jpx::DEPTH_11,
        jpx::DEPTH_12,
        jpx::DEPTH_13,
        jpx::DEPTH_14,
        jpx::DEPTH_15,
        jpx::DEPTH_16,
    ]
    .into_iter()
    .enumerate()
    {
        image::with_image(
            "/ColorSpace[/ICCBased 5 0 R]/Filter/JPXDecode/BitsPerComponent 1/Decode false",
            data,
            None,
            (3, 2),
            PROFILE,
            &Cache::new(),
            |image| {
                compare(
                    &decode_rgb_f64(image, 144, || true).unwrap().data,
                    &DEPTHS[index * 144..(index + 1) * 144],
                );
            },
        );
    }
    for data in [jpx::DEPTH_17, jpx::GRAY_ALPHA16] {
        image::with_image(
            "/ColorSpace[/ICCBased 5 0 R]/Filter/JPXDecode",
            data,
            None,
            (3, 2),
            PROFILE,
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

#[test]
fn gray_lut_images_keep_mask_limits_cancellation_and_scratch_ownership() {
    let cache = Cache::with_limits(4, 32 * 1024 * 1024);
    let samples = vec![128; 2050];
    let header = "/BitsPerComponent 16/ColorSpace[/ICCBased 5 0 R]";
    image::with_image(
        header,
        &samples,
        None,
        (1025, 1),
        PROFILE,
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
            let complete = decode_rgb_f64(image, 24600, || true).unwrap();
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
            PROFILE,
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
        PROFILE,
        &cache,
        |image| {
            assert!(matches!(
                decode_rgb_f64(image, 24600, || true),
                Err(Error::Decode)
            ));
        },
    );
}
