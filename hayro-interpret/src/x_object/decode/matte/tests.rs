use super::*;
use crate::cache::Cache;
use hayro_syntax::Pdf;
use hayro_syntax::object::ObjectIdentifier;

fn decoded(
    space: &str,
    width: u32,
    height: u32,
    image_entries: &str,
    data: &[u8],
    mask_entries: &str,
    alpha: &[u8],
) -> Option<DecodedImage> {
    let mut pdf = format!("%PDF-1.7\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Resources<<>>>>endobj\n4 0 obj<</Type/XObject/Subtype/Image/Width {width}/Height {height}/ColorSpace {space} {image_entries}/SMask 5 0 R/Length {}>>stream\n", data.len()).into_bytes();
    pdf.extend_from_slice(data);
    pdf.extend_from_slice(format!("\nendstream\nendobj\n5 0 obj<</Type/XObject/Subtype/Image/Width {width}/Height {height}/ColorSpace/DeviceGray {mask_entries}/Length {}>>stream\n",alpha.len()).as_bytes());
    pdf.extend_from_slice(alpha);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    if space.contains("6 0 R") {
        let profile = moxcms::ColorProfile::new_gray_with_gamma(1.0)
            .encode()
            .unwrap();
        pdf.extend_from_slice(
            format!("6 0 obj<</N 1/Length {}>>stream\n", profile.len()).as_bytes(),
        );
        pdf.extend(profile);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }
    if space.contains("7 0 R") {
        let function = b"{ pop pop pop pop pop pop pop }";
        pdf.extend_from_slice(
            format!(
                "7 0 obj<</FunctionType 4/Domain[{}]/Range[0 1]/Length {}>>stream\n",
                "0 1 ".repeat(8),
                function.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(function);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }
    pdf.extend_from_slice(b"trailer<</Root 1 0 R>>\n%%EOF");
    let pdf = Pdf::new(pdf).unwrap();
    let stream = pdf
        .xref()
        .get::<Stream<'_>>(ObjectIdentifier::new(4, 0))
        .unwrap();
    let warning: crate::WarningSinkFn = std::sync::Arc::new(|_| {});
    let obj = ImageXObject::new(
        &stream,
        pdf.pages()[0].resources(),
        &warning,
        &Cache::new(),
        None,
    )?;
    crate::x_object::decode::decode_image(&obj, None)
}

fn rgb(image: &DecodedImage) -> Vec<u8> {
    match &image.image {
        ImageData::Rgb(data) => data.data.clone(),
        ImageData::Luma(data) => data.data.iter().flat_map(|v| [*v; 3]).collect(),
    }
}

#[test]
fn matte_recovery_precedes_nonlinear_color_conversion() {
    for (space, decode, matte, expected) in [
        (
            "[/CalGray<</WhitePoint[.95047 1 1.08883]>>]",
            ".5 .5",
            ".9",
            [89; 3],
        ),
        (
            "[/CalRGB<</WhitePoint[.95047 1 1.08883]/Matrix[.95047 0 0 0 1 0 0 0 1.08883]>>]",
            ".5 .5 .5 .5 .5 .5",
            ".9 .9 .9",
            [89; 3],
        ),
        (
            "[/Lab<</WhitePoint[.95047 1 1.08883]>>]",
            "50 50 0 0 0 0",
            "80 0 0",
            [48; 3],
        ),
    ] {
        let image = decoded(
            space,
            1,
            1,
            &format!("/BitsPerComponent 8/Decode[{decode}]"),
            &vec![0; decode.split_whitespace().count() / 2],
            &format!("/BitsPerComponent 8/Decode[.5 .5]/Matte[{matte}]"),
            &[0],
        )
        .unwrap();
        assert_eq!(rgb(&image), expected, "{space}");
        assert_eq!(image.alpha.unwrap().data, [128]);
    }
}

#[test]
fn matte_native_alpha_is_not_rounded_before_recovering_color() {
    // C'=1/65535 and alpha=2/65535 recover C=1/2. Both would
    // become zero if unpacking or alpha storage narrowed before recovery.
    let image = decoded(
        "/DeviceRGB",
        1,
        1,
        "/BitsPerComponent 16",
        &[0, 1, 0, 1, 0, 1],
        "/BitsPerComponent 16/Matte[0 0 0]",
        &[0, 2],
    )
    .unwrap();
    assert_eq!(rgb(&image), [128; 3]);
    assert_eq!(image.alpha.unwrap().data, [0]);
    let image = decoded(
        "/DeviceRGB",
        1,
        1,
        "/BitsPerComponent 16/Decode[1 0 1 0 1 0]",
        &[255, 254, 255, 254, 255, 254],
        "/BitsPerComponent 16/Decode[1 0]/Matte[0 0 0]",
        &[255, 253],
    )
    .unwrap();
    assert_eq!(rgb(&image), [128; 3]);
}

#[test]
fn matte_zero_alpha_clipping_and_independent_interpolation_are_preserved() {
    let image = decoded(
        "/DeviceRGB",
        3,
        1,
        "/BitsPerComponent 8/Interpolate false",
        &[100, 150, 200, 0, 0, 0, 255, 255, 255],
        "/BitsPerComponent 8/Interpolate true/Matte[.25 .5 .75]",
        &[0, 128, 128],
    )
    .unwrap();
    assert_eq!(rgb(&image), [64, 128, 191, 0, 0, 0, 255, 255, 255]);
    assert!(image.alpha.unwrap().interpolate);
    let image = decoded(
        "[/Lab<</WhitePoint[1 1 1]/Range[0 0 0 0]>>]",
        1,
        1,
        "/BitsPerComponent 8/Decode[50 50 0 0 0 0]",
        &[0; 3],
        "/BitsPerComponent 8/Decode[.5 .5]/Matte[80 0 0]",
        &[0],
    )
    .unwrap();
    assert_eq!(rgb(&image), [48; 3]);
}

#[test]
fn matte_recovery_handles_packed_rows_and_subtractive_components() {
    for bits in [1, 2, 4, 8, 16] {
        let max = (1_u32 << bits) - 1;
        let mut data = Vec::new();
        let mut alpha = Vec::new();
        for _ in 0..2 {
            let mut row = vec![0_u8; (3 * bits as usize).div_ceil(8)];
            for (i, value) in [0, max / 2, max].into_iter().enumerate() {
                for bit in 0..bits as usize {
                    let offset = i * bits as usize + bit;
                    row[offset / 8] |=
                        (((value >> (bits as usize - bit - 1)) & 1) as u8) << (7 - offset % 8);
                }
            }
            data.extend(row);
            alpha.extend([255; 3]);
        }
        let image = decoded(
            "/DeviceGray",
            3,
            2,
            &format!("/BitsPerComponent {bits}"),
            &data,
            "/BitsPerComponent 8/Matte[.5]",
            &alpha,
        )
        .unwrap();
        assert_eq!(
            rgb(&image),
            [
                0,
                (f64::from(max / 2) / f64::from(max) * 255.0).round() as u8,
                255
            ]
            .into_iter()
            .flat_map(|v| [v; 3])
            .collect::<Vec<_>>()
            .repeat(2)
        );
    }
    let image = decoded(
        "/DeviceCMYK",
        1,
        1,
        "/BitsPerComponent 8/Decode[.25 .25 .25 .25 .25 .25 .25 .25]",
        &[0; 4],
        "/BitsPerComponent 8/Decode[.5 .5]/Matte[0 0 0 0]",
        &[0],
    )
    .unwrap();
    use hayro_syntax::object::FromBytes;
    let space =
        crate::color::ColorSpace::from_pdf_object(Object::from_bytes(b"/DeviceCMYK").unwrap())
            .unwrap();
    let mut expected = [0; 3];
    space.convert(&[128; 4], &mut expected).unwrap();
    assert_eq!(rgb(&image), expected);
}

#[test]
fn invalid_matte_arrays_samples_and_mask_dictionaries_fail_closed() {
    for mask in [
        "/BitsPerComponent 8/Matte[0 0 0 false]",
        "/BitsPerComponent 8/Matte[0 0]",
        "/BitsPerComponent 8/Matte[-1 0 0]",
        "/BitsPerComponent 8/Matte[0 0 2]",
        "/BitsPerComponent 8/Matte[0 0 0]/Decode[0 1 false]",
        "/BitsPerComponent 8/Matte[0 0 0]/ImageMask true",
        "/BitsPerComponent 8/Matte[0 0 0]/Mask null",
        "/BitsPerComponent 8/Matte[0 0 0]/SMask null",
        "/BitsPerComponent 8/Matte[0 0 0]/ColorSpace/DeviceRGB",
        "/BitsPerComponent 8/Matte[0 0 0]/Width 2",
        "/BitsPerComponent 3/Matte[0 0 0]",
        "/Matte[0 0 0]",
    ] {
        assert!(
            decoded(
                "/DeviceRGB",
                1,
                1,
                "/BitsPerComponent 8",
                &[128; 3],
                mask,
                &[128]
            )
            .is_none(),
            "{mask}"
        );
    }
    assert!(
        decoded(
            "/DeviceRGB",
            1,
            1,
            "/BitsPerComponent 16",
            &[0; 5],
            "/BitsPerComponent 8/Matte[0 0 0]",
            &[128]
        )
        .is_none()
    );
    assert!(
        decoded(
            "/DeviceRGB",
            1,
            1,
            "/BitsPerComponent 8",
            &[128; 3],
            "/BitsPerComponent 16/Matte[0 0 0]",
            &[128]
        )
        .is_none()
    );
}

#[test]
fn matte_recovery_precedes_icc_and_tint_transforms() {
    for (space, expected) in [
        ("[/ICCBased 6 0 R]", 89),
        (
            "[/Separation /Spot [/CalGray<</WhitePoint[1 1 1]>>] <</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>]",
            90,
        ),
        (
            "[/DeviceN [/Spot] [/CalGray<</WhitePoint[1 1 1]>>] <</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>]",
            90,
        ),
    ] {
        let image = decoded(
            space,
            1,
            1,
            "/BitsPerComponent 8/Decode[.5 .5]",
            &[0],
            "/BitsPerComponent 8/Decode[.5 .5]/Matte[.9]",
            &[0],
        )
        .unwrap();
        assert!(
            rgb(&image).iter().all(|v| v.abs_diff(expected) <= 1),
            "{space}: {:?}",
            rgb(&image)
        );
    }
    let image = decoded(
        "[/Indexed /DeviceRGB 0 <808080>]",
        1,
        1,
        "/BitsPerComponent 8",
        &[0],
        "/BitsPerComponent 8/Matte[0]",
        &[128],
    )
    .unwrap();
    assert_eq!(rgb(&image), [128; 3]);
}

use super::super::jpx_fixtures;

#[test]
fn matte_jpx_source_and_mask_retain_native_depth_and_dictionary_precedence() {
    let image = decoded(
        "/DeviceRGB",
        3,
        2,
        "/Filter/JPXDecode/BitsPerComponent 1/Decode false",
        jpx_fixtures::RGB16,
        "/BitsPerComponent 8/Decode[.5 .5]/Matte[0 0 0]",
        &[0; 6],
    )
    .unwrap();
    let expected: Vec<u8> = (0..6)
        .flat_map(|i| {
            (0..3).map(move |c| {
                let sample = (i * 10711 + c * 19789 + 32767) & 65535;
                ((2.0 * f64::from(sample) / 65535.0).min(1.0) * 255.0).round() as u8
            })
        })
        .collect();
    assert_eq!(rgb(&image), expected);
    let samples = [0_u16, 32767, 65535, 1, 65534, 32768];
    let source: Vec<u8> = samples
        .iter()
        .flat_map(|v| [v / 2; 3])
        .flat_map(u16::to_be_bytes)
        .collect();
    let image = decoded(
        "/DeviceRGB",
        3,
        2,
        "/BitsPerComponent 16",
        &source,
        "/Filter/JPXDecode/BitsPerComponent 1/Decode false/Matte[0 0 0]",
        jpx_fixtures::DEPTH_16,
    )
    .unwrap();
    let expected: Vec<u8> = samples
        .iter()
        .flat_map(|v| {
            [if *v == 0 {
                0
            } else {
                (f64::from(v / 2) / f64::from(*v) * 255.0).round() as u8
            }; 3]
        })
        .collect();
    assert_eq!(rgb(&image), expected);
    assert_eq!(
        image.alpha.unwrap().data,
        samples.map(|v| (f64::from(v) / 65535.0 * 255.0).round() as u8)
    );
}

#[test]
fn indexed_matte_recovers_palette_components_before_base_conversion() {
    for (base, palette, expected) in [
        ("/DeviceRGB", "8040c0e61e64", [26, 98, 255]),
        (
            "[/CalGray<</WhitePoint[.95047 1 1.08883]>>]",
            "80e6",
            [90; 3],
        ),
        (
            "[/CalRGB<</WhitePoint[.95047 1 1.08883]/Matrix[.95047 0 0 0 1 0 0 0 1.08883]>>]",
            "808080e6e6e6",
            [90; 3],
        ),
        (
            "[/Lab<</WhitePoint[.95047 1 1.08883]/Range[0 0 0 0]>>]",
            "800000cc0000",
            [49; 3],
        ),
        ("/DeviceCMYK", "8080808000000000", [0; 3]),
        ("[/ICCBased 6 0 R]", "80e6", [90; 3]),
        (
            "[/Separation /Spot [/CalGray<</WhitePoint[1 1 1]>>] <</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>]",
            "80e6",
            [90; 3],
        ),
        (
            "[/DeviceN [/Spot] [/CalGray<</WhitePoint[1 1 1]>>] <</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>]",
            "80e6",
            [90; 3],
        ),
    ] {
        for matte in ["1", ".75", ".5"] {
            let image = decoded(
                &format!("[/Indexed {base} 1 <{palette}>]"),
                1,
                1,
                "/BitsPerComponent 8",
                &[0],
                &format!("/BitsPerComponent 8/Decode[.5 .5]/Matte[{matte}]"),
                &[0],
            )
            .unwrap();
            assert_eq!(rgb(&image), expected, "{base}: {matte}");
            assert_eq!(image.alpha.unwrap().data, [128]);
        }
    }
}

#[test]
fn indexed_matte_preserves_native_indices_opacity_and_packed_rows() {
    let image = decoded(
        "[/Indexed /DeviceGray 1 <0100>]",
        2,
        1,
        "/BitsPerComponent 16/Decode[0 1]",
        &[0x7f, 0xff, 0x80, 0],
        "/BitsPerComponent 16/Matte[1]",
        &[2, 2, 2, 2],
    )
    .unwrap();
    // Adjacent 16-bit indices straddle 1/2. Alpha=514/65535=2/255;
    // palette component 1/255 recovers exactly 1/2 before byte storage.
    assert_eq!(rgb(&image), [128, 128, 128, 0, 0, 0]);
    assert_eq!(image.alpha.unwrap().data, [2, 2]);
    for bits in [1, 2, 4, 8, 16] {
        let maximum = (1_u32 << bits) - 1;
        let mut data = Vec::new();
        for _ in 0..2 {
            let mut row = vec![0; (3 * bits as usize).div_ceil(8)];
            for (i, sample) in [0, maximum, 0].into_iter().enumerate() {
                for bit in 0..bits as usize {
                    let offset = i * bits as usize + bit;
                    row[offset / 8] |=
                        (((sample >> (bits as usize - bit - 1)) & 1) as u8) << (7 - offset % 8);
                }
            }
            data.extend(row);
        }
        let image = decoded(
            "[/Indexed /DeviceGray 1 <10e0>]",
            3,
            2,
            &format!("/BitsPerComponent {bits}/Decode[2 -1]"),
            &data,
            "/BitsPerComponent 8/Matte[1]",
            &[255; 6],
        )
        .unwrap();
        assert_eq!(
            rgb(&image),
            [224, 224, 224, 16, 16, 16, 224, 224, 224].repeat(2),
            "{bits}"
        );
    }
}

#[test]
fn invalid_indexed_palettes_and_matte_components_fail_closed() {
    for space in [
        "[/Indexed /DeviceRGB 256 <808080>]",
        "[/Indexed /DeviceRGB -1 <808080>]",
        "[/Indexed /DeviceRGB 0.0 <808080>]",
        "[/Indexed /DeviceRGB 0 <808080> false]",
        "[/Indexed /DeviceRGB 0 <8080>]",
        "[/Indexed /DeviceRGB 0 <80808080>]",
        "[/Indexed [/Indexed /DeviceGray 0 <80>] 0 <00>]",
        "[/Indexed [/Pattern /DeviceRGB] 0 <808080>]",
    ] {
        assert!(
            decoded(
                space,
                1,
                1,
                "/BitsPerComponent 8",
                &[0],
                "/BitsPerComponent 8/Matte[0]",
                &[128]
            )
            .is_none(),
            "{space}"
        );
    }
    for matte in ["-1", "2", "false", "0 false", "0 0 0"] {
        assert!(
            decoded(
                "[/Indexed /DeviceRGB 1 <8040c0e61e64>]",
                1,
                1,
                "/BitsPerComponent 8",
                &[0],
                &format!("/BitsPerComponent 8/Matte[{matte}]"),
                &[128]
            )
            .is_none(),
            "{matte}"
        );
    }
}

#[test]
fn indexed_matte_jpx_and_varying_opacity_preserve_palette_ownership() {
    let image = decoded(
        "[/Indexed /DeviceGray 1 <80e6>]",
        3,
        2,
        "/Filter/JPXDecode/BitsPerComponent 1/Decode[1 0]",
        jpx_fixtures::DEPTH_16,
        "/BitsPerComponent 8/Decode[.5 .5]/Matte[1]",
        &[0; 6],
    )
    .unwrap();
    assert_eq!(
        rgb(&image),
        [26, 230, 230, 230, 230, 230]
            .into_iter()
            .flat_map(|v| [v; 3])
            .collect::<Vec<_>>()
    );
    let image = decoded(
        "[/Indexed /DeviceGray 1 <80e6>]",
        4,
        1,
        "/BitsPerComponent 8",
        &[0; 4],
        "/BitsPerComponent 8/Matte[1]",
        &[0, 64, 128, 255],
    )
    .unwrap();
    assert_eq!(
        rgb(&image),
        [230, 0, 27, 128]
            .into_iter()
            .flat_map(|v| [v; 3])
            .collect::<Vec<_>>()
    );
    assert_eq!(image.alpha.unwrap().data, [0, 64, 128, 255]);
}

#[test]
fn indexed_matte_many_component_conversion_crosses_bounded_batches() {
    let count = CONVERSION_BATCH_PIXELS + 3;
    let data: Vec<u8> = (0..count).map(|i| (i % 2) as u8).collect();
    let alpha = [2_u8, 2].repeat(count);
    let image = decoded(
        "[/Indexed [/DeviceN [/A /B /C /D /E /F /G /H] [/CalGray<</WhitePoint[1 1 1]>>] 7 0 R] 1 <01010101010101010000000000000000>]",
        count as u32, 1, "/BitsPerComponent 8", &data,
        "/BitsPerComponent 16/Matte[1]", &alpha,
    ).unwrap();
    let expected: Vec<u8> = data
        .iter()
        .flat_map(|index| [if *index == 0 { 188 } else { 0 }; 3])
        .collect();
    assert_eq!(rgb(&image), expected);
    assert_eq!(image.alpha.unwrap().data, vec![2; count]);
}
