use super::*;
use crate::cache::Cache;
use moxcms::{LutStore, ToneReprCurve};

// Independently authored gamma-4 Gray input profile, Lab PCS, no AToB tags.
// Original bytes and exact analytic/LittleCMS agreement for all byte values:
// pdf-test-suite/reports/icc-gray-lab-trc-audit-2026-09-21.
const PROFILE: &[u8] = include_bytes!("gray-lab-trc.bin");
const INTENTS: [RenderingIntent; 4] = [
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
    RenderingIntent::AbsoluteColorimetric,
];
const FROM_XYZ: [[f64; 3]; 3] = [
    [3.1339236463378164, -1.6169229392738516, -0.490733723087733],
    [-0.9784210516720576, 1.915842665313229, 0.03339912699596239],
    [
        0.07203553396859233,
        -0.22903203517027076,
        1.4057161576769963,
    ],
];

fn luminance(lightness: f64) -> f64 {
    let lightness = 100.0 * lightness;
    if lightness > 8.0 {
        ((lightness + 16.0) / 116.0).powi(3)
    } else {
        lightness * 27.0 / 24389.0
    }
}

fn encode(value: f64) -> u8 {
    let value = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn rgb(profile: &ICCProfile, input: &[u8]) -> Vec<u8> {
    let mut output = vec![0; input.len() * 3];
    assert!(profile.convert(input, &mut output).is_some());
    output
}

#[test]
fn original_lab_gray_trc_keeps_all_intents_and_owned_byte_tables() {
    let profile = ICCProfile::new(PROFILE, 1).unwrap();
    let owners: Vec<_> = INTENTS
        .map(|intent| profile.with_intent(intent).unwrap())
        .into();
    drop(profile);
    let input: Vec<_> = (0..1025).map(|i| i as u8).collect();
    for (intent, owner) in INTENTS.into_iter().zip(owners) {
        assert!(matches!(owner.transform(), Ok(IccTransform::Gray { .. })));
        let output = rgb(&owner, &input);
        for (&sample, actual) in input.iter().zip(output.chunks_exact(3)) {
            let expected = encode(luminance((f64::from(sample) / 255.0).powi(4)));
            assert!(
                actual.iter().all(|v| v.abs_diff(expected) <= 1),
                "{intent:?}: {sample} -> {actual:?}, expected {expected}"
            );
            assert_eq!(rgb(&owner, &[sample]), actual);
        }
        assert!(owner.convert(&[], &mut []).is_some());
        assert!(owner.convert(&[1], &mut [0; 2]).is_none());
        assert!(owner.convert_in_place(&mut [0; 3]).is_none());
        assert!(owner.device_rgb_bounds(|| false).is_err());
    }
}

#[test]
fn lab_gray_absolute_white_policy_precedes_destination_encoding() {
    for version in [2, 4] {
        for class in [ProfileClass::InputDevice, ProfileClass::DisplayDevice] {
            let mut bytes = PROFILE.to_vec();
            bytes[8] = version;
            let mut source = ColorProfile::new_from_slice(&bytes).unwrap();
            source.profile_class = class;
            let white = source.media_white_point.as_mut().unwrap();
            white.x = D50[0] * 0.72;
            white.y = D50[1];
            white.z = D50[2] * 1.13;
            let media_white = [white.x, white.y, white.z];
            let profile = ICCProfile::new_from_src_profile(source, 1).unwrap();
            for intent in INTENTS {
                let absolute_tint = intent == RenderingIntent::AbsoluteColorimetric
                    && !(version == 2 && class == ProfileClass::DisplayDevice);
                let owner = profile.with_intent(intent).unwrap();
                let input: Vec<_> = (0..=255).collect();
                let output = rgb(&owner, &input);
                for (&sample, actual) in input.iter().zip(output.chunks_exact(3)) {
                    let y = luminance((f64::from(sample) / 255.0).powi(4));
                    let white = if absolute_tint { media_white } else { D50 };
                    let xyz = white.map(|v| v * y);
                    let expected = FROM_XYZ
                        .map(|row| encode(row.into_iter().zip(xyz).map(|(a, b)| a * b).sum()));
                    assert!(
                        actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
                        "v{version} {class:?} {intent:?}, {sample}: {actual:?}, expected {expected:?}"
                    );
                }
                if absolute_tint {
                    assert!(owner.to_luma(&mut [128]).is_none());
                }
                assert!(owner.device_rgb_bounds(|| false).is_err());
            }
        }
    }
}

#[test]
fn an_unselected_lab_gray_lut_does_not_suppress_the_trc() {
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    let mut lut_source = gray_lut_tests::parametric_gray_profile();
    let Some(LutWarehouse::Multidimensional(mut lut)) = lut_source.lut_a_to_b_perceptual.take()
    else {
        unreachable!()
    };
    lut.clut = Some(LutStore::Store16(vec![
        0, 32896, 32896, 65535, 32896, 32896,
    ]));
    lut.a_curves = vec![ToneReprCurve::Parametric(vec![2.0])];
    lut.b_curves = vec![ToneReprCurve::Lut(vec![]); 3];
    lut.m_curves = vec![ToneReprCurve::Lut(vec![]); 3];
    source.lut_a_to_b_colorimetric = Some(LutWarehouse::Multidimensional(lut));
    let profile = ICCProfile::new_from_src_profile(source, 1).unwrap();
    let input: Vec<_> = (0..=255).collect();
    for intent in INTENTS {
        let exponent = match intent {
            RenderingIntent::Perceptual | RenderingIntent::Saturation => 4,
            _ => 2,
        };
        let owner = profile.with_intent(intent).unwrap();
        let output = rgb(&owner, &input);
        for (&sample, actual) in input.iter().zip(output.chunks_exact(3)) {
            let expected = encode(luminance((f64::from(sample) / 255.0).powi(exponent)));
            assert!(
                actual.iter().all(|v| v.abs_diff(expected) <= 1),
                "{intent:?}, {sample}: {actual:?}, expected {expected}"
            );
        }
    }
}

#[test]
fn lab_gray_construction_pressure_retries_and_only_tables_remain_charged() {
    let limit = 8 * 1024 * 1024;
    let cache = Cache::with_limits(4, limit);
    let profile = ICCProfile::new_with_memory(PROFILE, 1, Some(cache.icc_memory())).unwrap();
    let source_charge = cache.stats().icc_bytes;
    let held = cache
        .icc_memory()
        .reserve(limit - source_charge - 512 * 1024)
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(
            profile.transform(),
            Err(ColorConversionError::IccMemoryLimit)
        ));
        assert!(
            profile.data.transforms[profile.intent as usize]
                .get()
                .is_none()
        );
    }
    drop(held);
    let owners: Vec<_> = INTENTS
        .map(|intent| profile.with_intent(intent).unwrap())
        .into();
    assert_eq!(cache.stats().icc_bytes, source_charge + 4 * 1024);
    cache.clear();
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, source_charge + 4 * 1024);
    for owner in &owners {
        assert_eq!(rgb(owner, &[255]), [255, 255, 255]);
    }
    drop(owners);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 2);
}

#[test]
fn malformed_lab_gray_declarations_and_missing_curves_still_refuse() {
    assert!(ICCProfile::new(PROFILE, 3).is_none());
    let mut bytes = PROFILE.to_vec();
    bytes[12..16].copy_from_slice(b"link");
    assert!(ICCProfile::new(&bytes, 1).is_none());
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    source.gray_trc = None;
    let profile = ICCProfile::new_from_src_profile(source, 1).unwrap();
    assert!(matches!(
        profile.transform(),
        Err(ColorConversionError::IccTransform)
    ));
    assert!(profile.device_rgb_bounds(|| false).is_err());
}
