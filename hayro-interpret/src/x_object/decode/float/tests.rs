use super::*;
use crate::cache::Cache;
use hayro_syntax::Pdf;
use hayro_syntax::object::{ObjectIdentifier, Stream};

fn decode(
    header: &str,
    data: &[u8],
    limit: u64,
    checkpoint: impl FnMut() -> bool,
) -> Result<RgbF32Data, Error> {
    let mut bytes = format!(
        "%PDF-1.7\n1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
        2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
        3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 32 32]/Resources<<>>>> endobj\n\
        4 0 obj <</Type/XObject/Subtype/Image {header} /Length {}>>\nstream\n",
        data.len()
    )
    .into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream\nendobj\ntrailer <</Root 1 0 R>>\n%%EOF");
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
    )
    .unwrap();
    decode_rgb_f32(&image, limit, checkpoint)
}

fn floats(image: &RgbF32Data) -> Vec<f32> {
    image
        .data
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

#[test]
fn direct_sixteen_bit_and_decode_components_do_not_round_to_bytes() {
    let header = "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16";
    let image = decode(header, &[0x80, 0, 0x80, 1], 24, || true).unwrap();
    assert_eq!(
        floats(&image),
        [
            vec![32768.0_f32 / 65535.0; 3],
            vec![32769.0_f32 / 65535.0; 3]
        ]
        .concat()
    );
    assert_eq!(
        (image.width, image.height, image.scale_factors),
        (2, 1, (1.0, 1.0))
    );
    let image = decode(
        &format!("{header} /Decode [1 0]"),
        &[0, 1, 0xff, 0xfe],
        24,
        || true,
    )
    .unwrap();
    assert_eq!(
        floats(&image),
        [vec![65534.0_f32 / 65535.0; 3], vec![1.0_f32 / 65535.0; 3]].concat()
    );
    let image = decode(
        "/Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Decode [.1 .9 .2 .8 .3 .7]",
        &[64, 128, 192],
        12,
        || true,
    )
    .unwrap();
    let expected = [
        (0.1_f64 + 64.0 / 255.0 * 0.8) as f32,
        (0.2_f64 + 128.0 / 255.0 * 0.6) as f32,
        (0.3_f64 + 192.0 / 255.0 * 0.4) as f32,
    ];
    assert!(
        floats(&image)
            .iter()
            .zip(expected)
            .all(|(a, b)| (a - b).abs() < 1e-7)
    );
}

#[test]
fn packed_sample_rows_and_calibrated_conversion_preserve_real_values() {
    for (bits, data, expected) in [
        (1, vec![0x80, 0x60], vec![1.0, 0.0, 0.0, 0.0, 1.0, 1.0]),
        (
            2,
            vec![0x6c, 0x90],
            vec![1.0 / 3.0, 2.0 / 3.0, 1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0],
        ),
        (
            4,
            vec![0x18, 0xf0, 0x80, 0x10],
            vec![1.0 / 15.0, 8.0 / 15.0, 1.0, 8.0 / 15.0, 0.0, 1.0 / 15.0],
        ),
    ] {
        let image = decode(
            &format!("/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent {bits}"),
            &data,
            72,
            || true,
        )
        .unwrap();
        assert_eq!(
            floats(&image),
            expected
                .into_iter()
                .flat_map(|v| [v; 3])
                .collect::<Vec<_>>()
        );
    }
    let image = decode("/Width 1 /Height 1 /ColorSpace [/CalGray <</WhitePoint[0.95047 1 1.08883]/Gamma 2>>] /BitsPerComponent 16 /Interpolate true", &[0x80,0],12,||true).unwrap();
    let linear = (32768.0_f64 / 65535.0).powi(2);
    let expected = 1.055 * linear.powf(1.0 / 2.4) - 0.055;
    assert!(image.interpolate);
    assert!(
        floats(&image)
            .iter()
            .all(|v| (f64::from(*v) - expected).abs() < 2e-6)
    );
    assert!(
        floats(&image)
            .iter()
            .any(|v| (*v * 255.0).fract().abs() > 0.1)
    );
}

#[test]
fn malformed_truncated_unproved_and_over_budget_images_have_no_output() {
    let header = "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16";
    assert!(matches!(
        decode(header, &[0; 3], 24, || true),
        Err(Error::Decode)
    ));
    assert!(matches!(
        decode(header, &[0; 4], 23, || true),
        Err(Error::Limit { requested: 24 })
    ));
    assert!(matches!(
        decode(header, &[0; 4], 24, || false),
        Err(Error::Cancelled)
    ));
    for extra in [
        "/Decode [0]",
        "/Decode [0 1 false]",
        "/Decode [0 false]",
        "/Decode false",
        "/Mask [0 1]",
        "/SMask null",
        "/Filter /JPXDecode",
    ] {
        assert!(
            matches!(
                decode(&format!("{header} {extra}"), &[0; 4], 24, || true),
                Err(Error::Unsupported)
            ),
            "{extra}"
        );
    }
    let mut calls = 0;
    let result = decode(
        "/Width 1025 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &vec![128; 1025],
        12300,
        || {
            calls += 1;
            calls < 4
        },
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(calls, 4);
}

#[test]
fn decode_affine_keeps_opposing_bounds_and_clamps_only_after_mapping() {
    assert_eq!(decode_sample(1, 3, -f32::MAX / 2.0, f32::MAX), 0.0);
    assert_eq!(decode_sample(0, 65535, 0.125, 0.875), 0.125);
    assert_eq!(decode_sample(65535, 65535, 0.125, 0.875), 0.875);
    let image = decode(
        "/Width 3 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [-1 2]",
        &[0, 128, 255],
        36,
        || true,
    )
    .unwrap();
    assert_eq!(
        floats(&image),
        [
            vec![0.0; 3],
            vec![(129.0_f64 / 255.0) as f32; 3],
            vec![1.0; 3]
        ]
        .concat()
    );
}
