use super::*;
use crate::cache::Cache;
use crate::color::icc::equivalence::tests::{parsed, rgb_profile};
use moxcms::ToneReprCurve;

#[test]
fn native_quantizer_preimages_match_complete_binary64_search() {
    let mut previous = None;
    for bin in 0..=65535_u16 {
        let [low, high] = native_preimage(bin).unwrap();
        // Independently search the entire ordered positive binary64 domain.
        let (mut first, mut end) = (0_u64, 1.0_f64.to_bits() + 1);
        while first < end {
            let middle = first + (end - first) / 2;
            if f64::from_bits(middle)._as_usize() >= usize::from(bin) {
                end = middle;
            } else {
                first = middle + 1;
            }
        }
        assert_eq!(low.to_bits(), first);
        assert_eq!(low._as_usize(), usize::from(bin));
        assert_eq!(high._as_usize(), usize::from(bin));
        assert!(low <= high);
        if let Some(previous) = previous {
            assert_eq!(low.to_bits(), previous + 1);
        } else {
            assert_eq!(low.to_bits(), 0);
        }
        if bin != u16::MAX {
            assert_eq!(high.next_up()._as_usize(), usize::from(bin) + 1);
        }
        previous = Some(high.to_bits());
    }
    assert_eq!(previous, Some(1.0_f64.to_bits()));
}

#[test]
fn native_canonical_rgb_has_separate_complete_forward_bounds_for_all_intents() {
    let source = parsed(&rgb_profile());
    for intent in [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
        RenderingIntent::AbsoluteColorimetric,
    ] {
        let profile = source.with_intent(intent).unwrap();
        let byte = profile.device_rgb_bounds(|| false).unwrap();
        assert_eq!(byte.evaluated_colors, 12_288);
        let native = profile.image_device_rgb_bounds(|| false).unwrap();
        assert_eq!(native.evaluated_colors, 65_536 * 3 * 4 * 2);
        assert!(native.max_component_error >= 0.5 / 255.0);
        assert!(
            native.max_component_error <= 1.0 / 255.0,
            "{intent:?}: {native:?}"
        );
        assert_eq!(byte, profile.device_rgb_bounds(|| false).unwrap());
        let mut calls = 0;
        assert_eq!(
            native,
            profile
                .clone()
                .image_device_rgb_bounds(|| {
                    calls += 1;
                    false
                })
                .unwrap()
        );
        assert_eq!(calls, 1);
        assert_eq!(
            profile.image_device_rgb_bounds(|| true),
            Err(IccEquivalenceError::Cancelled)
        );
    }
}

#[test]
fn native_bounds_cover_interior_colors_with_signed_matrix_terms_and_unequal_curves() {
    let mut source = rgb_profile();
    source.red_colorant.y *= 1.015;
    source.green_colorant.x *= 0.985;
    source.blue_colorant.z *= 1.01;
    source.red_trc = Some(ToneReprCurve::Parametric(vec![2.0]));
    source.green_trc = Some(ToneReprCurve::Parametric(vec![2.1]));
    source.blue_trc = Some(ToneReprCurve::Parametric(vec![2.2]));
    let profile = parsed(&source);
    let bound = profile.image_device_rgb_bounds(|| false).unwrap();
    assert!(bound.max_component_error > 1.0 / 255.0);
    let mut seed = 17_u64;
    let mut input = vec![0.0; 65_536 * 3];
    for (index, pixel) in input.chunks_exact_mut(3).enumerate() {
        let [low, high] = native_preimage(index as u16).unwrap();
        pixel[0] = if index % 2 == 0 { low } else { high };
        for value in &mut pixel[1..] {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *value = (seed >> 11) as f64 / ((1_u64 << 53) - 1) as f64;
        }
    }
    let mut output = vec![0.0; input.len()];
    let transform = profile.native_transform().unwrap();
    for (input, output) in input.chunks(3072).zip(output.chunks_mut(3072)) {
        transform.executor.transform(input, output).unwrap();
    }
    for (input, converted) in input.into_iter().zip(output) {
        let byte = (converted.clamp(0.0, 1.0) * 255.0).round() as u8;
        let actual = f64::from(f32::from(byte) / 255.0);
        assert!((actual - input).abs() <= bound.max_component_error);
    }
}

#[test]
fn native_proof_refuses_luts_nonmonotone_tables_and_invalid_matrices() {
    let lut = ICCProfile::new(&lut_tests::profile_bytes(4, 3, false), 3).unwrap();
    assert_eq!(
        lut.image_device_rgb_bounds(|| false),
        Err(IccEquivalenceError::Unproved)
    );
    let mut source = rgb_profile();
    let mut curve: Vec<_> = (0..40_000)
        .map(|index| {
            let encoded = f64::from(index) / 39_999.0;
            let linear = if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            };
            (linear * 65535.0).round() as u16
        })
        .collect();
    // A narrow feature between neighboring byte inputs must not disappear
    // during validation of the complete native input table.
    curve[20_000] = 60_000;
    source.red_trc = Some(ToneReprCurve::Lut(curve));
    assert_eq!(
        parsed(&source).image_device_rgb_bounds(|| false),
        Err(IccEquivalenceError::Unproved)
    );
    for invalid in [f64::NAN, f64::INFINITY, 100.0] {
        let mut source = rgb_profile();
        source.red_colorant.x = invalid;
        let profile = ICCProfile::new_from_src_profile(source, 3).unwrap();
        assert_eq!(
            profile.image_device_rgb_bounds(|| false),
            Err(IccEquivalenceError::Unproved)
        );
    }
}

#[test]
fn native_proof_memory_and_mid_calculation_cancellation_remain_retryable() {
    let limit = 64 * 1024 * 1024;
    let cache = Cache::with_limits(4, limit);
    let bytes = rgb_profile().encode().unwrap();
    let profile = ICCProfile::new_with_memory(&bytes, 3, Some(cache.icc_memory())).unwrap();
    let source_charge = cache.stats().icc_bytes;
    let block = cache
        .icc_memory()
        .reserve(limit - source_charge - 1024)
        .unwrap();
    assert_eq!(
        profile.image_device_rgb_bounds(|| false),
        Err(IccEquivalenceError::MemoryLimit)
    );
    assert!(
        profile.data.image_equivalence[profile.intent as usize]
            .get()
            .is_none()
    );
    drop(block);
    assert_eq!(cache.stats().icc_bytes, source_charge);
    let mut calls = 0;
    assert_eq!(
        profile.image_device_rgb_bounds(|| {
            calls += 1;
            calls == 200
        }),
        Err(IccEquivalenceError::Cancelled)
    );
    assert_eq!(calls, 200);
    assert!(
        profile.data.image_equivalence[profile.intent as usize]
            .get()
            .is_none()
    );
    let retained = cache.stats().icc_bytes;
    assert!(retained > source_charge && retained < limit);
    let proof = profile.image_device_rgb_bounds(|| false).unwrap();
    assert_eq!(proof.evaluated_colors, 65_536 * 3 * 4 * 2);
    assert_eq!(cache.stats().icc_bytes, retained);
    let clone = profile.clone();
    cache.clear();
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, retained);
    drop(clone);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 1);
}
