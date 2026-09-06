use super::*;
use hayro_syntax::Pdf;

fn pdf(entry: &str, properties: bool) -> Pdf {
    let catalog = if properties {
        "/OCProperties<</OCGs[4 0 R 5 0 R]/D<<>>>>"
    } else {
        ""
    };
    let objects = [
        format!("<</Type/Catalog/Pages 2 0 R{catalog}>>"),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".into(),
        format!("<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]{entry}>>"),
        "<</Type/OCG/Name(A)>>".into(),
        "<</Type/OCG/Name(B)>>".into(),
        "<</Type/OCMD/VE[/Not 6 0 R]>>".into(),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let start = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in &offsets[1..] {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{start}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    Pdf::new(bytes).unwrap()
}

fn resolve(entry: &str) -> Result<Option<Expression>, Error> {
    let pdf = pdf(entry, true);
    resolve_optional_content(
        pdf.pages()[0].raw(),
        pdf.xref(),
        &mut OptionalContentBudget::default(),
        &|| false,
    )
}

fn visible(expression: &Expression, a: bool, b: bool) -> bool {
    match expression {
        Expression::Group(group) => {
            if group.object_number == 4 {
                a
            } else {
                b
            }
        }
        Expression::All(operands) => operands.iter().all(|e| visible(e, a, b)),
        Expression::Any(operands) => operands.iter().any(|e| visible(e, a, b)),
        Expression::Not(operand) => !visible(operand, a, b),
    }
}

#[test]
fn membership_policies_and_visibility_precedence_are_exact() {
    for a in [false, true] {
        for b in [false, true] {
            for (policy, expected) in [
                ("AnyOn", a || b),
                ("AllOn", a && b),
                ("AnyOff", !a || !b),
                ("AllOff", !a && !b),
            ] {
                let expression =
                    resolve(&format!("/OC<</Type/OCMD/OCGs[4 0 R 5 0 R]/P/{policy}>>"))
                        .unwrap()
                        .unwrap();
                assert_eq!(visible(&expression, a, b), expected);
            }
            let expression = resolve("/OC<</Type/OCMD/P/Unknown/VE[/And 4 0 R [/Not 5 0 R]]>>")
                .unwrap()
                .unwrap();
            assert_eq!(visible(&expression, a, b), a && !b);
        }
    }
    assert!(matches!(
        resolve("/OC 4 0 R"),
        Ok(Some(Expression::Group(_)))
    ));
    assert!(visible(
        &resolve("/OC<</Type/OCMD/OCGs[]/P/AnyOff>>")
            .unwrap()
            .unwrap(),
        false,
        false
    ));
    assert!(resolve("").unwrap().is_none());
}

#[test]
fn malformed_membership_never_uses_a_policy_or_partial_expression() {
    for entry in [
        "/OC false",
        "/OC 99 0 R",
        "/OC<<>>",
        "/OC<</Type/OCG>>",
        "/OC<</Type/OCMD/OCGs[4 0 R true]>>",
        "/OC<</Type/OCMD/OCGs false>>",
        "/OC<</Type/OCMD/P/Unknown>>",
        "/OC<</Type/OCMD/P false>>",
        "/OC<</Type/OCMD/VE false/P/AnyOn>>",
        "/OC<</Type/OCMD/VE[/And]>>",
        "/OC<</Type/OCMD/VE[/Not 4 0 R 5 0 R]>>",
        "/OC<</Type/OCMD/VE[/Or 4 0 R false]>>",
        "/OC 6 0 R",
    ] {
        assert!(resolve(entry).is_err(), "{entry}");
    }
    let pdf = pdf("/OC false", false);
    assert!(
        resolve_optional_content(
            pdf.pages()[0].raw(),
            pdf.xref(),
            &mut OptionalContentBudget {
                nodes: 0,
                bytes: 0,
                max_depth: 0
            },
            &|| false
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn annotation_membership_limits_are_aggregate_and_cancellable() {
    let pdf = pdf("/OC<</Type/OCMD/VE[/And 4 0 R 5 0 R]>>", true);
    let resolve = |budget: &mut OptionalContentBudget, cancelled: &dyn Fn() -> bool| {
        resolve_optional_content(pdf.pages()[0].raw(), pdf.xref(), budget, cancelled)
    };
    let mut budget = OptionalContentBudget {
        nodes: 3,
        ..OptionalContentBudget::default()
    };
    assert!(resolve(&mut budget, &|| false).is_ok());
    assert_eq!(resolve(&mut budget, &|| false), Err(Error::NodeLimit));
    assert_eq!(
        resolve(
            &mut OptionalContentBudget {
                bytes: 0,
                ..OptionalContentBudget::default()
            },
            &|| false
        ),
        Err(Error::ByteLimit)
    );
    assert_eq!(
        resolve(
            &mut OptionalContentBudget {
                max_depth: 1,
                ..OptionalContentBudget::default()
            },
            &|| false
        ),
        Err(Error::DepthLimit)
    );
    let calls = std::cell::Cell::new(0);
    assert_eq!(
        resolve(&mut OptionalContentBudget::default(), &|| {
            calls.set(calls.get() + 1);
            calls.get() > 3
        }),
        Err(Error::Cancelled)
    );
}
