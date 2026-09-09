//! PDF 1.7 §§7.4.7 and 8.9.7 prohibit `JBIG2Decode` in inline images.
use super::{DecodeFailure, ImageDecodeParams, Stream};
use crate::content::{TypedIter, ops::TypedInstruction};
use crate::object::{Dict, FromBytes};
use alloc::{format, vec::Vec};

#[cfg(feature = "images")]
#[path = "../filter/jbig2_fixtures.rs"]
mod fixtures;

fn page(black: bool) -> Vec<u8> {
    // Authored T.88 §§7.4.8/8.2 page-information segment: 9x2 pixels,
    // no regions, so every pixel retains its declared initial value.
    let mut bytes = alloc::vec![0, 0, 0, 1, 48, 0, 1, 0, 0, 0, 19];
    for value in [9_u32, 2, 0, 0] {
        bytes.extend(value.to_be_bytes());
    }
    bytes.extend([if black { 5 } else { 1 }, 0, 0]);
    bytes
}

#[test]
fn parsed_inline_jbig2_is_rejected_before_decoding() {
    for (filter, subtype) in [
        ("/Filter /JBIG2Decode", ""),
        ("/F [/AHx /JBIG2Decode]", ""),
        ("/F [/AHx /JBIG2Decode]", "/Type /XObject /Subtype /Image"),
    ] {
        let source = page(true);
        let encoded = if filter.contains("AHx") {
            source
                .iter()
                .flat_map(|b| format!("{b:02x}").into_bytes())
                .chain(*b">\n")
                .collect()
        } else {
            source
        };
        let mut body = format!("BI /W 9 /H 2 /CS /G /BPC 1 {filter} {subtype} ID\n").into_bytes();
        body.extend(encoded);
        // The segment's final NUL is itself PDF whitespace before EI.
        body.extend(b"EI Q");
        let mut instructions = TypedIter::new(&body);
        let TypedInstruction::InlineImage(image) = instructions.next().unwrap() else {
            panic!("inline image")
        };
        assert!(image.0.is_inline_image());
        assert!(matches!(
            image.0.decoded(),
            Err(DecodeFailure::InvalidFilterPlacement)
        ));
        assert!(matches!(
            image.0.clone().decoded_image(&ImageDecodeParams::default()),
            Err(DecodeFailure::InvalidFilterPlacement)
        ));
        assert!(matches!(
            instructions.next(),
            Some(TypedInstruction::RestoreState(_))
        ));
    }
}

#[test]
#[cfg(feature = "images")]
fn jbig2_image_xobjects_always_return_packed_rows() {
    for black in [false, true] {
        let source = page(black);
        let stream = Stream::new(
            &source,
            Dict::from_bytes(b"<< /Type /XObject /Subtype /Image /Width 9 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode >>").unwrap(),
        );
        assert!(!stream.is_inline_image());
        for indexed in [false, true] {
            let result = stream
                .decoded_image(&ImageDecodeParams {
                    is_indexed: indexed,
                    width: 9,
                    height: 2,
                    bpc: Some(1),
                    ..Default::default()
                })
                .unwrap();
            let expected = if black {
                alloc::vec![0, 0, 0, 0]
            } else {
                alloc::vec![255, 128, 255, 128]
            };
            assert_eq!(result.data.as_ref(), expected);
            let metadata = result.image_data.unwrap();
            assert_eq!(
                (metadata.width, metadata.height, metadata.bits_per_component),
                (9, 2, 1)
            );
        }
    }
}

#[test]
fn jbig2_dictionary_requires_exact_dimensions_depth_and_stencil_contracts() {
    use super::validate_jbig2_image_dictionary as validate;
    for dimensions in ["/Width 9 /Height 2", "/W 9 /H 2"] {
        for (entries, valid) in [
            ("/BitsPerComponent 1 /ColorSpace /DeviceGray", true),
            ("/BPC 1 /CS [/Indexed /DeviceRGB 1 <000000ffffff>]", true),
            ("/ImageMask true", true),
            ("/ImageMask true /BitsPerComponent null", true),
            ("/ImageMask true /BitsPerComponent 99 0 R", true),
            ("/ImageMask true /BPC 1 /Decode [1.0 0]", true),
            ("/ImageMask true /BPC 1 /Decode [0 1]", true),
            ("/ImageMask true /ColorSpace null", true),
            ("/ColorSpace /DeviceGray", false),
            ("/ColorSpace /DeviceGray /BPC null", false),
            ("/ColorSpace /DeviceGray /BPC 99 0 R", false),
            ("/ColorSpace /DeviceGray /BPC 1.0", false),
            ("/ColorSpace /DeviceGray /BPC 1.5", false),
            ("/ColorSpace /DeviceGray /BPC 257", false),
            ("/ColorSpace /DeviceGray /BPC -1", false),
            ("/ColorSpace /DeviceGray /BPC 0", false),
            ("/BPC 1", false),
            ("/BPC 1 /ColorSpace null", false),
            ("/BPC 1 /ImageMask /True /ColorSpace /DeviceGray", false),
            ("/ImageMask true /BPC 8", false),
            ("/ImageMask true /BPC 1.0", false),
            ("/ImageMask true /ColorSpace /DeviceGray", false),
            ("/ImageMask true /Decode [0 .5]", false),
            ("/ImageMask true /Decode [0 1 0]", false),
            ("/ImageMask true /Decode [0 /One]", false),
        ] {
            let bytes = format!("<< {dimensions} {entries} >>");
            let dict = Dict::from_bytes(bytes.as_bytes()).unwrap();
            assert_eq!(validate(&dict).is_ok(), valid, "{bytes}");
        }
    }
    for bad in ["null", "0", "-1", "1.0", "1.5", "4294967296", "/One"] {
        for dimensions in [
            format!("/Width {bad} /Height 2"),
            format!("/Width 9 /Height {bad}"),
        ] {
            let bytes = format!("<< {dimensions} /BPC 1 /ColorSpace /DeviceGray >>");
            assert!(
                validate(&Dict::from_bytes(bytes.as_bytes()).unwrap()).is_err(),
                "{bytes}"
            );
        }
    }
}

