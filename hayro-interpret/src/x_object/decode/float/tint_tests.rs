use super::tests::{with_image, with_image_objects};
use super::*;

const IDENTITY: &str = "<</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>";

fn values(image: &RgbF64Data) -> Vec<f64> {
    image
        .data
        .chunks_exact(8)
        .map(|bytes| f64::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

fn function_object(entries: &str, data: &[u8]) -> Vec<u8> {
    let mut object = format!("5 0 obj <<{entries}/Length {}>>\nstream\n", data.len()).into_bytes();
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream\nendobj\n");
    object
}

#[test]
fn direct_tint_alternates_preserve_the_complete_sixteen_bit_domain() {
    let samples: Vec<_> = (0..=u16::MAX).flat_map(u16::to_be_bytes).collect();
    for prefix in ["Separation /Ink", "DeviceN [/Ink]"] {
        for rgb in [false, true] {
            let (alternate, function) = if rgb {
                (
                    "/DeviceRGB",
                    "<</FunctionType 2/Domain[0 1]/C0[0 .25 1]/C1[1 .75 0]/N 1>>",
                )
            } else {
                ("/DeviceGray", IDENTITY)
            };
            let header = format!(
                "/Width 256/Height 256/BitsPerComponent 16/ColorSpace[/{prefix} {alternate} {function}]"
            );
            with_image(&header, &samples, |image| {
                let decoded = decode_rgb_f64(image, 65536 * 24, || true).unwrap();
                let actual = values(&decoded);
                for (sample, pixel) in actual.chunks_exact(3).enumerate() {
                    // This oracle is the existing binary32 function boundary,
                    // followed by exact widening, not the byte tint lookup.
                    let tint = (sample as f64 / 65535.0) as f32;
                    let expected = if rgb {
                        [tint, 0.25 + 0.5 * tint, 1.0 - tint]
                    } else {
                        [tint; 3]
                    };
                    assert_eq!(pixel, expected.map(f64::from), "{prefix} {sample}");
                }
                assert_ne!(actual[3], actual[0]);
            });
        }
    }
}

#[test]
fn native_decode_and_palette_selection_precede_the_tint_boundary() {
    for prefix in ["Separation /Ink", "DeviceN [/Ink]"] {
        let space = format!("[/{prefix} /DeviceGray {IDENTITY}]");
        for (decode, samples, expected) in [
            (
                "[1 0]",
                vec![0_u16, 32768, 65535],
                vec![1.0, (32767.0_f64 / 65535.0) as f32, 0.0],
            ),
            (
                "[-1 2]",
                vec![0_u16, 32768, 65535],
                vec![0.0, (32769.0_f64 / 65535.0) as f32, 1.0],
            ),
        ] {
            let bytes: Vec<_> = samples.into_iter().flat_map(u16::to_be_bytes).collect();
            let header =
                format!("/Width 3/Height 1/BitsPerComponent 16/ColorSpace {space}/Decode {decode}");
            with_image(&header, &bytes, |image| {
                assert_eq!(
                    values(&decode_rgb_f64(image, 72, || true).unwrap()),
                    expected
                        .into_iter()
                        .flat_map(|v| [f64::from(v); 3])
                        .collect::<Vec<_>>()
                );
            });
        }
        let header = format!(
            "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/Indexed {space} 1 <40c0>]/Decode[0 1]"
        );
        with_image(&header, &[0x7f, 0xff, 0x80, 0], |image| {
            assert_eq!(
                values(&decode_rgb_f64(image, 48, || true).unwrap()),
                [64.0_f32 / 255.0, 192.0_f32 / 255.0]
                    .into_iter()
                    .flat_map(|v| [f64::from(v); 3])
                    .collect::<Vec<_>>()
            );
        });
    }
}

#[test]
fn multiple_tints_read_all_native_components_before_rgb_conversion() {
    for components in [2, 4, 32, 64] {
        let names = (0..components)
            .map(|i| {
                if i == 0 {
                    "/None".to_owned()
                } else {
                    format!("/Ink{i}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let domain = "0 1 ".repeat(components);
        // Consume intermediate inputs without first pushing operands: the
        // 64-input control already fills the bounded calculator stack.
        let program = format!("{{ {} .25 }}", "exch pop ".repeat(components - 2));
        let object = function_object(
            &format!("/FunctionType 4/Domain[{domain}]/Range[0 1 0 1 0 1]"),
            program.as_bytes(),
        );
        let header = format!(
            "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/DeviceN[{names}]/DeviceRGB 5 0 R]"
        );
        let samples: Vec<_> = (0..2)
            .flat_map(|pixel| {
                (0..components).map(move |channel| {
                    if channel == 0 {
                        32768 + pixel
                    } else if channel == components - 1 {
                        65534 - pixel
                    } else {
                        12345
                    }
                })
            })
            .flat_map(u16::to_be_bytes)
            .collect();
        with_image_objects(&header, &samples, &object, |image| {
            let actual = values(
                &decode_rgb_f64(image, 48, || true)
                    .unwrap_or_else(|error| panic!("{components} components: {error:?}")),
            );
            for (i, pixel) in actual.chunks_exact(3).enumerate() {
                assert_eq!(
                    pixel,
                    [
                        f64::from(((32768 + i) as f64 / 65535.0) as f32),
                        f64::from(((65534 - i) as f64 / 65535.0) as f32),
                        0.25
                    ],
                    "{components} components"
                );
            }
        });
    }
}

#[test]
fn sampled_and_stitched_tints_keep_sub_byte_source_boundaries() {
    for prefix in ["Separation /Ink", "DeviceN [/Ink]"] {
        let header =
            format!("/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/{prefix}/DeviceGray 5 0 R]");
        let object = function_object(
            "/FunctionType 0/Domain[0 1]/Range[0 1]/Size[2]/BitsPerSample 8",
            &[0, 255],
        );
        with_image_objects(&header, &[0x80, 0, 0x80, 1], &object, |image| {
            let actual = values(&decode_rgb_f64(image, 48, || true).unwrap());
            for (i, pixel) in actual.chunks_exact(3).enumerate() {
                let expected = f64::from(((32768 + i) as f64 / 65535.0) as f32);
                assert!(pixel.iter().all(|v| (v - expected).abs() < 1e-7));
            }
            assert_ne!(actual[0], actual[3]);
        });
        let function = "<</FunctionType 3/Domain[0 1]/Range[0 1]/Functions[<</FunctionType 2/Domain[0 1]/C0[0]/C1[0]/N 1>><</FunctionType 2/Domain[0 1]/C0[1]/C1[1]/N 1>>]/Bounds[.5]/Encode[0 1 0 1]>>";
        let header = format!(
            "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/{prefix}/DeviceGray {function}]"
        );
        with_image(&header, &[0x7f, 0xff, 0x80, 0], |image| {
            assert_eq!(
                values(&decode_rgb_f64(image, 48, || true).unwrap()),
                [0.0, 0.0, 0.0, 1.0, 1.0, 1.0]
            );
        });
    }
}

#[test]
fn unsupported_tint_alternates_and_invalid_image_boundaries_still_refuse() {
    for (alternate, end) in [
        ("/DeviceCMYK", "0 0 0 1"),
        ("[/CalGray<</WhitePoint[.95047 1 1.08883]>>]", "1"),
        ("[/CalRGB<</WhitePoint[.95047 1 1.08883]>>]", "1 1 1"),
        ("[/Lab<</WhitePoint[.95047 1 1.08883]>>]", "100 0 0"),
    ] {
        let header = format!(
            "/Width 1/Height 1/BitsPerComponent 8/ColorSpace[/Separation/Ink {alternate}<</FunctionType 2/Domain[0 1]/C0[{end}]/C1[{end}]/N 1>>]"
        );
        with_image(&header, &[128], |image| {
            assert!(matches!(
                decode_rgb_f64(image, 24, || true),
                Err(Error::Unsupported)
            ));
        });
    }
    let header = format!(
        "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/Separation/Ink/DeviceGray {IDENTITY}]"
    );
    for extra in [
        "/Mask[0 1]",
        "/SMask null",
        "/Decode[0 false]",
        "/Decode[0 1 0 1]",
    ] {
        with_image(&format!("{header} {extra}"), &[0; 4], |image| {
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
    let header = format!(
        "/Width 1025/Height 1/BitsPerComponent 8/ColorSpace[/Separation/Ink/DeviceGray {IDENTITY}]"
    );
    with_image(&header, &vec![128; 1025], |image| {
        let mut calls = 0;
        let result = decode_rgb_f64(image, 1025 * 24, || {
            calls += 1;
            calls < 4
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(calls, 4);
    });
}

#[test]
fn explicit_jpx_tints_keep_codestream_precision_and_ignore_dictionary_decode() {
    use super::super::jpx_fixtures;
    for prefix in ["Separation /Ink", "DeviceN [/Ink]"] {
        let header = format!(
            "/Width 3/Height 2/ColorSpace[/{prefix}/DeviceGray {IDENTITY}]/BitsPerComponent 1/Decode false/Filter/JPXDecode"
        );
        with_image(&header, jpx_fixtures::DEPTH_16, |image| {
            let output = decode_rgb_f64(image, 144, || true).unwrap();
            let actual = values(&output);
            assert_eq!(actual.len(), 18);
            let expected = [0_u16, 32767, 65535, 1, 65534, 32768]
                .into_iter()
                .flat_map(|v| [f64::from((f64::from(v) / 65535.0) as f32); 3])
                .collect::<Vec<_>>();
            assert_eq!(actual, expected);
            assert!(actual.chunks_exact(3).all(|p| p[0] == p[1] && p[1] == p[2]));
            assert!(actual.iter().any(|v| (v * 255.0).fract() != 0.0));
        });
    }
}
