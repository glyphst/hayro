use super::Function;
use hayro_syntax::object::{FromBytes, Object};
use smallvec::smallvec;

fn parse(source: &str) -> Option<Function> {
    Function::new(&Object::from_bytes(source.as_bytes())?)
}

const ID: &str = "<< /FunctionType 2 /Domain [0 1] /N 1 >>";

fn stitching(domain: &str, children: &str, bounds: &str, encode: &str) -> String {
    format!(
        "<< /FunctionType 3 /Domain [{domain}] /Functions [{children}] /Bounds [{bounds}] /Encode [{encode}] >>"
    )
}

#[test]
fn true_endpoints_reversed_mapping_and_singletons() {
    for (domain, bounds, encode, count, input, expected) in [
        ("0 0.0001", "", "0 1", 1, 0.0, 0.0),
        ("0 0.0001", "", "0 1", 1, 0.000025, 0.25),
        ("0 0.0001", "", "0 1", 1, 0.0001, 1.0),
        ("100000000 100000016", "", "0 1", 1, 100000016.0, 1.0),
        ("0 1", "", "1 0", 1, 0.25, 0.75),
        ("0.5 0.5", "", "0.25 0.75", 1, 0.5, 0.25),
        ("0 1", "1", "0 1 0.25 0.75", 2, 1.0, 0.25),
        ("0 1", "0.5", "0 1 0.25 0.75", 2, 0.5, 0.25),
    ] {
        let source = stitching(domain, &format!("{ID} ").repeat(count), bounds, encode);
        let function = parse(&source).unwrap();
        assert_eq!(
            function.eval(smallvec![input]),
            Some(smallvec![expected]),
            "{source}"
        );
    }
}

#[test]
fn nested_tiny_encodes_and_adjacent_boundaries_remain_distinct() {
    let child = stitching(
        "0 0.00000001",
        &format!("{ID} {ID}"),
        "0.000000005",
        "0 1 0 1",
    );
    let parent = parse(&stitching("0 1", &child, "", "0 0.00000001")).unwrap();
    assert_eq!(parent.stitching_boundaries(), [0.5]);
    let close = parse(&stitching(
        "0 1",
        &format!("{ID} {ID} {ID}"),
        "0.5 0.50000006",
        "0 1 0 1 0 1",
    ))
    .unwrap();
    assert_eq!(close.stitching_boundaries(), [0.5, 0.5_f32.next_up()]);
}

#[test]
fn function_graph_depth_and_node_limits_are_shared() {
    let mut nested = ID.to_owned();
    for _ in 0..31 {
        nested = stitching("0 1", &nested, "", "0 1");
    }
    assert!(parse(&nested).is_some());
    assert!(parse(&stitching("0 1", &nested, "", "0 1")).is_none());
    let leaf = stitching(
        "0 1",
        &format!("{ID} ").repeat(64),
        &(1..64)
            .map(|n| format!("{} ", n as f32 / 64.0))
            .collect::<String>(),
        &"0 1 ".repeat(64),
    );
    let broad = stitching(
        "0 1",
        &format!("{leaf} ").repeat(64),
        &(1..64)
            .map(|n| format!("{} ", n as f32 / 64.0))
            .collect::<String>(),
        &"0 1 ".repeat(64),
    );
    assert!(parse(&broad).is_none());
}

#[test]
fn wide_affine_coefficients_and_tiny_power_residuals() {
    for (c0, c1, n, domain, input, expected) in [
        (
            "-300000000000000000000000000000000000000",
            "300000000000000000000000000000000000000",
            "1",
            "0 1",
            0.5,
            0.0,
        ),
        (
            "300000000000000000000000000000000000000",
            "0.25",
            "1",
            "0 1",
            1.0,
            0.25,
        ),
        (
            "300000000000000000000000000000000000000",
            "0.25",
            "0",
            "0 1",
            0.0,
            0.25,
        ),
        (
            "1000000000000000000000000000000",
            "0",
            "0.000000000000000000000000000001",
            "0.5 1",
            0.5,
            std::f32::consts::LN_2,
        ),
        ("0.5", "1", "16777217", "-1 -1", -1.0, 0.0),
        ("0.5", "1", "9223372036854775807", "-1 -1", -1.0, 0.0),
        ("0", "1", "2.0", "-1 1", -0.5, 0.25),
    ] {
        let source = format!(
            "<< /FunctionType 2 /Domain [{domain}] /Range [0 1] /C0 [{c0}] /C1 [{c1}] /N {n} >>"
        );
        let actual = parse(&source).unwrap().eval(smallvec![input]).unwrap()[0];
        assert!(
            (actual - expected).abs() <= f32::EPSILON,
            "{source}: {actual} != {expected}"
        );
    }
}

