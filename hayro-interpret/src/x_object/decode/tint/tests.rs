use super::*;
use crate::cache::Cache;
use crate::x_object::decode::decode_image;
use hayro_syntax::Pdf;
use hayro_syntax::object::{ObjectIdentifier, Stream};

fn decode(header: &str, data: &[u8], extra: &[u8]) -> Option<super::super::DecodedImage> {
    with_image(header, data, extra, |image| decode_image(image, None))
}

fn with_image<T>(
    header: &str,
    data: &[u8],
    extra: &[u8],
    callback: impl FnOnce(&ImageXObject<'_>) -> Option<T>,
) -> Option<T> {
    let mut bytes = format!(
        "%PDF-1.7\n1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
         2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
         3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Resources<<>>>> endobj\n\
         4 0 obj <</Type/XObject/Subtype/Image {header}/Length {}>> stream\n",
        data.len()
    )
    .into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(extra);
    bytes.extend_from_slice(b"\ntrailer <</Root 1 0 R>>\n%%EOF");
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
    // Existing raw-transfer wide arithmetic admission is not inferred from the
    // ordinary tint conversion's binary32 function evaluation.
    assert!(matches!(
        super::super::decode_rgb_f64(&image, u64::MAX, || true),
        Err(crate::FloatImageError::Unsupported)
    ));
    callback(&image)
}

const RAMP: &str = "<</FunctionType 2/Domain[0 1]/Range[0 1]/C0[-500]/C1[500]/N 1>>";

fn colors(image: &super::super::DecodedImage) -> Vec<u8> {
    if let ImageData::Luma(image) = &image.image {
        return image.data.clone();
    }
    let ImageData::Rgb(image) = &image.image else {
        panic!("RGB output")
    };
    image
        .data
        .chunks_exact(3)
        .map(|pixel| {
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[1], pixel[2]);
            pixel[0]
        })
        .collect()
}

#[test]
fn packed_and_byte_tints_apply_decode_before_the_function() {
    for prefix in ["Separation /Spot", "DeviceN [/Spot]"] {
        for (bits, data, expected) in [
            (1, vec![0x40, 0xa0], vec![0, 255, 0, 255, 0, 255]),
            (2, vec![0x18, 0xe4], vec![0, 85, 170, 255, 170, 85]),
            (
                4,
                vec![0x08, 0xf0, 0xf8, 0x00],
                vec![0, 136, 255, 255, 136, 0],
            ),
            (
                8,
                vec![0, 128, 255, 255, 128, 0],
                vec![0, 128, 255, 255, 128, 0],
            ),
        ] {
            let header = format!(
                "/Width 3/Height 2/BitsPerComponent {bits}/ColorSpace[/{prefix} /DeviceGray {RAMP}]/Decode[.5 .501]"
            );
            let image = decode(&header, &data, b"").unwrap();
            assert!(
                colors(&image)
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| a.abs_diff(*b) <= 1),
                "{prefix} {bits}: {:?}",
                colors(&image)
            );
            let reversed = decode(&header.replace("[.5 .501]", "[.501 .5]"), &data, b"").unwrap();
            assert!(
                colors(&reversed)
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| a.abs_diff(255 - *b) <= 1)
            );
        }
    }
}

#[test]
fn sixteen_bit_neighbors_do_not_collapse_to_one_tint() {
    for prefix in ["Separation /Spot", "DeviceN [/Spot]"] {
        let header = format!(
            "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/{prefix} /DeviceGray <</FunctionType 2/Domain[0 1]/Range[0 1]/C0[-32767]/C1[32768]/N 1>>]"
        );
        let image = decode(&header, &[0x7f, 0xff, 0x80, 0], b"").unwrap();
        assert_eq!(colors(&image), [0, 255]);
        assert!(decode(&header, &[0x7f, 0xff, 0x80], b"").is_none());
    }
}

