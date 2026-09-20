use super::*;

const TO_XYZ: [[f64; 3]; 3] = [
    [0.4360747, 0.3850649, 0.1430804],
    [0.2225045, 0.7168786, 0.0606169],
    [0.0139322, 0.0971045, 0.7141733],
];
const FROM_XYZ: [[f64; 3]; 3] = [
    [3.1338561, -1.6168667, -0.4906146],
    [-0.9787684, 1.9161415, 0.0334540],
    [0.0719453, -0.2289914, 1.4052427],
];
// Independent ICC.1:2004-10 Table 12 control, not imported from production.
const BLACK: [f64; 3] = [0.003357, 0.003479, 0.002869];
const WHITE: [f64; 3] = [0.9642, 1.0, 0.8249];
const INTENTS: [RenderingIntent; 4] = [
    RenderingIntent::Perceptual,
    RenderingIntent::RelativeColorimetric,
    RenderingIntent::Saturation,
    RenderingIntent::AbsoluteColorimetric,
];

fn fixed(value: f64) -> [u8; 4] {
    ((value * 65536.0).round() as i32).to_be_bytes()
}

fn text_tag(version: u8, copyright: bool) -> Vec<u8> {
    let text = if copyright {
        "Authored Glyphst fixture; MIT OR Apache-2.0"
    } else {
        "Glyphst ICC ColorSpace class diagnostic"
    };
    if version == 2 {
        let mut bytes = if copyright {
            b"text\0\0\0\0".to_vec()
        } else {
            let mut bytes = b"desc\0\0\0\0".to_vec();
            bytes.extend(((text.len() + 1) as u32).to_be_bytes());
            bytes
        };
        bytes.extend(text.as_bytes());
        bytes.push(0);
        if !copyright {
            bytes.extend([0; 78]);
        }
        bytes
    } else {
        let text: Vec<_> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        let mut bytes = b"mluc\0\0\0\0".to_vec();
        bytes.extend(1_u32.to_be_bytes());
        bytes.extend(12_u32.to_be_bytes());
        bytes.extend(b"enUS");
        bytes.extend((text.len() as u32).to_be_bytes());
        bytes.extend(28_u32.to_be_bytes());
        bytes.extend(text);
        bytes
    }
}

fn lut(components: usize, exponent: i32, black: bool, reverse: bool) -> Vec<u8> {
    let (inputs, outputs, grid, entries) = if reverse {
        (3, components, 17_usize, 2_usize)
    } else {
        (components, 3, 2, 256)
    };
    let mut bytes = b"mft2\0\0\0\0".to_vec();
    bytes.extend([inputs as u8, outputs as u8, grid as u8, 0]);
    bytes.extend(
        [1., 0., 0., 0., 1., 0., 0., 0., 1.]
            .into_iter()
            .flat_map(fixed),
    );
    bytes.extend((entries as u16).to_be_bytes());
    bytes.extend(2_u16.to_be_bytes());
    for _ in 0..inputs {
        for i in 0..entries {
            let value = (i as f64 / (entries - 1) as f64).powi(if reverse { 1 } else { exponent });
            bytes.extend(((value * 65535.0).round() as u16).to_be_bytes());
        }
    }
    for index in 0..grid.pow(inputs as u32) {
        let values: Vec<_> = (0..inputs)
            .map(|channel| {
                (index / grid.pow((inputs - channel - 1) as u32) % grid) as f64 / (grid - 1) as f64
            })
            .collect();
        let output: Vec<f64> = if reverse {
            let xyz = std::array::from_fn::<_, 3, _>(|i| {
                let v = values[i] * 65535.0 / 32768.0;
                if black {
                    (v - BLACK[i]) * WHITE[i] / (WHITE[i] - BLACK[i])
                } else {
                    v
                }
            });
            let rgb = FROM_XYZ.map(|row| {
                row.into_iter()
                    .zip(xyz)
                    .map(|(a, b)| a * b)
                    .sum::<f64>()
                    .clamp(0.0, 1.0)
            });
            let k = (rgb.into_iter().map(|v| 1.0 - v).fold(1.0, f64::min) * 4.0).min(1.0);
            let device: Vec<_> = if components == 3 {
                rgb.to_vec()
            } else {
                rgb.into_iter()
                    .map(|v| ((1.0 - 0.25 * k - v) / 0.75).clamp(0.0, 1.0))
                    .chain([k])
                    .collect()
            };
            device
                .into_iter()
                .map(|v| v.powf(1.0 / f64::from(exponent)) * 65535.0)
                .collect()
        } else {
            let rgb = std::array::from_fn::<_, 3, _>(|i| {
                if components == 3 {
                    values[i]
                } else {
                    1.0 - 0.75 * values[i] - 0.25 * values[3]
                }
            });
            TO_XYZ
                .into_iter()
                .enumerate()
                .map(|(i, row)| {
                    let xyz: f64 = row.into_iter().zip(rgb).map(|(a, b)| a * b).sum();
                    (if black {
                        xyz * (1.0 - BLACK[i] / WHITE[i]) + BLACK[i]
                    } else {
                        xyz
                    }) * 32768.0
                })
                .collect()
        };
        for value in output {
            bytes.extend((value.round() as u16).to_be_bytes());
        }
    }
    bytes.extend([0, 0, 255, 255].repeat(outputs));
    bytes
}

