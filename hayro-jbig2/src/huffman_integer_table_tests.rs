use super::*;
use alloc::string::String;

fn encoded(prefix: &str, offset: Option<u32>) -> Vec<u8> {
    let suffix = offset.map(|value| alloc::format!("{value:032b}"));
    let bits: String = prefix
        .chars()
        .chain(suffix.iter().flat_map(|s| s.chars()))
        .collect();
    let mut bytes = vec![0; bits.len().div_ceil(8)];
    for (i, bit) in bits.bytes().enumerate() {
        bytes[i / 8] |= (bit - b'0') << (7 - i % 8);
    }
    bytes
}

#[test]
fn huffman_integer_standard_values_are_exact_at_signed_limits() {
    let tables = StandardHuffmanTables::new();
    let table = tables.table_o();
    // Explicit Table B.15 prefixes and unsigned suffixes from Annex B.
    for (prefix, offset, expected) in [
        ("1111110", 0, -25),
        ("1111110", 2_147_483_622, -2_147_483_647),
        ("1111110", 2_147_483_623, i32::MIN),
        ("1111111", 0, 25),
        ("1111111", 2_147_483_621, 2_147_483_646),
        ("1111111", 2_147_483_622, i32::MAX),
    ] {
        let data = encoded(prefix, Some(offset));
        assert_eq!(table.decode(&mut Reader::new(&data)), Ok(Some(expected)));
    }
    for (prefix, offset) in [
        ("1111110", 2_147_483_624),
        ("1111111", 2_147_483_623),
        ("1111110", 2_147_483_648),
        ("1111111", 2_147_483_648),
        ("1111110", u32::MAX),
        ("1111111", u32::MAX),
    ] {
        let data = encoded(prefix, Some(offset));
        assert!(table.decode(&mut Reader::new(&data)).is_err());
    }
}

#[test]
fn huffman_integer_custom_offsets_keep_the_unsigned_high_bit() {
    let lower = HuffmanTable::build(&[TableLine::lower(2_147_483_645, 1, 32)]).unwrap();
    let upper = HuffmanTable::build(&[TableLine::upper(-2_147_483_646, 1, 32)]).unwrap();
    for (offset, expected_lower, expected_upper) in [
        (0, 2_147_483_645, -2_147_483_646),
        (2_147_483_647, -2, 1),
        (2_147_483_648, -3, 2),
        (4_294_967_293, i32::MIN, i32::MAX),
    ] {
        let data = encoded("0", Some(offset));
        assert_eq!(
            lower.decode(&mut Reader::new(&data)),
            Ok(Some(expected_lower))
        );
        assert_eq!(
            upper.decode(&mut Reader::new(&data)),
            Ok(Some(expected_upper))
        );
    }
    for offset in [4_294_967_294, u32::MAX] {
        let data = encoded("0", Some(offset));
        assert!(lower.decode(&mut Reader::new(&data)).is_err());
        assert!(upper.decode(&mut Reader::new(&data)).is_err());
    }
    let impossible = HuffmanTable::build(&[TableLine::lower(-2_147_483_649, 1, 32)]).unwrap();
    for offset in [0, u32::MAX] {
        assert!(
            impossible
                .decode(&mut Reader::new(&encoded("0", Some(offset))))
                .is_err()
        );
    }
}

#[test]
fn huffman_integer_prefix_space_assigns_every_code_once() {
    let lines: Vec<_> = (0..33)
        .map(|value| TableLine::new(value, (value + 1).min(32) as u8, 0))
        .collect();
    let table = HuffmanTable::build(&lines).unwrap();
    for value in 0..33 {
        let mut prefix = "1".repeat(value as usize);
        if value < 32 {
            prefix.push('0');
        }
        let data = encoded(&prefix, None);
        assert_eq!(table.decode(&mut Reader::new(&data)), Ok(Some(value)));
    }
    for lengths in [&[1, 1, 1][..], &[2, 2, 2, 2, 2], &[1, 2, 2, 2]] {
        let invalid: Vec<_> = lengths
            .iter()
            .map(|length| TableLine::new(0, *length, 0))
            .collect();
        assert!(HuffmanTable::build(&invalid).is_err());
    }
}
