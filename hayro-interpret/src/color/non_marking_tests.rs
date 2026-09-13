use super::*;
use crate::{CacheKey, Paint};
use hayro_syntax::object::FromBytes;

fn space(source: &str) -> Option<ColorSpace> {
    ColorSpace::from_pdf_object(Object::from_bytes(source.as_bytes())?)
}

const TINT: &str = "<< /FunctionType 2 /Domain [0 1] /C0 [.1 .2 .8] /C1 [.1 .2 .8] /N 1 >>";

#[test]
fn non_marking_identity_survives_conversion_intent_and_cache_keys() {
    for source in [
        format!("[/Separation /None /DeviceRGB {TINT}]"),
        format!("[/Separation /N#6fne /DeviceRGB {TINT}]"),
        format!("[/DeviceN [/None] /DeviceRGB {TINT}]"),
        format!("[/Indexed [/Separation /None /DeviceRGB {TINT}] 1 <00ff>]"),
        format!("[/Separation /None [/CalRGB << /WhitePoint [.9505 1 1.089] >>] {TINT}]"),
    ] {
        let color_space = space(&source).expect("non-marking space");
        assert!(color_space.is_non_marking(), "{source}");
        let color_space = color_space
            .with_rendering_intent(RenderingIntent::Saturation)
            .expect("ignored alternate intent conversion");
        let color = Color::new(color_space.clone(), color_space.initial_color(), 1.0);
        assert!(color.is_non_marking());
        assert_eq!(color.to_rgba().to_rgba8(), [0; 4]);
        let mut rgb = [255; 3];
        color_space
            .convert(&vec![127; color_space.num_components() as usize], &mut rgb)
            .unwrap();
        assert_eq!(rgb, [0; 3]);
        let paint = Paint::Color(color);
        let transparent = Paint::Color(Color::from_rgba(AlphaColor::TRANSPARENT));
        assert!(paint.is_non_marking(false));
        assert!(!transparent.is_non_marking(false));
        assert_ne!(paint.cache_key(), transparent.cache_key());
    }
}

#[test]
fn marking_names_keep_alternate_color() {
    for names in ["[/Separation /Spot", "[/DeviceN [/Spot]"] {
        let color_space = space(&format!("{names} /DeviceRGB {TINT}]")).unwrap();
        assert!(!color_space.is_non_marking());
        assert_eq!(
            color_space
                .to_rgba(&vec![0.5; color_space.num_components() as usize], 1.0)
                .to_rgba8(),
            [26, 51, 204, 255]
        );
    }
}

#[test]
fn invalid_non_marking_declarations_do_not_hide_their_tail() {
    for source in [
        format!("[/DeviceN [] /DeviceRGB {TINT}]"),
        format!("[/DeviceN [/None 0 /Spot] /DeviceRGB {TINT}]"),
        format!("[/DeviceN [/None null] /DeviceRGB {TINT}]"),
        format!("[/DeviceN [/Spot /Spot] /DeviceRGB {TINT}]"),
        format!("[/DeviceN [/All] /DeviceRGB {TINT}]"),
        format!("[/DeviceN [/None] /DeviceRGB {TINT} << /Subtype /NChannel >>]"),
        format!("[/DeviceN [/None] /DeviceRGB {TINT} 0]"),
        format!("[/Separation /None /DeviceRGB {TINT} /Extra]"),
        "[/Separation /None /Missing 17]".to_owned(),
        "[/Separation /None /DeviceRGB 17]".to_owned(),
    ] {
        assert!(space(&source).is_none(), "{source}");
    }
}

#[test]
fn special_spaces_are_not_valid_tint_alternates() {
    let gray = "<< /FunctionType 2 /Domain [0 1] /C0 [.5] /C1 [.5] /N 1 >>";
    for prefix in [
        "/Separation /None",
        "/Separation /All",
        "/Separation /Spot",
        "/DeviceN [/None]",
        "/DeviceN [/Spot]",
    ] {
        assert!(space(&format!("[{prefix} /DeviceRGB {TINT}]")).is_some());
        for (alternate, tint) in [
            ("[/Indexed /DeviceRGB 1 <000000ffffff>]".to_owned(), gray),
            (format!("[/Separation /Spot /DeviceRGB {TINT}]"), gray),
            (format!("[/DeviceN [/Spot] /DeviceRGB {TINT}]"), gray),
            ("[/Pattern /DeviceRGB]".to_owned(), TINT),
        ] {
            let source = format!("[{prefix} {alternate} {tint}]");
            assert!(space(&source).is_none(), "{source}");
        }
    }
}

#[test]
fn ignored_tint_functions_still_require_matching_arity() {
    for prefix in [
        "/Separation /None",
        "/Separation /All",
        "/Separation /Spot",
        "/DeviceN [/None]",
        "/DeviceN [/Spot]",
    ] {
        for outputs in [".5", ".1 .2 .3 .4"] {
            let source = format!(
                "[{prefix} /DeviceRGB << /FunctionType 2 /Domain [0 1] /C0 [{outputs}] /C1 [{outputs}] /N 1 >>]"
            );
            assert!(space(&source).is_none(), "{source}");
        }
    }
    for names in ["/None /None", "/None /Spot", "/One /Two"] {
        let source = format!("[/DeviceN [{names}] /DeviceRGB {TINT}]");
        assert!(space(&source).is_none(), "{source}");
    }
}
