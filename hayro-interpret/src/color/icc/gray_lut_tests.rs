use super::*;
use crate::cache::Cache;
use moxcms::{LutMultidimensionalType, LutStore, LutWarehouse, Matrix3d, ToneReprCurve};

// Original Gray input bytes, authored independently of the CMS writer.
// Analytic and independent CMM-stage controls: external pdf-test-suite
// reports/icc-gray-lut-audit-2026-09-21. kTRC gamma 4 deliberately differs
// from the perceptual/colorimetric/saturation LUT gammas 1/2/3.
const PROFILE: &[u8] = include_bytes!("gray-lut-input.bin");
const INTENTS: [RenderingIntent; 4] = [
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
    RenderingIntent::AbsoluteColorimetric,
];

fn encode(linear: f64) -> u8 {
    let encoded = if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

fn rgb(profile: &ICCProfile, input: &[u8]) -> Vec<u8> {
    let mut output = vec![0; input.len() * 3];
    assert!(profile.convert(input, &mut output).is_some());
    output
}

#[test]
fn original_gray_luts_preserve_all_intents_and_owned_tables() {
    let profile = ICCProfile::new(PROFILE, 1).unwrap();
    let owners: Vec<_> = INTENTS
        .map(|intent| profile.with_intent(intent).unwrap())
        .into();
    drop(profile);
    let input: Vec<_> = (0..1025).map(|v| v as u8).collect();
    for (intent, profile) in INTENTS.into_iter().zip(owners) {
        assert!(matches!(profile.transform(), Ok(IccTransform::Gray { .. })));
        let exponent = match intent {
            RenderingIntent::Perceptual => 1,
            RenderingIntent::Saturation => 3,
            _ => 2,
        };
        let output = rgb(&profile, &input);
        for (&sample, actual) in input.iter().zip(output.chunks_exact(3)) {
            let expected = encode((f64::from(sample) / 255.0).powi(exponent));
            assert!(
                actual.iter().all(|v| v.abs_diff(expected) <= 1),
                "{intent:?}: {sample} -> {actual:?}, expected {expected}"
            );
        }
        assert!(profile.convert(&[], &mut []).is_some());
        assert!(profile.convert(&[0], &mut [0; 2]).is_none());
        assert!(profile.convert_in_place(&mut [0; 3]).is_none());
    }
}

#[test]
fn missing_gray_intent_tags_use_perceptual_lut_with_requested_black_policy() {
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    source.lut_a_to_b_colorimetric = None;
    source.lut_a_to_b_saturation = None;
    let profile = ICCProfile::new_from_src_profile(source, 1).unwrap();
    let input: Vec<_> = (0..=255).collect();
    for intent in INTENTS {
        let output = rgb(&profile.with_intent(intent).unwrap(), &input);
        for (&sample, actual) in input.iter().zip(output.chunks_exact(3)) {
            let gray = f64::from(sample) / 255.0;
            let expected = if matches!(
                intent,
                RenderingIntent::Perceptual | RenderingIntent::Saturation
            ) {
                [encode(gray); 3]
            } else {
                let white = [0.9642, 1.0, 0.8249];
                let black = [0.003357, 0.003479, 0.002869];
                let xyz: [f64; 3] =
                    std::array::from_fn(|i| gray * (white[i] - black[i]) + black[i]);
                [
                    [3.1338561, -1.6168667, -0.4906146],
                    [-0.9787684, 1.9161415, 0.0334540],
                    [0.0719453, -0.2289914, 1.4052427],
                ]
                .map(|row| encode(row.into_iter().zip(xyz).map(|(a, b)| a * b).sum()))
            };
            assert!(
                actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
                "{intent:?}: {sample} -> {actual:?}, expected {expected:?}"
            );
        }
    }
}

#[test]
fn an_unselected_gray_lut_does_not_replace_the_trc() {
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    source.lut_a_to_b_perceptual = None;
    source.lut_a_to_b_saturation = None;
    let profile = ICCProfile::new_from_src_profile(source, 1).unwrap();
    let input: Vec<_> = (0..=255).collect();
    for intent in [RenderingIntent::Perceptual, RenderingIntent::Saturation] {
        let output = rgb(&profile.with_intent(intent).unwrap(), &input);
        for (&sample, actual) in input.iter().zip(output.chunks_exact(3)) {
            let expected = encode((f64::from(sample) / 255.0).powi(4));
            assert!(actual.iter().all(|v| v.abs_diff(expected) <= 1));
        }
    }
    let colorimetric = rgb(
        &profile
            .with_intent(RenderingIntent::RelativeColorimetric)
            .unwrap(),
        &[128],
    );
    assert!(
        colorimetric
            .iter()
            .all(|v| v.abs_diff(encode((128.0_f64 / 255.0).powi(2))) <= 1)
    );
}

#[test]
fn absolute_gray_lut_tint_keeps_color_and_refuses_luma_shortcut() {
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    source.media_white_point.as_mut().unwrap().x *= 0.8;
    let profile = ICCProfile::new_from_src_profile(source, 1).unwrap();
    let relative = profile
        .with_intent(RenderingIntent::RelativeColorimetric)
        .unwrap();
    let absolute = profile
        .with_intent(RenderingIntent::AbsoluteColorimetric)
        .unwrap();
    let neutral = rgb(&relative, &[128]);
    assert!(neutral.iter().all(|v| v.abs_diff(neutral[0]) <= 1));
    let tinted = rgb(&absolute, &[128]);
    assert!(tinted[0].abs_diff(tinted[1]) > 20);
    assert!(absolute.to_luma(&mut [128]).is_none());
    // A forward gray ramp is not a bidirectional device RGB group proof.
    assert!(absolute.device_rgb_bounds(|| false).is_err());
}

pub(super) fn parametric_gray_profile() -> ColorProfile {
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    let mut grid_points = [0; 16];
    grid_points[0] = 2;
    source.lut_a_to_b_perceptual = Some(LutWarehouse::Multidimensional(LutMultidimensionalType {
        num_input_channels: 1,
        num_output_channels: 3,
        grid_points,
        clut: Some(LutStore::Store16(vec![0, 0, 0, 31595, 32768, 27030])),
        a_curves: vec![ToneReprCurve::Parametric(vec![1.25])],
        b_curves: vec![ToneReprCurve::Parametric(vec![1.25]); 3],
        m_curves: vec![ToneReprCurve::Parametric(vec![1.25]); 3],
        matrix: Matrix3d::IDENTITY,
        bias: Default::default(),
    }));
    source.lut_a_to_b_colorimetric = None;
    source.lut_a_to_b_saturation = None;
    source
}

#[test]
fn gray_lut_lab_pcs_preserves_lut8_lut16_and_mab_encodings() {
    for format in [8, 16, 0] {
        let mut source = parametric_gray_profile();
        source.pcs = DataColorSpace::Lab;
        let clut = if format == 16 {
            vec![0, 32768, 32768, 65280, 32768, 32768]
        } else {
            vec![0, 128 * 257, 128 * 257, 65535, 128 * 257, 128 * 257]
        };
        let selected = if format == 0 {
            let Some(LutWarehouse::Multidimensional(mut lut)) = source.lut_a_to_b_perceptual.take()
            else {
                unreachable!()
            };
            lut.clut = Some(LutStore::Store16(clut));
            lut.a_curves = vec![ToneReprCurve::Parametric(vec![1.0])];
            lut.b_curves = vec![ToneReprCurve::Parametric(vec![1.0]); 3];
            lut.m_curves = vec![ToneReprCurve::Parametric(vec![1.0]); 3];
            LutWarehouse::Multidimensional(lut)
        } else {
            let original = ColorProfile::new_from_slice(PROFILE).unwrap();
            let Some(LutWarehouse::Lut(mut lut)) = original.lut_a_to_b_colorimetric else {
                unreachable!()
            };
            if format == 8 {
                lut.lut_type = moxcms::LutType::Lut8;
                lut.num_input_table_entries = 256;
                lut.num_output_table_entries = 256;
                lut.input_table = LutStore::Store8((0..=255).collect());
                lut.output_table = LutStore::Store8((0..3).flat_map(|_| 0..=255).collect());
                lut.clut_table =
                    LutStore::Store8(clut.into_iter().map(|v| (v / 257) as u8).collect());
            } else {
                lut.num_input_table_entries = 2;
                lut.input_table = LutStore::Store16(vec![0, 65535]);
                lut.clut_table = LutStore::Store16(clut);
            }
            LutWarehouse::Lut(lut)
        };
        source.lut_a_to_b_colorimetric = Some(selected);
        let profile = ICCProfile::new_from_src_profile(source, 1)
            .unwrap()
            .with_intent(RenderingIntent::RelativeColorimetric)
            .unwrap();
        let input: Vec<_> = (0..=255).collect();
        let output = rgb(&profile, &input);
        for (&sample, actual) in input.iter().zip(output.chunks_exact(3)) {
            let lightness = f64::from(sample) / 255.0 * 100.0;
            let y = if lightness > 8.0 {
                ((lightness + 16.0) / 116.0).powi(3)
            } else {
                lightness * 27.0 / 24389.0
            };
            let expected = encode(y);
            assert!(
                actual.iter().all(|v| v.abs_diff(expected) <= 1),
                "format {format}: {sample} -> {actual:?}, expected {expected}"
            );
        }
    }
}

#[test]
fn temporary_gray_executor_pressure_can_retry_and_only_tables_stay_charged() {
    let bytes = parametric_gray_profile().encode().unwrap();
    let limit = 8 * 1024 * 1024;
    let cache = Cache::with_limits(4, limit);
    let profile = ICCProfile::new_with_memory(&bytes, 1, Some(cache.icc_memory())).unwrap();
    let source_charge = cache.stats().icc_bytes;
    // The construction reservation covers seven expanded curves, the executor
    // and temporary work. Leaving 1.5 MiB cannot cover that conservative bound,
    // even though the retained table is tiny.
    let held = cache
        .icc_memory()
        .reserve(limit - source_charge - 1536 * 1024)
        .unwrap();
    assert!(matches!(
        profile.transform(),
        Err(ColorConversionError::IccMemoryLimit)
    ));
    assert!(
        profile.data.transforms[profile.intent as usize]
            .get()
            .is_none()
    );
    drop(held);
    let mut owners = Vec::new();
    for intent in INTENTS {
        let bound = profile.with_intent(intent).unwrap();
        assert_eq!(rgb(&bound, &[255]).len(), 3);
        owners.push(bound);
    }
    assert_eq!(cache.stats().icc_bytes - source_charge, 4 * 1024);
    cache.clear();
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, source_charge + 4 * 1024);
    drop(owners);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 1);
}
