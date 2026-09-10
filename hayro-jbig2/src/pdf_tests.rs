use super::{Image, SegmentType};
use alloc::{vec, vec::Vec};

fn segment(number: u8, kind: u8, page: u8, payload: &[u8], refs: &[u8]) -> Vec<u8> {
    assert!(refs.len() < 5);
    let retain = ((1_u16 << (refs.len() + 1)) - 1) as u8;
    let mut bytes = vec![0, 0, 0, number, kind, ((refs.len() as u8) << 5) | retain];
    bytes.extend(refs);
    bytes.push(page);
    bytes.extend((payload.len() as u32).to_be_bytes());
    bytes.extend(payload);
    bytes
}

fn page(number: u8, association: u8) -> Vec<u8> {
    let mut payload = Vec::new();
    for value in [9_u32, 2, 0, 0] {
        payload.extend(value.to_be_bytes());
    }
    payload.extend([5, 0, 0]);
    segment(number, 48, association, &payload, &[])
}

#[test]
fn pdf_allows_unordered_segments_gaps_and_noncanonical_page_numbers() {
    for association in [1, 7] {
        let first = page(2, association);
        let stripe = segment(5, 50, association, &[0, 0, 0, 1], &[]);
        let global = segment(0, 0, 0, &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0], &[]);
        let data = [stripe, first].concat();
        let image = Image::new_embedded_pdf(&data, Some(&global)).unwrap();
        assert_eq!((image.width(), image.height()), (9, 2));
        assert_eq!(
            image.segments[1].header.segment_type,
            SegmentType::PageInformation
        );
    }
}

#[test]
fn pdf_rejects_invalid_partition_and_page_structure() {
    let first = page(1, 1);
    let global = segment(0, 0, 0, &[], &[]);
    for data in [
        page(1, 0),
        [first.clone(), page(2, 2)].concat(),
        [first.clone(), page(2, 1)].concat(),
        [global, first.clone()].concat(),
        [segment(0, 0, 1, &[], &[]), first.clone()].concat(),
        [first.clone(), segment(2, 49, 1, &[], &[])].concat(),
        [first.clone(), segment(2, 51, 0, &[], &[])].concat(),
        [first.clone(), segment(2, 51, 1, &[], &[]), vec![0xff]].concat(),
        [vec![0xff, 0xaa], first.clone(), vec![0xff, 0xab]].concat(),
        [
            vec![0x97, b'J', b'B', b'2', 13, 10, 26, 10, 3],
            first.clone(),
        ]
        .concat(),
    ] {
        assert!(Image::new_embedded_pdf(&data, None).is_err(), "{data:?}");
    }
    for globals in [
        page(0, 0),
        segment(0, 0, 1, &[], &[]),
        segment(0, 50, 0, &[0, 0, 0, 1], &[]),
        segment(0, 51, 0, &[], &[]),
    ] {
        assert!(Image::new_embedded_pdf(&first, Some(&globals)).is_err());
    }
    // Existing non-PDF embedded API keeps its acceptance of end-of-page.
    assert!(Image::new_embedded(&[first, segment(2, 49, 1, &[], &[])].concat(), None).is_ok());
}

#[test]
fn pdf_requires_unique_segments_and_existing_compatible_references() {
    let first = page(1, 1);
    for (data, globals) in [
        (first.clone(), segment(1, 0, 0, &[], &[])),
        (
            [first.clone(), segment(2, 0, 1, &[], &[0])].concat(),
            vec![],
        ),
        (first.clone(), segment(2, 0, 0, &[], &[1])),
        (
            [first.clone(), segment(2, 0, 1, &[], &[2])].concat(),
            vec![],
        ),
    ] {
        assert!(Image::new_embedded_pdf(&data, Some(&globals)).is_err());
    }
    let data = [segment(3, 0, 1, &[], &[0]), first].concat();
    let globals = segment(0, 0, 0, &[], &[]);
    assert!(Image::new_embedded_pdf(&data, Some(&globals)).is_ok());
}
