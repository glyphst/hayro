use super::*;

fn source() -> ColorProfile {
    crate::color::icc::gray_lut_tests::parametric_gray_profile()
}

fn reference(x: f64) -> [f64; 3] {
    let xyz = [31595.0, 32768.0, 27030.0]
        .map(|v| (v / 65535.0 * x.powf(1.25)).powf(1.25).powf(1.25) * 65535.0 / 32768.0);
    [
        [3.1339236463378164, -1.6169229392738516, -0.490733723087733],
        [-0.9784210516720576, 1.915842665313229, 0.03339912699596239],
        [
            0.07203553396859233,
            -0.22903203517027076,
            1.4057161576769963,
        ],
    ]
    .map(|row| {
        let v = (row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]).clamp(0.0, 1.0);
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    })
}

#[test]
fn gray_mab_native_domain_preserves_original_curves_and_the_ordinary_route() {
    let profile = ICCProfile::new_from_src_profile(source(), 1)
        .unwrap()
        .with_intent(RenderingIntent::RelativeColorimetric)
        .unwrap();
    assert!(profile.has_float_image_conversion());
    let input: Vec<_> = (0..=u16::MAX).map(|v| f64::from(v) / 65535.0).collect();
    let mut before = vec![0; input.len() * 3];
    profile.convert_native(&input, &mut before).unwrap();
    let mut actual = vec![0.0; before.len()];
    profile.convert_native_f64(&input, &mut actual).unwrap();
    for (&x, pixel) in input.iter().zip(actual.chunks_exact(3)) {
        let expected = reference(x);
        for (&a, e) in pixel.iter().zip(expected) {
            assert!((a - e).abs() <= 1e-12, "{x}: {a} != {e}");
        }
    }
    let mut after = vec![0; before.len()];
    profile.convert_native(&input, &mut after).unwrap();
    assert_eq!(before, after);
    if let Some(root) = std::env::var_os("GLYPHST_MAB_ORIGINALS") {
        let root = std::path::PathBuf::from(root);
        for pcs in ["xyz", "lab"] {
            for kind in ["linear", "nonlinear", "mixed"] {
                let name = format!("{pcs}-{kind}");
                let bytes = std::fs::read(root.join(format!("{name}.icc"))).unwrap();
                let profile = ICCProfile::new(&bytes, 1).unwrap();
                for (index, intent) in INTENTS.into_iter().enumerate() {
                    let profile = profile.with_intent(intent).unwrap();
                    profile.convert_native_f64(&input, &mut actual).unwrap();
                    let expected =
                        std::fs::read(root.join(format!("mab-{name}-intent{index}.f64"))).unwrap();
                    assert_eq!(expected.len(), actual.len() * 8);
                    for (&v, bytes) in actual.iter().zip(expected.chunks_exact(8)) {
                        let e = f64::from_le_bytes(bytes.try_into().unwrap());
                        assert!((v - e).abs() <= 1e-12, "{name} intent{index}: {v} != {e}");
                    }
                }
            }
        }
    }
}

#[test]
fn unselected_gray_mab_tags_preserve_the_original_tone_curve() {
    let input: Vec<_> = (0..=u16::MAX).map(|v| f64::from(v) / 65535.0).collect();
    for pcs in [DataColorSpace::Xyz, DataColorSpace::Lab] {
        let mut source = source();
        source.pcs = pcs;
        source.gray_trc = Some(moxcms::ToneReprCurve::Lut(vec![4 * 256]));
        source.lut_a_to_b_colorimetric = source.lut_a_to_b_perceptual.take();
        let profile = ICCProfile::new(&source.encode().unwrap(), 1).unwrap();
        let mut curve_only = source.clone();
        curve_only.lut_a_to_b_colorimetric = None;
        let reference = ICCProfile::new(&curve_only.encode().unwrap(), 1).unwrap();
        for intent in [RenderingIntent::Perceptual, RenderingIntent::Saturation] {
            let profile = profile.with_intent(intent).unwrap();
            assert!(profile.has_float_image_conversion());
            let mut ordinary = vec![0; input.len() * 3];
            profile.convert_native(&input, &mut ordinary).unwrap();
            let mut actual = vec![0.0; ordinary.len()];
            let mut expected = vec![0.0; ordinary.len()];
            profile.convert_native_f64(&input, &mut actual).unwrap();
            reference
                .with_intent(intent)
                .unwrap()
                .convert_native_f64(&input, &mut expected)
                .unwrap();
            assert_eq!(actual, expected, "{pcs:?} {intent:?}");
            let mut after = vec![0; ordinary.len()];
            profile.convert_native(&input, &mut after).unwrap();
            assert_eq!(ordinary, after);
        }
        source.gray_trc = None;
        assert!(matches!(
            ICCProfile::new(&source.encode().unwrap(), 1)
                .unwrap()
                .with_intent(RenderingIntent::Perceptual),
            Err(ColorConversionError::IccTransform)
        ));
    }
}

