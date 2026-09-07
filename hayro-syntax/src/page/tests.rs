use super::PageStreamError;
use crate::Pdf;
use crate::object::{Dict, Name, Null, Object, ObjectIdentifier};

#[test]
fn typed_keywords_reject_prefixes_without_losing_following_dictionary_fields() {
    let pdf = pdf("", b"<< /A falsejunk /B true /C nulljunk /D /Keep >>");
    let dict = pdf
        .xref()
        .get::<Dict<'_>>(ObjectIdentifier::new(5, 0))
        .unwrap();
    assert!(dict.get::<bool>(b"A").is_none());
    assert!(dict.get::<Null>(b"C").is_none());
    assert_eq!(dict.get::<bool>(b"B"), Some(true));
    assert_eq!(dict.get::<Name<'_>>(b"D").unwrap().as_ref(), b"Keep");
    // Generic reads preserve their original prefix recovery, including the
    // boolean's type. Generic null recovery is not proof of typed absence.
    assert!(matches!(
        dict.get::<Object<'_>>(b"A"),
        Some(Object::Boolean(false))
    ));
    assert!(matches!(
        dict.get::<Object<'_>>(b"C"),
        Some(Object::Null(_))
    ));
    assert_eq!(dict.entries().count(), 4);
}

#[test]
fn xref_definition_distinguishes_undefined_null_and_unreadable_objects() {
    for body in [&b"null"[..], &b"invalid-object-body"[..]] {
        let pdf = pdf("", body);
        let xref = pdf.xref();
        assert!(xref.contains_object(ObjectIdentifier::new(5, 0)));
        assert!(!xref.contains_object(ObjectIdentifier::new(5, 1)));
        assert!(!xref.contains_object(ObjectIdentifier::new(99, 0)));
        assert!(!xref.contains_object(ObjectIdentifier::new(0, 65535)));
        // Use the strict typed read: the generic Object reader can repair an
        // unknown bare keyword to null, which must not prove optional absence.
        let value = xref.get::<Null>(ObjectIdentifier::new(5, 0));
        if body == b"null" {
            assert!(value.is_some());
        } else {
            assert!(value.is_none());
        }
    }
}

fn pdf(contents: &str, bad: &[u8]) -> Pdf {
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] {contents} >>").into_bytes(),
        b"<< /Length 1 >> stream\nq\nendstream".to_vec(),
        bad.to_vec(),
        b"<< /Length 1 >> stream\nQ\nendstream".to_vec(),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF").as_bytes(),
    );
    Pdf::new(bytes).unwrap()
}

#[test]
fn failed_members_invalidate_the_cached_page_stream() {
    for contents in [
        "/Contents 5 0 R",
        "/Contents [5 0 R 4 0 R 6 0 R]",
        "/Contents [4 0 R 5 0 R 6 0 R]",
        "/Contents [4 0 R 6 0 R 5 0 R]",
    ] {
        for bad in [
            &b"<< /Length 1 /Filter /FlateDecode >> stream\n\x07\nendstream"[..],
            &b"<< /Length 1 /Filter /Unknown >> stream\nq\nendstream"[..],
        ] {
            let pdf = pdf(contents, bad);
            let page = &pdf.pages()[0];
            for _ in 0..2 {
                assert!(matches!(
                    page.page_stream_checked(),
                    Err(PageStreamError::Decode(_))
                ));
                assert!(page.page_stream().is_none());
                assert!(page.typed_operations().next().is_none());
            }
        }
    }
}

#[test]
fn absent_contents_and_whitespace_joining_remain_valid() {
    for contents in ["", "/Contents null"] {
        assert!(
            pdf(contents, b"null").pages()[0]
                .page_stream_checked()
                .unwrap()
                .is_none()
        );
    }
    let pdf = pdf("/Contents [4 0 R 6 0 R]", b"null");
    assert_eq!(
        pdf.pages()[0].page_stream_checked().unwrap(),
        Some(&b"q Q "[..])
    );
}

#[test]
fn external_and_unresolved_members_have_explicit_errors() {
    let pdf = pdf(
        "/Contents [4 0 R 5 0 R 6 0 R]",
        b"<< /Length 1 /F (never-fetched) >> stream\nq\nendstream",
    );
    assert!(matches!(
        pdf.pages()[0].page_stream_checked(),
        Err(PageStreamError::ExternalStream)
    ));
    let pdf = self::pdf("/Contents [4 0 R 99 0 R 6 0 R]", b"null");
    assert!(matches!(
        pdf.pages()[0].page_stream_checked(),
        Err(PageStreamError::InvalidContents)
    ));
}
