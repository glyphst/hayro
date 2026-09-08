use super::{Function, TransferFunction, selected_transfer_function};
use crate::color::AlphaColor;
use crate::{ActiveTransferFunction, CacheKey};
use hayro_syntax::object::{Dict, FromBytes, Object};

const INVERT: &str = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>";

fn parse(source: &str) -> Option<TransferFunction> {
    TransferFunction::new(Function::new(&Object::from_bytes(source.as_bytes())?)?)
}

#[test]
fn null_is_absent_and_non_null_tr2_wins() {
    for (source, key, value) in [
        (
            "<< /TR /Identity /TR2 null >>",
            b"TR".as_slice(),
            b"Identity".as_slice(),
        ),
        ("<< /TR /Bogus /TR2 /Default >>", b"TR2", b"Default"),
        ("<< /TR null /TR2 /Identity >>", b"TR2", b"Identity"),
    ] {
        let dict = Dict::from_bytes(source.as_bytes()).unwrap();
        let (selected, Object::Name(name)) = selected_transfer_function(&dict).unwrap() else {
            panic!("name")
        };
        assert_eq!(selected, key);
        assert_eq!(name.as_ref(), value);
    }
    let dict = Dict::from_bytes(b"<< /TR null /TR2 null >>").unwrap();
    assert!(selected_transfer_function(&dict).is_none());
}

#[test]
fn real_inputs_preserve_adjacent_stitching_sides() {
    let source = "<< /FunctionType 3 /Domain [0 1] /Range [0 1] /Functions [<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >> << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [1] /N 1 >>] /Bounds [0.5] /Encode [0 1 0 1] >>";
    let function = parse(source).unwrap();
    for (input, expected) in [
        (0.5_f32.next_down(), 0.0),
        (0.5, 1.0),
        (0.5_f32.next_up(), 1.0),
    ] {
        assert_eq!(function.apply_f32(input), Some(expected));
    }
    let square = parse("<< /FunctionType 2 /Domain [0 1] /N 2 >>").unwrap();
    assert_eq!(square.apply_f32(0.25), Some(0.0625));
    assert_eq!(square.apply_f32(f32::NAN), None);
}

#[test]
fn byte_table_uses_actual_byte_inputs_and_validates_outputs() {
    let function = parse(INVERT).unwrap();
    let mut bytes = std::array::from_fn::<_, 256, _>(|index| index as u8);
    function.apply_to(&mut bytes);
    assert_eq!(bytes, std::array::from_fn(|index| 255 - index as u8));
    for source in [
        "<< /FunctionType 2 /Domain [0 1] /C0 [0 0] /C1 [1 1] /N 1 >>",
        "<< /FunctionType 2 /Domain [0 1] /C0 [2] /C1 [2] /N 1 >>",
    ] {
        assert!(parse(source).is_none());
    }
}

#[test]
fn channel_array_keeps_order_identity_and_alpha() {
    let source = format!("[/Identity {INVERT} /Identity {INVERT}]");
    let object = Object::from_bytes(source.as_bytes()).unwrap();
    let function = ActiveTransferFunction::from_object(&object, false)
        .unwrap()
        .unwrap();
    let color = function
        .apply(&AlphaColor::new([0.25, 0.5, 0.75, 0.125]))
        .unwrap();
    assert_eq!(color.components(), [0.25, 0.5, 0.75, 0.125]);
    let mut bytes = [64, 64, 64, 0, 255, 128];
    function.apply_to(&mut bytes);
    assert_eq!(bytes, [64, 191, 64, 0, 0, 128]);
}

#[test]
fn malformed_names_shapes_and_unused_fourth_channels_fail() {
    for source in [
        "42",
        "false",
        "(Identity)",
        "/Bogus",
        "/Default",
        "[]",
        "[/Identity /Identity /Identity]",
        "[/Identity /Identity /Identity /Identity /Identity]",
        "[/Identity /Identity /Identity null]",
        "[/Identity /Identity /Identity /Default]",
    ] {
        let object = Object::from_bytes(source.as_bytes()).unwrap();
        assert!(
            ActiveTransferFunction::from_object(&object, false).is_err(),
            "{source}"
        );
    }
    let object = Object::from_bytes(b"/Default").unwrap();
    assert!(
        ActiveTransferFunction::from_object(&object, true)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cache_identity_includes_full_function_and_channel_order() {
    let first = parse(INVERT).unwrap();
    assert_eq!(first.cache_key(), first.clone().cache_key());
    assert_eq!(first.cache_key(), parse(INVERT).unwrap().cache_key());
    assert_ne!(first.cache_key(), TransferFunction::identity().cache_key());
    let key = |source: &str| {
        ActiveTransferFunction::from_object(&Object::from_bytes(source.as_bytes()).unwrap(), false)
            .unwrap()
            .unwrap()
            .cache_key()
    };
    assert_ne!(
        key(&format!("[{INVERT} /Identity /Identity /Identity]")),
        key(&format!("[/Identity {INVERT} /Identity /Identity]"))
    );
}
