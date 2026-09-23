use super::*;
use moxcms::LutStore;

fn source(lut8: bool, lab: bool) -> ColorProfile {
    let mut source = ColorProfile::new_from_slice(PROFILE).unwrap();
    source.pcs = if lab {
        DataColorSpace::Lab
    } else {
        DataColorSpace::Xyz
    };
    let Some(LutWarehouse::Lut(mut lut)) = source.lut_a_to_b_colorimetric.take() else {
        unreachable!()
    };
    if lut8 {
        lut.lut_type = LutType::Lut8;
        lut.num_input_table_entries = 256;
        lut.num_output_table_entries = 256;
        lut.input_table = LutStore::Store8((0..=255).collect());
        lut.output_table = LutStore::Store8((0..3).flat_map(|_| 0..=255).collect());
        lut.clut_table = LutStore::Store8(if lab {
            vec![0, 128, 128, 255, 128, 128]
        } else {
            vec![0, 0, 0, 123, 128, 106]
        });
    } else {
        lut.num_input_table_entries = 2;
        lut.num_output_table_entries = 2;
        lut.input_table = LutStore::Store16(vec![0, 65535]);
        lut.output_table = LutStore::Store16(vec![0, 65535, 0, 65535, 0, 65535]);
        lut.clut_table = LutStore::Store16(vec![0, 32768, 32768, 65280, 32768, 32768]);
    }
    source.lut_a_to_b_perceptual = Some(LutWarehouse::Lut(lut.clone()));
    source.lut_a_to_b_colorimetric = Some(LutWarehouse::Lut(lut.clone()));
    source.lut_a_to_b_saturation = Some(LutWarehouse::Lut(lut));
    source
}

#[test]
fn original_lab_and_lut8_values_survive_wide_conversion_and_byte_reuse() {
    let input: Vec<_> = (0..=65535_u16).map(|v| f64::from(v) / 65535.0).collect();
    for (lut8, lab) in [(true, false), (true, true), (false, true)] {
        let profile = ICCProfile::new_from_src_profile(source(lut8, lab), 1)
            .unwrap()
            .with_intent(RenderingIntent::RelativeColorimetric)
            .unwrap();
        assert!(profile.has_float_image_conversion());
        let mut before = vec![0; input.len() * 3];
        profile.convert_native(&input, &mut before).unwrap();
        let mut actual = vec![0.0; input.len() * 3];
        profile.convert_native_f64(&input, &mut actual).unwrap();
        let mut after = vec![0; before.len()];
        profile.convert_native(&input, &mut after).unwrap();
        assert_eq!(before, after);
        for (&v, actual) in input.iter().zip(actual.chunks_exact(3)) {
            let xyz = if lab {
                let y = if v * 100.0 <= 8.0 {
                    v * 2700.0 / 24389.0
                } else {
                    ((v * 100.0 + 16.0) / 116.0).powi(3)
                };
                [0.9642 * y, y, 0.8249 * y]
            } else {
                [v * 123.0 / 128.0, v, v * 106.0 / 128.0]
            };
            // Independent inverse of ICC sRGB registry B.2 D50 colorants.
            let expected = [
                [3.1339236463378164, -1.6169229392738516, -0.490733723087733],
                [-0.9784210516720576, 1.915842665313229, 0.03339912699596239],
                [
                    0.07203553396859233,
                    -0.22903203517027076,
                    1.4057161576769963,
                ],
            ]
            .map(|row| {
                let v = row
                    .into_iter()
                    .zip(xyz)
                    .map(|(a, b)| a * b)
                    .sum::<f64>()
                    .clamp(0.0, 1.0);
                if v <= 0.0031308 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            });
            for (&a, e) in actual.iter().zip(expected) {
                assert!(
                    (a - e).abs() <= 1e-12,
                    "LUT8={lut8} Lab={lab} {v}: {a} != {e}"
                );
            }
        }
    }
}

#[test]
fn gray_lut_formats_retry_quota_refusals_and_retain_owned_tables() {
    for (lut8, lab) in [(true, false), (true, true), (false, true)] {
        let cache = Cache::with_limits(4, 32 * 1024 * 1024);
        let bytes = source(lut8, lab).encode().unwrap();
        let profile = ICCProfile::new_with_memory(&bytes, 1, Some(cache.icc_memory())).unwrap();
        let held = cache
            .icc_memory()
            .reserve(32 * 1024 * 1024 - cache.stats().icc_bytes - 1024)
            .unwrap();
        assert!(matches!(
            profile.native_transform_for(true),
            Err(ColorConversionError::IccMemoryLimit)
        ));
        assert!(
            profile.data.float_transforms[profile.intent as usize]
                .get()
                .is_none()
        );
        drop(held);
        let mut output = [0.0; 3];
        profile.convert_native_f64(&[0.37], &mut output).unwrap();
        let retained = cache.stats().icc_bytes;
        assert!(retained > 0);
        cache.clear();
        assert_eq!(cache.stats().icc_bytes, retained);
        profile.convert_native_f64(&[0.5], &mut output).unwrap();
        drop(profile);
        assert_eq!(cache.stats().icc_bytes, 0);
    }
}
