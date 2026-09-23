use super::*;
use crate::color::icc::equivalence::original_lut_tags_only;
use moxcms::{LutType, LutWarehouse};

const PROFILE: &[u8] = include_bytes!("gray-lut-input.bin");

#[path = "native_lut_float_tests/references.rs"]
mod references;

#[path = "native_lut_float_tests/images.rs"]
mod images;

#[test]
fn gray_lut_float_domains_preserve_intents_without_changing_byte_conversion() {
    let profile = ICCProfile::new(PROFILE, 1).unwrap();
    let input: Vec<_> = (0..=65535_u16).map(|v| f64::from(v) / 65535.0).collect();
    for (index, intent) in INTENTS.into_iter().enumerate() {
        let profile = profile.with_intent(intent).unwrap();
        assert!(profile.has_float_image_conversion());
        let mut before = vec![0; input.len() * 3];
        profile.convert_native(&input, &mut before).unwrap();
        let mut actual = vec![0.0; input.len() * 3];
        profile.convert_native_f64(&input, &mut actual).unwrap();
        for (&sample, expected) in references::SAMPLES.iter().zip(&references::COLORS[index]) {
            for (&value, &expected) in actual[sample * 3..sample * 3 + 3].iter().zip(expected) {
                assert!(
                    (value - expected).abs() <= 1e-12,
                    "{intent:?} {sample}: {value} != {expected}"
                );
            }
        }
        let (sample, channel, cut) = references::CUTS[index];
        let value = actual[sample * 3 + channel];
        assert!(value < cut && f64::from(value as f32) >= cut);
        if let Some(directory) = std::env::var_os("GLYPHST_GRAY_LUT_ORIGINALS") {
            let reference = std::fs::read(
                std::path::Path::new(&directory).join(format!("intent{index}-untransferred.f64")),
            )
            .unwrap();
            assert_eq!(reference.len(), actual.len() * 8);
            for (&value, bytes) in actual.iter().zip(reference.chunks_exact(8)) {
                let expected = f64::from_le_bytes(bytes.try_into().unwrap());
                assert!(
                    (value - expected).abs() <= 1e-12,
                    "{intent:?}: {value} != {expected}"
                );
            }
        }
        let mut after = vec![0; before.len()];
        profile.convert_native(&input, &mut after).unwrap();
        assert_eq!(before, after);
        assert!(profile.convert_native_f64(&[], &mut []).is_some());
        assert!(profile.convert_native_f64(&[0.0], &mut [0.0; 2]).is_none());
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut out = [17.0; 3];
            assert!(profile.convert_native_f64(&[value], &mut out).is_none());
            assert_eq!(out, [17.0; 3]);
        }
    }
}

#[test]
fn gray_lut_float_intent_fallback_and_unsupported_formats_are_explicit() {
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    source.lut_a_to_b_colorimetric = None;
    source.lut_a_to_b_saturation = None;
    let fallback = ICCProfile::new_from_src_profile(source.clone(), 1).unwrap();
    source.lut_a_to_b_colorimetric = source.lut_a_to_b_perceptual.clone();
    source.lut_a_to_b_saturation = source.lut_a_to_b_perceptual.clone();
    let explicit = ICCProfile::new_from_src_profile(source, 1).unwrap();
    let input = [0.0, 0.0003, 0.25, 0.7, 1.0];
    for intent in INTENTS {
        let mut actual = [0.0; 15];
        let mut expected = [0.0; 15];
        fallback
            .with_intent(intent)
            .unwrap()
            .convert_native_f64(&input, &mut actual)
            .unwrap();
        explicit
            .with_intent(intent)
            .unwrap()
            .convert_native_f64(&input, &mut expected)
            .unwrap();
        assert_eq!(actual, expected);
    }
    let mut invalid = Vec::new();
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    source.pcs = DataColorSpace::Lab;
    invalid.push((source, 1));
    invalid.push((
        crate::color::icc::gray_lut_tests::parametric_gray_profile(),
        1,
    ));
    for kind in [LutType::Lut8, LutType::LutMab, LutType::LutMba] {
        let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
        let Some(LutWarehouse::Lut(lut)) = source.lut_a_to_b_perceptual.as_mut() else {
            unreachable!()
        };
        lut.lut_type = kind;
        invalid.push((source, 1));
    }
    for (space, count) in [(DataColorSpace::Rgb, 3), (DataColorSpace::Cmyk, 4)] {
        let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
        source.color_space = space;
        invalid.push((source, count));
    }
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    let Some(LutWarehouse::Lut(lut)) = source.lut_a_to_b_perceptual.as_mut() else {
        unreachable!()
    };
    lut.matrix.v[0][0] = 0.5;
    invalid.push((source, 1));
    for (source, count) in invalid {
        let profile = ICCProfile::new_from_src_profile(source, count).unwrap();
        assert!(!profile.has_float_image_conversion());
        assert!(profile.native_transform_for(true).is_err());
    }
}

