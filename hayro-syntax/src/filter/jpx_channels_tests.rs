use super::opacity_fixtures::{ALPHA_FIRST, RGB8};
use super::*;

const RGBA: &[[u16; 3]] = &[[0, 0, 1], [1, 0, 2], [2, 0, 3], [3, 1, 0]];

fn boxed(name: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = ((data.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    out
}

fn boxes(data: &[u8]) -> Vec<(&[u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        out.push((
            data[offset + 4..offset + 8].try_into().unwrap(),
            &data[offset + 8..offset + length],
        ));
        offset += length;
    }
    out
}

fn definitions(entries: &[[u16; 3]]) -> Vec<u8> {
    let mut out = (entries.len() as u16).to_be_bytes().to_vec();
    out.extend(entries.iter().flatten().flat_map(|v| v.to_be_bytes()));
    out
}

fn rewrite(
    input: &[u8],
    cdef: Option<&[u8]>,
    extra: &[(&[u8; 4], &[u8])],
    signed: Option<usize>,
) -> Vec<u8> {
    let descriptors = signed.map(|signed| {
        let codestream = boxes(input)
            .into_iter()
            .find(|(name, _)| *name == b"jp2c")
            .unwrap()
            .1;
        let count = u16::from_be_bytes(codestream[40..42].try_into().unwrap()) as usize;
        let mut bits = (0..count)
            .map(|index| codestream[42 + index * 3])
            .collect::<Vec<_>>();
        bits[signed] |= 128;
        bits
    });
    let mut out = Vec::new();
    for (name, value) in boxes(input) {
        let mut value = value.to_vec();
        if name == b"jp2h" {
            value = boxes(&value)
                .into_iter()
                .filter(|(name, _)| ![b"cdef", b"pclr", b"cmap", b"bpcc"].contains(name))
                .flat_map(|(name, data)| {
                    if name == b"ihdr"
                        && let Some(descriptors) = &descriptors
                    {
                        let mut header = data.to_vec();
                        header[10] = 255;
                        let mut encoded = boxed(name, &header);
                        encoded.extend(boxed(b"bpcc", descriptors));
                        encoded
                    } else {
                        boxed(name, data)
                    }
                })
                .collect();
            if let Some(cdef) = cdef {
                value.extend(boxed(b"cdef", cdef));
            }
            for (name, data) in extra {
                value.extend(boxed(name, data));
            }
        }
        if name == b"jp2c"
            && let Some(signed) = signed
        {
            assert_eq!(&value[..4], b"\xffO\xffQ");
            value[42 + signed * 3] |= 128;
        }
        out.extend(boxed(name, &value));
    }
    out
}

fn parameters() -> ImageDecodeParams {
    ImageDecodeParams {
        num_components: Some(3),
        width: 1,
        height: 1,
        jpx_alpha_mode: Some(EmbeddedImageAlphaMode::Unassociated),
        ..Default::default()
    }
}

fn with_color_specification(input: &[u8], specification: &[u8]) -> Vec<u8> {
    boxes(input)
        .into_iter()
        .flat_map(|(name, data)| {
            if name == b"jp2h" {
                let mut header: Vec<u8> = boxes(data)
                    .into_iter()
                    .filter(|(name, _)| *name != b"colr")
                    .flat_map(|(name, value)| boxed(name, value))
                    .collect();
                header.extend(boxed(b"colr", specification));
                boxed(name, &header)
            } else {
                boxed(name, data)
            }
        })
        .collect()
}

#[test]
fn explicit_colour_space_precedes_container_colour_count_validation() {
    let data = with_color_specification(RGB8, &[1, 0, 0, 0, 0, 0, 12]);
    assert!(
        decode(
            &data,
            &ImageDecodeParams {
                num_components: None,
                ..parameters()
            }
        )
        .is_none()
    );
    let decoded = decode(&data, &parameters()).unwrap();
    let samples = decoded.image_data.unwrap().jpx_samples.unwrap();
    assert_eq!(
        samples
            .components
            .iter()
            .map(|c| c.samples[0])
            .collect::<Vec<_>>(),
        [64.0, 128.0, 32.0, 128.0]
    );
    // Unsupported container colour data falls back by declared colour count,
    // independently of auxiliary/opacity codestream components.
    assert_rgba(
        &with_color_specification(
            RGB8,
            &[
                4, 0, 1, 0x17, 0x2a, 0x9c, 0x50, 0x13, 0x88, 0x4e, 0x11, 0xa6, 0x43, 0x25, 0x76,
                0xb3, 0x42, 0xd9, 0x0c,
            ],
        ),
        [64.0, 128.0, 32.0, 128.0],
    );
}

#[test]
fn pdf_indexed_preserves_original_indices_but_validates_container_channels() {
    let data = with_color_specification(fixtures::PALETTE16, &[1, 0, 0, 0, 0, 0, 12]);
    let params = ImageDecodeParams {
        num_components: Some(1),
        is_indexed: true,
        width: 3,
        height: 2,
        ..Default::default()
    };
    let decoded = decode(&data, &params).unwrap();
    assert_eq!(decoded.image_data.unwrap().bits_per_component, 1);
    assert_eq!(decoded.data.as_ref(), [0x20, 0xa0]); // rows 001 and 101
    let invalid = rewrite(
        &data,
        Some(&definitions(&[[0, 0, 1], [1, 0, 1], [2, 0, 3]])),
        &[
            (b"pclr", &[0, 1, 3, 7, 7, 7, 0, 0, 0]),
            (b"cmap", &[0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 2]),
        ],
        None,
    );
    assert!(decode(&invalid, &params).is_none());
}

#[test]
fn duplicate_header_or_codestream_cannot_replace_validated_channel_state() {
    for name in [b"jp2h", b"jp2c"] {
        let value = boxes(RGB8)
            .into_iter()
            .find(|(kind, _)| *kind == name)
            .unwrap()
            .1;
        let data = [RGB8, boxed(name, value).as_slice()].concat();
        assert!(decode(&data, &parameters()).is_none());
    }
}

fn assert_rgba(data: &[u8], expected: [f32; 4]) {
    for num_components in [None, Some(3)] {
        let decoded = decode(
            data,
            &ImageDecodeParams {
                num_components,
                ..parameters()
            },
        )
        .unwrap();
        let metadata = decoded.image_data.unwrap();
        assert_eq!(metadata.alpha.unwrap(), [expected[3] as u8]);
        let samples = metadata.jpx_samples.unwrap();
        assert_eq!(samples.components.len(), 4);
        for (component, expected) in samples.components.iter().zip(expected) {
            assert_eq!(component.bits_per_component, 8);
            assert_eq!(component.samples, [expected]);
        }
    }
}

#[test]
fn repeated_opacity_associations_resolve_one_original_channel_before_colour_ordering() {
    for kind in [1, 2] {
        let cdef = definitions(&[
            [0, kind, 3],
            [1, 0, 1],
            [0, kind, 1],
            [3, 0, 3],
            [0, kind, 2],
            [2, 0, 2],
        ]);
        assert_rgba(
            &rewrite(ALPHA_FIRST, Some(&cdef), &[], None),
            [64.0, 128.0, 32.0, 128.0],
        );
    }
}

#[test]
fn auxiliary_channels_do_not_become_colour_or_inferred_opacity() {
    for kind in [0, 65535] {
        let cdef = definitions(&[[0, kind, 65535], [1, 0, 1], [2, 0, 2], [3, 0, 3]]);
        let data = rewrite(RGB8, Some(&cdef), &[], None);
        for num_components in [None, Some(3)] {
            let decoded = decode(
                &data,
                &ImageDecodeParams {
                    num_components,
                    jpx_alpha_mode: None,
                    ..parameters()
                },
            )
            .unwrap();
            assert_eq!(decoded.data.as_ref(), [128, 32, 128]);
            assert!(decoded.image_data.unwrap().alpha.is_none());
        }
        assert!(decode(&data, &parameters()).is_none());
    }
}

#[test]
fn palette_channel_count_mapping_and_direct_opacity_survive_explicit_pdf_space() {
    // Raw component 0 is index 64. Components 1 and 2 are unused; component 3
    // is direct opacity. Palette column count is not codestream/channel count.
    let mut palette = vec![0, 65, 3, 7, 7, 7];
    palette.resize(6 + 64 * 3, 0);
    palette.extend_from_slice(&[64, 128, 32]);
    let mapping = [0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 1, 2, 0, 3, 0, 0];
    let cdef = definitions(RGBA);
    for signed_unused in [None, Some(1)] {
        let data = rewrite(
            RGB8,
            Some(&cdef),
            &[(b"pclr", &palette), (b"cmap", &mapping)],
            signed_unused,
        );
        assert_rgba(&data, [64.0, 128.0, 32.0, 128.0]);
    }
    let signed_alpha = rewrite(
        RGB8,
        Some(&cdef),
        &[(b"pclr", &palette), (b"cmap", &mapping)],
        Some(3),
    );
    assert!(decode(&signed_alpha, &parameters()).is_none());
}

#[test]
fn opacity_signedness_follows_direct_mapping_instead_of_codestream_position() {
    let palette = [0, 1, 1, 7, 0];
    let mapping = [0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0];
    let cdef = definitions(RGBA);
    let extra = [(b"pclr", palette.as_slice()), (b"cmap", mapping.as_slice())];
    assert_rgba(
        &rewrite(ALPHA_FIRST, Some(&cdef), &extra, None),
        [64.0, 128.0, 32.0, 128.0],
    );
    assert!(
        decode(
            &rewrite(ALPHA_FIRST, Some(&cdef), &extra, Some(0)),
            &parameters()
        )
        .is_none()
    );
}

#[test]
fn malformed_or_incomplete_channel_definitions_never_fall_back_to_a_guessed_mask() {
    for entries in [
        vec![[0, 0, 1], [1, 0, 1], [2, 0, 3], [3, 1, 0]], // duplicate colour
        vec![[0, 3, 1], [1, 0, 2], [2, 0, 3], [3, 1, 0]], // reserved type
        vec![[0, 0, 0], [1, 0, 2], [2, 0, 3], [3, 1, 0]], // whole-image colour
        vec![[0, 0, 1], [1, 0, 2], [2, 0, 3]],            // missing channel
        vec![[0, 0, 1], [1, 0, 2], [2, 0, 3], [4, 1, 0]], // nonexistent channel
        vec![[0, 0, 1], [1, 0, 2], [2, 0, 3], [3, 1, 1]], // partial opacity
        vec![[0, 0, 1], [1, 0, 2], [2, 0, 3], [3, 1, 0], [3, 1, 0]], // duplicate description
        vec![
            [0, 0, 1],
            [1, 0, 2],
            [2, 0, 3],
            [3, 1, 1],
            [3, 2, 2],
            [3, 1, 3],
        ], // mixed opacity types
    ] {
        let data = rewrite(RGB8, Some(&definitions(&entries)), &[], None);
        assert!(decode(&data, &parameters()).is_none(), "{entries:?}");
    }
    let valid = definitions(RGBA);
    for malformed in [
        &valid[..valid.len() - 1],
        &[valid.as_slice(), b"!"].concat(),
        &[0, 0],
    ] {
        assert!(decode(&rewrite(RGB8, Some(malformed), &[], None), &parameters()).is_none());
    }
    assert!(
        decode(
            &rewrite(RGB8, Some(&valid), &[(b"cdef", &valid)], None),
            &parameters()
        )
        .is_none()
    );
    assert!(decode(&rewrite(RGB8, None, &[], None), &parameters()).is_none());
    assert!(decode(&rewrite(RGB8, Some(&valid), &[], Some(3)), &parameters()).is_none());
}

#[test]
fn invalid_palette_mapping_metadata_is_not_silently_discarded() {
    let palette = [0, 1, 1, 7, 0];
    let cdef = definitions(RGBA);
    for mapping in [
        &[][..],
        &[0, 0, 0][..],
        &[0, 4, 0, 0][..],
        &[0, 0, 1, 1][..],
        &[0, 0, 0, 1][..],
        &[0, 0, 2, 0][..],
    ] {
        let data = rewrite(
            RGB8,
            Some(&cdef),
            &[(b"pclr", &palette), (b"cmap", mapping)],
            None,
        );
        assert!(decode(&data, &parameters()).is_none());
    }
    for extra in [
        vec![(b"pclr", palette.as_slice())],
        vec![(b"cmap", &[0, 0, 0, 0][..])],
    ] {
        assert!(decode(&rewrite(RGB8, Some(&cdef), &extra, None), &parameters()).is_none());
    }
}