// Original LUT16 ramps. The CMYK model is affine, so allowed differences
// between tetrahedral and multilinear interpolation do not change the oracle.
// Inverse tags supply class structure, not a device-substitution proof.
pub(super) fn profile_bytes(version: u8, components: usize, separate_intents: bool) -> Vec<u8> {
    let mut white = b"XYZ \0\0\0\0".to_vec();
    white.extend(WHITE.into_iter().flat_map(fixed));
    let mut tags = vec![
        (*b"desc", text_tag(version, false)),
        (*b"cprt", text_tag(version, true)),
        (*b"wtpt", white),
    ];
    for tag in 0..if separate_intents { 3 } else { 1 } {
        let black = version == 4 && tag != 1;
        tags.push((
            [b'A', b'2', b'B', b'0' + tag],
            lut(components, i32::from(tag) + 1, black, false),
        ));
        tags.push((
            [b'B', b'2', b'A', b'0' + tag],
            lut(components, i32::from(tag) + 1, black, true),
        ));
    }
    let mut bytes = vec![0; 132 + 12 * tags.len()];
    bytes[8..12].copy_from_slice(&[version, 0x20, 0, 0]);
    bytes[12..16].copy_from_slice(b"spac");
    bytes[16..20].copy_from_slice(if components == 3 { b"RGB " } else { b"CMYK" });
    bytes[20..24].copy_from_slice(b"XYZ ");
    bytes[24..36].copy_from_slice(
        &[2026_u16, 9, 20, 0, 0, 0]
            .into_iter()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>(),
    );
    bytes[36..40].copy_from_slice(b"acsp");
    bytes[68..80].copy_from_slice(&WHITE.into_iter().flat_map(fixed).collect::<Vec<_>>());
    bytes[128..132].copy_from_slice(&(tags.len() as u32).to_be_bytes());
    for (i, (tag, payload)) in tags.into_iter().enumerate() {
        let at = 132 + i * 12;
        let offset = bytes.len() as u32;
        bytes[at..at + 4].copy_from_slice(&tag);
        bytes[at + 4..at + 8].copy_from_slice(&offset.to_be_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend(payload);
        bytes.resize(bytes.len().next_multiple_of(4), 0);
    }
    let size = bytes.len() as u32;
    bytes[..4].copy_from_slice(&size.to_be_bytes());
    bytes
}

fn expected(pixel: &[u8], version: u8, separate_intents: bool, intent: usize) -> [u8; 3] {
    let exponent = if separate_intents {
        [1, 2, 3, 2][intent]
    } else {
        1
    };
    let values: Vec<_> = pixel
        .iter()
        .map(|v| (f64::from(*v) / 255.0).powi(exponent))
        .collect();
    let mut rgb = std::array::from_fn(|i| {
        if pixel.len() == 3 {
            values[i]
        } else {
            1.0 - 0.75 * values[i] - 0.25 * values[3]
        }
    });
    if version == 4 && !separate_intents && matches!(intent, 1 | 3) {
        // Colorimetric fallback uses the AToB0 values without perceptual PCS linking.
        let xyz = std::array::from_fn::<_, 3, _>(|i| {
            let xyz: f64 = TO_XYZ[i].into_iter().zip(rgb).map(|(a, b)| a * b).sum();
            xyz * (1.0 - BLACK[i] / WHITE[i]) + BLACK[i]
        });
        rgb = FROM_XYZ.map(|row| row.into_iter().zip(xyz).map(|(a, b)| a * b).sum());
    }
    rgb.map(|v| {
        let encoded = if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

#[test]
fn colorspace_lut_intents_match_independent_controls_across_batch_boundaries() {
    for version in [2, 4] {
        for components in [3, 4] {
            for separate_intents in [false, true] {
                let bytes = profile_bytes(version, components, separate_intents);
                let profile = ICCProfile::new(&bytes, components).unwrap();
                drop(bytes);
                for (intent_index, intent) in INTENTS.into_iter().enumerate() {
                    let bound = profile.with_intent(intent).unwrap();
                    for length in [0, 1, 1023, 1024, 1025, 2051] {
                        let input: Vec<_> = (0..length)
                            .flat_map(|v| {
                                (0..components).map(move |channel| (v * (channel + 1)) as u8)
                            })
                            .collect();
                        let mut output = vec![0; length * 3];
                        bound.convert(&input, &mut output).unwrap();
                        for (pixel, actual) in
                            input.chunks_exact(components).zip(output.chunks_exact(3))
                        {
                            let control = expected(pixel, version, separate_intents, intent_index);
                            assert!(
                                actual.iter().zip(control).all(|(a, b)| a.abs_diff(b) <= 1),
                                "v{version} N={components} tags={separate_intents} {intent:?}: {pixel:?} -> {actual:?}, expected {control:?}"
                            );
                        }
                        if components == 3 {
                            let mut in_place = input.clone();
                            bound.convert_in_place(&mut in_place).unwrap();
                            assert_eq!(in_place, output);
                        }
                        assert!(
                            bound
                                .convert(&input, &mut vec![0; length * 3 + 1])
                                .is_none()
                        );
                    }
                    assert!(bound.convert(&[0; 5], &mut [0; 3]).is_none());
                    let again = profile.with_intent(intent).unwrap();
                    assert!(std::ptr::eq(
                        bound.transform().unwrap(),
                        again.transform().unwrap()
                    ));
                }
            }
        }
    }
}

#[test]
fn colorspace_admission_preserves_profile_class_version_and_channel_limits() {
    for version in [2, 4] {
        for components in [3, 4] {
            let mut bytes = profile_bytes(version, components, false);
            for class in [b"scnr", b"mntr", b"prtr", b"spac"] {
                bytes[12..16].copy_from_slice(class);
                let profile = ICCProfile::new(&bytes, components).unwrap();
                profile
                    .with_intent(RenderingIntent::RelativeColorimetric)
                    .unwrap();
            }
            for class in [b"link", b"abst", b"nmcl"] {
                bytes[12..16].copy_from_slice(class);
                assert!(ICCProfile::new(&bytes, components).is_none());
            }
            bytes[12..16].copy_from_slice(b"spac");
            assert!(ICCProfile::new(&bytes, 7 - components).is_none());
            bytes[8] = 5;
            assert!(ICCProfile::new(&bytes, components).is_none());
            assert!(ICCProfile::new(&bytes[..127], components).is_none());
        }
    }
}

#[test]
fn colorspace_class_requires_both_perceptual_directions() {
    for version in [2, 4] {
        for components in [3, 4] {
            for missing in [b"A2B0", b"B2A0"] {
                let mut bytes = profile_bytes(version, components, false);
                let count = u32::from_be_bytes(bytes[128..132].try_into().unwrap()) as usize;
                let entry = bytes[132..132 + count * 12]
                    .chunks_exact_mut(12)
                    .find(|entry| &entry[..4] == missing)
                    .unwrap();
                entry[..4].copy_from_slice(b"test");
                assert!(ColorProfile::new_from_slice(&bytes).is_ok());
                assert!(ICCProfile::new(&bytes, components).is_none());
            }
        }
        let mut bytes = ColorProfile::new_srgb().encode().unwrap();
        bytes[8] = version;
        bytes[12..16].copy_from_slice(b"spac");
        assert!(ICCProfile::new(&bytes, 3).is_none());
    }
}
