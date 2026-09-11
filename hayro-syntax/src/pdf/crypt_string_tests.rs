//! PDF 1.7 §§7.5.8.2 and 7.6.1 dictionary string exemptions.
use super::fixtures;
use crate::Pdf;
use crate::object::{Array, Dict, FromBytes, ObjectIdentifier, Stream, String as PdfString};

fn objects(case: &fixtures::Case) -> Vec<Vec<u8>> {
    case.objects
        .iter()
        .map(|value| fixtures::hex(value))
        .collect()
}

fn assert_strings(dict: &Dict<'_>, expected: &[u8]) {
    assert_eq!(
        dict.get::<PdfString<'_>>(b"Probe").unwrap().as_ref(),
        expected
    );
    let nested = dict.get::<Array<'_>>(b"Nested").unwrap();
    let mut values = nested.flex_iter();
    assert_eq!(values.next::<PdfString<'_>>().unwrap().as_ref(), expected);
    let inner = values.next::<Dict<'_>>().unwrap();
    assert_eq!(
        inner.get::<PdfString<'_>>(b"Probe").unwrap().as_ref(),
        expected
    );
    // A reference must start a new decryption scope.
    let info = inner.get::<Dict<'_>>(b"Info").unwrap();
    assert_eq!(
        info.get::<PdfString<'_>>(b"Title").unwrap().as_ref(),
        b"Crypt selection fixture"
    );
}

const PROBES: &[u8] = b" /Probe (clear\\040bytes) /Nested [<636c656172206279746573> << /Probe (clear bytes) /Info 7 0 R >>]";

#[test]
fn crypt_encryption_dictionary_strings_stay_clear_after_authentication() {
    for name in [
        "v2-same-defaults",
        "aesv2-same-defaults",
        "r5-aes-padding-0",
        "r6-aes-padding-0",
    ] {
        let case = fixtures::CASES
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let mut objects = objects(case);
        let original = objects[4].clone();
        objects[4].truncate(original.len() - 2);
        objects[4].extend_from_slice(PROBES);
        objects[4].extend_from_slice(b" >>");
        for password in ["", "owner-test"] {
            let pdf = Pdf::new_with_password(fixtures::document(case, objects.clone()), password)
                .unwrap();
            let dict = pdf
                .xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(5, 0))
                .unwrap();
            assert_strings(&dict, b"clear bytes");
            let original = Dict::from_bytes(&original).unwrap();
            for key in [b"O".as_slice(), b"U", b"OE", b"UE", b"Perms"] {
                assert_eq!(
                    dict.get::<PdfString<'_>>(key),
                    original.get::<PdfString<'_>>(key),
                    "{name} {key:?}"
                );
            }
        }
    }
}

#[test]
fn crypt_xref_dictionary_exemption_is_direct_and_does_not_escape_references() {
    for name in [
        "v2-same-defaults",
        "aesv2-same-defaults",
        "r5-aes-padding-0",
        "r6-aes-padding-0",
    ] {
        let case = fixtures::CASES
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let mut objects = objects(case);
        objects[7] = b"<< /Type /XRef /Length 3".to_vec();
        objects[7].extend_from_slice(PROBES);
        objects[7].extend_from_slice(b" >>\nstream\nabc\nendstream");
        for password in ["", "owner-test"] {
            let pdf = Pdf::new_with_password(fixtures::document(case, objects.clone()), password)
                .unwrap();
            let stream = pdf
                .xref()
                .get::<Stream<'_>>(ObjectIdentifier::new(8, 0))
                .unwrap();
            assert_strings(stream.dict(), b"clear bytes");
            // Both supported dictionary access paths must agree.
            let dict = pdf
                .xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(8, 0))
                .unwrap();
            assert_strings(&dict, b"clear bytes");
            assert_eq!(stream.decoded().unwrap().as_ref(), b"abc");
        }
        // A name in an ordinary dictionary is not a cross-reference stream.
        objects[6].splice(2..2, b" /Type /XRef".iter().copied());
        let pdf = Pdf::new(fixtures::document(case, objects)).unwrap();
        let dict = pdf
            .xref()
            .get::<Dict<'_>>(ObjectIdentifier::new(7, 0))
            .unwrap();
        assert_eq!(
            dict.get::<PdfString<'_>>(b"Title").unwrap().as_ref(),
            case.title.as_bytes()
        );
    }
}

#[test]
fn crypt_real_xref_stream_and_hybrid_dictionaries_preserve_id_and_nested_strings() {
    for case in super::string_exemptions::cases() {
        for password in ["", "owner-test"] {
            let pdf = Pdf::new_with_password(case.bytes.clone(), password)
                .unwrap_or_else(|error| panic!("{}: {error:?}", case.name));
            assert!(
                !pdf.xref().is_repaired(),
                "{} unexpectedly repaired",
                case.name
            );
            let dict = pdf
                .xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(10, 0))
                .unwrap();
            assert_strings(&dict, b"clear bytes");
            let ids = dict.get::<Array<'_>>(b"ID").unwrap();
            let values: Vec<_> = ids.iter::<PdfString<'_>>().collect();
            assert_eq!(values.len(), 2);
            assert_eq!(values[0].as_ref(), case.doc_id.as_slice());
            assert_eq!(values[0], values[1]);
            let enc = dict.get::<Dict<'_>>(b"Encrypt").unwrap();
            assert_strings(&enc, b"clear bytes");
            assert_eq!(
                pdf.pages()[0].page_stream_checked().unwrap().unwrap(),
                case.content
            );
        }
    }
}
