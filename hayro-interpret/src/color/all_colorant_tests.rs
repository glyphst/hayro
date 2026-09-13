use super::*;
use crate::{CacheKey, Paint};
use hayro_syntax::object::FromBytes;

const BLUE: &str = "<< /FunctionType 2 /Domain [0 1] /C0 [.1 .2 .8] /C1 [.1 .2 .8] /N 1 >>";

fn space(source: &str) -> ColorSpace {
    ColorSpace::from_pdf_object(Object::from_bytes(source.as_bytes()).unwrap()).unwrap()
}

#[test]
fn all_colorants_ignore_alternate_conversion_and_preserve_real_tints() {
    for name in ["All", "A#6cl"] {
        for alternate in ["/DeviceRGB", "[/CalRGB << /WhitePoint [.9505 1 1.089] >>]"] {
            let source = space(&format!("[/Separation /{name} {alternate} {BLUE}]"));
            for intent in [
                RenderingIntent::Perceptual,
                RenderingIntent::RelativeColorimetric,
                RenderingIntent::Saturation,
                RenderingIntent::AbsoluteColorimetric,
            ] {
                let source = source
                    .with_rendering_intent(intent)
                    .expect("ignored alternate intent");
                for tint in [-1.0_f32, 0.0, 0.1, 0.1234567, 0.5, 0.9, 1.0, 2.0] {
                    let gray = 1.0 - tint.clamp(0.0, 1.0);
                    assert_eq!(
                        source.to_rgba(&[tint], 0.375).components(),
                        [gray, gray, gray, 0.375]
                    );
                    assert!(!source.is_non_marking());
                }
            }
        }
    }
}

#[test]
fn all_colorant_bytes_and_indexed_palettes_apply_subtractive_tints() {
    let source = space(&format!("[/Separation /All /DeviceRGB {BLUE}]"));
    let bytes: Vec<u8> = (0..=255).collect();
    let mut output = vec![0; bytes.len() * 3];
    source.convert(&bytes, &mut output).unwrap();
    assert_eq!(
        output,
        bytes
            .iter()
            .flat_map(|value| [255 - value; 3])
            .collect::<Vec<_>>()
    );
    let indexed = space(&format!(
        "[/Indexed [/Separation /All /DeviceRGB {BLUE}] 3 <ff008040>]"
    ));
    for (index, gray) in [
        (-2.0, 0),
        (0.49, 0),
        (0.5, 255),
        (1.49, 255),
        (1.5, 127),
        (2.5, 191),
        (8.0, 191),
    ] {
        assert_eq!(
            indexed.to_rgba(&[index], 1.0).to_rgba8(),
            [gray, gray, gray, 255]
        );
    }
}

#[test]
fn all_colorants_keep_identity_beyond_the_rgb_preview() {
    let source = space(&format!("[/Separation /All /DeviceRGB {BLUE}]"));
    let all = Color::new(source, smallvec![0.5], 1.0);
    let gray = Color::new(space("/DeviceGray"), smallvec![0.5], 1.0);
    assert_eq!(all.to_rgba().components(), gray.to_rgba().components());
    assert_ne!(
        Paint::Color(all).cache_key(),
        Paint::Color(gray).cache_key(),
        "CMYK destinations need the original All tint"
    );
}

#[test]
fn all_colorant_image_conversion_retains_binary64_precision() {
    let source = space(&format!("[/Separation /All /DeviceRGB {BLUE}]"));
    for tint in [0.0, 1.0 / 65535.0, 0.123456789012, 0.5, 1.0] {
        assert_eq!(source.image_rgb_f64(&[tint]), Some([1.0 - tint; 3]));
    }
}
