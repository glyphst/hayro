//! Independent producer vectors copied from glyphst-pdf fixtures/generated.
//! `crypt_filters.py` uses Python hashlib and cryptography, never Hayro ciphers.
use crate::object::stream::LimitedStreamDecodeFailure;
use crate::object::{Dict, ObjectIdentifier, Stream, String as PdfString};
use crate::reader::{Reader, ReaderContext, ReaderExt};
use crate::{PasswordAuthentication, Pdf};

#[allow(dead_code)]
#[path = "crypt_fixtures.rs"]
mod fixtures;

#[test]
fn independent_standard_crypt_filters_decrypt_exactly_for_both_password_roles() {
    for case in fixtures::CASES.iter().filter(|case| case.valid) {
        for (password, role) in [
            ("", PasswordAuthentication::User),
            ("owner-test", PasswordAuthentication::Owner),
        ] {
            let pdf = Pdf::new_with_password(fixtures::pdf(case), password)
                .unwrap_or_else(|error| panic!("{} {role:?}: {error:?}", case.name));
            if case.encrypted {
                assert_eq!(
                    pdf.encryption_info().unwrap().authentication(),
                    role,
                    "{}",
                    case.name
                );
            } else {
                assert!(pdf.encryption_info().is_none());
            }
            for (id, expected) in [4, 6, 8].into_iter().zip(case.expected) {
                let stream = pdf
                    .xref()
                    .get::<Stream<'_>>(ObjectIdentifier::new(id, 0))
                    .unwrap();
                assert_eq!(
                    stream.decoded().unwrap().as_ref(),
                    fixtures::hex(expected),
                    "{} object {id}",
                    case.name
                );
            }
            let info = pdf
                .xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(7, 0))
                .unwrap();
            assert_eq!(
                info.get::<PdfString<'_>>(b"Title").unwrap().as_ref(),
                case.title.as_bytes(),
                "{}",
                case.name
            );
            assert_eq!(
                pdf.pages()[0].page_stream_checked().unwrap().unwrap(),
                fixtures::hex(case.expected[0]),
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn invalid_crypt_definitions_and_ciphertext_do_not_return_partial_content() {
    for case in fixtures::CASES.iter().filter(|case| !case.valid) {
        if let Ok(pdf) = Pdf::new(fixtures::pdf(case)) {
            let stream = pdf
                .xref()
                .get::<Stream<'_>>(ObjectIdentifier::new(4, 0))
                .unwrap();
            assert!(stream.decoded().is_err(), "{}", case.name);
            assert!(
                pdf.pages()[0].page_stream_checked().is_err(),
                "{}",
                case.name
            );
            if !stream.dict().contains_key(b"Filter") {
                assert!(stream.raw_data_checked().is_err(), "{}", case.name);
                assert!(stream.raw_data().is_empty(), "{}", case.name);
                assert!(
                    matches!(
                        stream.decoded_flate_with_limit(1024),
                        Err(LimitedStreamDecodeFailure::Decode)
                    ),
                    "{}",
                    case.name
                );
            }
        }
    }
}

#[test]
fn crypt_padding_errors_in_strings_never_expose_ciphertext() {
    for name in [
        "aesv2-same-defaults",
        "r5-aes-padding-0",
        "r6-aes-padding-0",
    ] {
        let case = fixtures::CASES
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let mut objects: Vec<_> = case
            .objects
            .iter()
            .map(|value| fixtures::hex(value))
            .collect();
        let title = &mut objects[6];
        let end = title.iter().position(|byte| *byte == b'>').unwrap();
        title.drain(end - 2..end); // Remove one ciphertext byte, retaining valid PDF syntax.
        let pdf = Pdf::new(fixtures::document(case, objects)).unwrap();
        let info = pdf
            .xref()
            .get::<Dict<'_>>(ObjectIdentifier::new(7, 0))
            .unwrap();
        assert!(info.contains_key(b"Title"));
        assert!(info.get::<PdfString<'_>>(b"Title").is_none(), "{name}");
        assert_eq!(
            pdf.pages()[0].page_stream_checked().unwrap().unwrap(),
            fixtures::hex(case.expected[0])
        );
    }
}

#[test]
fn xref_stream_bytes_are_exempt_from_the_default_stream_cipher() {
    for name in [
        "v2-same-defaults",
        "aesv2-same-defaults",
        "r6-aes-padding-0",
    ] {
        let case = fixtures::CASES
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let mut objects: Vec<_> = case
            .objects
            .iter()
            .map(|value| fixtures::hex(value))
            .collect();
        objects[7] = b"<< /Type /XRef /Length 3 >>\nstream\nabc\nendstream".to_vec();
        let pdf = Pdf::new(fixtures::document(case, objects)).unwrap();
        let stream = pdf
            .xref()
            .get::<Stream<'_>>(ObjectIdentifier::new(8, 0))
            .unwrap();
        assert_eq!(
            stream.raw_data_checked().unwrap().as_ref(),
            b"abc",
            "{name}"
        );
        assert_eq!(stream.decoded().unwrap().as_ref(), b"abc", "{name}");
    }
}

#[test]
fn strings_in_content_and_object_streams_are_not_decrypted_twice() {
    let case = fixtures::CASES
        .iter()
        .find(|case| case.name == "aesv2-same-defaults")
        .unwrap();
    let pdf = Pdf::new(fixtures::pdf(case)).unwrap();
    for content_stream in [false, true] {
        let mut context = ReaderContext::new(pdf.xref(), content_stream);
        context.set_obj_number(ObjectIdentifier::new(4, 0));
        context.set_in_object_stream(!content_stream);
        let value = Reader::new(b"(already decrypted)")
            .read_with_context::<PdfString<'_>>(&context)
            .unwrap();
        assert_eq!(value.as_ref(), b"already decrypted");
    }
}
