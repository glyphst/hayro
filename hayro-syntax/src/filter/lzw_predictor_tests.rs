//! PDF 32000-1:2008 7.4.4: independently packed codes and predictor samples.
use super::lzw_flate::{flate, lzw};
use super::predictor::{PredictorParams, apply_predictor};
use crate::object::Dict;
use crate::reader::{Reader, ReaderExt};
use alloc::{format, vec, vec::Vec};

fn dict(data: &[u8]) -> Dict<'_> {
    Reader::new(data).read_without_context().unwrap()
}

// A code writer, independent of the decoder's dictionary. Count emitted data
// codes between clears to locate the 9/10/11/12-bit transitions (Table 8).
fn codes(input: &[u16], early: bool) -> Vec<u8> {
    let mut bits = Vec::new();
    let mut width = 9;
    let mut count = 0;
    for &code in input {
        assert!(usize::from(code) < 1 << width);
        bits.extend((0..width).rev().map(|bit| (code >> bit) as u8 & 1));
        if code == 256 {
            width = 9;
            count = 0;
        } else if code != 257 {
            count += 1;
            if width < 12 && count == (1 << width) - 257 - usize::from(early) {
                width += 1;
            }
        }
    }
    bits.chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0, |byte, (i, bit)| byte | bit << (7 - i))
        })
        .collect()
}

#[test]
fn lzw_width_changes_clear_resets_and_dictionary_references() {
    for early in [false, true] {
        let params = format!("<< /EarlyChange {} >>", u8::from(early));
        let params = dict(params.as_bytes());
        for length in [
            0, 1, 253, 254, 255, 256, 765, 766, 767, 768, 1789, 1790, 1791, 1792, 3838, 3839,
        ] {
            let expected: Vec<_> = (0..length).map(|i| (i % 256) as u8).collect();
            let mut input = vec![256];
            input.extend(expected.iter().map(|b| u16::from(*b)));
            input.push(257);
            assert_eq!(
                lzw::decode(&codes(&input, early), &params),
                Some(expected.clone()),
                "{early} {length}"
            );
            input.pop();
            input.extend([256, 256, 65, 258, 259, 256, 66, 257]);
            let mut expected = expected;
            expected.extend_from_slice(b"AAAAAAB");
            assert_eq!(
                lzw::decode(&codes(&input, early), &params),
                Some(expected),
                "reset {early} {length}"
            );
        }
    }
}

#[test]
fn lzw_requires_initial_clear_eod_zero_padding_and_valid_codes() {
    for early in [false, true] {
        let params = format!("<< /EarlyChange {} >>", u8::from(early));
        let params = dict(params.as_bytes());
        for length in 0..16 {
            let mut input = vec![256];
            input.extend(core::iter::repeat_n(65, length));
            assert!(lzw::decode(&codes(&input, early), &params).is_none());
            input.push(257);
            let valid = codes(&input, early);
            assert_eq!(lzw::decode(&valid, &params), Some(vec![65; length]));
            for end in 0..valid.len() {
                assert!(lzw::decode(&valid[..end], &params).is_none());
            }
            let padding = (8 - input.len() * 9 % 8) % 8;
            for bit in 0..padding {
                let mut bad = valid.clone();
                *bad.last_mut().unwrap() |= 1 << bit;
                assert!(lzw::decode(&bad, &params).is_none());
            }
            let mut suffix = valid;
            suffix.extend([255, 0, 128]);
            assert_eq!(lzw::decode(&suffix, &params), Some(vec![65; length]));
        }
        for invalid in [
            &[65, 257][..],
            &[257],
            &[256, 258, 257],
            &[256, 65, 259, 257],
            &[256, 65, 256, 258, 257],
        ] {
            assert!(lzw::decode(&codes(invalid, early), &params).is_none());
        }
        let mut full = vec![256];
        full.extend(core::iter::repeat_n(65, 3840));
        full.push(257);
        assert!(lzw::decode(&codes(&full, early), &params).is_none());
    }
}

