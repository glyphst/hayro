use super::*;
use crate::cache::Cache;
use crate::color::ColorSpace;
use hayro_syntax::Pdf;
use hayro_syntax::object::{FromBytes, Object};

#[test]
fn ranges_require_complete_normalized_numbers_for_supported_profiles() {
    for components in [1, 3, 4] {
        let normalized = "0 1 ".repeat(components);
        for entry in [
            String::new(),
            "/Range null".into(),
            "/Range 999 0 R".into(),
            format!("/Range[{normalized}]"),
            format!("/Range[{}]", "-0.0 +1.000 ".repeat(components)),
        ] {
            let bytes = format!("<<{entry}>>");
            let dict = Dict::from_bytes(bytes.as_bytes()).unwrap();
            assert!(
                ICCProfile::validate_range(&dict, components).is_some(),
                "{entry}"
            );
        }
        for entry in [
            "/Range true".into(),
            "/Range()".into(),
            "/Range<<>>".into(),
            "/Range[]".into(),
            format!("/Range[{}]", "0 1 ".repeat(components - 1)),
            format!("/Range[{normalized} 0 1]"),
            format!("/Range[{normalized} null]"),
            format!("/Range[{}null 1]", "0 1 ".repeat(components - 1)),
            format!("/Range[{}999 0 R 1]", "0 1 ".repeat(components - 1)),
            format!("/Range[{}(bad) 1]", "0 1 ".repeat(components - 1)),
            format!("/Range[{}1 0]", "0 1 ".repeat(components - 1)),
            format!("/Range[{}0 1.0000000001]", "0 1 ".repeat(components - 1)),
            format!("/Range[{}-.0000000001 1]", "0 1 ".repeat(components - 1)),
            format!(
                "/Range[{}0 {}]",
                "0 1 ".repeat(components - 1),
                "9".repeat(400)
            ),
        ] {
            let bytes = format!("<<{entry}>>");
            let dict = Dict::from_bytes(bytes.as_bytes()).unwrap();
            assert!(
                ICCProfile::validate_range(&dict, components).is_none(),
                "{entry}"
            );
        }
    }
    let dict = Dict::from_bytes(b"<<>>").unwrap();
    for components in [0, 2, 5, usize::MAX] {
        assert!(ICCProfile::validate_range(&dict, components).is_none());
    }
}

fn document(range: &str, tail: &str, indirect: &str) -> Pdf {
    let mut profile = ColorProfile::new_srgb();
    profile.cicp = None;
    let profile = profile.encode().unwrap();
    let mut bytes = format!(
        "%PDF-1.7\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
         2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
         3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 10 10]\
         /Resources<</ColorSpace<</C[/ICCBased 4 0 R {tail}]>>>>>>endobj\n\
         4 0 obj<</N 3 {range}/Length {}>>stream\n",
        profile.len()
    )
    .into_bytes();
    bytes.extend(profile);
    bytes.extend(format!("\nendstream\nendobj\n{indirect}\n").as_bytes());
    // Keep defined unreadable objects in a real xref. Repair scanning may
    // omit such objects, turning this into a different, absent-value case.
    let offsets: Vec<_> = (1..=6)
        .map(|id| {
            let marker = format!("\n{id} 0 obj");
            bytes
                .windows(marker.len())
                .position(|w| w == marker.as_bytes())
                .map(|at| at + 1)
        })
        .collect();
    let xref = bytes.len();
    bytes.extend(b"xref\n0 7\n0000000000 65535 f \n");
    for offset in offsets {
        match offset {
            Some(offset) => bytes.extend(format!("{offset:010} 00000 n \n").as_bytes()),
            None => bytes.extend(b"0000000000 00000 f \n"),
        }
    }
    bytes.extend(format!("trailer<</Size 7/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF").as_bytes());
    Pdf::new(bytes).unwrap()
}

fn resolve(pdf: &Pdf, cache: &Cache) -> Option<ColorSpace> {
    let object = pdf.pages()[0]
        .resources()
        .color_spaces
        .get::<Object<'_>>(b"C")?;
    ColorSpace::new(object, cache)
}

#[test]
fn range_references_and_array_tails_are_validated_before_cached_profile_reuse() {
    let valid = document("/Range 5 0 R", "", "5 0 obj[0 1 0 1 0 1]endobj");
    let invalid = document("/Range 5 0 R", "", "5 0 obj[0 1 0 1 0 2]endobj");
    // Identical profile bytes and raw stream dictionaries cannot reuse an
    // admission decision that depends on the resolved Range object.
    for order in [[&valid, &invalid], [&invalid, &valid]] {
        let cache = Cache::new();
        for _ in 0..3 {
            for pdf in order {
                assert_eq!(resolve(pdf, &cache).is_some(), std::ptr::eq(pdf, &valid));
            }
        }
        assert!(cache.stats().hits > 0);
    }
    for (range, extra) in [
        ("/Range 5 0 R", "5 0 obj null endobj"),
        (
            "/Range[5 0 R 6 0 R 0 1 0 1]",
            "5 0 obj 0 endobj\n6 0 obj 1 endobj",
        ),
    ] {
        assert!(resolve(&document(range, "", extra), &Cache::new()).is_some());
    }
    for (range, tail, extra) in [
        ("", "null", ""),
        ("", "999 0 R", ""),
        ("/Range 5 0 R", "", "5 0 obj(bad)endobj"),
        ("/Range 5 0 R", "", "5 0 obj<zz>endobj"),
        ("/Range[5 0 R 1 0 1 0 1]", "", "5 0 obj null endobj"),
    ] {
        assert!(
            resolve(&document(range, tail, extra), &Cache::new()).is_none(),
            "range={range:?}, tail={tail:?}, extra={extra:?}"
        );
    }
}
