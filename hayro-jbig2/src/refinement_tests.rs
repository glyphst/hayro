use super::{Decoder, DecoderContext, Image, refinement_test_samples::*};
use crate::bitmap::Bitmap;
use crate::decode::{CombinationOperator, generic_refinement};
use crate::reader::Reader;
use alloc::{vec, vec::Vec};

fn segment(number: u8, kind: u8, payload: &[u8], refs: &[u8]) -> Vec<u8> {
    let mut data = vec![0, 0, 0, number, kind];
    if refs.len() < 5 {
        let retain = ((1_u16 << (refs.len() + 1)) - 1) as u8;
        data.push(((refs.len() as u8) << 5) | retain);
    } else {
        data.extend((0xe000_0000 | refs.len() as u32).to_be_bytes());
        for start in (0..=refs.len()).step_by(8) {
            let bits = (refs.len() + 1 - start).min(8);
            data.push(((1_u16 << bits) - 1) as u8);
        }
    }
    data.extend(refs);
    data.push(1);
    data.extend((payload.len() as u32).to_be_bytes());
    data.extend(payload);
    data
}

fn page(w: u32, h: u32, flags: u8) -> Vec<u8> {
    let mut data = Vec::new();
    for value in [w, h, 0, 0] {
        data.extend(value.to_be_bytes());
    }
    data.extend([flags, 0, 0]);
    segment(1, 48, &data, &[])
}

fn region(rect: [u32; 4], op: u8, header: &[u8], encoded: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    for value in rect {
        data.extend(value.to_be_bytes());
    }
    data.push(op);
    data.extend(header);
    data.extend(encoded);
    data
}

fn generic(number: u8, rect: [u32; 4], op: u8, intermediate: bool, data: &[u8]) -> Vec<u8> {
    segment(
        number,
        if intermediate { 36 } else { 39 },
        &region(rect, op, &[0, 3, 255, 253, 255, 2, 254, 254, 254], data),
        &[],
    )
}

fn refine(number: u8, kind: u8, rect: [u32; 4], op: u8, refs: &[u8], data: &[u8]) -> Vec<u8> {
    segment(
        number,
        kind,
        &region(rect, op, &[0, 255, 255, 255, 255], data),
        refs,
    )
}

#[derive(Default)]
struct Pixels(Vec<u8>);
impl Decoder for Pixels {
    fn push_pixel(&mut self, black: bool) {
        self.0.push(u8::from(black));
    }
    fn push_pixel_chunk(&mut self, black: bool, chunks: u32) {
        self.0
            .resize(self.0.len() + chunks as usize * 8, u8::from(black));
    }
    fn next_line(&mut self) {}
}

fn decode(data: &[u8], ctx: &mut DecoderContext) -> Vec<u8> {
    let image = Image::new_embedded_pdf(data, None).unwrap();
    let mut output = Pixels::default();
    image.decode_with(&mut output, ctx).unwrap();
    output.0
}

fn base(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| (0..w).map(move |x| u8::from((x * 7 + y * 3 + x * y) % 5 != 0)))
        .collect()
}

fn target(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| (0..w).map(move |x| u8::from((x * 3 + y * 5 + x * y) % 7 < 3)))
        .collect()
}

fn composite(mut page: Vec<u8>, w: usize, rect: [u32; 4], pixels: &[u8], op: u8) -> Vec<u8> {
    let [rw, rh, ox, oy] = rect.map(u64::from);
    for y in 0..rh {
        for x in 0..rw {
            let px = ox + x;
            let py = oy + y;
            if px < w as u64 && py < (page.len() / w) as u64 {
                let index = py as usize * w + px as usize;
                let a = page[index];
                let b = pixels[(y * rw + x) as usize];
                page[index] = [a | b, a & b, a ^ b, 1 ^ (a ^ b), b][op as usize];
            }
        }
    }
    page
}

#[test]
fn page_refinement_uses_a_bounded_local_reference_for_both_templates() {
    let rect = [9, 7, 4, 2];
    let expected = composite(base(17, 11), 17, rect, &target(9, 7), 4);
    let mut ctx = DecoderContext::default();
    for kind in [42, 43] {
        for (header, data) in [
            (&[0, 255, 255, 255, 255][..], TARGET),
            (&[1][..], TEMPLATE1),
            (&[0, 254, 0, 2, 1][..], CUSTOM),
        ] {
            let bytes = [
                page(17, 11, 99),
                generic(2, [17, 11, 0, 0], 4, false, BASE),
                segment(3, kind, &region(rect, 4, header, data), &[]),
            ]
            .concat();
            assert_eq!(decode(&bytes, &mut ctx), expected);
        }
    }
}

