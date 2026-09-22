use super::*;

fn image_pixels(
    header: &str,
    samples: &[u8],
    extra: Option<Vec<u8>>,
    size: (u32, u32),
    profile: &[u8],
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    with_image(
        header,
        samples,
        extra,
        size,
        profile,
        &Cache::new(),
        |image| {
            let decoded = image.decoded_image(None)?;
            assert_eq!((decoded.image.width(), decoded.image.height()), size);
            let rgb = match decoded.image {
                crate::ImageData::Rgb(rgb) => rgb.data,
                crate::ImageData::Luma(gray) => {
                    gray.data.into_iter().flat_map(|v| [v; 3]).collect()
                }
            };
            Some((rgb, decoded.alpha.map(|alpha| alpha.data)))
        },
    )
}

pub(super) fn with_image<T>(
    header: &str,
    samples: &[u8],
    extra: Option<Vec<u8>>,
    size: (u32, u32),
    profile: &[u8],
    cache: &Cache,
    run: impl FnOnce(&crate::x_object::ImageXObject<'_>) -> T,
) -> T {
    use crate::x_object::ImageXObject;
    use hayro_syntax::{
        Pdf,
        object::{ObjectIdentifier, Stream},
    };

    let mut objects = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 256 256]/Resources<</ColorSpace<</DefaultGray[/ICCBased 5 0 R]>>/XObject<</I 4 0 R>>>>/Contents 6 0 R>>".to_vec(),
        stream(&format!("/Type/XObject/Subtype/Image/Width {}/Height {}{header}", size.0, size.1), samples),
        stream("/N 1", profile),
        stream("", b"q 256 0 0 256 0 0 cm /I Do Q"),
    ];
    objects.extend(extra);
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.into_iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend(format!("{} 0 obj\n", index + 1).bytes());
        bytes.extend(object);
        bytes.extend(b"\nendobj\n");
    }
    let start = bytes.len();
    bytes.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).bytes());
    for offset in offsets.iter().skip(1) {
        bytes.extend(format!("{offset:010} 00000 n \n").bytes());
    }
    bytes.extend(
        format!(
            "trailer\n<</Root 1 0 R/Size {}>>\nstartxref\n{start}\n%%EOF\n",
            offsets.len()
        )
        .bytes(),
    );
    let pdf = Pdf::new(bytes).unwrap();
    let stream = pdf
        .xref()
        .get::<Stream<'_>>(ObjectIdentifier::new(4, 0))
        .unwrap();
    let warning: crate::WarningSinkFn = Arc::new(|_| {});
    let image =
        ImageXObject::new(&stream, pdf.pages()[0].resources(), &warning, cache, None).unwrap();
    run(&image)
}

fn stream(header: &str, data: &[u8]) -> Vec<u8> {
    let mut bytes = format!("<<{header}/Length {}>>\nstream\n", data.len()).into_bytes();
    bytes.extend(data);
    bytes.extend(b"\nendstream");
    bytes
}

#[test]
fn image_decode_and_tint_outputs_reach_icc_without_byte_quantization() {
    let wide: Vec<_> = (0..=65535_u16).flat_map(u16::to_be_bytes).collect();
    assert!(
        image_pixels(
            "/BitsPerComponent 16/ColorSpace[/ICCBased 5 0 R]",
            &wide[..wide.len() - 1],
            None,
            (256, 256),
            &profile_bytes(true, 8)
        )
        .is_none()
    );
    let byte: Vec<_> = (0..=65535_u16)
        .map(|v| (f64::from(v) / 257.0).round() as u8)
        .collect();
    for (bits, decode, tint, low, high) in [
        (16, "", false, 0.0, 1.0),
        (16, "/Decode[1 0]", false, 1.0, 0.0),
        (16, "/Decode[0.125 0.875]", false, 0.125, 0.875),
        (8, "", true, 0.25, 0.75),
    ] {
        let space = if tint {
            "[/Separation/GrayInk[/ICCBased 5 0 R]<</FunctionType 2/Domain[0 1]/C0[0.25]/C1[0.75]/N 1>>]"
        } else {
            "[/ICCBased 5 0 R]"
        };
        let header = format!("/BitsPerComponent {bits}/ColorSpace {space}{decode}");
        let (actual, alpha) = image_pixels(
            &header,
            if bits == 16 { &wide } else { &byte },
            None,
            (256, 256),
            &profile_bytes(true, 8),
        )
        .unwrap();
        assert!(alpha.is_none());
        assert_eq!(actual.len(), 65536 * 3);
        for (sample, pixel) in actual.chunks_exact(3).enumerate() {
            let input = if bits == 16 {
                sample as f64 / 65535.0
            } else {
                f64::from(byte[sample]) / 255.0
            };
            let wanted = expected(true, 8, low + (high - low) * input);
            assert!(
                pixel.iter().all(|&v| v.abs_diff(wanted) <= 1),
                "bits={bits} decode={decode} tint={tint} sample={sample}: {pixel:?}, expected {wanted}"
            );
        }
    }
}

