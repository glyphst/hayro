//! PDF 1.7 §§7.4.7 and 8.9.7 prohibit `JBIG2Decode` in inline images.
use super::{DecodeFailure, ImageDecodeParams, Stream};
use crate::content::{TypedIter, ops::TypedInstruction};
use crate::object::{Dict, FromBytes};
use alloc::{format, vec::Vec};

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
fn jbig2_image_xobjects_keep_luma_and_packed_row_decoding() {
    for black in [false, true] {
        let source = page(black);
        let stream = Stream::new(
            &source,
            Dict::from_bytes(b"<< /Type /XObject /Subtype /Image /Filter /JBIG2Decode >>").unwrap(),
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
            let expected = if indexed {
                if black {
                    alloc::vec![0, 0, 0, 0]
                } else {
                    alloc::vec![255, 128, 255, 128]
                }
            } else {
                alloc::vec![if black { 0 } else { 255 }; 18]
            };
            assert_eq!(result.data.as_ref(), expected);
            let metadata = result.image_data.unwrap();
            assert_eq!(
                (metadata.width, metadata.height, metadata.bits_per_component),
                (9, 2, if indexed { 1 } else { 8 })
            );
        }
    }
}