#[test]
fn page_refinement_preserves_word_boundaries_and_untouched_pixels() {
    for (x, encoded) in [(31, WIDE_TARGET), (30, WIDE_OFFSET30)] {
        let rect = [33, 9, x, 3];
        let bytes = [
            page(73, 15, 99),
            generic(2, [73, 15, 0, 0], 4, false, WIDE_BASE),
            refine(3, 43, rect, 4, &[], encoded),
        ]
        .concat();
        let expected = composite(base(73, 15), 73, rect, &target(33, 9), 4);
        assert_eq!(decode(&bytes, &mut DecoderContext::default()), expected);
    }
}

#[test]
fn page_refinement_clips_edges_and_unsigned_page_locations() {
    for (x, y, encoded) in [
        (13, 8, EDGE_TARGET),
        (0xffff_fffc, 2, ZERO_TARGET),
        (4, 0xffff_fffe, ZERO_TARGET),
        (0x8000_0004, 2, ZERO_TARGET),
        (4, 0x8000_0002, ZERO_TARGET),
    ] {
        let rect = [9, 7, x, y];
        let bytes = [
            page(17, 11, 99),
            generic(2, [17, 11, 0, 0], 4, false, BASE),
            refine(3, 43, rect, 4, &[], encoded),
        ]
        .concat();
        assert_eq!(
            decode(&bytes, &mut DecoderContext::default()),
            composite(base(17, 11), 17, rect, &target(9, 7), 4)
        );
    }
}

#[test]
fn full_page_refinement_supports_page_and_auxiliary_references() {
    for auxiliary in [false, true] {
        let bytes = [
            page(9, 7, 99),
            generic(2, [9, 7, 0, 0], 4, auxiliary, REFERENCE),
            refine(
                3,
                43,
                [9, 7, 0, 0],
                4,
                if auxiliary { &[2] } else { &[] },
                TARGET,
            ),
        ]
        .concat();
        assert_eq!(decode(&bytes, &mut DecoderContext::default()), target(9, 7));
    }
}

#[test]
fn auxiliary_refinement_moves_buffers_through_chains_and_resets_context() {
    let rect = [9, 7, 4, 2];
    let mut ctx = DecoderContext::default();
    for _ in 0..2 {
        for op in 0..5 {
            for chain in [false, true] {
                let mut parts = vec![
                    page(17, 11, 99),
                    generic(2, [17, 11, 0, 0], 4, false, BASE),
                    generic(3, rect, op, true, REFERENCE),
                    refine(4, if chain { 40 } else { 43 }, rect, op, &[3], TARGET),
                ];
                let mut expected = target(9, 7);
                if chain {
                    parts.push(refine(5, 43, rect, op, &[4], CHAIN_TARGET));
                    expected.iter_mut().for_each(|pixel| *pixel ^= 1);
                }
                assert_eq!(
                    decode(&parts.concat(), &mut ctx),
                    composite(base(17, 11), 17, rect, &expected, op)
                );
            }
        }
    }
}

#[test]
fn refinement_rejects_mismatched_auxiliary_geometry_or_operator() {
    for (rect, op) in [
        ([8, 7, 4, 2], 4),
        ([9, 6, 4, 2], 4),
        ([9, 7, 3, 2], 4),
        ([9, 7, 4, 3], 4),
        ([9, 7, 4, 2], 0),
    ] {
        let bytes = [
            page(17, 11, 99),
            generic(2, [9, 7, 4, 2], 4, true, REFERENCE),
            refine(3, 43, rect, op, &[2], TARGET),
        ]
        .concat();
        let mut output = Pixels::default();
        assert!(
            Image::new_embedded_pdf(&bytes, None)
                .unwrap()
                .decode(&mut output)
                .is_err()
        );
        assert!(output.0.is_empty());
    }
}

#[test]
fn pdf_rejects_wrong_reference_types_counts_reuse_and_orphans() {
    let rect = [9, 7, 4, 2];
    let aux = generic(2, rect, 4, true, REFERENCE);
    for parts in [
        vec![aux.clone()],
        vec![refine(2, 40, rect, 4, &[], TARGET)],
        vec![
            generic(2, rect, 4, false, REFERENCE),
            refine(3, 43, rect, 4, &[2], TARGET),
        ],
        vec![
            segment(2, 0, &[], &[]),
            refine(3, 43, rect, 4, &[2], TARGET),
        ],
        vec![
            aux.clone(),
            refine(3, 43, rect, 4, &[2], TARGET),
            refine(4, 43, rect, 4, &[2], TARGET),
        ],
        vec![aux.clone(), refine(3, 43, rect, 4, &[2, 2], TARGET)],
        vec![aux.clone(), refine(3, 40, rect, 4, &[2], TARGET)],
        vec![refine(2, 43, rect, 0, &[], TARGET)],
    ] {
        let bytes = [page(17, 11, 99), parts.concat()].concat();
        assert!(Image::new_embedded_pdf(&bytes, None).is_err());
    }
    // Extension references do not consume auxiliary buffers (T.88 §7.3.1).
    let bytes = [
        page(17, 11, 99),
        aux,
        segment(3, 62, &[0x20, 0, 0, 0, 0], &[2]),
        refine(4, 43, rect, 4, &[2], TARGET),
    ]
    .concat();
    assert!(Image::new_embedded_pdf(&bytes, None).is_ok());
}

