use super::tests::{with_image, with_image_objects};
use super::*;
use crate::color::ColorSpace;
use hayro_syntax::object::{FromBytes, Object};

const SPACES: [(&str, &str, &str); 3] = [
    (
        "calgray",
        "[/CalGray<</WhitePoint[.95047 1 1.08883]/Gamma 2>>]",
        "<</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>",
    ),
    (
        "calrgb",
        "[/CalRGB<</WhitePoint[.95047 1 1.08883]/Gamma[1 2 3]/Matrix[.4124564 .2126729 .0193339 .3575761 .7151522 .1191920 .1804375 .0721750 .9503041]>>]",
        "<</FunctionType 2/Domain[0 1]/C0[0 .25 1]/C1[1 .75 0]/N 1>>",
    ),
    (
        "lab",
        "[/Lab<</WhitePoint[.95047 1 1.08883]/Range[-10 10 -20 20]>>]",
        "<</FunctionType 2/Domain[0 1]/C0[0 20 -30]/C1[100 -20 30]/N 1>>",
    ),
];

fn space(definition: &str) -> ColorSpace {
    ColorSpace::from_pdf_object(Object::from_bytes(definition.as_bytes()).unwrap()).unwrap()
}

fn floats(image: &RgbF64Data) -> Vec<f64> {
    image
        .data
        .chunks_exact(8)
        .map(|v| f64::from_le_bytes(v.try_into().unwrap()))
        .collect()
}

fn expected(kind: &str, base: &ColorSpace, tint: f64) -> [f64; 3] {
    let t = f64::from(tint.clamp(0.0, 1.0) as f32);
    let components = match kind {
        "calgray" => vec![t],
        "calrgb" => vec![
            t,
            f64::from((0.25 + 0.5 * t) as f32),
            f64::from((1.0 - t) as f32),
        ],
        "lab" => vec![
            f64::from((100.0 * t) as f32),
            f64::from((20.0 - 40.0 * t) as f32),
            f64::from((-30.0 + 60.0 * t) as f32),
        ],
        _ => unreachable!(),
    };
    base.image_rgb_f64(&components).unwrap()
}

#[test]
fn cie_tints_keep_full_domain_conversion_and_existing_byte_image_policy() {
    let samples: Vec<_> = (0..=u16::MAX).flat_map(u16::to_be_bytes).collect();
    for (kind, alternate, function) in SPACES {
        let base = space(alternate);
        for family in ["Separation/Ink", "DeviceN[/Ink]"] {
            let header = format!(
                "/Width 256/Height 256/BitsPerComponent 16/ColorSpace[/{family}{alternate}{function}]"
            );
            with_image(&header, &samples, |image| {
                let actual = floats(&decode_rgb_f64(image, 65536 * 24, || true).unwrap());
                let color = image.color_space.as_ref().unwrap();
                let mut retained_extra_precision = false;
                for (sample, pixel) in actual.chunks_exact(3).enumerate() {
                    let tint = sample as f64 / 65535.0;
                    let reference = expected(kind, &base, tint);
                    assert!(
                        pixel
                            .iter()
                            .zip(reference)
                            .all(|(a, b)| (*a - b).abs() < 1e-12),
                        "{kind} {family} {sample}"
                    );
                    let ordinary = color.image_rgb_f64(&[tint]).unwrap();
                    assert_eq!(ordinary, reference.map(|v| f64::from(v as f32)));
                    retained_extra_precision |= pixel != ordinary;
                }
                assert!(retained_extra_precision);
            });
        }
    }
}

