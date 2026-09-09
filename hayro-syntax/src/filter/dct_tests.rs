use super::*;
use crate::object::FromBytes;

#[path = "dct_fixtures.rs"]
mod fixtures;

fn with_adobe(data: &[u8], transform: u8) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8, 0xff, 0xee, 0, 14];
    out.extend_from_slice(b"Adobe\0\x64\0\0\0\0");
    out.push(transform);
    out.extend_from_slice(&data[2..]);
    out
}

fn decoded(data: &[u8], params: &[u8], components: u8) -> Option<FilterResult<'static>> {
    decode(
        data,
        &Dict::from_bytes(params).unwrap(),
        &ImageDecodeParams {
            width: 8,
            height: 8,
            bpc: Some(8),
            num_components: Some(components),
            ..Default::default()
        },
    )
}

fn check(data: &[u8], params: &[u8], expected: &[u8]) {
    let result = decoded(data, params, expected.len() as u8).expect("decode JPEG");
    assert_eq!(result.data.len(), 64 * expected.len());
    for pixel in result.data.chunks_exact(expected.len()) {
        for (actual, expected) in pixel.iter().zip(expected) {
            assert!(actual.abs_diff(*expected) <= 1, "{pixel:?} != {expected:?}");
        }
    }
}

#[test]
fn adobe_marker_overrides_dictionary_transform_in_baseline_and_progressive_images() {
    for data in [fixtures::THREE, fixtures::PROGRESSIVE, fixtures::RGB_IDS] {
        check(
            &with_adobe(data, 1),
            b"<</ColorTransform 0>>",
            &[154, 18, 64],
        );
        check(
            &with_adobe(data, 0),
            b"<</ColorTransform 1>>",
            &[64, 128, 192],
        );
        check(
            &with_adobe(data, 1),
            b"<</ColorTransform /Ignored>>",
            &[154, 18, 64],
        );
    }
    check(
        &with_adobe(fixtures::FOUR, 2),
        b"<</ColorTransform 0>>",
        &[101, 237, 191, 0],
    );
    check(
        &with_adobe(fixtures::FOUR, 0),
        b"<</ColorTransform 1>>",
        &[64, 128, 192, 0],
    );
}

#[test]
fn dictionary_and_default_transforms_preserve_pdf_channel_semantics() {
    for data in [fixtures::THREE, fixtures::PROGRESSIVE, fixtures::RGB_IDS] {
        check(data, b"<<>>", &[154, 18, 64]);
        check(data, b"<</ColorTransform null>>", &[154, 18, 64]);
        check(data, b"<</ColorTransform 0>>", &[64, 128, 192]);
        check(data, b"<</ColorTransform 1>>", &[154, 18, 64]);
    }
    check(fixtures::FOUR, b"<<>>", &[64, 128, 192, 0]);
    check(
        fixtures::FOUR,
        b"<</ColorTransform 1>>",
        &[101, 237, 191, 0],
    );
    for params in [
        b"<<>>".as_slice(),
        b"<</ColorTransform 0>>",
        b"<</ColorTransform 1>>",
    ] {
        check(fixtures::GRAY, params, &[64]);
        check(fixtures::TWO, params, &[64, 192]);
    }
    // Generic stream decoding has no PDF image dimensions to patch.
    assert_eq!(
        decode(
            fixtures::GRAY,
            &Dict::from_bytes(b"<<>>").unwrap(),
            &ImageDecodeParams::default()
        )
        .unwrap()
        .data
        .len(),
        64
    );
}

#[test]
fn mismatched_components_depths_and_active_transform_parameters_are_rejected() {
    for (data, actual) in [
        (fixtures::GRAY, 1),
        (fixtures::TWO, 2),
        (fixtures::THREE, 3),
        (fixtures::FOUR, 4),
    ] {
        for declared in 0..=5 {
            assert_eq!(
                decoded(data, b"<<>>", declared).is_some(),
                declared == actual
            );
        }
    }
    for value in ["-1", "2", "0.0", "0.5", "1.0", "/Invalid", "true", "[0]"] {
        let params = format!("<</ColorTransform {value}>>");
        assert!(
            decoded(fixtures::THREE, params.as_bytes(), 3).is_none(),
            "{value}"
        );
    }
    for (data, count, marker) in [
        (fixtures::THREE, 3, 2),
        (fixtures::FOUR, 4, 1),
        (fixtures::THREE, 3, 3),
    ] {
        assert!(decoded(&with_adobe(data, marker), b"<<>>", count).is_none());
    }
    for bits in [0, 1, 2, 4, 16] {
        assert!(
            decode(
                fixtures::THREE,
                &Dict::from_bytes(b"<<>>").unwrap(),
                &ImageDecodeParams {
                    width: 8,
                    height: 8,
                    bpc: Some(bits),
                    num_components: Some(3),
                    ..Default::default()
                }
            )
            .is_none()
        );
    }
}

#[test]
fn jpeg_header_walk_checks_payload_boundaries_and_marker_precedence() {
    let input = with_adobe(fixtures::THREE, 1);
    let (sof, _) = header_metadata(&input).unwrap();
    for end in 0..sof + 2 {
        assert!(header_metadata(&input[..end]).is_none(), "truncation {end}");
    }
    let mut padded = input.clone();
    padded.insert(sof, 0xff);
    check(&padded, b"<</ColorTransform 0>>", &[154, 18, 64]);
    let mut fake = vec![0xff, 0xd8, 0xff, 0xfe, 0, 18];
    fake.extend_from_slice(&with_adobe(fixtures::THREE, 0)[2..18]);
    fake.extend_from_slice(&input[2..]);
    check(&fake, b"<</ColorTransform 0>>", &[154, 18, 64]);
    assert!(header_metadata(&with_adobe(&input, 0)).is_none());
    for length in [0, 1, 2, 7, 13, 255] {
        let mut invalid = input.clone();
        invalid[5] = length;
        assert!(
            header_metadata(&invalid).is_none(),
            "segment length {length}"
        );
    }
}

#[test]
fn dct_stream_depth_requires_the_integer_eight_even_through_filter_aliases() {
    use crate::object::Stream;
    for filter in ["/DCTDecode", "/DCT", "[/DCTDecode]"] {
        for key in ["BitsPerComponent", "BPC"] {
            for value in ["8", "4", "8.0", "8.5", "null", "/Invalid"] {
                let bytes = format!("<</Filter {filter}/{key} {value}>>");
                let stream =
                    Stream::new(fixtures::GRAY, Dict::from_bytes(bytes.as_bytes()).unwrap());
                assert_eq!(
                    stream.decoded().is_ok(),
                    value == "8",
                    "{filter} {key} {value}"
                );
            }
        }
    }
}