#[test]
fn gray_mab_quota_refusals_retry_and_all_owners_release_their_curves() {
    let cache = Cache::with_limits(4, 32 * 1024 * 1024);
    let bytes = source().encode().unwrap();
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
    let owners: Vec<_> = INTENTS
        .into_iter()
        .map(|intent| profile.with_intent(intent).unwrap())
        .collect();
    let mut output = [0.0; 3];
    for owner in &owners {
        owner.convert_native_f64(&[0.375], &mut output).unwrap();
    }
    let retained = cache.stats().icc_bytes;
    let held = cache
        .icc_memory()
        .reserve(32 * 1024 * 1024 - retained - 1024)
        .unwrap();
    assert!(owners[0].convert_native_f64(&[0.5], &mut output).is_none());
    drop(held);
    cache.clear();
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, retained);
    for owner in &owners {
        owner.convert_native_f64(&[0.5], &mut output).unwrap();
    }
    drop(owners);
    assert_eq!(cache.stats().icc_bytes, 0);
}

#[test]
fn gray_mab_wrong_direction_version_and_orphaned_stages_cannot_enter_float_images() {
    let original = source().encode().unwrap();
    let count = u32::from_be_bytes(original[128..132].try_into().unwrap()) as usize;
    let at = (0..count)
        .map(|i| 132 + i * 12)
        .find(|&at| &original[at..at + 4] == b"A2B0")
        .unwrap();
    let offset = u32::from_be_bytes(original[at + 4..at + 8].try_into().unwrap()) as usize;
    for change in 0..7 {
        let mut bytes = original.clone();
        match change {
            0 => bytes[offset..offset + 4].copy_from_slice(b"mBA "),
            1 => bytes[8..12].copy_from_slice(&[2, 0x10, 0, 0]),
            _ => bytes[offset + 12 + 4 * (change - 2)..offset + 16 + 4 * (change - 2)].fill(0),
        }
        if let Some(profile) = ICCProfile::new(&bytes, 1) {
            assert!(!profile.has_float_image_conversion(), "case {change}");
            assert!(profile.native_transform_for(true).is_err());
        }
    }
}

#[test]
fn gray_mab_images_preserve_decode_palette_and_tint_values_before_transfer() {
    let profile = source().encode().unwrap();
    let values = [0_u16, 1, 32767, 32768, 32769, 49152, 65534, 65535];
    let samples: Vec<_> = values.into_iter().flat_map(u16::to_be_bytes).collect();
    let tint = "<</FunctionType 2/Domain[0 1]/C0[.25]/C1[.75]/N 1>>";
    for (space, extra, route) in [
        ("[/ICCBased 5 0 R]".to_string(), "", 0),
        ("/DeviceGray".to_string(), "", 0),
        ("[/ICCBased 5 0 R]".to_string(), "/Decode[1 0]", 1),
        ("[/ICCBased 5 0 R]".to_string(), "/Decode[-.25 1.25]", 2),
        (
            "[/Indexed[/ICCBased 5 0 R]3<0055aaff>]".to_string(),
            "/Decode[0 3]",
            3,
        ),
        (format!("[/Separation/Ink[/ICCBased 5 0 R]{tint}]"), "", 4),
        (format!("[/DeviceN[/Ink][/ICCBased 5 0 R]{tint}]"), "", 4),
    ] {
        image::with_image(
            &format!("/BitsPerComponent 16/ColorSpace{space}{extra}"),
            &samples,
            None,
            (values.len() as u32, 1),
            &profile,
            &Cache::new(),
            |image| {
                let output =
                    crate::x_object::decode_rgb_f64(image, (values.len() * 24) as u64, || true)
                        .unwrap();
                assert_eq!(output.data.len(), values.len() * 24);
                for (&v, pixel) in values.iter().zip(output.data.chunks_exact(24)) {
                    let x = f64::from(v) / 65535.0;
                    let x = match route {
                        1 => 1.0 - x,
                        2 => (-0.25 + 1.5 * x).clamp(0.0, 1.0),
                        3 => (x * 3.0).round() / 3.0,
                        4 => f64::from(0.25_f32 + 0.5 * (x as f32)),
                        _ => x,
                    };
                    for (bytes, e) in pixel.chunks_exact(8).zip(reference(x)) {
                        let a = f64::from_le_bytes(bytes.try_into().unwrap());
                        assert!((a - e).abs() <= 1e-12, "{space} {v}: {a} != {e}");
                    }
                }
            },
        );
    }
}