#[test]
#[cfg(feature = "images")]
fn jbig2_packed_generic_streams_and_image_dimensions() {
    let generic = Stream::new(
        fixtures::PATTERN,
        Dict::from_bytes(b"<< /Filter /JBIG2Decode >>").unwrap(),
    );
    assert_eq!(generic.decoded().unwrap().as_ref(), &[0xaa, 0x80, 0x55, 0]);
    let generic = Stream::new(
        fixtures::ENCODED_GLOBAL,
        Dict::from_bytes(b"<< /Filter /JBIG2Decode >>").unwrap(),
    );
    let global = generic.decoded().unwrap();
    assert_eq!(global.len(), 21);
    assert_eq!(global[4], 0); // symbol dictionary
    assert_eq!(global[6], 0); // global page association
    for (width, height, valid) in [(9, 2, true), (1, 2, false), (9, 1, false), (18, 2, false)] {
        let bytes = format!(
            "<< /Subtype /Image /Width {width} /Height {height} /BPC 1 /ColorSpace /DeviceGray /Filter /JBIG2Decode >>"
        );
        let stream = Stream::new(
            fixtures::PATTERN,
            Dict::from_bytes(bytes.as_bytes()).unwrap(),
        );
        assert_eq!(stream.decoded().is_ok(), valid);
        for components in [1, 3] {
            assert_eq!(
                stream
                    .decoded_image(&ImageDecodeParams {
                        width,
                        height,
                        bpc: Some(1),
                        num_components: Some(components),
                        ..Default::default()
                    })
                    .is_ok(),
                valid && components == 1
            );
        }
    }
}

#[cfg(feature = "images")]
fn pdf_stream(entries: &str, data: &[u8]) -> Vec<u8> {
    [
        format!("<< {entries} /Length {} >>\nstream\n", data.len()).into_bytes(),
        data.to_vec(),
        b"\nendstream".to_vec(),
    ]
    .concat()
}

#[cfg(feature = "images")]
fn globals_pdf(value: &str, global: Vec<u8>) -> crate::Pdf {
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 2] >>".to_vec(),
        pdf_stream(
            &format!("/Filter /JBIG2Decode /DecodeParms << /JBIG2Globals {value} >>"),
            &page(true),
        ),
        global,
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend(object);
        bytes.extend(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    crate::Pdf::new(bytes).unwrap()
}

#[test]
#[cfg(feature = "images")]
fn jbig2_globals_resolve_null_decode_filters_and_reject_cycles() {
    for (value, valid) in [
        ("null", true),
        ("99 0 R", true),
        ("42", false),
        ("/Global", false),
        ("[]", false),
        ("(data)", false),
        ("<< >>", false),
    ] {
        let pdf = globals_pdf(value, b"null".to_vec());
        let stream = pdf
            .xref()
            .get::<Stream<'_>>(crate::object::ObjectIdentifier::new(4, 0))
            .unwrap();
        assert_eq!(stream.decoded().is_ok(), valid, "{value}");
    }
    for (global, valid) in [
        (pdf_stream("", b""), true),
        (
            pdf_stream("/Filter /JBIG2Decode", fixtures::ENCODED_GLOBAL),
            true,
        ),
        (pdf_stream("/Filter /MadeUp", b""), false),
        (pdf_stream("/Filter /FlateDecode", b"notzlib"), false),
        (pdf_stream("/Filter /ASCIIHexDecode", b"zz>"), false),
        (pdf_stream("", &page(true)), false),
        (
            pdf_stream(
                "/Filter /JBIG2Decode /DecodeParms << /JBIG2Globals 5 0 R >>",
                fixtures::ENCODED_GLOBAL,
            ),
            false,
        ),
        (
            pdf_stream(
                "/Filter /JBIG2Decode /DecodeParms << /JBIG2Globals 4 0 R >>",
                fixtures::ENCODED_GLOBAL,
            ),
            false,
        ),
    ] {
        let pdf = globals_pdf("5 0 R", global);
        let stream = pdf
            .xref()
            .get::<Stream<'_>>(crate::object::ObjectIdentifier::new(4, 0))
            .unwrap();
        let result = stream.decoded();
        assert_eq!(result.is_ok(), valid, "{result:?}");
        if valid {
            assert_eq!(result.unwrap().as_ref(), &[0, 0, 0, 0]);
        }
    }
}
