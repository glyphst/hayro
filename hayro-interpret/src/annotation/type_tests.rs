use super::*;
use hayro_syntax::Pdf;

fn resolve(entry: &str, value: &str) -> Result<Affine, AnnotationAppearanceError> {
    let declaration = if entry.is_empty() {
        String::new()
    } else {
        format!("/Type {entry}")
    };
    let art = "1 0 0 rg 0 0 8 8 re f";
    let objects = [
        "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_owned(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Annots[4 0 R]>>".to_owned(),
        "<</Type/Annot/Subtype/Square/Rect[4 8 20 24]/AP<</N 5 0 R>>>>".to_owned(),
        format!(
            "<<{declaration}/Subtype/Form/BBox[0 0 8 8]/Length {}>>\nstream\n{art}\nendstream",
            art.len()
        ),
        value.to_owned(),
        "null".to_owned(),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    let pdf = Pdf::new(pdf).expect("authored appearance PDF");
    let annotation = pdf
        .xref()
        .get::<Dict<'_>>(hayro_syntax::object::ObjectIdentifier::new(4, 0))
        .expect("appearance annotation");
    resolve_annotation_appearance(&annotation)
        .map(|appearance| appearance.expect("appearance").transform)
}

#[test]
fn optional_appearance_type_omission_and_xobject_keep_the_exact_mapping() {
    for (entry, value) in [
        ("", "null"),
        ("/XObject", "null"),
        ("/X#4Fbject", "null"),
        ("6 0 R", "/XObject"),
        ("null", "null"),
        ("7 0 R", "null"),
        ("999 0 R", "null"),
        ("6 0 R", "null"),
    ] {
        let actual = resolve(entry, value).expect("optional Type or exact XObject name");
        assert_eq!(actual.as_coeffs(), [2.0, 0.0, 0.0, 2.0, 4.0, 8.0]);
    }
}

#[test]
fn defined_malformed_appearance_types_are_not_parser_recovery_defaults() {
    for value in [
        "false",
        "1",
        "1.0",
        "/Form",
        "/Image",
        "/XObjectjunk",
        "(XObject)",
        "[]",
        "[/XObject]",
        "<< >>",
        "garbage",
        "nulljunk",
    ] {
        for entry in [value, "6 0 R"] {
            assert!(
                matches!(
                    resolve(entry, value),
                    Err(AnnotationAppearanceError::InvalidFormType)
                ),
                "{entry}: {value}"
            );
        }
    }
    for value in ["(", "<<", "<< /Length 0 >>\nstream\n\nendstream"] {
        assert!(
            matches!(
                resolve("6 0 R", value),
                Err(AnnotationAppearanceError::InvalidFormType)
            ),
            "{value}"
        );
    }
}