#[test]
fn cie_tint_decode_and_palette_selection_precede_conversion() {
    for (kind, alternate, function) in SPACES {
        let base = space(alternate);
        for family in ["Separation/Ink", "DeviceN[/Ink]"] {
            let definition = format!("[/{family}{alternate}{function}]");
            for (decode, tints) in [
                ("[1 0]", [1.0, 32767.0 / 65535.0, 0.0]),
                ("[-1 2]", [0.0, 32769.0 / 65535.0, 1.0]),
            ] {
                let header = format!(
                    "/Width 3/Height 1/BitsPerComponent 16/ColorSpace{definition}/Decode{decode}"
                );
                with_image(&header, &[0, 0, 0x80, 0, 0xff, 0xff], |image| {
                    let actual = floats(&decode_rgb_f64(image, 72, || true).unwrap());
                    let reference: Vec<_> = tints
                        .into_iter()
                        .flat_map(|t| expected(kind, &base, t))
                        .collect();
                    assert!(
                        actual
                            .iter()
                            .zip(reference)
                            .all(|(a, b)| (*a - b).abs() < 1e-12)
                    );
                });
            }
            let indexed = space(&format!("[/Indexed{definition}1<40c0>]"));
            let header = format!(
                "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/Indexed{definition}1<40c0>]/Decode[0 1]"
            );
            with_image(&header, &[0x7f, 0xff, 0x80, 0], |image| {
                let actual = floats(&decode_rgb_f64(image, 48, || true).unwrap());
                let reference: Vec<_> = [64_u8, 192]
                    .into_iter()
                    .flat_map(|v| expected(kind, &base, f64::from(f32::from(v) / 255.0)))
                    .collect();
                assert!(
                    actual
                        .iter()
                        .zip(reference)
                        .all(|(a, b)| (*a - b).abs() < 1e-12)
                );
            });
            for (index, entry) in [(0.5_f64.next_down(), 64_u8), (0.5, 192_u8)] {
                let actual = indexed.image_cie_tint_rgb(&[index]).unwrap();
                let reference = expected(kind, &base, f64::from(f32::from(entry) / 255.0));
                assert!(
                    actual
                        .into_iter()
                        .zip(reference)
                        .all(|(a, b)| (a - b).abs() < 1e-12)
                );
            }
            assert!(indexed.image_cie_tint_rgb(&[f64::NAN]).is_none());
            assert!(indexed.image_cie_tint_rgb(&[]).is_none());
            assert!(indexed.image_cie_tint_rgb(&[0.0, 1.0]).is_none());
        }
    }
}

#[test]
fn packed_cie_tints_keep_row_padding_and_final_float_storage_width() {
    for (kind, alternate, function) in SPACES {
        let base = space(alternate);
        for family in ["Separation/Ink", "DeviceN[/Ink]"] {
            for (bits, data, samples) in [
                (1, vec![0x40, 0xa0], vec![0, 1, 0, 1, 0, 1]),
                (2, vec![0x18, 0xe4], vec![0, 1, 2, 3, 2, 1]),
                (4, vec![0x08, 0xf0, 0xf8, 0x00], vec![0, 8, 15, 15, 8, 0]),
                (
                    8,
                    vec![0, 128, 255, 255, 128, 0],
                    vec![0, 128, 255, 255, 128, 0],
                ),
            ] {
                let header = format!(
                    "/Width 3/Height 2/BitsPerComponent {bits}/ColorSpace[/{family}{alternate}{function}]"
                );
                with_image(&header, &data, |image| {
                    let actual = floats(&decode_rgb_f64(image, 144, || true).unwrap());
                    let reference: Vec<_> = samples
                        .into_iter()
                        .flat_map(|v| {
                            expected(kind, &base, f64::from(v) / f64::from((1_u32 << bits) - 1))
                        })
                        .collect();
                    assert!(
                        actual
                            .iter()
                            .zip(&reference)
                            .all(|(a, b)| (*a - *b).abs() < 1e-12)
                    );
                    let narrow = decode_rgb_f32(image, 72, || true).unwrap();
                    let narrow: Vec<_> = narrow
                        .data
                        .chunks_exact(4)
                        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
                        .collect();
                    assert_eq!(
                        narrow,
                        reference.into_iter().map(|v| v as f32).collect::<Vec<_>>()
                    );
                });
            }
        }
    }
}

#[test]
fn multiple_native_tints_keep_calculator_outputs_before_cie_conversion() {
    let alternate = SPACES[1].1;
    let program = b"{ .25 }";
    let mut function = format!(
        "5 0 obj<</FunctionType 4/Domain[0 1 0 1]/Range[0 1 0 1 0 1]/Length {}>>stream\n",
        program.len()
    )
    .into_bytes();
    function.extend_from_slice(program);
    function.extend_from_slice(b"\nendstream\nendobj\n");
    let header = format!(
        "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/DeviceN[/None/Ink]{alternate}5 0 R]"
    );
    with_image_objects(
        &header,
        &[0x80, 0, 0xff, 0xfe, 0x80, 1, 0xff, 0xfd],
        &function,
        |image| {
            let actual = floats(&decode_rgb_f64(image, 48, || true).unwrap());
            let base = space(alternate);
            for (i, pixel) in actual.chunks_exact(3).enumerate() {
                let reference = base
                    .image_rgb_f64(&[
                        f64::from(((32768 + i) as f64 / 65535.0) as f32),
                        f64::from(((65534 - i) as f64 / 65535.0) as f32),
                        0.25,
                    ])
                    .unwrap();
                assert!(
                    pixel
                        .iter()
                        .zip(reference)
                        .all(|(a, b)| (*a - b).abs() < 1e-12)
                );
            }
        },
    );
}

