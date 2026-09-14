use super::tint_tests::{function_space, space};
use super::{Color, ColorSpaceKind};

#[test]
fn native_alternates_keep_real_values_and_original_identity() {
    for (alternate, kind, start, end, expected) in [
        (
            "DeviceGray",
            ColorSpaceKind::DeviceGray,
            "0",
            "1",
            vec![0.5001],
        ),
        (
            "DeviceRGB",
            ColorSpaceKind::DeviceRgb,
            "0 .25 .125",
            "1 .25 .125",
            vec![0.5001, 0.25, 0.125],
        ),
        (
            "DeviceCMYK",
            ColorSpaceKind::DeviceCmyk,
            "0 .25 .125 .5",
            "1 .25 .125 .5",
            vec![0.5001, 0.25, 0.125, 0.5],
        ),
    ] {
        for (prefix, original) in [
            ("Separation /Spot", ColorSpaceKind::Separation),
            ("DeviceN [/Spot]", ColorSpaceKind::DeviceN),
        ] {
            let definition = format!(
                "[/{prefix} /{alternate} <</FunctionType 2/Domain[0 1]/C0[{start}]/C1[{end}]/N 1>>]"
            );
            let named = space(&definition);
            assert_eq!(named.kind(), original);
            assert_eq!(named.device_alternate_kind(), Some(kind));
            assert_eq!(
                named
                    .device_alternate_components(&[0.5001])
                    .unwrap()
                    .as_slice(),
                expected
            );
            let color = Color::new(named.clone(), smallvec::smallvec![0.5001], 0.25);
            assert_eq!(color.color_space_kind(), original);
            assert_eq!(color.opacity(), 0.25);
            assert_eq!(color.device_alternate_kind(), Some(kind));
            assert_eq!(
                color.device_alternate_components().unwrap().as_slice(),
                expected
            );
            let pattern = space(&format!("[/Pattern {definition}]"));
            assert_eq!(pattern.device_alternate_kind(), Some(kind));
            assert_eq!(
                pattern
                    .device_alternate_components(&[0.5001])
                    .unwrap()
                    .as_slice(),
                expected
            );
            for invalid in [vec![], vec![0.5, 0.5], vec![f32::NAN], vec![f32::INFINITY]] {
                assert!(named.device_alternate_components(&invalid).is_none());
            }
        }
    }
}

#[test]
fn native_tints_clip_inputs_outputs_and_preserve_index_boundaries() {
    let definition = "[/Separation /Spot /DeviceCMYK <</FunctionType 2/Domain[0 1]/C0[-1 2 .125 .5]/C1[2 -1 .125 .5]/N 1>>]";
    let named = space(definition);
    for (input, expected) in [
        (-f64::MAX, [0.0, 1.0, 0.125, 0.5]),
        (f64::MAX, [1.0, 0.0, 0.125, 0.5]),
    ] {
        assert_eq!(
            named
                .image_device_alternate_components(&[input])
                .unwrap()
                .as_slice(),
            expected
        );
    }
    let palette = space(&format!("[/Indexed {definition} 1 <00ff>]"));
    for (index, expected) in [
        (0.5_f64.next_down(), [0.0, 1.0, 0.125, 0.5]),
        (0.5, [1.0, 0.0, 0.125, 0.5]),
    ] {
        assert_eq!(
            palette
                .image_device_alternate_components(&[index])
                .unwrap()
                .as_slice(),
            expected
        );
    }
}

#[test]
fn multiple_components_include_mixed_none_and_function_failures() {
    for names in ["/A /B", "/None /Spot"] {
        let named = function_space(
            &format!("DeviceN [{names}]"),
            "/DeviceCMYK",
            "/FunctionType 4/Domain[0 1 0 1]/Range[0 1 0 1 0 1 0 1]",
            b"{ exch .5 mul exch .25 mul .125 .5 }",
        );
        assert_eq!(
            named
                .device_alternate_components(&[0.5001, 0.75])
                .unwrap()
                .as_slice(),
            [0.25005, 0.1875, 0.125, 0.5]
        );
        assert!(named.device_alternate_components(&[0.5]).is_none());
    }
    let named = function_space(
        "Separation /Spot",
        "/DeviceGray",
        "/FunctionType 4/Domain[0 1]/Range[0 1]",
        b"{ dup .5 gt { pop 0 0 div } if }",
    );
    assert!(named.device_alternate_components(&[0.75]).is_none());
    assert_eq!(
        named
            .device_alternate_components(&[0.25])
            .unwrap()
            .as_slice(),
        [0.25]
    );
}

#[test]
fn special_colorants_and_nondevice_alternates_do_not_claim_native_identity() {
    let tint = "<</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>";
    for definition in [
        "/DeviceGray".to_owned(),
        "[/Indexed/DeviceGray 1<00ff>]".to_owned(),
        format!("[/Separation /All /DeviceGray {tint}]"),
        format!("[/Separation /None /DeviceGray {tint}]"),
        format!("[/DeviceN [/None] /DeviceGray {tint}]"),
        format!("[/Separation /Spot [/CalGray<</WhitePoint[1 1 1]>>] {tint}]"),
        format!("[/Indexed [/Separation /None /DeviceGray {tint}] 1<00ff>]"),
    ] {
        let named = space(&definition);
        assert_eq!(named.device_alternate_kind(), None, "{definition}");
        assert!(named.device_alternate_components(&[0.5]).is_none());
    }
}