#[test]
fn tint_decode_preserves_color_key_and_soft_mask_alpha() {
    let base = format!(
        "/Width 3/Height 1/BitsPerComponent 2/ColorSpace[/Separation /Spot /DeviceGray {RAMP}]/Decode[.5 .501]"
    );
    let image = decode(&format!("{base}/Mask[1 2]"), &[0x18], b"").unwrap();
    assert_eq!(colors(&image), [0, 85, 170]);
    assert_eq!(image.alpha.unwrap().data, [255, 0, 0]);
    let extra = b"5 0 obj <</Type/XObject/Subtype/Image/Width 3/Height 1/BitsPerComponent 8/ColorSpace/DeviceGray/Length 3>> stream\n\x00\x80\xff\nendstream\nendobj";
    let image = decode(&format!("{base}/SMask 5 0 R"), &[0x18], extra).unwrap();
    assert_eq!(colors(&image), [0, 85, 170]);
    assert_eq!(image.alpha.unwrap().data, [0, 128, 255]);
}

#[test]
fn matte_recovery_precedes_tint_evaluation() {
    let transform = "<</FunctionType 3/Domain[0 1]/Range[0 1]/Functions[<</FunctionType 2/Domain[0 1]/C0[0]/C1[0]/N 1>><</FunctionType 2/Domain[0 1]/C0[1]/C1[1]/N 1>>]/Bounds[.50003]/Encode[0 1 0 1]>>";
    let header = format!(
        "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/Separation /Spot /DeviceGray {transform}]/SMask 5 0 R"
    );
    let extra = b"5 0 obj <</Type/XObject/Subtype/Image/Width 2/Height 1/BitsPerComponent 8/ColorSpace/DeviceGray/Matte[.5]/Length 2>> stream\n\x80\xff\nendstream\nendobj";
    let image = decode(&header, &[0x80, 1, 0x80, 1], extra).unwrap();
    assert_eq!(colors(&image), [255, 0]);
    assert_eq!(image.alpha.unwrap().data, [128, 255]);
}

#[test]
fn jpx_tints_keep_native_depth_and_ignore_pdf_decode_overrides() {
    use crate::x_object::decode::jpx_fixtures;
    for (bits, data) in [
        (1, jpx_fixtures::DEPTH_1),
        (3, jpx_fixtures::DEPTH_3),
        (8, jpx_fixtures::DEPTH_8),
        (12, jpx_fixtures::DEPTH_12),
        (16, jpx_fixtures::DEPTH_16),
    ] {
        let header = "/Width 3/Height 2/Filter/JPXDecode/BitsPerComponent 1/Decode false/ColorSpace[/Separation /Spot /DeviceGray <</FunctionType 2/Domain[0 1]/Range[0 1]/C0[-32768]/C1[32767]/N 1>>]";
        let image = decode(header, data, b"").unwrap();
        let maximum: u32 = (1_u32 << bits) - 1;
        let expected =
            [0, maximum / 2, maximum, 1, maximum - 1, maximum.div_ceil(2)].map(|sample| {
                ((65535.0 * f64::from(sample) / f64::from(maximum) - 32768.0).clamp(0.0, 1.0)
                    * 255.0)
                    .round() as u8
            });
        assert_eq!(colors(&image), expected, "depth {bits}");
    }
}

#[test]
fn cmyk_tints_retain_decode_precision_and_original_mask_samples() {
    let ramp = "<</FunctionType 2/Domain[0 1]/Range[0 1 0 1 0 1 0 1]/C0[-500 501 .25 .5]/C1[500 -499 .25 .5]/N 1>>";
    for prefix in ["Separation /Spot", "DeviceN [/Spot]"] {
        for (bits, samples) in [
            (1, vec![0x40]),
            (2, vec![0x30]),
            (4, vec![0x0f]),
            (8, vec![0, 255]),
            (16, vec![0, 0, 255, 255]),
        ] {
            let maximum = (1_u32 << bits) - 1;
            let header = format!(
                "/Width 2/Height 1/BitsPerComponent {bits}/ColorSpace[/{prefix} /DeviceCMYK {ramp}]/Decode[.5 .501]/Mask[0 0]"
            );
            let decoded = with_image(&header, &samples, b"", |obj| {
                super::super::decode_device_cmyk_image(obj, None)
            })
            .unwrap();
            assert_eq!(
                decoded.image.data,
                [0, 255, 64, 128, 255, 0, 64, 128],
                "{prefix} {bits}/{maximum}"
            );
            assert_eq!(decoded.alpha.unwrap().data, [0, 255]);
            let decoded = with_image(
                &header.replace("[.5 .501]", "[.501 .5]"),
                &samples,
                b"",
                |obj| super::super::decode_device_cmyk_image(obj, None),
            )
            .unwrap();
            assert_eq!(decoded.image.data, [255, 0, 64, 128, 0, 255, 64, 128]);
            assert_eq!(decoded.alpha.unwrap().data, [0, 255]);
        }
    }
}