#[test]
fn unrecognized_or_duplicate_lut_declarations_cannot_enter_float_conversion() {
    assert!(original_lut_tags_only(PROFILE));
    let count = u32::from_be_bytes(PROFILE[128..132].try_into().unwrap()) as usize;
    let tag = (0..count)
        .map(|i| 132 + i * 12)
        .find(|&at| &PROFILE[at..at + 4] == b"A2B2")
        .unwrap();
    for name in [b"D2B0", b"B2D0", b"A2B3", b"B2A3", b"A2B0"] {
        let mut bytes = PROFILE.to_vec();
        bytes[tag..tag + 4].copy_from_slice(name);
        assert!(!original_lut_tags_only(&bytes));
        if let Some(profile) = ICCProfile::new(&bytes, 1) {
            assert!(!profile.has_float_image_conversion());
        }
    }
    let mut bytes = PROFILE.to_vec();
    let offset = u32::from_be_bytes(bytes[tag + 4..tag + 8].try_into().unwrap()) as usize;
    bytes[offset..offset + 4].copy_from_slice(b"mpet");
    assert!(!original_lut_tags_only(&bytes));
    let mut bytes = PROFILE.to_vec();
    bytes[tag + 4..tag + 8].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(!original_lut_tags_only(&bytes));
    for end in [0, 128, 131, 132 + count * 12 - 1] {
        assert!(!original_lut_tags_only(&PROFILE[..end]));
    }
}

#[test]
fn gray_lut_float_owners_keep_reservations_and_retry_after_quota_refusal() {
    let limit = 32 * 1024 * 1024;
    let cache = Cache::with_limits(4, limit);
    let source = ICCProfile::new_with_memory(PROFILE, 1, Some(cache.icc_memory())).unwrap();
    let source_charge = cache.stats().icc_bytes;
    let held = cache
        .icc_memory()
        .reserve(limit - source_charge - 2 * 1024 * 1024)
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            source.native_transform_for(true),
            Err(ColorConversionError::IccMemoryLimit)
        ));
        assert!(
            source.data.float_transforms[source.intent as usize]
                .get()
                .is_none()
        );
    }
    drop(held);
    let mut owners = Vec::new();
    for intent in INTENTS {
        let owner = source.with_intent(intent).unwrap();
        owner.native_transform_for(true).unwrap();
        owners.push(owner);
    }
    let retained = cache.stats().icc_bytes;
    assert!(retained > source_charge);
    let held = cache
        .icc_memory()
        .reserve(limit - retained - 64 * 1024)
        .unwrap();
    let mut output = [17.0; 3];
    assert!(owners[0].convert_native_f64(&[0.5], &mut output).is_none());
    assert_eq!(output, [17.0; 3]);
    drop(held);
    owners[0].convert_native_f64(&[0.5], &mut output).unwrap();
    assert_eq!(cache.stats().icc_bytes, retained);
    cache.clear();
    drop(source);
    assert_eq!(cache.stats().icc_bytes, retained);
    for owner in &owners {
        owner.convert_native_f64(&[1.0], &mut output).unwrap();
    }
    drop(owners);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 3);
}

#[test]
fn concurrent_gray_lut_float_conversions_share_one_executor() {
    let cache = Cache::with_limits(4, 32 * 1024 * 1024);
    let source = ICCProfile::new_with_memory(PROFILE, 1, Some(cache.icc_memory())).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let source = source.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let input = vec![0.5; 4099];
                let mut output = vec![0.0; input.len() * 3];
                barrier.wait();
                source.convert_native_f64(&input, &mut output).unwrap();
                assert!(
                    output
                        .iter()
                        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                );
                source.native_transform_for(true).unwrap() as *const OwnedNativeTransform as usize
            })
        })
        .collect();
    let pointers: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(pointers.iter().all(|p| *p == pointers[0]));
    let retained = cache.stats().icc_bytes;
    assert!(retained > 0);
    cache.clear();
    assert_eq!(cache.stats().icc_bytes, retained);
    drop(source);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 0);
}