#[test]
fn pdf_enforces_segment_reference_contracts_and_table_caps() {
    for kind in [16, 36, 38, 39, 48, 50, 52, 53] {
        let bytes = [
            page(17, 11, 99),
            segment(2, 0, &[], &[]),
            segment(3, kind, &[], &[2]),
        ]
        .concat();
        assert!(
            Image::new_embedded_pdf(&bytes, None).is_err(),
            "kind {kind}"
        );
    }
    for kind in [20, 22, 23] {
        for refs in [&[][..], &[2][..], &[2, 2][..]] {
            let bytes = [
                page(17, 11, 99),
                segment(2, 0, &[], &[]),
                segment(3, kind, &[], refs),
            ]
            .concat();
            assert!(Image::new_embedded_pdf(&bytes, None).is_err());
        }
    }
    for (kind, cap) in [(0, 4), (4, 8), (6, 8), (7, 8)] {
        for count in [cap, cap + 1] {
            let mut parts = vec![page(17, 11, 99)];
            for index in 0..count {
                parts.push(segment(index + 2, 53, &[], &[]));
            }
            let refs: Vec<_> = (2..count + 2).collect();
            parts.push(segment(20, kind, &region([9, 7, 4, 2], 4, &[], &[]), &refs));
            if kind == 4 {
                parts.push(refine(21, 43, [9, 7, 4, 2], 4, &[20], TARGET));
            }
            assert_eq!(
                Image::new_embedded_pdf(&parts.concat(), None).is_ok(),
                count == cap
            );
        }
    }
}

#[test]
fn pdf_requires_page_flags_and_honors_replace_exception() {
    for (flags, auxiliary) in [(97, false), (67, true), (227, false), (35, true)] {
        let bytes = [
            page(9, 7, flags),
            generic(2, [9, 7, 0, 0], 4, auxiliary, REFERENCE),
            refine(
                3,
                43,
                [9, 7, 0, 0],
                4,
                if auxiliary { &[2] } else { &[] },
                TARGET,
            ),
        ]
        .concat();
        assert!(Image::new_embedded_pdf(&bytes, None).is_err());
    }
    // No direct override is needed for a page refinement's mandated REPLACE.
    let bytes = [
        page(9, 7, 3),
        generic(2, [9, 7, 0, 0], 0, false, REFERENCE),
        refine(3, 43, [9, 7, 0, 0], 4, &[], TARGET),
    ]
    .concat();
    assert_eq!(decode(&bytes, &mut DecoderContext::default()), target(9, 7));
}

#[test]
fn refinement_rejects_reserved_flags_and_noncausal_target_at_pixels() {
    for header in [
        [4, 255, 255, 255, 255],
        [0, 0, 0, 255, 255],
        [0, 255, 1, 255, 255],
        [0, 1, 0, 255, 255],
    ] {
        let data = region([9, 7, 0, 0], 4, &header, TARGET);
        assert!(generic_refinement::parse(&mut Reader::new(&data)).is_err());
    }
    for at in [[254, 0, 2, 1], [128, 128, 127, 127]] {
        let header = [&[0][..], &at].concat();
        let data = region([9, 7, 0, 0], 4, &header, TARGET);
        assert!(generic_refinement::parse(&mut Reader::new(&data)).is_ok());
    }
}

#[test]
fn cropped_reference_masks_padding_and_clips_without_coordinate_wrap() {
    for default in [false, true] {
        let mut bitmap = Bitmap::new_with(73, 7, 0, 0, default).unwrap();
        if !default {
            for y in 0..7 {
                for x in 0..73 {
                    bitmap.set_pixel(x, y, u8::from((x * 7 + y * 3) % 5 != 0));
                }
            }
        }
        for x in [0, 1, 30, 31, 32, 64, 72, 73, 0x8000_0000, u32::MAX] {
            for y in [0, 2, 6, 7, u32::MAX] {
                for width in [1, 31, 32, 33, 73] {
                    let cropped = bitmap.crop(x, y, width, 9).unwrap();
                    for ry in 0..9 {
                        for rx in 0..width {
                            let sx = u64::from(x) + u64::from(rx);
                            let sy = u64::from(y) + u64::from(ry);
                            let expected = if sx < 73 && sy < 7 {
                                bitmap.get_pixel(sx as u32, sy as u32)
                            } else {
                                0
                            };
                            assert_eq!(cropped.get_pixel(rx, ry), expected);
                        }
                    }
                }
            }
        }
        let original = bitmap.data.clone();
        for (x, y) in [(u32::MAX - 4, 0), (0, u32::MAX - 4)] {
            bitmap.combine_region(
                &Bitmap::new_with(9, 7, x, y, !default).unwrap(),
                CombinationOperator::Replace,
            );
            assert_eq!(bitmap.data, original);
        }
    }
}