#[test]
fn cmyk_tints_recover_matte_before_function_and_final_quantization() {
    let transform = "<</FunctionType 3/Domain[0 1]/Range[0 1 0 1 0 1 0 1]/Functions[<</FunctionType 2/Domain[0 1]/C0[0 1 .25 .5]/C1[0 1 .25 .5]/N 1>><</FunctionType 2/Domain[0 1]/C0[1 0 .25 .5]/C1[1 0 .25 .5]/N 1>>]/Bounds[.50003]/Encode[0 1 0 1]>>";
    let header = format!(
        "/Width 2/Height 1/BitsPerComponent 16/ColorSpace[/Separation /Spot /DeviceCMYK {transform}]/SMask 5 0 R"
    );
    let mask = b"5 0 obj <</Type/XObject/Subtype/Image/Width 2/Height 1/BitsPerComponent 8/ColorSpace/DeviceGray/Matte[.5]/Length 2>>stream\n\x80\xff\nendstream\nendobj";
    let decoded = with_image(&header, &[0x80, 1, 0x80, 1], mask, |obj| {
        super::super::decode_device_cmyk_image(obj, None)
    })
    .unwrap();
    assert_eq!(decoded.image.data, [255, 0, 64, 128, 0, 255, 64, 128]);
    assert_eq!(decoded.alpha.unwrap().data, [128, 255]);
    assert!(
        with_image(&header, &[0x80, 1, 0x80], mask, |obj| {
            super::super::decode_device_cmyk_image(obj, None)
        })
        .is_none()
    );
}

#[test]
fn native_gray_retention_preserves_cached_byte_conversion() {
    let header = "/Width 3/Height 1/BitsPerComponent 8/ColorSpace[/DeviceN[/Spot]/DeviceGray<</FunctionType 2/Domain[0 1]/C0[.125]/C1[.875]/N 1>>]";
    let decoded = decode(header, &[0, 128, 255], b"").unwrap();
    let ImageData::Luma(data) = decoded.image else {
        panic!("retained Gray alternate")
    };
    assert_eq!(data.data, [32, 128, 223]);
}

#[test]
fn exact_byte_cmyk_lookup_preserves_palette_keys_and_repeated_samples() {
    let tint = "[/Separation /Spot /DeviceCMYK <</FunctionType 2/Domain[0 1]/C0[0 1 .25 .5]/C1[1 0 .25 .5]/N 1>>]";
    for (space, source, expected) in [
        (
            tint.to_owned(),
            vec![0, 64, 128, 255, 128, 64],
            vec![0, 64, 128, 255, 128, 64],
        ),
        (
            format!("[/Indexed {tint} 3<00ff4080>]"),
            vec![0, 1, 2, 3, 2, 1],
            vec![0, 255, 64, 128, 64, 255],
        ),
    ] {
        let header = format!("/Width 6/Height 1/BitsPerComponent 8/ColorSpace {space}");
        let decoded = with_image(&header, &source, b"", |obj| {
            super::super::decode_device_cmyk_image(obj, None)
        })
        .unwrap();
        for (pixel, expected) in decoded.image.data.chunks_exact(4).zip(expected) {
            assert_eq!(pixel, [expected, 255 - expected, 64, 128]);
        }
    }
}
