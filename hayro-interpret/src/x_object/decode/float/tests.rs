use super::*;
use crate::cache::Cache;
use hayro_syntax::Pdf;
use hayro_syntax::object::{ObjectIdentifier, Stream};

fn decode(
    header: &str,
    data: &[u8],
    limit: u64,
    checkpoint: impl FnMut() -> bool,
) -> Result<RgbF32Data, Error> {
    with_image(header, data, |image| {
        decode_rgb_f32(image, limit, checkpoint)
    })
}

fn with_image<T>(header: &str, data: &[u8], run: impl FnOnce(&ImageXObject<'_>) -> T) -> T {
    let mut bytes = format!(
        "%PDF-1.7\n1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
        2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
        3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Resources<<>>>> endobj\n\
        4 0 obj <</Type/XObject/Subtype/Image {header} /Length {}>>\nstream\n",
        data.len()
    )
    .into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream\nendobj\ntrailer <</Root 1 0 R>>\n%%EOF");
    let pdf = Pdf::new(bytes).unwrap();
    let stream = pdf
        .xref()
        .get::<Stream<'_>>(ObjectIdentifier::new(4, 0))
        .unwrap();
    let warning: crate::WarningSinkFn = std::sync::Arc::new(|_| {});
    let image = ImageXObject::new(
        &stream,
        pdf.pages()[0].resources(),
        &warning,
        &Cache::new(),
        None,
    )
    .unwrap();
    run(&image)
}

fn floats(image: &RgbF32Data) -> Vec<f32> {
    image
        .data
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

#[test]
fn direct_sixteen_bit_and_decode_components_do_not_round_to_bytes() {
    let header = "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16";
    let image = decode(header, &[0x80, 0, 0x80, 1], 24, || true).unwrap();
    assert_eq!(
        floats(&image),
        [
            vec![32768.0_f32 / 65535.0; 3],
            vec![32769.0_f32 / 65535.0; 3]
        ]
        .concat()
    );
    assert_eq!(
        (image.width, image.height, image.scale_factors),
        (2, 1, (1.0, 1.0))
    );
    let image = decode(
        &format!("{header} /Decode [1 0]"),
        &[0, 1, 0xff, 0xfe],
        24,
        || true,
    )
    .unwrap();
    assert_eq!(
        floats(&image),
        [vec![65534.0_f32 / 65535.0; 3], vec![1.0_f32 / 65535.0; 3]].concat()
    );
    let image = decode(
        "/Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Decode [.1 .9 .2 .8 .3 .7]",
        &[64, 128, 192],
        12,
        || true,
    )
    .unwrap();
    let expected = [
        (0.1_f64 + 64.0 / 255.0 * 0.8) as f32,
        (0.2_f64 + 128.0 / 255.0 * 0.6) as f32,
        (0.3_f64 + 192.0 / 255.0 * 0.4) as f32,
    ];
    assert!(
        floats(&image)
            .iter()
            .zip(expected)
            .all(|(a, b)| (a - b).abs() < 1e-7)
    );
}

#[test]
fn packed_sample_rows_and_calibrated_conversion_preserve_real_values() {
    for (bits, data, expected) in [
        (1, vec![0x80, 0x60], vec![1.0, 0.0, 0.0, 0.0, 1.0, 1.0]),
        (
            2,
            vec![0x6c, 0x90],
            vec![1.0 / 3.0, 2.0 / 3.0, 1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0],
        ),
        (
            4,
            vec![0x18, 0xf0, 0x80, 0x10],
            vec![1.0 / 15.0, 8.0 / 15.0, 1.0, 8.0 / 15.0, 0.0, 1.0 / 15.0],
        ),
    ] {
        let image = decode(
            &format!("/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent {bits}"),
            &data,
            72,
            || true,
        )
        .unwrap();
        assert_eq!(
            floats(&image),
            expected
                .into_iter()
                .flat_map(|v| [v; 3])
                .collect::<Vec<_>>()
        );
    }
    let image = decode("/Width 1 /Height 1 /ColorSpace [/CalGray <</WhitePoint[0.95047 1 1.08883]/Gamma 2>>] /BitsPerComponent 16 /Interpolate true", &[0x80,0],12,||true).unwrap();
    let linear = (32768.0_f64 / 65535.0).powi(2);
    let expected = 1.055 * linear.powf(1.0 / 2.4) - 0.055;
    assert!(image.interpolate);
    assert!(
        floats(&image)
            .iter()
            .all(|v| (f64::from(*v) - expected).abs() < 2e-6)
    );
    assert!(
        floats(&image)
            .iter()
            .any(|v| (*v * 255.0).fract().abs() > 0.1)
    );
}

#[test]
fn malformed_truncated_unproved_and_over_budget_images_have_no_output() {
    let header = "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16";
    assert!(matches!(
        decode(header, &[0; 3], 24, || true),
        Err(Error::Decode)
    ));
    assert!(matches!(
        decode(header, &[0; 4], 23, || true),
        Err(Error::Limit { requested: 24 })
    ));
    assert!(matches!(
        decode(header, &[0; 4], 24, || false),
        Err(Error::Cancelled)
    ));
    for extra in [
        "/Decode [0]",
        "/Decode [0 1 false]",
        "/Decode [0 false]",
        "/Decode false",
        "/Mask [0 1]",
        "/SMask null",
    ] {
        assert!(
            matches!(
                decode(&format!("{header} {extra}"), &[0; 4], 24, || true),
                Err(Error::Unsupported)
            ),
            "{extra}"
        );
    }
    assert!(matches!(
        decode(&format!("{header} /Filter /JPXDecode"), &[0; 4], 24, || {
            true
        }),
        Err(Error::Decode)
    ));
    let mut calls = 0;
    let result = decode(
        "/Width 1025 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &vec![128; 1025],
        12300,
        || {
            calls += 1;
            calls < 4
        },
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(calls, 4);
}

#[test]
fn decode_affine_keeps_opposing_bounds_and_clamps_only_after_mapping() {
    assert_eq!(decode_sample(1, 3, -f32::MAX / 2.0, f32::MAX), 0.0);
    assert_eq!(decode_sample(0, 65535, 0.125, 0.875), 0.125);
    assert_eq!(decode_sample(65535, 65535, 0.125, 0.875), 0.875);
    let image = decode(
        "/Width 3 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [-1 2]",
        &[0, 128, 255],
        36,
        || true,
    )
    .unwrap();
    assert_eq!(
        floats(&image),
        [
            vec![0.0; 3],
            vec![(129.0_f64 / 255.0) as f32; 3],
            vec![1.0; 3]
        ]
        .concat()
    );
}

#[test]
fn binary64_decode_retains_source_side_of_binary32_cutoff_and_charges_capacity() {
    let header = "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16";
    with_image(header, &[0xff, 0xfe], |image| {
        let data = decode_rgb_f64(image, 24, || true).unwrap();
        assert_eq!(data.data.len(), 24);
        for bytes in data.data.chunks_exact(8) {
            let value = f64::from_le_bytes(bytes.try_into().unwrap());
            assert_eq!(value, 65534.0 / 65535.0);
            assert!(value < f64::from(value as f32));
        }
        assert!(matches!(
            decode_rgb_f64(image, 23, || true),
            Err(Error::Limit { requested: 24 })
        ));
        assert!(matches!(
            decode_rgb_f64(image, 24, || false),
            Err(Error::Cancelled)
        ));
    });
    with_image(header, &[0xff], |image| {
        assert!(matches!(
            decode_rgb_f64(image, 24, || true),
            Err(Error::Decode)
        ));
    });
    with_image(
        "/Width 1 /Height 1 /ColorSpace [/CalGray <</WhitePoint[0.95047 1 1.08883]/Gamma 2>>] /BitsPerComponent 16",
        &[0x80, 0],
        |image| {
            let wide = decode_rgb_f64(image, 24, || true).unwrap();
            let narrow = decode_rgb_f32(image, 12, || true).unwrap();
            assert!(
                wide.data
                    .chunks_exact(8)
                    .zip(floats(&narrow))
                    .any(|(bytes, value)| {
                        let wide = f64::from_le_bytes(bytes.try_into().unwrap());
                        wide != f64::from(value) && wide as f32 == value
                    })
            );
        },
    );
}

use super::super::jpx_fixtures;

#[test]
fn jpx_samples_retain_native_depth_and_ignore_nonmask_decode_and_dictionary_depth() {
    for (bits, data) in [
        (1, jpx_fixtures::DEPTH_1),
        (2, jpx_fixtures::DEPTH_2),
        (3, jpx_fixtures::DEPTH_3),
        (4, jpx_fixtures::DEPTH_4),
        (5, jpx_fixtures::DEPTH_5),
        (6, jpx_fixtures::DEPTH_6),
        (7, jpx_fixtures::DEPTH_7),
        (8, jpx_fixtures::DEPTH_8),
        (9, jpx_fixtures::DEPTH_9),
        (10, jpx_fixtures::DEPTH_10),
        (11, jpx_fixtures::DEPTH_11),
        (12, jpx_fixtures::DEPTH_12),
        (13, jpx_fixtures::DEPTH_13),
        (14, jpx_fixtures::DEPTH_14),
        (15, jpx_fixtures::DEPTH_15),
        (16, jpx_fixtures::DEPTH_16),
        (17, jpx_fixtures::DEPTH_17),
    ] {
        for extra in [
            "/ColorSpace /DeviceGray",
            "/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]",
            "/ColorSpace /DeviceGray /BitsPerComponent 8 /Decode false",
        ] {
            let header = format!("/Width 3 /Height 2 /Filter /JPXDecode {extra}");
            with_image(&header, data, |image| {
                let result = decode_rgb_f64(image, 144, || true);
                if bits > 16 {
                    assert!(matches!(result, Err(Error::Unsupported)));
                    return;
                }
                let result = result.unwrap();
                let maximum = (1_u32 << bits) - 1;
                let expected = [0, maximum / 2, maximum, 1, maximum - 1, maximum.div_ceil(2)];
                for (pixel, sample) in result.data.chunks_exact(24).zip(expected) {
                    for component in pixel.chunks_exact(8) {
                        assert_eq!(
                            f64::from_le_bytes(component.try_into().unwrap()),
                            f64::from(sample) / f64::from(maximum)
                        );
                    }
                }
                assert!(matches!(
                    decode_rgb_f64(image, 143, || true),
                    Err(Error::Limit { requested: 144 })
                ));
                assert!(matches!(
                    decode_rgb_f64(image, 144, || false),
                    Err(Error::Cancelled)
                ));
            });
        }
    }
}

#[test]
fn jpx_rgb_keeps_native_color_samples_and_pdf_space_overrides_container_conversion() {
    for (bits, width, height, data) in [
        (12, 3, 2, jpx_fixtures::RGB12),
        (16, 3, 2, jpx_fixtures::RGB16),
        (16, 8, 8, jpx_fixtures::RGB16_DWT_MCT),
        (16, 3, 2, jpx_fixtures::RGB16_SYCC),
    ] {
        let header = format!(
            "/Width {width} /Height {height} /Filter /JPXDecode /ColorSpace /DeviceRGB /BitsPerComponent 1 /Decode [1 0]"
        );
        with_image(&header, data, |image| {
            let result = decode_rgb_f64(image, width * height * 24, || true).unwrap();
            let maximum = (1_u32 << bits) - 1;
            for (i, pixel) in result.data.chunks_exact(24).enumerate() {
                for (c, component) in pixel.chunks_exact(8).enumerate() {
                    let sample = (i as u32 * 10711 + c as u32 * 19789 + 32767) & maximum;
                    assert_eq!(
                        f64::from_le_bytes(component.try_into().unwrap()),
                        f64::from(sample) / f64::from(maximum),
                        "depth {bits}, {width}x{height}, pixel {i}, component {c}"
                    );
                }
            }
        });
    }
}

#[test]
fn implicit_jpx_palette_is_resolved_and_raw_transfer_requires_explicit_pdf_space() {
    with_image(
        "/Width 3 /Height 2 /Filter /JPXDecode",
        jpx_fixtures::PALETTE16,
        |image| {
            let context = decode_context(image, None).unwrap();
            assert_eq!(context.bits_per_component, 16);
            assert_eq!(context.color_space.kind(), ColorSpaceKind::DeviceRgb);
            assert!(matches!(
                decode_rgb_f64(image, 144, || true),
                Err(Error::Unsupported)
            ));
            let palette = [[12345, 32768, 65535], [65534, 32769, 1]];
            for (pixel, index) in context.decoded.data.chunks_exact(6).zip([0, 0, 1, 1, 0, 1]) {
                for (component, sample) in pixel.chunks_exact(2).zip(palette[index]) {
                    assert_eq!(u16::from_be_bytes(component.try_into().unwrap()), sample);
                }
            }
        },
    );
    for (header, data) in [
        (
            "/Width 3 /Height 2 /Filter /JPXDecode",
            jpx_fixtures::PALETTE_MIXED,
        ),
        (
            "/Width 3 /Height 2 /Filter /JPXDecode /ColorSpace /DeviceRGB",
            jpx_fixtures::DEPTH_16,
        ),
        (
            "/Width 4 /Height 2 /Filter /JPXDecode",
            jpx_fixtures::DEPTH_16,
        ),
        (
            "/Width 3 /Height 2 /Filter /JPXDecode",
            &jpx_fixtures::DEPTH_16[..64],
        ),
    ] {
        with_image(header, data, |image| {
            assert!(decode_context(image, None).is_none());
        });
    }
    for extra in ["", "/SMaskInData 1", "/SMaskInData 2"] {
        with_image(
            &format!("/Width 3 /Height 2 /Filter /JPXDecode /ColorSpace /DeviceGray {extra}"),
            jpx_fixtures::GRAY_ALPHA16,
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
fn jpx_mask_decode_and_calibrated_conversion_follow_pdf_dictionary_rules() {
    with_image(
        "/Width 3 /Height 2 /Filter /JPXDecode /ImageMask true /BitsPerComponent 1",
        jpx_fixtures::DEPTH_8,
        |image| {
            assert!(decode_context(image, None).is_none());
        },
    );
    with_image(
        "/Width 3 /Height 2 /Filter /JPXDecode /ImageMask true /Decode [1 0] /BitsPerComponent 8",
        jpx_fixtures::DEPTH_1,
        |image| {
            let context = decode_context(image, None).unwrap();
            assert_eq!(context.bits_per_component, 1);
            assert_eq!(context.decode_arr.as_slice(), [(1.0, 0.0)]);
            assert!(matches!(
                decode_rgb_f64(image, 144, || true),
                Err(Error::Unsupported)
            ));
        },
    );
    with_image(
        "/Width 3 /Height 2 /Filter /JPXDecode /ColorSpace [/CalGray <</WhitePoint[0.95047 1 1.08883]/Gamma 2>>] /Decode [1 0]",
        jpx_fixtures::DEPTH_16,
        |image| {
            let result = decode_rgb_f64(image, 144, || true).unwrap();
            for (pixel, sample) in result
                .data
                .chunks_exact(24)
                .zip([0, 32767, 65535, 1, 65534, 32768])
            {
                let linear = (f64::from(sample) / 65535.0).powi(2);
                let expected = if linear <= 0.0031308 {
                    12.92 * linear
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                };
                for component in pixel.chunks_exact(8) {
                    assert!(
                        (f64::from_le_bytes(component.try_into().unwrap()) - expected).abs() < 2e-6
                    );
                }
            }
        },
    );
    with_image(
        "/Width 8 /Height 8 /Filter /JPXDecode /ColorSpace /DeviceRGB",
        jpx_fixtures::RGB16_DWT_MCT,
        |image| {
            let context = decode_context(image, Some((4, 4))).unwrap();
            assert_eq!((context.width, context.height), (4, 4));
            assert_eq!(context.bits_per_component, 16);
            assert_eq!(context.scale_factors, (2.0, 2.0));
        },
    );
}

#[test]
fn lab_native_samples_decode_before_color_conversion_at_all_pdf_depths() {
    for bits in [1, 2, 4, 8, 16] {
        let maximum = (1_u32 << bits) - 1;
        let mut data = Vec::new();
        // Two three-pixel rows; pad each row separately for packed depths.
        let samples = [0, maximum / 2, maximum, maximum, 1, 0];
        for row in samples.chunks_exact(3) {
            let mut row_data = vec![0_u8; (9 * bits as usize).div_ceil(8)];
            for (i, sample) in row.iter().flat_map(|v| [*v, 0, 0]).enumerate() {
                for bit in 0..bits as usize {
                    let offset = i * bits as usize + bit;
                    row_data[offset / 8] |=
                        (((sample >> (bits as usize - bit - 1)) & 1) as u8) << (7 - offset % 8);
                }
            }
            data.extend(row_data);
        }
        for decode in ["0 100 0 0 0 0", "100 0 0 0 0 0", "-50 150 0 0 0 0"] {
            let bounds: Vec<f64> = decode
                .split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect();
            let header = format!(
                "/Width 3 /Height 2 /ColorSpace [/Lab <</WhitePoint[.95047 1 1.08883]>>] /BitsPerComponent {bits} /Decode [{decode}]"
            );
            with_image(&header, &data, |image| {
                let wide = decode_rgb_f64(image, 144, || true).unwrap();
                let ordinary = super::super::decode_image(image, None).unwrap();
                let crate::ImageData::Rgb(ordinary) = ordinary.image else {
                    panic!("Lab remains RGB")
                };
                for ((sample, pixel), bytes) in samples
                    .iter()
                    .zip(wide.data.chunks_exact(24))
                    .zip(ordinary.data.chunks_exact(3))
                {
                    let lightness = (bounds[0]
                        + f64::from(*sample) / f64::from(maximum) * (bounds[1] - bounds[0]))
                        .clamp(0.0, 100.0);
                    let y = if lightness <= 8.0 {
                        lightness * 27.0 / 24389.0
                    } else {
                        ((lightness + 16.0) / 116.0).powi(3)
                    };
                    let expected = if y <= 0.0031308 {
                        12.92 * y
                    } else {
                        1.055 * y.powf(1.0 / 2.4) - 0.055
                    };
                    for (channel, byte) in pixel.chunks_exact(8).zip(bytes) {
                        let value = f64::from_le_bytes(channel.try_into().unwrap());
                        assert!(
                            (value - expected).abs() < 2e-6,
                            "{bits}, {decode}, {sample}: {value} != {expected}"
                        );
                        assert_eq!(*byte, (value * 255.0).round() as u8);
                    }
                }
                assert!(matches!(
                    decode_rgb_f64(image, 143, || true),
                    Err(Error::Limit { requested: 144 })
                ));
                assert!(matches!(
                    decode_rgb_f64(image, 144, || false),
                    Err(Error::Cancelled)
                ));
            });
        }
    }
}

#[test]
fn lab_sixteen_bit_neighbors_survive_transfer_cut_and_invalid_inputs_fail_closed() {
    let header = "/Width 2 /Height 1 /ColorSpace [/Lab <</WhitePoint[.95047 1 1.08883]>>] /BitsPerComponent 16 /Decode [0 100 0 0 0 0]";
    let data: Vec<u8> = [32768_u16, 0, 0, 32769, 0, 0]
        .into_iter()
        .flat_map(u16::to_be_bytes)
        .collect();
    with_image(header, &data, |image| {
        let wide = decode_rgb_f64(image, 48, || true).unwrap();
        let samples: Vec<f64> = wide
            .data
            .chunks_exact(8)
            .map(|v| f64::from_le_bytes(v.try_into().unwrap()))
            .collect();
        assert!(samples[..3].iter().all(|v| *v < 0.466342));
        assert!(samples[3..].iter().all(|v| *v > 0.466342));
    });
    with_image(header, &data[..11], |image| {
        assert!(matches!(
            decode_rgb_f64(image, 48, || true),
            Err(Error::Decode)
        ));
        assert!(super::super::decode_image(image, None).is_none());
    });
    for extra in [
        "/Decode [0 100 0 0 0 0 false]",
        "/Decode false",
        "/Mask [0 0 0 0 0 0]",
        "/SMask null",
    ] {
        with_image(&format!("{header} {extra}"), &data, |image| {
            assert!(
                matches!(decode_rgb_f64(image, 48, || true), Err(Error::Unsupported)),
                "{extra}"
            );
        });
    }
}

#[test]
fn jpx_lab_uses_native_components_and_ignores_dictionary_decode() {
    let header = "/Width 3 /Height 2 /Filter /JPXDecode /ColorSpace [/Lab <</WhitePoint[.95047 1 1.08883]/Range[-10 10 -20 20]>>] /BitsPerComponent 1 /Decode false";
    with_image(header, jpx_fixtures::RGB16, |image| {
        let wide = decode_rgb_f64(image, 144, || true).unwrap();
        let space = image.color_space.as_ref().unwrap();
        for (i, pixel) in wide.data.chunks_exact(24).enumerate() {
            let samples: [f64; 3] = core::array::from_fn(|c| {
                f64::from((i as u32 * 10711 + c as u32 * 19789 + 32767) & 65535) / 65535.0
            });
            let expected = space
                .image_rgb_f64(&[
                    samples[0] * 100.0,
                    -10.0 + samples[1] * 20.0,
                    -20.0 + samples[2] * 40.0,
                ])
                .unwrap();
            for (channel, expected) in pixel.chunks_exact(8).zip(expected) {
                assert!((f64::from_le_bytes(channel.try_into().unwrap()) - expected).abs() < 1e-12);
            }
        }
    });
}