#[test]
fn filter_parameters_require_active_integer_values_and_honor_defaults() {
    // Independently generated zlib-compressed "Hello".
    let flate_data = [
        0x78, 0x9c, 0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0, 0x05, 0x8c, 0x01, 0xf5,
    ];
    let lzw_data = codes(&[256, 72, 101, 108, 108, 111, 257], true);
    for entries in [
        "",
        "/Predictor null /Colors null /BitsPerComponent null /Columns null /EarlyChange null",
        "/Predictor 1 /Colors /Unused /BitsPerComponent false /Columns -1",
    ] {
        let text = format!("<< {entries} >>");
        let params = dict(text.as_bytes());
        assert_eq!(
            flate::decode(&flate_data, &params).as_deref(),
            Some(b"Hello".as_slice())
        );
        assert_eq!(
            lzw::decode(&lzw_data, &params).as_deref(),
            Some(b"Hello".as_slice())
        );
    }
    for value in [
        "-1", "2", "255", "256", "1.0", "0.0", "1.5", "true", "/Bad", "[]", "<<>>",
    ] {
        let text = format!("<< /EarlyChange {value} >>");
        let params = dict(text.as_bytes());
        assert!(lzw::decode(&lzw_data, &params).is_none(), "{value}");
        assert_eq!(
            flate::decode(&flate_data, &params).as_deref(),
            Some(b"Hello".as_slice())
        );
    }
    for (key, bad) in [
        (
            "Predictor",
            &[
                "0", "3", "9", "16", "255", "256", "1.0", "1.5", "-1", "/Bad",
            ][..],
        ),
        ("Colors", &["0", "-1", "1.0", "1.5", "/Bad"]),
        ("Columns", &["0", "-1", "1.0", "1.5", "/Bad"]),
        (
            "BitsPerComponent",
            &["0", "3", "7", "9", "15", "17", "256", "-1", "8.0", "/Bad"],
        ),
    ] {
        for value in bad {
            let text = if key == "Predictor" {
                format!("<< /Predictor {value} >>")
            } else {
                format!("<< /Predictor 2 /{key} {value} >>")
            };
            let params = dict(text.as_bytes());
            assert!(flate::decode(&flate_data, &params).is_none(), "{text}");
            assert!(lzw::decode(&lzw_data, &params).is_none(), "{text}");
        }
    }
}

#[test]
fn predictor_rows_are_complete_tags_are_valid_and_geometry_is_checked() {
    for predictor in [2, 10, 11, 12, 13, 14, 15] {
        let params = PredictorParams {
            predictor,
            colors: 3,
            bits_per_component: 8,
            columns: 3,
        };
        let stride = if predictor == 2 { 9 } else { 10 };
        for trailing in 1..stride {
            assert!(apply_predictor(vec![0; stride + trailing], &params).is_none());
        }
        assert_eq!(apply_predictor(vec![], &params), Some(vec![]));
        if predictor >= 10 {
            for tag in 5..=255 {
                let mut bytes = vec![0; 2 * stride];
                bytes[stride] = tag;
                assert!(
                    apply_predictor(bytes, &params).is_none(),
                    "{predictor} {tag}"
                );
            }
        }
    }
    for (colors, columns, bits) in [
        (0, 1, 8),
        (1, 0, 8),
        (usize::MAX, 2, 1),
        (1, usize::MAX, 16),
        (usize::MAX, 1, 16),
    ] {
        let params = PredictorParams {
            predictor: 2,
            colors,
            columns,
            bits_per_component: bits,
        };
        assert!(apply_predictor(vec![], &params).is_none());
    }
    for predictor in [2, 15] {
        let params = PredictorParams {
            predictor,
            colors: 1,
            bits_per_component: 1,
            columns: usize::MAX,
        };
        assert_eq!(apply_predictor(vec![], &params), Some(vec![]));
    }
    // Row padding is preserved and never treated as a component of the next row.
    let params = PredictorParams {
        predictor: 2,
        colors: 1,
        bits_per_component: 2,
        columns: 3,
    };
    assert_eq!(
        apply_predictor(vec![0x57, 0x57], &params),
        Some(vec![0x6f, 0x6f])
    );
}