#[test]
fn invalid_domain_shapes_coefficients_and_exponents_are_rejected() {
    for entries in [
        "/FunctionType 2.0 /Domain [0 1] /N 1",
        "/FunctionType 2 /Domain [0 1] /C0 [0 0] /N 1",
        "/FunctionType 2 /Domain [0 1] /C0 /Bad /N 1",
        "/FunctionType 2 /Domain [0 1] /C0 [] /N 1",
        "/FunctionType 2 /Domain [0 1 2] /N 1",
        "/FunctionType 2 /Domain [0 1 0 1] /N 1",
        "/FunctionType 2 /Domain [0 /Bad 1] /N 1",
        "/FunctionType 2 /Domain [0 1] /Range /Bad /N 1",
        "/FunctionType 2 /Domain [0 1] /Range [0 1 0 1] /N 1",
        "/FunctionType 2 /Domain [-1 1] /N 0.5",
        "/FunctionType 2 /Domain [0 1] /N -1",
        "/FunctionType 2 /Domain [-1 1] /C0 [0] /C1 [0] /N 0.5",
        "/FunctionType 2 /Domain [-1 -1] /N 9223372036854775807.0",
        "/FunctionType 2 /Domain [0 1] /C0 [1000000000000000000000000000000] /C1 [-1000000000000000000000000000000] /Range [0 1] /N 2",
    ] {
        assert!(parse(&format!("<< {entries} >>")).is_none(), "{entries}");
    }
}

#[test]
fn optional_null_and_input_arity() {
    let function =
        parse("<< /FunctionType 2 /Domain [0 1] /Range null /C0 null /C1 null /N 1 >>").unwrap();
    assert_eq!(function.eval(smallvec![0.25]), Some(smallvec![0.25]));
    for source in [ID.to_owned(), stitching("0 1", ID, "", "0 1")] {
        let function = parse(&source).unwrap();
        for input in [
            smallvec![],
            smallvec![0.0, 0.0],
            smallvec![f32::NAN],
            smallvec![f32::INFINITY],
        ] {
            assert!(function.eval(input).is_none());
        }
    }
}

#[test]
fn stitching_dictionary_shape_is_required_and_children_must_agree() {
    for (domain, children, bounds, encode) in [
        ("0 1", "".into(), "", "0 1"),
        ("0 1", "/Bad".into(), "", "0 1"),
        ("0 1 0 1", ID.into(), "", "0 1"),
        ("0 1", ID.into(), "0.5", "0 1"),
        ("0 1", ID.into(), "", "0"),
        ("0 1", ID.into(), "", "0 1 0 1"),
        ("0 1", format!("{ID} {ID}"), "0", "0 1 0 1"),
        ("0 1", format!("{ID} {ID}"), "2", "0 1 0 1"),
        ("0.5 0.5", format!("{ID} {ID}"), "0.5", "0 1 0 1"),
        ("0 1", format!("{ID} {ID} {ID}"), "0.5 0.5", "0 1 0 1 0 1"),
        (
            "0 1",
            format!("{ID} << /FunctionType 2 /Domain [0 1] /C0 [0 0] /C1 [1 1] /N 1 >>"),
            "0.5",
            "0 1 0 1",
        ),
    ] {
        let source = stitching(domain, &children, bounds, encode);
        assert!(parse(&source).is_none(), "{source}");
    }
    let valid = stitching("0 1", ID, "", "0 1");
    for token in ["/Bounds []", "/Encode [0 1]"] {
        assert!(parse(&valid.replace(token, "")).is_none());
    }
}