#[test]
fn cie_tint_masks_decode_limits_and_cancellation_keep_their_boundaries() {
    for (_, alternate, function) in SPACES {
        let header = format!(
            "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/Separation/Ink{alternate}{function}]"
        );
        for extra in [
            "/Mask[0 1]",
            "/SMask null",
            "/Decode[0 false]",
            "/Decode[0 1 0 1]",
        ] {
            with_image(&format!("{header}{extra}"), &[0; 4], |image| {
                assert!(matches!(
                    decode_rgb_f64(image, 48, || true),
                    Err(Error::Unsupported)
                ));
            });
        }
        with_image(&header, &[0; 3], |image| {
            assert!(matches!(
                decode_rgb_f64(image, 48, || true),
                Err(Error::Decode)
            ));
        });
        with_image(&header, &[0; 4], |image| {
            assert!(matches!(
                decode_rgb_f64(image, 47, || true),
                Err(Error::Limit { requested: 48 })
            ));
            assert!(matches!(
                decode_rgb_f64(image, 48, || false),
                Err(Error::Cancelled)
            ));
        });
        let header = header.replace("/Width 2", "/Width 1025");
        with_image(&header, &[0; 2050], |image| {
            let mut calls = 0;
            assert!(matches!(
                decode_rgb_f64(image, 1025 * 24, || {
                    calls += 1;
                    calls < 4
                }),
                Err(Error::Cancelled)
            ));
            assert_eq!(calls, 4);
        });
    }
}

#[test]
fn explicit_jpx_cie_tints_retain_codestream_samples() {
    use super::super::jpx_fixtures;
    for (kind, alternate, function) in SPACES {
        let base = space(alternate);
        for family in ["Separation/Ink", "DeviceN[/Ink]"] {
            let header = format!(
                "/Width 3/Height 2/ColorSpace[/{family}{alternate}{function}]/BitsPerComponent 1/Decode false/Filter/JPXDecode"
            );
            with_image(&header, jpx_fixtures::DEPTH_16, |image| {
                let actual = floats(&decode_rgb_f64(image, 144, || true).unwrap());
                let reference: Vec<_> = [0_u16, 32767, 65535, 1, 65534, 32768]
                    .into_iter()
                    .flat_map(|sample| expected(kind, &base, f64::from(sample) / 65535.0))
                    .collect();
                assert!(
                    actual
                        .iter()
                        .zip(reference)
                        .all(|(a, b)| (*a - b).abs() < 1e-12)
                );
            });
        }
    }
}

#[test]
fn original_independent_cie_tint_images_match_binary64_oracles() {
    use crate::cache::Cache;
    use hayro_syntax::Pdf;
    use hayro_syntax::object::{ObjectIdentifier, Stream};
    let Some(root) = std::env::var_os("GLYPHST_CIE_TINT_ORACLE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    for (kind, _, _) in SPACES {
        let reference = std::fs::read(root.join(format!("{kind}-untransferred.f64"))).unwrap();
        assert_eq!(reference.len(), 65536 * 24);
        for family in ["separation", "devicen"] {
            let pdf =
                Pdf::new(std::fs::read(root.join(format!("{family}-{kind}-power16.pdf"))).unwrap())
                    .unwrap();
            let stream = pdf
                .xref()
                .get::<Stream<'_>>(ObjectIdentifier::new(5, 0))
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
            let actual = decode_rgb_f64(&image, 65536 * 24, || true).unwrap();
            assert_eq!(actual.data.len(), reference.len());
            let mut maximum = 0.0_f64;
            for (a, b) in actual.data.chunks_exact(8).zip(reference.chunks_exact(8)) {
                let a = f64::from_le_bytes(a.try_into().unwrap());
                let b = f64::from_le_bytes(b.try_into().unwrap());
                maximum = maximum.max((a - b).abs());
            }
            assert!(maximum < 1e-12, "{family} {kind}: {maximum}");
            eprintln!("original {family}-{kind}: maximum float error {maximum}");
        }
    }
}
