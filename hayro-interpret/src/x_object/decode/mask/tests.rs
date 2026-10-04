use super::*;
use crate::{LumaData, cache::Cache, x_object::ImageXObject};
use hayro_syntax::{
    Pdf,
    object::{ObjectIdentifier, Stream},
};

fn decoded(main: &str, mask: &str, samples: &[u8]) -> Option<(LumaData, Option<LumaData>)> {
    let mut bytes = format!("%PDF-1.7\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Resources<<>>>>endobj\n4 0 obj<</Type/XObject/Subtype/Image/W 2/H 2/ImageMask true/BPC 1/SMask 5 0 R {main}/Length 2>>stream\n").into_bytes();
    bytes.extend_from_slice(&[0x40, 0x80]);
    bytes.extend_from_slice(format!("\nendstream\nendobj\n5 0 obj<</Type/XObject/Subtype/Image/W 3/H 1/ColorSpace/DeviceGray/BPC 8 {mask}/Length {}>>stream\n", samples.len()).as_bytes());
    bytes.extend_from_slice(samples);
    bytes.extend_from_slice(b"\nendstream\nendobj\ntrailer<</Root 1 0 R>>\n%%EOF");
    let pdf = Pdf::new(bytes).unwrap();
    let stream = pdf
        .xref()
        .get::<Stream<'_>>(ObjectIdentifier::new(4, 0))
        .unwrap();
    let warning: crate::WarningSinkFn = std::sync::Arc::new(|_| {});
    let image = ImageXObject::new(
        &stream,
        pdf.pages()[0].resources(),
        &warning,
        &Cache::new(),
        None,
    )?;
    let (shape, alpha) = decode_stencil_alpha(&image, None)?;
    Some((shape.luma, alpha))
}

#[test]
fn stencil_shape_and_associated_opacity_keep_independent_grids_and_decode() {
    let (shape, alpha) = decoded("", "/I true /Intent garbage", &[64, 128, 192]).unwrap();
    assert_eq!((shape.width, shape.height), (2, 2));
    assert_eq!(shape.data, [255, 0, 0, 255]);
    let alpha = alpha.unwrap();
    assert_eq!((alpha.width, alpha.height), (3, 1));
    assert_eq!(alpha.data, [64, 128, 192]);
    assert!(alpha.interpolate);
    let (shape, alpha) = decoded("/D [1 0]", "/D [1 0]", &[64, 128, 192]).unwrap();
    assert_eq!(shape.data, [0, 255, 255, 0]);
    assert_eq!(alpha.unwrap().data, [191, 127, 63]);
}

#[test]
fn unselected_stencil_masks_preserve_omission_and_selected_sources_fail_closed() {
    for omission in ["null", "999 0 R"] {
        let (shape, alpha) = decoded(
            &format!("/SMask {omission}"),
            "/Type /Wrong /Matte garbage",
            &[0],
        )
        .unwrap();
        assert_eq!(shape.data, [255, 0, 0, 255]);
        assert!(alpha.is_none());
    }
    for entry in [
        "/Type /Wrong",
        "/Subtype /Form",
        "/ColorSpace /DeviceRGB",
        "/IM true",
        "/ImageMask garbage",
        "/Mask garbage",
        "/SMask garbage",
        "/Matte [0]",
        "/D [0]",
    ] {
        assert!(decoded("", entry, &[64, 128, 192]).is_none(), "{entry}");
    }
    assert!(decoded("/SMask garbage", "", &[64, 128, 192]).is_none());
    assert!(decoded("/Mask 5 0 R", "", &[64, 128, 192]).is_none());
}
