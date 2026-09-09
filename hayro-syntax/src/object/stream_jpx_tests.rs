//! PDF 1.7 §7.4.9: `JPXDecode` may only consume image `XObjects`.
#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../filter/jpx_fixtures.rs"]
mod fixtures;

use super::{DecodeFailure, ImageDecodeParams, Stream};
use crate::content::{TypedIter, ops::TypedInstruction};
use crate::object::dict::InlineImageDict;
use crate::object::{Dict, FromBytes};
use crate::reader::{Reader, ReaderExt};
use alloc::{format, vec::Vec};

#[test]
fn inline_flate_preserves_whitespace_checksums_and_only_ignores_ei_separation() {
    // One-byte stored zlib blocks whose final checksum bytes are PDF whitespace.
    for sample in [8_u8, 9, 12, 31] {
        let dictionary = Reader::new(b"/F /Fl ID")
            .read_without_context::<InlineImageDict<'_>>()
            .unwrap();
        let checksum_low = sample + 1;
        let mut encoded = alloc::vec![
            0x78,
            1,
            1,
            1,
            0,
            0xfe,
            0xff,
            sample,
            0,
            checksum_low,
            0,
            checksum_low
        ];
        for separator in [b" " as &[u8], b"\n", b"\r\n", b" \t\r\n"] {
            let data = [encoded.as_slice(), separator].concat();
            let stream = Stream::new(&data, dictionary.get_dict().clone());
            assert_eq!(
                stream
                    .decoded_image(&ImageDecodeParams::default())
                    .unwrap()
                    .data
                    .as_ref(),
                [sample]
            );
            #[cfg(feature = "unsafe")]
            assert!(
                Stream::new(
                    &data,
                    Dict::from_bytes(b"<< /Filter /FlateDecode >>").unwrap()
                )
                .decoded()
                .is_err()
            );
        }
        encoded.extend_from_slice(b"junk\n");
        #[cfg(feature = "unsafe")]
        assert!(
            Stream::new(&encoded, dictionary.get_dict().clone())
                .decoded_image(&ImageDecodeParams::default())
                .is_err()
        );
    }
}

#[test]
fn generic_decoding_rejects_jpx_before_any_filter_runs() {
    for filter in [
        "/JPXDecode",
        "[/ASCIIHexDecode /JPXDecode]",
        "[/JPXDecode /FlateDecode]",
    ] {
        for subtype in ["", "/Subtype /Image", "/Subtype /Form"] {
            let dictionary = format!("<< /Filter {filter} {subtype} >>");
            let stream = Stream::new(
                b"not encoded image bytes",
                Dict::from_bytes(dictionary.as_bytes()).unwrap(),
            );
            assert!(matches!(
                stream.decoded(),
                Err(DecodeFailure::InvalidFilterPlacement)
            ));
        }
    }
}

#[test]
#[cfg(feature = "images")]
fn image_api_requires_an_image_xobject_for_jpx() {
    let params = ImageDecodeParams {
        width: 3,
        height: 2,
        num_components: Some(1),
        ..Default::default()
    };
    for subtype in ["", "/Subtype /Form", "/Subtype /Image"] {
        let dictionary = format!("<< /Filter /JPXDecode {subtype} >>");
        let stream = Stream::new(
            fixtures::DEPTH_8,
            Dict::from_bytes(dictionary.as_bytes()).unwrap(),
        );
        let result = stream.decoded_image(&params);
        if subtype == "/Subtype /Image" {
            let result = result.unwrap();
            assert_eq!(result.data.as_ref(), [0, 127, 255, 1, 254, 128]);
            assert_eq!(result.image_data.unwrap().bits_per_component, 8);
        } else {
            assert!(matches!(result, Err(DecodeFailure::InvalidFilterPlacement)));
        }
    }
}

#[test]
fn parsed_inline_provenance_survives_clone_and_spoofed_subtype() {
    for entries in [
        "/Filter /JPXDecode",
        "/F /JPXDecode /Type /XObject /Subtype /Image",
        "/F [/ASCIIHexDecode /JPXDecode] /Subtype /Image",
    ] {
        let encoded: Vec<u8> = if entries.contains("ASCIIHex") {
            fixtures::DEPTH_8
                .iter()
                .flat_map(|b| format!("{b:02x}").into_bytes())
                .chain(*b">")
                .collect()
        } else {
            fixtures::DEPTH_8.to_vec()
        };
        let mut content = format!("BI /W 3 /H 2 /BPC 8 /CS /G {entries} ID\n").into_bytes();
        content.extend(encoded);
        content.extend_from_slice(b"\nEI Q");
        let mut instructions = TypedIter::new(&content);
        let TypedInstruction::InlineImage(image) = instructions.next().unwrap() else {
            panic!("inline image")
        };
        assert!(image.0.is_inline_image());
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
