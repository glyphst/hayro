// Authored T.88 custom Huffman range controls (Annex B.2–B.4).
// Arithmetic bitmaps use agl/jbig2enc d0dfca46216c98f11312a9c9f15615ed490cd7b3
// (Apache-2.0) and the independently authored scalar refinement adapter.
// These are test data, not encoder source. Twelve valid bitmaps match Poppler;
// installed Poppler refuses fourteen wide/terminal-range controls. Exact values
// and independently encoded pixels remain the oracle for those controls.
// Provenance: pdf-test-suite/reports/jbig2-huffman-ranges-2026-09-10/.
use super::*;
use alloc::string::String;

struct ValueCase {
    table: &'static [u8],
    code: &'static str,
    value: i32,
}
const VALUES: &[ValueCase] = &[
    // overshoot-upper-t0
    ValueCase {
        table: &[114, 0, 0, 0, 0, 0, 0, 0, 3, 64, 132],
        code: "100000000000000000000000000000000",
        value: 3,
    },
    // overshoot-normal-t0
    ValueCase {
        table: &[114, 0, 0, 0, 0, 0, 0, 0, 3, 64, 132],
        code: "001",
        value: 1,
    },
    // multiline-upper-t0
    ValueCase {
        table: &[114, 0, 0, 0, 0, 0, 0, 0, 5, 128, 96, 33],
        code: "000000000000000000000000000000000",
        value: 5,
    },
    // multiline-normal-t0
    ValueCase {
        table: &[114, 0, 0, 0, 0, 0, 0, 0, 5, 128, 96, 33],
        code: "1101",
        value: 3,
    },
    // negative-upper-t0
    ValueCase {
        table: &[114, 255, 255, 255, 253, 0, 0, 0, 0, 64, 132],
        code: "100000000000000000000000000000000",
        value: 0,
    },
    // negative-normal-t0
    ValueCase {
        table: &[114, 255, 255, 255, 253, 0, 0, 0, 0, 64, 132],
        code: "010",
        value: -1,
    },
    // terminal31-normal-t0
    ValueCase {
        table: &[114, 0, 0, 0, 0, 127, 255, 255, 255, 71, 196],
        code: "00000000000000000000000000000001",
        value: 1,
    },
    // terminal31-upper-t0
    ValueCase {
        table: &[114, 0, 0, 0, 0, 127, 255, 255, 255, 71, 196],
        code: "100000000000000000000000000000000",
        value: 2147483647,
    },
    // terminal32-min-t0
    ValueCase {
        table: &[114, 128, 0, 0, 0, 127, 255, 255, 255, 72, 4],
        code: "000000000000000000000000000000000",
        value: -2147483648,
    },
    // terminal32-zero-t0
    ValueCase {
        table: &[114, 128, 0, 0, 0, 127, 255, 255, 255, 72, 4],
        code: "010000000000000000000000000000000",
        value: 0,
    },
    // terminal32-max-previous-t0
    ValueCase {
        table: &[114, 128, 0, 0, 0, 127, 255, 255, 255, 72, 4],
        code: "011111111111111111111111111111110",
        value: 2147483646,
    },
    // terminal32-upper-t0
    ValueCase {
        table: &[114, 128, 0, 0, 0, 127, 255, 255, 255, 72, 4],
        code: "100000000000000000000000000000000",
        value: 2147483647,
    },
    // wide33-normal-t0
    ValueCase {
        table: &[114, 128, 0, 0, 0, 127, 255, 255, 255, 72, 68],
        code: "0010000000000000000000000000000000",
        value: 0,
    },
    // wide64-normal-t0
    ValueCase {
        table: &[114, 128, 0, 0, 0, 127, 255, 255, 255, 80, 4],
        code: "00000000000000000000000000000000010000000000000000000000000000000",
        value: 0,
    },
    // wide255-normal-t0
    ValueCase {
        table: &[114, 128, 0, 0, 0, 127, 255, 255, 255, 127, 196],
        code: "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000",
        value: 0,
    },
    // unused255-upper-t0
    ValueCase {
        table: &[114, 0, 0, 0, 0, 0, 0, 0, 3, 63, 196],
        code: "000000000000000000000000000000000",
        value: 3,
    },
];
const INVALID_BOUNDS: &[&[u8]] = &[
    &[114, 0, 0, 0, 0, 0, 0, 0, 0, 16],
    &[114, 0, 0, 0, 1, 0, 0, 0, 0, 16],
    &[114, 0, 0, 0, 0, 0, 0, 0, 0, 64, 4],
    &[114, 0, 0, 0, 1, 0, 0, 0, 0, 64, 4],
];
const INVALID_VALUES: &[(&[u8], &str)] = &[
    (
        &[114, 0, 0, 0, 0, 0, 0, 0, 1, 72, 4],
        "010000000000000000000000000000000",
    ),
    (
        &[114, 128, 0, 0, 0, 127, 255, 255, 255, 72, 68],
        "0100000000000000000000000000000000",
    ),
    (
        &[114, 128, 0, 0, 0, 127, 255, 255, 255, 80, 4],
        "01000000000000000000000000000000000000000000000000000000000000000",
    ),
    (
        &[114, 128, 0, 0, 0, 127, 255, 255, 255, 80, 4],
        "00000000000000000000000000000000100000000000000000000000000000000",
    ),
    (
        &[114, 128, 0, 0, 0, 127, 255, 255, 255, 127, 196],
        "0100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    ),
    (
        &[114, 128, 0, 0, 0, 127, 255, 255, 255, 127, 196],
        "0000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    ),
    (
        &[114, 128, 0, 0, 0, 127, 255, 255, 255, 127, 196],
        "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000",
    ),
    (
        &[114, 127, 255, 255, 254, 127, 255, 255, 255, 64, 132],
        "010",
    ),
];
fn encoded(bits: &str) -> Vec<u8> {
    let mut bytes = vec![0; bits.len().div_ceil(8)];
    for (i, bit) in bits.bytes().enumerate() {
        bytes[i / 8] |= (bit - b'0') << (7 - i % 8);
    }
    bytes
}

#[test]
fn huffman_ranges_decode_exact_values_and_consume_complete_suffixes() {
    for case in VALUES {
        let table = HuffmanTable::read_custom(&mut Reader::new(case.table)).unwrap();
        let bits: String = [case.code, case.code, "10100101"].concat();
        let data = encoded(&bits);
        let mut reader = Reader::new(&data);
        assert_eq!(table.decode(&mut reader), Ok(Some(case.value)));
        assert_eq!(table.decode(&mut reader), Ok(Some(case.value)));
        assert_eq!(reader.read_bits(8), Some(0xa5));
    }
}

#[test]
fn huffman_ranges_reject_empty_and_reversed_declarations() {
    for data in INVALID_BOUNDS {
        assert!(HuffmanTable::read_custom(&mut Reader::new(data)).is_err());
    }
}

#[test]
fn huffman_ranges_reject_out_of_domain_values_after_parsing_valid_tables() {
    for (data, code) in INVALID_VALUES {
        let table = HuffmanTable::read_custom(&mut Reader::new(data)).unwrap();
        assert!(table.decode(&mut Reader::new(&encoded(code))).is_err());
    }
}

#[test]
fn huffman_ranges_require_complete_table_fields_and_numeric_suffixes() {
    for case in VALUES {
        for end in 0..case.table.len() {
            assert!(HuffmanTable::read_custom(&mut Reader::new(&case.table[..end])).is_err());
        }
        let table = HuffmanTable::read_custom(&mut Reader::new(case.table)).unwrap();
        let data = encoded(case.code);
        // Remove whole bytes so zero padding cannot restore an omitted suffix bit.
        for end in 0..data.len() {
            assert!(table.decode(&mut Reader::new(&data[..end])).is_err());
        }
    }
}
