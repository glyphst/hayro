use super::fixtures;
use crate::Pdf;
use crate::object::{Array, Dict, FromBytes, ObjectIdentifier};

#[test]
fn lazy_unreadable_values_do_not_panic_or_disappear() {
    let mut failures = Vec::new();
    for cipher in [
        "aesv2-same-defaults",
        "r5-aes-padding-0",
        "r6-aes-padding-0",
    ] {
        let case = fixtures::CASES
            .iter()
            .find(|case| case.name == cipher)
            .unwrap();
        let mut objects: Vec<_> = case
            .objects
            .iter()
            .map(|value| fixtures::hex(value))
            .collect();
        objects[6] = b"<< /A 1 /Bad <00> /Tail 2 /Nested [1 <00> 2] >>".to_vec();
        for password in ["", "owner-test"] {
            let pdf = Pdf::new_with_password(fixtures::document(case, objects.clone()), password)
                .unwrap();
            let dict = pdf
                .xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(7, 0))
                .unwrap();
            for (name, action) in [
                ("entries", 0),
                ("array", 1),
                ("display", 2),
                ("array-debug", 3),
            ] {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match action {
                        0 => assert_eq!(dict.entries().count(), 4),
                        1 => assert_eq!(
                            dict.get::<Array<'_>>(b"Nested").unwrap().raw_iter().count(),
                            3
                        ),
                        2 => {
                            let _ = format!("{dict}");
                        }
                        _ => {
                            let _ = format!("{:?}", dict.get::<Array<'_>>(b"Nested").unwrap());
                        }
                    }));
                if result.is_err() {
                    failures.push(format!("{cipher} {password:?} {name}"));
                }
            }
        }
    }
    let dict = Dict::from_bytes(b"<< /A + /Tail 2 >>").unwrap();
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| format!("{dict:?}"))).is_err() {
        failures.push("unencrypted dictionary debug".into());
    }
    assert!(failures.is_empty(), "panicked: {failures:?}");
}

#[test]
fn lazy_errors_preserve_array_slots_dictionary_keys_and_decryption_scope() {
    use crate::object::{MaybeRef, Object, String as PdfString};
    for cipher in [
        "aesv2-same-defaults",
        "r5-aes-padding-0",
        "r6-aes-padding-0",
    ] {
        let case = fixtures::CASES
            .iter()
            .find(|case| case.name == cipher)
            .unwrap();
        let mut objects: Vec<_> = case
            .objects
            .iter()
            .map(|value| fixtures::hex(value))
            .collect();
        objects.push(b"<< /A 1 /Bad <00> /Tail 2 /Nested [1 <00> 7 0 R <00> 2] >>".to_vec());
        for password in ["", "owner-test"] {
            let pdf = Pdf::new_with_password(fixtures::document(case, objects.clone()), password)
                .unwrap();
            let dict = pdf
                .xref()
                .get::<Dict<'_>>(ObjectIdentifier::new(10, 0))
                .unwrap();
            let entries = dict.entries().collect::<Vec<_>>();
            assert_eq!(entries.len(), 4);
            assert_eq!(entries[1].0.as_ref(), b"Bad");
            let error = entries[1].1.as_ref().unwrap_err();
            assert!(dict.data()[error.offset()..].starts_with(b"<00>"));
            assert_eq!(entries[3].0.as_ref(), b"Tail");
            assert!(
                matches!(&entries[3].1, Ok(MaybeRef::NotRef(Object::Number(n))) if n.as_i64() == 2)
            );
            let array = dict.get::<Array<'_>>(b"Nested").unwrap();
            let values = array.raw_iter().collect::<Vec<_>>();
            assert_eq!(values.len(), 5);
            for index in [1, 3] {
                let error = values[index].as_ref().unwrap_err();
                assert!(array.data()[error.offset()..].starts_with(b"<00>"));
            }
            let reference = values[2].as_ref().unwrap().as_obj_ref().unwrap();
            let info = pdf.xref().get::<Dict<'_>>(reference.into()).unwrap();
            assert_eq!(
                info.get::<PdfString<'_>>(b"Title").unwrap().as_ref(),
                case.title.as_bytes()
            );
            assert!(
                matches!(&values[4], Ok(MaybeRef::NotRef(Object::Number(n))) if n.as_i64() == 2)
            );
            let display = format!("{dict}");
            assert_eq!(display.matches("<unreadable object at byte ").count(), 3);
            assert!(display.ends_with(" /Tail 2 >>"));
            assert!(!display.contains("null"));
            assert!(!display.contains("<00>"));
        }
    }
}
