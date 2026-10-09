use super::*;
use hayro_syntax::Pdf;
use hayro_syntax::reader::{Reader, ReaderExt};

const FIELDS: [&[u8]; 10] = [
    MCID,
    ACTUAL_TEXT,
    ALT,
    LANG,
    TYPE,
    SUBTYPE,
    NAME,
    O,
    BBOX,
    METADATA,
];

fn properties(tag: &[u8], entries: &str, value: &str, named: bool) -> MarkedContentProperties {
    let dictionary = format!("<< {entries} /PrivateNull null /PrivateInteger 9007199254740993 >>");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 32 32] >>".to_owned(),
        dictionary.clone(),
        value.to_owned(),
        "null".to_owned(),
        "<< /Type /Metadata /Subtype /XML /Length 4 >>\nstream\n<x/>\nendstream".to_owned(),
    ];
    let pdf = authored_pdf(&objects);
    let dict = if named {
        pdf.xref()
            .get::<Dict<'_>>(ObjectIdentifier::new(4, 0))
            .expect("named dictionary")
    } else {
        Reader::new(dictionary.as_bytes())
            .read_without_context::<Dict<'_>>()
            .expect("inline dictionary")
    };
    marked_content_properties(&dict, tag, named, 1 << 20, 1 << 20, 32)
}

fn authored_pdf(objects: &[String]) -> Pdf {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    Pdf::new(bytes).expect("authored marked-property PDF")
}

#[test]
fn property_resource_bindings_retain_parent_ownership_and_declared_local_shadowing() {
    let name = Name::new(b"P").expect("property name");
    for value in ["null", "false", "[]", "garbage", "nulljunk", "(", "<<"] {
        let pdf = authored_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 32 32] /Resources << /Properties << /P 4 0 R >> >> >>".to_owned(),
            "<< /MCID 41 >>".to_owned(),
            "<< /Properties << /P 6 0 R >> >>".to_owned(),
            "<< /MCID 42 >>".to_owned(),
            "<< /Properties << /P 8 0 R >> >>".to_owned(),
            value.to_owned(),
            "<< /Properties << /P 999 0 R >> >>".to_owned(),
            "<< /Properties << /P null >> >>".to_owned(),
        ]);
        let parent = pdf.pages()[0].resources().clone();
        for resources in [
            Dict::empty(),
            pdf.xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(9, 0))
                .unwrap(),
            pdf.xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(10, 0))
                .unwrap(),
        ] {
            let inherited = Resources::from_parent(resources, parent.clone());
            let (object, owner) = inherited
                .get_property_binding(&name)
                .expect("inherited binding");
            let Some(Object::Dict(dict)) = object else {
                panic!("inherited dictionary")
            };
            assert_eq!(dict.get::<i32>(MCID), Some(41));
            assert_eq!(
                owner.properties.get_ref(b"P").map(ObjectIdentifier::from),
                Some(ObjectIdentifier::new(4, 0))
            );
        }
        let local = Resources::from_parent(
            pdf.xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(5, 0))
                .unwrap(),
            parent.clone(),
        );
        let (object, owner) = local.get_property_binding(&name).expect("local binding");
        let Some(Object::Dict(dict)) = object else {
            panic!("local dictionary")
        };
        assert_eq!(dict.get::<i32>(MCID), Some(42));
        assert_eq!(
            owner.properties.get_ref(b"P").map(ObjectIdentifier::from),
            Some(ObjectIdentifier::new(6, 0))
        );
        let malformed = Resources::from_parent(
            pdf.xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(7, 0))
                .unwrap(),
            parent,
        );
        let (object, owner) = malformed
            .get_property_binding(&name)
            .expect("declared local binding");
        if value == "null" {
            let Some(Object::Dict(dict)) = object else {
                panic!("actual-null binding inherits")
            };
            assert_eq!(dict.get::<i32>(MCID), Some(41));
            assert_eq!(
                owner.properties.get_ref(b"P").map(ObjectIdentifier::from),
                Some(ObjectIdentifier::new(4, 0))
            );
            continue;
        }
        assert!(!matches!(object, Some(Object::Dict(_))), "{value}");
        assert_eq!(
            owner.properties.get_ref(b"P").map(ObjectIdentifier::from),
            Some(ObjectIdentifier::new(8, 0))
        );
        assert_eq!(object.is_none(), value == "(" || value == "<<");
    }
}

fn declaration(key: &[u8], entry: &str) -> String {
    if entry.is_empty() {
        String::new()
    } else {
        format!("/{} {entry}", std::str::from_utf8(key).unwrap())
    }
}

