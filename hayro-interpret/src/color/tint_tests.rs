use super::{ColorSpace, ToRgb};
use hayro_syntax::Pdf;
use hayro_syntax::object::{FromBytes, Object, ObjectIdentifier};

pub(super) fn space(definition: &str) -> ColorSpace {
    ColorSpace::from_pdf_object(Object::from_bytes(definition.as_bytes()).unwrap()).unwrap()
}

pub(super) fn function_space(
    prefix: &str,
    alternate: &str,
    entries: &str,
    data: &[u8],
) -> ColorSpace {
    let mut bytes = format!(
        "%PDF-1.7\n1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
         2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
         3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Resources<<>>>> endobj\n\
         4 0 obj [/{prefix} {alternate} 5 0 R] endobj\n\
         5 0 obj <<{entries}/Length {}>> stream\n",
        data.len()
    )
    .into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream\nendobj\ntrailer <</Root 1 0 R>>\n%%EOF");
    let pdf = Pdf::new(bytes).unwrap();
    let object = pdf
        .xref()
        .get::<Object<'_>>(ObjectIdentifier::new(4, 0))
        .unwrap();
    ColorSpace::from_pdf_object(object).unwrap()
}

#[test]
fn real_tints_reach_functions_before_byte_quantization() {
    let ramp = "<</FunctionType 2/Domain [0 1]/Range [0 1]/C0 [-249.5]/C1 [250.5]/N 1>>";
    for prefix in ["Separation /Spot", "DeviceN [/Spot]"] {
        let color = space(&format!("[/{prefix} /DeviceGray {ramp}]"));
        let pattern = space(&format!("[/Pattern [/{prefix} /DeviceGray {ramp}]]"));
        assert_eq!(
            pattern.to_rgba(&[0.5], 0.75).components(),
            [0.5, 0.5, 0.5, 0.75]
        );
        let mut output = [0; 3];
        color.convert_values(&[0.5], &mut output).unwrap();
        assert_eq!(output, [128; 3]);
        for (tint, expected) in [(0.499, 0.0), (0.5, 0.5), (0.5001, 0.55), (0.502, 1.0)] {
            let actual = color.to_rgba(&[tint], 0.75).components();
            assert!(
                (actual[0] - expected).abs() < 0.0001,
                "{prefix} {tint}: {actual:?}"
            );
            assert_eq!(actual[0], actual[1]);
            assert_eq!(actual[1], actual[2]);
            assert_eq!(actual[3], 0.75);
        }
    }
}

#[test]
fn multiple_tints_and_mixed_none_keep_real_component_order() {
    for names in ["/A /B", "/None /Spot"] {
        let color = function_space(
            &format!("DeviceN [{names}]"),
            "/DeviceRGB",
            "/FunctionType 4/Domain [0 1 0 1]/Range [0 1 0 1 0 1]",
            b"{ exch .501 lt { 0 } { 1 } ifelse exch .501 lt { 0 } { 1 } ifelse 0.25 }",
        );
        for (input, expected) in [
            ([0.5, 0.502], [0.0, 1.0, 0.25, 1.0]),
            ([0.502, 0.5], [1.0, 0.0, 0.25, 1.0]),
            ([0.5, 0.5], [0.0, 0.0, 0.25, 1.0]),
        ] {
            assert_eq!(color.to_rgba(&input, 1.0).components(), expected);
        }
    }
}

#[test]
fn tint_outputs_reach_calibrated_alternates_before_bytes() {
    let alternate = "[/CalGray <</WhitePoint [.9505 1 1.089]/Gamma 0.1>>]";
    let direct = space(alternate);
    for prefix in ["Separation /Spot", "DeviceN [/Spot]"] {
        let color = space(&format!(
            "[/{prefix} {alternate} <</FunctionType 2/Domain[0 1]/C0[.001]/C1[.001]/N 1>>]"
        ));
        let expected = direct.to_rgba(&[0.001], 1.0).to_rgba8();
        assert_eq!(color.to_rgba(&[0.5], 1.0).to_rgba8(), expected);
        let mut bytes = [0; 3];
        color.convert(&[128], &mut bytes).unwrap();
        assert_eq!(bytes, expected[..3]);
        let palette = space(&format!(
            "[/Indexed [/{prefix} {alternate} <</FunctionType 2/Domain[0 1]/C0[.001]/C1[.001]/N 1>>] 1 <00ff>]"
        ));
        assert_eq!(palette.to_rgba(&[1.0], 1.0).to_rgba8(), expected);
    }
}

#[test]
fn sampled_and_stitched_tints_preserve_sub_byte_inputs() {
    for prefix in ["Separation /Spot", "DeviceN [/Spot]"] {
        let sampled = function_space(
            prefix,
            "/DeviceGray",
            "/FunctionType 0/Domain [.499 .501]/Range [0 1]/Size [2]/BitsPerSample 8",
            &[0, 255],
        );
        assert!((sampled.to_rgba(&[0.5], 1.0).components()[0] - 0.5).abs() < 0.0001);
        let stitched = space(&format!(
            "[/{prefix} /DeviceGray <</FunctionType 3/Domain[0 1]/Range[0 1]/Functions[<</FunctionType 2/Domain[0 1]/C0[0]/C1[0]/N 1>><</FunctionType 2/Domain[0 1]/C0[1]/C1[1]/N 1>>]/Bounds[.501]/Encode[0 1 0 1]>>]"
        ));
        assert_eq!(stitched.to_rgba(&[0.5], 1.0).to_rgba8(), [0, 0, 0, 255]);
        assert_eq!(stitched.to_rgba(&[0.502], 1.0).to_rgba8(), [255; 4]);
    }
}

#[test]
fn native_image_tints_clip_before_function_input_and_keep_palette_rounding() {
    let identity = "<</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>";
    for prefix in ["Separation /Spot", "DeviceN [/Spot]"] {
        let color = space(&format!("[/{prefix} /DeviceGray {identity}]"));
        assert_eq!(color.image_rgb_f64(&[f64::MAX]), Some([1.0; 3]));
        assert_eq!(color.image_rgb_f64(&[-f64::MAX]), Some([0.0; 3]));
        assert!(color.image_rgb_f64(&[f64::NAN]).is_none());
        let palette = space(&format!(
            "[/Indexed [/{prefix} /DeviceGray {identity}] 1 <00ff>]"
        ));
        assert_eq!(
            palette.image_rgb_f64(&[0.5_f64.next_down()]),
            Some([0.0; 3])
        );
        assert_eq!(palette.image_rgb_f64(&[0.5]), Some([1.0; 3]));
    }
}
