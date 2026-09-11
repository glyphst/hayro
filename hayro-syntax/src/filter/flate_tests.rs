//! PDF Flate envelopes and DEFLATE grammar through both decoder paths.
use super::flate_portable;
use super::lzw_flate::flate::{self, LimitedDecodeFailure};
use crate::object::stream::{DecodeFailure, LimitedStreamDecodeFailure};
use crate::object::{Dict, Stream};
use crate::reader::{Reader, ReaderContext, ReaderExt};
use alloc::{format, vec::Vec};

#[path = "flate_test_vectors.rs"]
mod vectors;

fn expand(encoded: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for token in encoded.split_ascii_whitespace() {
        let (hex, count) = token.split_once(':').unwrap_or((token, "1"));
        let byte = u8::from_str_radix(hex, 16).unwrap();
        bytes.extend(core::iter::repeat_n(byte, count.parse::<usize>().unwrap()));
    }
    bytes
}

fn stream_bytes(data: &[u8]) -> Vec<u8> {
    let mut bytes =
        format!("<< /Length {} /Filter /FlateDecode >> stream\n", data.len()).into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream");
    bytes
}

#[test]
fn independently_encoded_streams_and_output_boundaries() {
    for &(name, valid, input, expected) in vectors::VECTORS {
        let input = expand(input);
        let expected = expand(expected);
        let object = stream_bytes(&input);
        let stream = Reader::new(&object)
            .read_with_context::<Stream<'_>>(&ReaderContext::dummy())
            .unwrap();
        if valid {
            assert_eq!(stream.decoded().unwrap().as_ref(), expected, "{name}");
            assert_eq!(
                stream
                    .decoded_flate_with_limit(expected.len())
                    .unwrap()
                    .as_ref(),
                expected,
                "{name} exact limit"
            );
            assert_eq!(
                flate_portable::decode(&input, expected.len(), false, false).unwrap(),
                expected,
                "{name} portable"
            );
            if !expected.is_empty() {
                assert_eq!(
                    stream.decoded_flate_with_limit(expected.len() - 1),
                    Err(LimitedStreamDecodeFailure::LimitExceeded),
                    "{name}"
                );
                assert_eq!(
                    flate_portable::decode(&input, expected.len() - 1, false, false),
                    Err(LimitedDecodeFailure::LimitExceeded),
                    "{name} portable"
                );
            }
        } else {
            assert!(
                matches!(stream.decoded(), Err(DecodeFailure::StreamDecode)),
                "{name}"
            );
            assert!(
                stream
                    .decoded_flate_with_limit(expected.len() + 1024)
                    .is_err(),
                "{name} bounded"
            );
            assert!(
                flate_portable::decode(&input, expected.len() + 1024, false, false).is_err(),
                "{name} portable"
            );
        }
    }
}

#[test]
fn only_inline_images_permit_pdf_whitespace_after_adler32() {
    let suffix = b"\0\t\n\x0c\r ";
    for &(name, valid, input, expected) in vectors::VECTORS {
        if !valid {
            continue;
        }
        let mut input = expand(input);
        let expected = expand(expected);
        input.extend_from_slice(suffix);
        assert!(
            flate::decode(&input, &Dict::default()).is_none(),
            "{name} ordinary suffix"
        );
        assert_eq!(
            flate::decode_inline(&input, &Dict::default()).unwrap(),
            expected,
            "{name} inline suffix"
        );
        assert_eq!(
            flate_portable::decode(&input, expected.len(), true, false).unwrap(),
            expected,
            "{name} portable suffix"
        );
        input.push(b'X');
        assert!(
            flate::decode_inline(&input, &Dict::default()).is_none(),
            "{name} inline garbage"
        );
        assert!(
            flate_portable::decode(&input, expected.len(), true, false).is_err(),
            "{name} portable garbage"
        );
    }
}

#[test]
fn compressed_truncations_never_return_a_valid_prefix() {
    for &(name, valid, input, _) in vectors::VECTORS {
        if !valid {
            continue;
        }
        let input = expand(input);
        for end in 0..input.len() {
            // Exhaust short streams; for large stored blocks also probe the
            // header and every possible truncation of the final trailer.
            if input.len() > 512 && end >= 16 && end + 8 < input.len() {
                continue;
            }
            assert!(
                flate::decode(&input[..end], &Dict::default()).is_none(),
                "{name} at {end}"
            );
            assert!(
                flate_portable::decode(&input[..end], usize::MAX, false, false).is_err(),
                "{name} portable at {end}"
            );
        }
    }
}

#[test]
fn invalid_zlib_headers_and_concatenated_streams_are_rejected() {
    let (_, _, encoded, _) = vectors::VECTORS[0];
    let input = expand(encoded);
    for cmf in 0..=255_u8 {
        let mut stream = input.clone();
        stream[0] = cmf;
        stream[1] = (31 - (u16::from(cmf) * 256) % 31) as u8 % 31;
        let valid = cmf & 15 == 8 && cmf >> 4 <= 7;
        assert_eq!(
            flate::decode(&stream, &Dict::default()).is_some(),
            valid,
            "CMF {cmf}"
        );
        assert_eq!(
            flate_portable::decode(&stream, usize::MAX, false, false).is_ok(),
            valid,
            "portable CMF {cmf}"
        );
    }
    let mut concatenated = input.clone();
    concatenated.extend_from_slice(&input);
    assert!(flate::decode_inline(&concatenated, &Dict::default()).is_none());
    assert!(flate_portable::decode(&concatenated, usize::MAX, true, false).is_err());
}

#[test]
fn empty_stream_has_a_zero_byte_output_budget() {
    let input = expand("78 9c 01 00 00 ff ff 00 00 00 01");
    assert_eq!(flate::decode_with_limit(&input, 0).unwrap(), b"");
    assert_eq!(
        flate_portable::decode(&input, 0, false, false).unwrap(),
        b""
    );
}

#[test]
fn inline_checksum_bytes_are_preserved_and_compression_level_is_ignored() {
    for value in [255_u8, 8, 9, 11, 12, 31] {
        for level in 0..4_u8 {
            let mut flags = level * 64;
            flags += ((31 - u16::from_be_bytes([0x78, flags]) % 31) % 31) as u8;
            let mut input = alloc::vec![0x78, flags, 1, 1, 0, 254, 255, value];
            let a = u32::from(value) + 1;
            input.extend_from_slice(&((a << 16) | a).to_be_bytes());
            assert!(crate::trivia::is_white_space_character(
                *input.last().unwrap()
            ));
            assert_eq!(
                flate::decode_inline(&input, &Dict::default()).unwrap(),
                [value]
            );
            assert_eq!(
                flate_portable::decode(&input, 1, true, false).unwrap(),
                [value]
            );
            input.push(b' ');
            assert_eq!(
                flate::decode_inline(&input, &Dict::default()).unwrap(),
                [value]
            );
            assert_eq!(
                flate_portable::decode(&input, 1, true, false).unwrap(),
                [value]
            );
            input[1] ^= 1;
            assert!(flate::decode_inline(&input, &Dict::default()).is_none());
            assert!(flate_portable::decode(&input, 1, true, false).is_err());
        }
    }
}