#[test]
fn known_marked_property_null_and_undefined_declarations_are_omitted() {
    for key in FIELDS {
        for entry in ["", "null", "6 0 R", "999 0 R", "5 0 R"] {
            let actual = properties(b"Span", &declaration(key, entry), "null", true);
            assert!(actual.unavailable_keys.is_empty(), "{key:?}/{entry}");
            assert!(
                actual
                    .additional_properties
                    .iter()
                    .any(|item| item.key == b"PrivateNull"
                        && item.value == MarkedContentPropertyValue::Null)
            );
            assert!(
                actual
                    .additional_properties
                    .iter()
                    .any(|item| item.key == b"PrivateInteger"
                        && item.value == MarkedContentPropertyValue::Integer(9007199254740993))
            );
        }
        for entry in ["", "null"] {
            assert!(
                properties(b"Span", &declaration(key, entry), "null", false)
                    .unavailable_keys
                    .is_empty()
            );
        }
    }
}

#[test]
fn marked_content_mcid_retains_exact_signed_integers_and_refuses_reals() {
    for value in ["0", "7", "2147483647", "-2147483648"] {
        for entry in [value, "5 0 R"] {
            let actual = properties(b"Span", &declaration(MCID, entry), value, true);
            assert_eq!(actual.mcid, Some(value.parse().unwrap()));
            assert!(actual.unavailable_keys.is_empty());
        }
    }
    for value in [
        "0.0",
        "7.0",
        "7.5",
        "-.5",
        "2147483648",
        "-2147483649",
        "garbage",
        "nulljunk",
        "false",
        "[]",
    ] {
        for entry in [value, "5 0 R"] {
            let actual = properties(b"Span", &declaration(MCID, entry), value, true);
            assert_eq!(actual.mcid, None, "{entry}/{value}");
            assert_eq!(
                actual.unavailable_keys,
                vec![MCID.to_vec()],
                "{entry}/{value}"
            );
        }
    }
}

#[test]
fn background_artifacts_require_bbox_without_affecting_other_tags_or_types() {
    for entry in ["", "null", "6 0 R", "999 0 R", "5 0 R"] {
        let entries = format!("/Type /Background {}", declaration(BBOX, entry));
        assert_eq!(
            properties(b"Artifact", &entries, "null", true).unavailable_keys,
            vec![BBOX.to_vec()]
        );
    }
    for tag in [b"Artifact".as_slice(), b"Span".as_slice()] {
        for entry in ["[1 2 3 4]", "5 0 R"] {
            let actual = properties(
                tag,
                &format!("/Type /Background /BBox {entry}"),
                "[1 2 3 4]",
                true,
            );
            assert_eq!(actual.bounding_box, Some([1.0, 2.0, 3.0, 4.0]));
            assert!(actual.unavailable_keys.is_empty());
        }
    }
    assert!(
        properties(b"Span", "/Type /Background", "null", true)
            .unavailable_keys
            .is_empty()
    );
    for kind in ["Pagination", "Layout", "Page"] {
        assert!(
            properties(b"Artifact", &format!("/Type /{kind}"), "null", true)
                .unavailable_keys
                .is_empty()
        );
    }
    assert_eq!(
        properties(b"Artifact", "/Type /Background", "null", false).unavailable_keys,
        vec![BBOX.to_vec()]
    );
}

#[test]
fn marked_property_recovery_unreadable_and_inline_references_stay_unavailable() {
    for key in FIELDS {
        for value in ["false", "[]", "garbage", "nulljunk"] {
            for entry in [value, "5 0 R"] {
                assert_eq!(
                    properties(b"Span", &declaration(key, entry), value, true).unavailable_keys,
                    vec![key.to_vec()],
                    "{key:?}/{entry}/{value}"
                );
            }
        }
        for value in ["(", "<<", "<< /Length 0 >>\nstream\n\nendstream"] {
            assert_eq!(
                properties(b"Span", &declaration(key, "5 0 R"), value, true).unavailable_keys,
                vec![key.to_vec()],
                "{key:?}/{value}"
            );
        }
        for entry in ["5 0 R", "6 0 R", "999 0 R"] {
            assert_eq!(
                properties(b"Span", &declaration(key, entry), "null", false).unavailable_keys,
                vec![key.to_vec()],
                "inline/{key:?}/{entry}"
            );
        }
    }
    let actual = properties(b"Span", "/Metadata 7 0 R", "null", true);
    let metadata = actual.metadata.expect("owned XML metadata");
    assert_eq!(metadata.subtype, b"XML");
    assert_eq!(metadata.data, b"<x/>");
    assert!(actual.unavailable_keys.is_empty());
}