#[test]
fn native_icc_matte_and_color_key_masks_preserve_source_and_opacity() {
    let wide: Vec<_> = (0..=65535_u16).flat_map(u16::to_be_bytes).collect();
    let opacity: Vec<_> = (0..65536).map(|i| [0, 1, 128, 254, 255][i % 5]).collect();
    let header = "/BitsPerComponent 16/ColorSpace[/ICCBased 5 0 R]/SMask 7 0 R";
    let mask = stream(
        "/Type/XObject/Subtype/Image/Width 256/Height 256/BitsPerComponent 8/ColorSpace/DeviceGray/Matte[.25]",
        &opacity,
    );
    let (actual, alpha) = image_pixels(
        header,
        &wide,
        Some(mask),
        (256, 256),
        &profile_bytes(true, 8),
    )
    .unwrap();
    assert_eq!(alpha.unwrap(), opacity);
    for (index, rgb) in actual.chunks_exact(3).enumerate() {
        let alpha = f64::from(opacity[index]) / 255.0;
        let value = if alpha == 0.0 {
            0.25
        } else {
            (0.25 + (index as f64 / 65535.0 - 0.25) / alpha).clamp(0.0, 1.0)
        };
        let wanted = expected(true, 8, value);
        assert!(
            rgb.iter().all(|&v| v.abs_diff(wanted) <= 1),
            "sample={index}: {rgb:?}, expected {wanted}"
        );
    }
    let (actual, alpha) = image_pixels(
        "/BitsPerComponent 16/ColorSpace[/ICCBased 5 0 R]/Mask[32767 32768]",
        &wide,
        None,
        (256, 256),
        &profile_bytes(true, 8),
    )
    .unwrap();
    for (index, (rgb, alpha)) in actual.chunks_exact(3).zip(alpha.unwrap()).enumerate() {
        let wanted = expected(true, 8, index as f64 / 65535.0);
        assert!(rgb.iter().all(|&v| v.abs_diff(wanted) <= 1));
        assert_eq!(
            alpha,
            if (32767..=32768).contains(&index) {
                0
            } else {
                255
            }
        );
    }
}

#[test]
fn native_icc_default_palette_and_devicen_alternates_preserve_values() {
    let wide: Vec<_> = (0..=65535_u16).flat_map(u16::to_be_bytes).collect();
    for (space, decode, palette, tint) in [
        ("/DeviceGray", "", false, false),
        (
            "[/DeviceN[/GrayInk][/ICCBased 5 0 R]<</FunctionType 2/Domain[0 1]/C0[0.25]/C1[0.75]/N 1>>]",
            "",
            false,
            true,
        ),
        (
            "[/Indexed[/ICCBased 5 0 R]3<0055aaff>]",
            "/Decode[0 3]",
            true,
            false,
        ),
        (
            "[/Indexed[/Separation/GrayInk[/ICCBased 5 0 R]<</FunctionType 2/Domain[0 1]/C0[0.25]/C1[0.75]/N 1>>]3<0055aaff>]",
            "/Decode[0 3]",
            true,
            true,
        ),
    ] {
        let header = format!("/BitsPerComponent 16/ColorSpace{space}{decode}");
        let (actual, alpha) =
            image_pixels(&header, &wide, None, (256, 256), &profile_bytes(true, 8)).unwrap();
        assert!(alpha.is_none());
        for (index, rgb) in actual.chunks_exact(3).enumerate() {
            let mut value = index as f64 / 65535.0;
            if palette {
                value = (value * 3.0).round() / 3.0;
            }
            if tint {
                value = 0.25 + 0.5 * value;
            }
            let wanted = expected(true, 8, value);
            assert!(
                rgb.iter().all(|&v| v.abs_diff(wanted) <= 1),
                "{space} sample={index}: {rgb:?}, expected {wanted}"
            );
        }
    }
}

use crate::jpx_opacity_fixtures as jpx;

#[test]
fn embedded_native_opacity_recovery_precedes_icc_conversion() {
    // Original lossless JPX encodes color 32768, opacity 32769, each 16-bit.
    // Gamma 255 exposes the loss if recovered color is rounded to one byte.
    for mode in [1, 2] {
        let header = format!(
            "/Filter/JPXDecode/SMaskInData {mode}/ColorSpace[/ICCBased 5 0 R]/Decode[1 0]/BitsPerComponent 1"
        );
        let (actual, alpha) = image_pixels(
            &header,
            jpx::ALPHA16_BOUNDARY,
            None,
            (1, 1),
            &profile_bytes(true, 255),
        )
        .unwrap();
        let input = 32768.0 / if mode == 2 { 32769.0 } else { 65535.0 };
        let wanted = expected(true, 255, input);
        assert!(
            actual.iter().all(|&v| v.abs_diff(wanted) <= 1),
            "mode={mode} {actual:?}, expected {wanted}"
        );
        assert_eq!(alpha.unwrap(), [128]);
        if mode == 2 {
            assert!(wanted.abs_diff(255) >= 2);
        }
    }
}
