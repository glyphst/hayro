use super::*;
use crate::cache::Cache;
use crate::color::RenderingIntent;

const ORIGINAL: &[u8] = include_bytes!("gray-lab-trc.bin");
const INTENTS: [RenderingIntent; 4] = [
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
    RenderingIntent::AbsoluteColorimetric,
];

fn profile_bytes(lab: bool, gamma: u16) -> Vec<u8> {
    let mut bytes = ORIGINAL.to_vec();
    if !lab {
        bytes[20..24].copy_from_slice(b"XYZ ");
    }
    let count = u32::from_be_bytes(bytes[128..132].try_into().unwrap());
    for index in 0..count as usize {
        let at = 132 + index * 12;
        if &bytes[at..at + 4] == b"kTRC" {
            let offset = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
            assert_eq!(&bytes[offset..offset + 12], b"curv\0\0\0\0\0\0\0\x01");
            bytes[offset + 12..offset + 14].copy_from_slice(&(gamma * 256).to_be_bytes());
            return bytes;
        }
    }
    panic!("authored tone curve missing")
}

fn expected(lab: bool, gamma: u16, sample: f64) -> u8 {
    (expected_float(lab, gamma, sample) * 255.0).round() as u8
}

fn expected_float(lab: bool, gamma: u16, sample: f64) -> f64 {
    let curve = sample.powi(i32::from(gamma));
    let linear = if lab {
        let lightness = curve * 100.0;
        if lightness > 8.0 {
            ((lightness + 16.0) / 116.0).powi(3)
        } else {
            lightness * 27.0 / 24389.0
        }
    } else {
        curve
    };
    if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn original_gray_profiles_preserve_the_complete_native_sample_domain() {
    // Original profile bytes and full-domain LittleCMS controls are retained
    // in reports/icc-gray-high-depth-audit-2026-09-21. The equations here do
    // not call the production profile converter or quantize its input first.
    let input: Vec<_> = (0..=65535_u16).map(|v| f64::from(v) / 65535.0).collect();
    for lab in [false, true] {
        for gamma in [4, 8] {
            let source = ICCProfile::new(&profile_bytes(lab, gamma), 1).unwrap();
            for intent in INTENTS {
                let source = source.with_intent(intent).unwrap();
                let mut actual = vec![0; input.len() * 3];
                source.convert_native(&input, &mut actual).unwrap();
                for (&value, pixel) in input.iter().zip(actual.chunks_exact(3)) {
                    let expected = expected(lab, gamma, value);
                    assert!(
                        pixel.iter().all(|&c| c.abs_diff(expected) <= 1),
                        "lab={lab} gamma={gamma} intent={intent:?} input={value}: {pixel:?}, expected {expected}"
                    );
                }
            }
        }
    }
}

#[test]
fn native_transform_and_conversion_pressure_retry_without_losing_owners() {
    let limit = 32 * 1024 * 1024;
    let cache = Cache::with_limits(4, limit);
    let source = ICCProfile::new_with_memory(ORIGINAL, 1, Some(cache.icc_memory())).unwrap();
    let source_charge = cache.stats().icc_bytes;
    let held = cache
        .icc_memory()
        .reserve(limit - source_charge - 2 * 1024 * 1024)
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            source.native_transform(),
            Err(ColorConversionError::IccMemoryLimit)
        ));
        assert!(
            source.data.native_transforms[source.intent as usize]
                .get()
                .is_none()
        );
    }
    drop(held);
    let mut owners = Vec::new();
    for intent in INTENTS {
        let owner = source.with_intent(intent).unwrap();
        owner.native_transform().unwrap();
        owners.push(owner);
    }
    let retained = cache.stats().icc_bytes;
    assert!(retained > source_charge);
    let held = cache
        .icc_memory()
        .reserve(limit - retained - 64 * 1024)
        .unwrap();
    let mut output = [17, 31, 47];
    assert!(owners[0].convert_native(&[0.50001], &mut output).is_none());
    assert_eq!(output, [17, 31, 47]);
    drop(held);
    owners[0].convert_native(&[0.50001], &mut output).unwrap();
    assert!(
        output
            .iter()
            .all(|&v| v.abs_diff(expected(true, 4, 0.50001)) <= 1)
    );
    assert_eq!(cache.stats().icc_bytes, retained);
    cache.clear();
    drop(source);
    assert_eq!(cache.stats().icc_bytes, retained);
    for owner in &owners {
        owner.convert_native(&[1.0], &mut output).unwrap();
        assert_eq!(output, [255; 3]);
    }
    drop(owners);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 3);
}

#[path = "native_image_tests.rs"]
mod image;

#[path = "native_float_tests.rs"]
mod float;

#[path = "native_lut_float_tests.rs"]
mod lut_float;

#[test]
fn concurrent_native_conversions_share_one_owned_executor() {
    let cache = Cache::with_limits(4, 32 * 1024 * 1024);
    let source = ICCProfile::new_with_memory(ORIGINAL, 1, Some(cache.icc_memory())).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let source = source.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let input = vec![0.87501; 4099];
                let mut output = vec![0; input.len() * 3];
                barrier.wait();
                source.convert_native(&input, &mut output).unwrap();
                assert!(
                    output
                        .iter()
                        .all(|&v| v.abs_diff(expected(true, 4, 0.87501)) <= 1)
                );
                source.native_transform().unwrap() as *const OwnedNativeTransform as usize
            })
        })
        .collect();
    let pointers: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(pointers.iter().all(|&pointer| pointer == pointers[0]));
    let charge = cache.stats().icc_bytes;
    assert!(charge > 0);
    let mut output = [0; 3];
    source.convert_native(&[0.87501], &mut output).unwrap();
    assert_eq!(cache.stats().icc_bytes, charge);
    cache.clear();
    assert_eq!(cache.stats().icc_bytes, charge);
    drop(source);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 0);
}
