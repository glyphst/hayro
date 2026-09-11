//! PDF 32000-1:2008 7.4.2, 7.4.3 and 7.4.5 byte and termination contracts.
use super::{ascii_85, ascii_hex, run_length};
use alloc::vec;
use alloc::vec::Vec;

const WHITE: &[u8] = &[0, 9, 10, 12, 13, 32];

#[test]
fn hex_streams_validate_all_byte_classes_and_preserve_string_bodies() {
    for byte in 0..=u8::MAX {
        let digit = match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let expected = if let Some(digit) = digit {
            Some(vec![0xa0 | digit])
        } else if byte == b'>' || WHITE.contains(&byte) {
            Some(vec![0xa0])
        } else {
            None
        };
        assert_eq!(ascii_hex::decode(&[b'A', byte, b'>']), expected, "{byte}");
    }
    for input in [b"".as_slice(), b"A", b"AB", b"AB \r\n"] {
        assert!(ascii_hex::decode(input).is_none(), "{input:?}");
        assert!(ascii_hex::decode_into::<Vec<u8>>(input).is_some());
    }
    assert_eq!(ascii_hex::decode(b">non-hex suffix"), Some(vec![]));
    assert_eq!(ascii_hex::decode(b"aB0>ignored"), Some(vec![0xab, 0]));
    assert_eq!(ascii_hex::decode(b"a\0B\t0\n>ignored"), Some(vec![0xab, 0]));
}

#[test]
fn ascii85_validates_every_byte_and_requires_contiguous_end_markers() {
    for byte in 0..=u8::MAX {
        let expected = if (b'!'..=b'u').contains(&byte) {
            Some(vec![0, 0, 0, byte - b'!'])
        } else if WHITE.contains(&byte) {
            Some(vec![0, 0, 0])
        } else {
            None
        };
        assert_eq!(
            ascii_85::decode(&[b'!', b'!', b'!', b'!', byte, b'~', b'>']),
            expected,
            "{byte}"
        );
        let marker = [b'z', b'~', byte, b'>'];
        assert_eq!(
            ascii_85::decode(&marker),
            (byte == b'>').then(|| vec![0; 4]),
            "end marker byte {byte}"
        );
    }
    for input in [b"".as_slice(), b"z", b"!!!!!", b"87cURDZ", b"z~"] {
        assert!(ascii_85::decode(input).is_none(), "{input:?}");
    }
    assert_eq!(ascii_85::decode(b"~>ignored"), Some(vec![]));
    assert_eq!(ascii_85::decode(b"z~>~bad"), Some(vec![0; 4]));
}

#[test]
fn ascii85_group_boundaries_and_partial_words_preserve_exact_samples() {
    // Independently obtained with Python's base64.a85encode (zero padding
    // before division by 85). Include the largest full word and every tail.
    for (input, expected) in [
        (b"z~>".as_slice(), &[0_u8; 4][..]),
        (b"!!!!!~>", &[0; 4]),
        (b"!!~>", &[0]),
        (b"!!!~>", &[0, 0]),
        (b"!!!!~>", &[0, 0, 0]),
        (b"rr~>", &[255]),
        (b"s8N~>", &[255, 255]),
        (b"s8W*~>", &[255, 255, 255]),
        (b"s8W-!~>", &[255, 255, 255, 255]),
        (b"87cURDZ~>", &b"Hello"[..]),
    ] {
        assert_eq!(
            ascii_85::decode(input).as_deref(),
            Some(expected),
            "{input:?}"
        );
        let mut spaced = Vec::new();
        for &byte in &input[..input.len() - 2] {
            spaced.push(byte);
            spaced.extend_from_slice(WHITE);
        }
        spaced.extend_from_slice(b"~>");
        assert_eq!(ascii_85::decode(&spaced).as_deref(), Some(expected));
    }
    for count in 1..=4 {
        let mut input = b"z".to_vec();
        input.extend(core::iter::repeat_n(b'!', count));
        input.extend_from_slice(b"z~>");
        assert!(ascii_85::decode(&input).is_none(), "partial group {count}");
    }
    for input in [b"!~>".as_slice(), b"z!~>", b"s8W-\"~>", b"uuuuu~>", b"uu~>"] {
        assert!(ascii_85::decode(input).is_none(), "{input:?}");
    }
}

#[test]
fn run_length_preserves_all_literal_and_repeat_lengths_and_data_bytes() {
    for byte in 0..=u8::MAX {
        assert_eq!(run_length::decode(&[0, byte, 128]), Some(vec![byte]));
    }
    for length in 1..=128_u8 {
        let expected: Vec<_> = (0..length).map(|byte| byte ^ 0x80).collect();
        let mut input = vec![length - 1];
        input.extend_from_slice(&expected);
        input.push(128);
        assert_eq!(run_length::decode(&input), Some(expected));
    }
    for header in 129..=u8::MAX {
        for byte in [0, 128, 255] {
            assert_eq!(
                run_length::decode(&[header, byte, 128]),
                Some(vec![byte; 257 - usize::from(header)])
            );
        }
    }
    assert_eq!(run_length::decode(&[128, 127, 0]), Some(vec![]));
    assert_eq!(
        run_length::decode(&[0, 128, 255, 10, 128]),
        Some(vec![128, 10, 10])
    );
}

#[test]
fn truncated_run_length_streams_never_return_a_valid_prefix() {
    for length in 1..=128_u8 {
        for available in 0..length {
            let mut input = vec![0, b'X', length - 1];
            input.extend(core::iter::repeat_n(b'A', usize::from(available)));
            assert!(run_length::decode(&input).is_none(), "{length} {available}");
        }
    }
    for header in 129..=u8::MAX {
        assert!(run_length::decode(&[0, b'X', header]).is_none());
    }
    for input in [b"".as_slice(), &[0, b'X'], &[255, b'X']] {
        assert!(run_length::decode(input).is_none());
    }
}
