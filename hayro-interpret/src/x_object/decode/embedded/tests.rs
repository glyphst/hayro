use super::*;
use crate::cache::Cache;
use hayro_syntax::Pdf;
use hayro_syntax::object::{ObjectIdentifier, Stream};

#[allow(dead_code)]
#[path = "../../../../../hayro-syntax/src/filter/jpx_opacity_fixtures.rs"]
mod fixtures;

fn with_image<T>(
    data: &[u8],
    space: &str,
    mode: &str,
    extra: &str,
    callback: impl FnOnce(&ImageXObject<'_>) -> Option<T>,
) -> Option<T> {
    let mut bytes = format!("%PDF-1.7\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Resources<<>>>>endobj\n4 0 obj<</Type/XObject/Subtype/Image/Width 1/Height 1/Filter/JPXDecode/SMaskInData {mode}{space}{extra}/Length {}>>stream\n",data.len()).into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream\nendobj\ntrailer<</Root 1 0 R>>\n%%EOF");
    let pdf = Pdf::new(bytes).unwrap();
    let stream = pdf
        .xref()
        .get::<Stream<'_>>(ObjectIdentifier::new(4, 0))
        .unwrap();
    let warning: crate::WarningSinkFn = std::sync::Arc::new(|_| {});
    let object = ImageXObject::new(
        &stream,
        pdf.pages()[0].resources(),
        &warning,
        &Cache::new(),
        None,
    )?;
    callback(&object)
}

fn image(data: &[u8], space: &str, mode: &str, extra: &str) -> Option<DecodedImage> {
    with_image(data, space, mode, extra, |object| {
        crate::x_object::decode::decode_image(object, None)
    })
}

fn rgb(image: &DecodedImage) -> Vec<u8> {
    match &image.image {
        crate::ImageData::Rgb(data) => data.data.clone(),
        crate::ImageData::Luma(data) => data.data.iter().flat_map(|v| [*v; 3]).collect(),
    }
}

#[test]
fn native_opacity_and_color_precede_calgray_gamma() {
    let space = "/ColorSpace[/CalGray<</WhitePoint[.95047 1 1.08883]/Gamma .01>>]";
    // Independent 60-digit scalar oracle: source 128/32768 (mode 2),
    // 128/65535 (mode 1), followed by Gamma .01 and sRGB encoding.
    for (mode, expected) in [("2", 249), ("1", 248)] {
        let decoded = image(fixtures::GRAY16, space, mode, "").unwrap();
        assert_eq!(rgb(&decoded), [expected; 3]);
        assert_eq!(decoded.alpha.unwrap().data, [128]);
    }
    // A single native opacity step must survive before an amplifying gamma.
    // 60-digit oracle: sRGB((32768/32769)^1000) * 255 = 251.6008789219817...
    let decoded = image(
        fixtures::ALPHA16_BOUNDARY,
        "/ColorSpace[/CalGray<</WhitePoint[.95047 1 1.08883]/Gamma 1000>>]",
        "2",
        "",
    )
    .unwrap();
    assert_eq!(rgb(&decoded), [252; 3]);
    assert_eq!(decoded.alpha.unwrap().data, [128]);
}

#[test]
fn channel_recovery_precedes_lab_offsets_and_index_rounding() {
    let decoded = image(
        fixtures::LAB8,
        "/ColorSpace[/Lab<</WhitePoint[.95047 1 1.08883]>>]",
        "2",
        "",
    )
    .unwrap();
    // 64 * 255 / 128 = 127.5: L=50, a=b=0 in the default [-100,100] ranges.
    assert_eq!(rgb(&decoded), [119; 3]);
    let decoded = image(
        fixtures::INDEXED16,
        "/ColorSpace[/Indexed/DeviceRGB 3<000000ffffffFF00000000FF>]",
        "2",
        "",
    )
    .unwrap();
    // Source index 1 / .4 = 2.5 rounds to palette entry 3.
    assert_eq!(rgb(&decoded), [0, 0, 255]);
    assert_eq!(decoded.alpha.unwrap().data, [102]);
}

#[test]
fn implicit_sycc_recovers_channels_before_conversion() {
    let decoded = image(fixtures::SYCC8, "", "2", "").unwrap();
    // Y=63.75, Cb=Cr=127.5; then subtract the encoded chroma offset 128.
    assert_eq!(rgb(&decoded), [63, 64, 63]);
    assert_eq!(decoded.alpha.unwrap().data, [128]);
}

#[test]
fn parent_matte_does_not_change_jpx_channel_multiplication() {
    for matte in ["", "/Matte[1 0 1]", "/Matte/Unused"] {
        let decoded = image(fixtures::RGB8, "/ColorSpace/DeviceRGB", "2", matte).unwrap();
        assert_eq!(rgb(&decoded), [128, 255, 64]);
    }
}

#[test]
fn opacity_mode_requires_an_integer_and_an_actual_channel() {
    for mode in ["1.5", "1.0", "-1", "3", "/Bad", "true"] {
        assert!(
            image(fixtures::GRAY16, "/ColorSpace/DeviceGray", mode, "").is_none(),
            "{mode}"
        );
    }
    assert!(image(fixtures::NO_ALPHA, "/ColorSpace/DeviceGray", "1", "").is_none());
    for mode in ["0", "null", "99 0 R"] {
        let decoded = image(fixtures::RGB8, "/ColorSpace/DeviceRGB", mode, "").unwrap();
        assert_eq!(rgb(&decoded), [64, 128, 32]);
        assert!(decoded.alpha.is_none());
    }
}

#[test]
fn native_depths_and_channel_order_preserve_opacity() {
    for (bits, data) in [
        (1, fixtures::GRAY_ALPHA_1),
        (2, fixtures::GRAY_ALPHA_2),
        (4, fixtures::GRAY_ALPHA_4),
        (8, fixtures::GRAY_ALPHA_8),
        (12, fixtures::GRAY_ALPHA_12),
        (16, fixtures::GRAY_ALPHA_16),
    ] {
        let maximum = (1_u32 << bits) - 1;
        let color = maximum / 8;
        let opacity = maximum.div_ceil(2);
        let decoded = image(
            data,
            "/ColorSpace/DeviceGray",
            "2",
            "/Decode[1 0]/BitsPerComponent 1",
        )
        .unwrap();
        let expected =
            ((u64::from(color) * 255 * 2 + u64::from(opacity)) / (2 * u64::from(opacity))) as u8;
        assert_eq!(rgb(&decoded), [expected; 3], "{bits}");
        assert_eq!(
            decoded.alpha.unwrap().data,
            [
                ((u64::from(opacity) * 255 * 2 + u64::from(maximum)) / (2 * u64::from(maximum)))
                    as u8
            ]
        );
    }
    for space in ["", "/ColorSpace/DeviceRGB"] {
        let decoded = image(
            fixtures::ALPHA_FIRST,
            space,
            "2",
            "/SMask/Unused/Mask[0 255]",
        )
        .unwrap();
        assert_eq!(rgb(&decoded), [128, 255, 64]);
        assert_eq!(decoded.alpha.unwrap().data, [128]);
        let decoded = image(fixtures::ZERO_ALPHA, space, "2", "").unwrap();
        assert_eq!(rgb(&decoded), [0; 3]);
        assert_eq!(decoded.alpha.unwrap().data, [0]);
        let decoded = image(fixtures::FULL_ALPHA, space, "2", "").unwrap();
        assert_eq!(rgb(&decoded), [64, 128, 32]);
        assert_eq!(decoded.alpha.unwrap().data, [255]);
    }
}

#[test]
fn subtractive_source_components_recover_before_cmyk_conversion() {
    let decoded = with_image(
        fixtures::CMYK_ALPHA,
        "/ColorSpace/DeviceCMYK",
        "2",
        "",
        |object| crate::x_object::decode::decode_device_cmyk_image(object, None),
    )
    .unwrap();
    assert_eq!(decoded.image.data, [128, 255, 64, 32]);
    assert_eq!(decoded.alpha.unwrap().data, [128]);
}
