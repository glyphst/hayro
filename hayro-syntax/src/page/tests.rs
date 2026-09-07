use super::{PageStreamError, UserUnitError};
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
    pdf_with_page_tree("", contents, bad)
}

fn pdf_with_page_tree(parent: &str, contents: &str, bad: &[u8]) -> Pdf {
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Count 1 /Kids [3 0 R] {parent} >>").into_bytes(),
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
fn user_unit_scales_rotated_cropped_page_coordinates_in_both_axis_conventions() {
    for (rotation, inverted, upright, dimensions) in [
        (0, [40.0, 90.0], [40.0, 30.0], (160.0, 120.0)),
        (90, [30.0, 40.0], [30.0, 120.0], (120.0, 160.0)),
        (180, [120.0, 30.0], [120.0, 90.0], (160.0, 120.0)),
        (270, [90.0, 120.0], [90.0, 40.0], (120.0, 160.0)),
    ] {
        let pdf = pdf_with_page_tree(
            &format!("/CropBox [10 20 90 80] /Rotate {rotation} /UserUnit 4"),
            "/UserUnit 2",
            b"null",
        );
        let page = &pdf.pages()[0];
        assert_eq!(page.user_unit(), Ok(2.0));
        assert_eq!(page.base_dimensions(), (80.0, 60.0));
        assert_eq!(page.render_dimensions(), dimensions);
        for (invert_y, expected) in [(true, inverted), (false, upright)] {
            let [a, b, c, d, e, f] = page.initial_transform(invert_y).as_coeffs();
            assert_eq!([a * 30.0 + c * 35.0 + e, b * 30.0 + d * 35.0 + f], expected);
        }
    }
    // Unlike CropBox/Rotate, UserUnit is not inherited from the Pages node.
    let pdf = pdf_with_page_tree("/UserUnit 4", "", b"null");
    assert_eq!(pdf.pages()[0].user_unit(), Ok(1.0));
    assert_eq!(pdf.pages()[0].render_dimensions(), (100.0, 100.0));
}

#[test]
fn user_unit_defaults_and_supported_boundaries_are_explicit() {
    for (entry, body, expected) in [
        ("", "null", Ok(1.0)),
        ("null", "null", Ok(1.0)),
        ("99 0 R", "null", Ok(1.0)),
        ("5 1 R", "null", Ok(1.0)),
        ("5 0 R", "null", Ok(1.0)),
        ("5 0 R", "2.5", Ok(2.5)),
        ("0.5", "null", Ok(0.5)),
        ("75000", "null", Ok(75000.0)),
        ("75001", "null", Err(UserUnitError::OutOfRange)),
        ("0", "null", Err(UserUnitError::Invalid)),
        ("-1", "null", Err(UserUnitError::Invalid)),
        ("true", "null", Err(UserUnitError::Invalid)),
        ("(2)", "null", Err(UserUnitError::Invalid)),
        ("/Two", "null", Err(UserUnitError::Invalid)),
        ("[]", "null", Err(UserUnitError::Invalid)),
        ("<< >>", "null", Err(UserUnitError::Invalid)),
        ("unknown-keyword", "null", Err(UserUnitError::Invalid)),
        ("nulljunk", "null", Err(UserUnitError::Invalid)),
        ("5 0 R", "unknown-keyword", Err(UserUnitError::Invalid)),
        ("5 0 R", "5 0 R", Err(UserUnitError::Invalid)),
    ] {
        let entries = if entry.is_empty() {
            String::new()
        } else {
            format!("/UserUnit {entry}")
        };
        let pdf = pdf(&entries, body.as_bytes());
        let page = &pdf.pages()[0];
        assert_eq!(page.user_unit(), expected, "{entry}: {body}");
        let dimension = (100.0 * expected.unwrap_or(1.0)) as f32;
        assert_eq!(page.render_dimensions(), (dimension, dimension));
    }
    for entries in [
        "/UserUnit 0.000000000000000000000000000000000000000000000000001",
        "/MediaBox [0 0 10000000000000000000000000000000000000 100] /UserUnit 75000",
    ] {
        assert_eq!(
            pdf(entries, b"null").pages()[0].user_unit(),
            Err(UserUnitError::GeometryRange)
        );
    }
}

#[test]
fn small_source_boxes_are_scaled_before_pixel_dimension_rounding() {
    for (width, height, unit) in [(0.25, 0.5, 400.0), (0.00000001, 0.00000002, 75000.0)] {
        let pdf = pdf(
            &format!("/MediaBox [0 0 {width} {height}] /UserUnit {unit}"),
            b"null",
        );
        let page = &pdf.pages()[0];
        assert_eq!(
            page.render_dimensions(),
            ((width * unit) as f32, (height * unit) as f32)
        );
        assert_eq!(page.render_dimensions_f64(), (width * unit, height * unit));
    }
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
