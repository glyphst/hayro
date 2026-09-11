use super::{Dict, ImageDecodeParams, Stream};
use crate::content::{TypedIter, ops::TypedInstruction};
use crate::object::FromBytes;

#[test]
fn crypt_identity_defaults_and_order_work_without_encryption() {
    for entries in [
        "/Filter /Crypt",
        "/Filter /Crypt /DecodeParms null",
        "/Filter /Crypt /DecodeParms << /Name /Identity >>",
        "/Filter [/Crypt] /DecodeParms [null]",
        "/Filter [/Crypt] /DecodeParms [<< /Name /Identity >>]",
        "/Filter [/Crypt /ASCIIHexDecode] /DecodeParms [null null]",
    ] {
        let bytes = format!("<< {entries} >>");
        let stream = Stream::new(b"41>", Dict::from_bytes(bytes.as_bytes()).unwrap());
        assert_eq!(stream.raw_data_checked().unwrap().as_ref(), b"41>");
        let expected: &[u8] = if entries.contains("ASCIIHexDecode") {
            b"A"
        } else {
            b"41>"
        };
        assert_eq!(stream.decoded().unwrap().as_ref(), expected, "{entries}");
    }
}

#[test]
fn crypt_rejects_unknown_names_bad_parameters_and_invalid_placement() {
    for entries in [
        "/Filter /Crypt /DecodeParms << /Name /StdCF >>",
        "/Filter /Crypt /DecodeParms << /Name 42 >>",
        "/Filter /Crypt /DecodeParms << /Type /Bad >>",
        "/Filter /Crypt /DecodeParms << /Type 42 >>",
        "/Filter /Crypt /DecodeParms 42",
        "/Filter /Crypt /DecodeParms [null]",
        "/Filter [/Crypt] /DecodeParms << >>",
        "/Filter [/Crypt] /DecodeParms []",
        "/Filter [/Crypt] /DecodeParms [null null]",
        "/Filter [/Crypt] /DecodeParms [42]",
        "/Filter [/ASCIIHexDecode /Crypt]",
        "/Filter [/Crypt /Crypt]",
        "/Type /XRef /Filter /Crypt",
    ] {
        let bytes = format!("<< {entries} >>");
        let stream = Stream::new(b"41>", Dict::from_bytes(bytes.as_bytes()).unwrap());
        assert!(stream.decoded().is_err(), "{entries}");
        assert!(
            stream.decoded_image(&ImageDecodeParams::default()).is_err(),
            "{entries}"
        );
    }
}

#[test]
fn inline_identity_crypt_is_a_standard_filter() {
    // PDF 1.7 §8.9.7 allows standard filters except JBIG2Decode and JPXDecode.
    let mut instructions = TypedIter::new(
        b"BI /W 1 /H 1 /CS /G /BPC 8 /F [/Crypt /AHx] /DP [null null] ID\n41>\nEI Q",
    );
    let TypedInstruction::InlineImage(image) = instructions.next().unwrap() else {
        panic!("inline image")
    };
    assert!(image.0.is_inline_image());
    assert_eq!(
        image
            .0
            .decoded_image(&ImageDecodeParams::default())
            .unwrap()
            .data
            .as_ref(),
        b"A"
    );
    assert!(matches!(
        instructions.next(),
        Some(TypedInstruction::RestoreState(_))
    ));
}
