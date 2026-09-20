use super::*;
use moxcms::ToneReprCurve;

fn rgb_profile() -> ColorProfile {
    let mut source = ColorProfile::new_srgb();
    source.media_white_point = Some(moxcms::Xyzd::new(D50[0], D50[1], D50[2]));
    source.cicp = None;
    source
}

fn parsed(source: &ColorProfile) -> ICCProfile {
    let mut bytes = source.encode().unwrap();
    bytes[8..12].copy_from_slice(&[4, 0x20, 0, 0]);
    ICCProfile::new(&bytes, 3).unwrap()
}

#[test]
fn normalized_float_quantizer_preimages_partition_every_byte() {
    let mut previous = None;
    for byte in 0..=255 {
        let [low, high] = quantizer_preimage(byte);
        assert_eq!(quantize(low), byte);
        assert_eq!(quantize(high), byte);
        assert!(low <= high);
        if let Some(previous) = previous {
            assert_eq!(low.to_bits(), previous + 1);
            assert_eq!(quantize(low.next_down()), byte - 1);
        } else {
            assert_eq!(low.to_bits(), 0);
        }
        if byte != 255 {
            assert_eq!(quantize(high.next_up()), byte + 1);
        }
        previous = Some(high.to_bits());
    }
    assert_eq!(previous, Some(1.0_f32.to_bits()));
}

#[test]
fn canonical_rgb_has_complete_bidirectional_bounds_for_every_intent() {
    let profile = parsed(&rgb_profile());
    for intent in [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
        RenderingIntent::AbsoluteColorimetric,
    ] {
        let selected = profile.with_intent(intent).unwrap();
        let proof = selected.device_rgb_bounds(|| false).unwrap();
        assert_eq!(proof.evaluated_colors, 12_288);
        assert!(proof.max_component_error >= 0.5 / 255.0);
        assert!(
            proof.max_component_error < 0.501 / 255.0,
            "{intent:?}: {proof:?}"
        );
    }
}

#[test]
fn bounds_cover_all_rgb_bytes_in_both_directions() {
    // Complete finite-domain oracle, separate from the monotonic corner proof.
    // Include signed cross-channel matrix terms and unequal (all distinct) TRCs.
    let mut source = rgb_profile();
    source.red_colorant.y *= 1.015;
    source.green_colorant.x *= 0.985;
    source.blue_colorant.z *= 1.01;
    source.red_trc = Some(ToneReprCurve::Parametric(vec![2.0]));
    source.green_trc = Some(ToneReprCurve::Parametric(vec![2.1]));
    source.blue_trc = Some(ToneReprCurve::Parametric(vec![2.2]));
    let profile = parsed(&source);
    let proof = profile.device_rgb_bounds(|| false).unwrap();
    let (source, destination, options) = profile.conversion_profiles().unwrap();
    for (from, to) in [(&source, &destination), (&destination, &source)] {
        let transform = from
            .create_transform_8bit(Layout::Rgb, to, Layout::Rgb, options)
            .unwrap();
        let mut input = [0_u8; 768];
        let mut output = [0_u8; 768];
        for green in 0..=255 {
            for blue in 0..=255 {
                for (red, pixel) in input.chunks_exact_mut(3).enumerate() {
                    pixel.copy_from_slice(&[red as u8, green, blue]);
                }
                transform.transform(&input, &mut output).unwrap();
                for (source, actual) in input.iter().zip(output) {
                    let error = (f64::from(f32::from(*source) / 255.0)
                        - f64::from(f32::from(actual) / 255.0))
                    .abs();
                    assert!(
                        error <= proof.max_component_error,
                        "{source} -> {actual}: {proof:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn media_white_and_unequal_or_nonmonotone_curves_are_not_identity() {
    let mut source = rgb_profile();
    source.media_white_point = Some(moxcms::Xyzd::new(0.9, 0.7, 0.6));
    let profile = parsed(&source);
    assert!(
        profile
            .device_rgb_bounds(|| false)
            .unwrap()
            .max_component_error
            < 1.0 / 255.0
    );
    let absolute = profile
        .with_intent(RenderingIntent::AbsoluteColorimetric)
        .unwrap();
    assert!(
        absolute
            .device_rgb_bounds(|| false)
            .unwrap()
            .max_component_error
            > 0.1
    );

    let mut source = rgb_profile();
    source.blue_trc = Some(ToneReprCurve::Parametric(vec![1.0]));
    assert!(
        parsed(&source)
            .device_rgb_bounds(|| false)
            .unwrap()
            .max_component_error
            > 0.1
    );
    let mut curve: Vec<_> = (0_u16..=255).map(|v| v * 257).collect();
    curve[16] = u16::MAX;
    source.red_trc = Some(ToneReprCurve::Lut(curve.clone()));
    source.green_trc = Some(ToneReprCurve::Lut(curve.clone()));
    source.blue_trc = Some(ToneReprCurve::Lut(curve));
    assert_eq!(
        parsed(&source).device_rgb_bounds(|| false),
        Err(IccEquivalenceError::Unproved)
    );
}

#[test]
fn raw_lut_declarations_and_cancellation_cannot_be_hidden() {
    let mut bytes = rgb_profile().encode().unwrap();
    // Unknown/malformed payloads must not make a declared A/B LUT look absent.
    for tag in [b"A2B0", b"B2A1", b"D2B2", b"B2D3"] {
        bytes[132..136].copy_from_slice(tag);
        assert!(!matrix_tags_only(&bytes));
    }
    let profile = parsed(&rgb_profile());
    assert_eq!(
        profile.device_rgb_bounds(|| true),
        Err(IccEquivalenceError::Cancelled)
    );
    let mut calls = 0;
    assert_eq!(
        profile.device_rgb_bounds(|| {
            calls += 1;
            calls > 10
        }),
        Err(IccEquivalenceError::Cancelled)
    );
    assert!(profile.device_rgb_bounds(|| false).is_ok());
    let mut checkpoints = 0;
    let reused = profile.clone().device_rgb_bounds(|| {
        checkpoints += 1;
        false
    });
    assert!(reused.is_ok());
    assert_eq!(checkpoints, 1, "owned profile clones reuse their proof");
    assert_eq!(
        profile.device_rgb_bounds(|| true),
        Err(IccEquivalenceError::Cancelled)
    );
}