#[test]
fn golden_packed_rows_preserve_all_five_depths_for_tiff_and_png() {
    // Scalar encoder controls: PDF 1.7 component subtraction and PNG byte filters.
    type SampleCase<'a> = (usize, u8, u8, &'a [u8], &'a [u8]);
    let cases: &[SampleCase<'_>] = &[
        (1, 1, 2, &[96, 224, 96], &[64, 160, 64]),
        (1, 1, 15, &[1, 64, 3, 128, 4, 160], &[64, 160, 64]),
        (
            3,
            1,
            2,
            &[95, 128, 191, 128, 95, 128],
            &[85, 0, 170, 128, 85, 0],
        ),
        (
            3,
            1,
            15,
            &[1, 85, 171, 3, 128, 43, 4, 171, 171],
            &[85, 0, 170, 128, 85, 0],
        ),
        (1, 2, 2, &[60, 124, 188], &[56, 76, 144]),
        (1, 2, 15, &[1, 56, 3, 48, 4, 68], &[56, 76, 144]),
        (
            3,
            2,
            2,
            &[57, 85, 64, 77, 85, 64, 145, 85, 64],
            &[57, 57, 0, 78, 78, 64, 147, 147, 128],
        ),
        (
            3,
            2,
            15,
            &[1, 57, 0, 199, 3, 50, 11, 25, 4, 69, 0, 237],
            &[57, 57, 0, 78, 78, 64, 147, 147, 128],
        ),
        (
            1,
            4,
            2,
            &[3, 48, 83, 48, 163, 48],
            &[3, 96, 88, 176, 173, 0],
        ),
        (
            1,
            4,
            15,
            &[1, 3, 93, 3, 87, 84, 4, 85, 80],
            &[3, 96, 88, 176, 173, 0],
        ),
        (
            3,
            4,
            2,
            &[
                3, 105, 153, 153, 144, 88, 185, 153, 153, 144, 173, 9, 153, 153, 144,
            ],
            &[
                3, 105, 207, 37, 128, 88, 190, 20, 122, 208, 173, 3, 105, 207, 32,
            ],
        ),
        (
            3,
            4,
            15,
            &[
                1, 3, 105, 204, 188, 177, 3, 87, 138, 129, 9, 134, 4, 85, 69, 17, 204, 80,
            ],
            &[
                3, 105, 207, 37, 128, 88, 190, 20, 122, 208, 173, 3, 105, 207, 32,
            ],
        ),
        (
            1,
            8,
            2,
            &[0, 3, 3, 5, 3, 3, 10, 3, 3],
            &[0, 3, 6, 5, 8, 11, 10, 13, 16],
        ),
        (
            1,
            8,
            15,
            &[1, 0, 3, 3, 3, 5, 4, 4, 4, 5, 3, 3],
            &[0, 3, 6, 5, 8, 11, 10, 13, 16],
        ),
        (
            3,
            8,
            2,
            &[
                0, 3, 6, 9, 9, 9, 9, 9, 9, 5, 8, 11, 9, 9, 9, 9, 9, 9, 10, 13, 16, 9, 9, 9, 9, 9, 9,
            ],
            &[
                0, 3, 6, 9, 12, 15, 18, 21, 24, 5, 8, 11, 14, 17, 20, 23, 26, 29, 10, 13, 16, 19,
                22, 25, 28, 31, 34,
            ],
        ),
        (
            3,
            8,
            15,
            &[
                1, 0, 3, 6, 9, 9, 9, 9, 9, 9, 3, 5, 7, 8, 7, 7, 7, 7, 7, 7, 4, 5, 5, 5, 5, 5, 5, 5,
                5, 5,
            ],
            &[
                0, 3, 6, 9, 12, 15, 18, 21, 24, 5, 8, 11, 14, 17, 20, 23, 26, 29, 10, 13, 16, 19,
                22, 25, 28, 31, 34,
            ],
        ),
        (
            1,
            16,
            2,
            &[
                0, 0, 37, 37, 37, 37, 71, 71, 37, 37, 37, 37, 142, 142, 37, 37, 37, 37,
            ],
            &[
                0, 0, 37, 37, 74, 74, 71, 71, 108, 108, 145, 145, 142, 142, 179, 179, 216, 216,
            ],
        ),
        (
            1,
            16,
            15,
            &[
                1, 0, 0, 37, 37, 37, 37, 3, 71, 71, 54, 54, 54, 54, 4, 71, 71, 37, 37, 37, 37,
            ],
            &[
                0, 0, 37, 37, 74, 74, 71, 71, 108, 108, 145, 145, 142, 142, 179, 179, 216, 216,
            ],
        ),
        (
            3,
            16,
            2,
            &[
                0, 0, 37, 37, 74, 74, 111, 111, 111, 111, 111, 111, 111, 111, 110, 111, 110, 111,
                71, 71, 108, 108, 145, 145, 111, 111, 111, 111, 110, 111, 110, 111, 110, 111, 111,
                111, 142, 142, 179, 179, 216, 216, 111, 111, 110, 111, 110, 111, 110, 111, 111,
                111, 111, 111,
            ],
            &[
                0, 0, 37, 37, 74, 74, 111, 111, 148, 148, 185, 185, 222, 222, 3, 3, 40, 40, 71, 71,
                108, 108, 145, 145, 182, 182, 219, 219, 0, 0, 37, 37, 74, 74, 111, 111, 142, 142,
                179, 179, 216, 216, 253, 253, 34, 34, 71, 71, 108, 108, 145, 145, 182, 182,
            ],
        ),
        (
            3,
            16,
            15,
            &[
                1, 0, 0, 37, 37, 74, 74, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111,
                111, 3, 71, 71, 90, 90, 108, 108, 91, 91, 91, 91, 91, 91, 91, 91, 219, 219, 91, 91,
                4, 71, 71, 71, 71, 71, 71, 71, 71, 71, 71, 71, 71, 71, 71, 111, 111, 71, 71,
            ],
            &[
                0, 0, 37, 37, 74, 74, 111, 111, 148, 148, 185, 185, 222, 222, 3, 3, 40, 40, 71, 71,
                108, 108, 145, 145, 182, 182, 219, 219, 0, 0, 37, 37, 74, 74, 111, 111, 142, 142,
                179, 179, 216, 216, 253, 253, 34, 34, 71, 71, 108, 108, 145, 145, 182, 182,
            ],
        ),
    ];
    for &(colors, bits, predictor, encoded, expected) in cases {
        let predictors: &[u8] = if predictor == 2 {
            &[2]
        } else {
            &[10, 11, 12, 13, 14, 15]
        };
        for &predictor in predictors {
            let params = PredictorParams {
                predictor,
                colors,
                bits_per_component: bits,
                columns: 3,
            };
            let input = encoded.to_vec();
            let pointer = input.as_ptr();
            let capacity = input.capacity();
            let output = apply_predictor(input, &params).unwrap();
            assert_eq!(output, expected, "{colors} {bits} {predictor}");
            assert_eq!(output.as_ptr(), pointer);
            assert_eq!(output.capacity(), capacity);
        }
    }
}
