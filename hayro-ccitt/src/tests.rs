use super::*;
use alloc::vec::Vec;

fn settings(encoding: EncodingMode, rows: u32, end_of_block: bool) -> DecodeSettings {
    DecodeSettings {
        columns: 8,
        rows,
        end_of_block,
        end_of_line: false,
        rows_are_byte_aligned: false,
        encoding,
        invert_black: false,
    }
}
fn bits(input: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, bit) in input
        .bytes()
        .filter(|b| *b == b'0' || *b == b'1')
        .enumerate()
    {
        if i % 8 == 0 {
            out.push(0);
        }
        *out.last_mut().unwrap() |= (bit - b'0') << (7 - i % 8);
    }
    out
}

#[test]
fn pdf_marker_overrides_rows_and_limits_are_enforced_before_emitting() {
    let data = bits("1 1 1 000000000001 000000000001");
    for rows in [0, 1, 3, u32::MAX] {
        let s = settings(EncodingMode::Group4, rows, true);
        let image = decode_pdf(&data, s, 0, 3).unwrap();
        assert_eq!(image.data, [255; 3]);
        assert_eq!(image.height, 3);
        assert_eq!(image.consumed, 4);
        assert!(matches!(
            decode_pdf(&data, s, 0, 2),
            Err(DecodeError::LimitExceeded)
        ));
    }
    assert!(decode_pdf(&[0xe0], settings(EncodingMode::Group4, 3, true), 0, 3).is_err());
    assert_eq!(
        decode_pdf(&[0xe0], settings(EncodingMode::Group4, 0, false), 0, 3)
            .unwrap()
            .data,
        [255; 3]
    );
}

#[derive(Default)]
struct Rows(Vec<Vec<bool>>);
impl Decoder for Rows {
    fn push_pixels(&mut self, white: bool, count: u32) {
        if self.0.is_empty() {
            self.0.push(Vec::new());
        }
        self.0
            .last_mut()
            .unwrap()
            .extend(core::iter::repeat_n(white, count as usize));
    }
    fn next_line(&mut self) {
        self.0.push(Vec::new());
    }
}
#[test]
fn jbig2_row_cap_optional_eofb_and_consumption_are_preserved() {
    let mut ctx = DecoderContext::new(settings(EncodingMode::Group4, 3, true));
    for (data, consumed) in [
        (bits("111 000000000001 000000000001 11111111"), 4),
        (bits("111 00000 11111111"), 1),
    ] {
        let mut rows = Rows::default();
        assert_eq!(decode(&data, &mut rows, &mut ctx).unwrap(), consumed);
        assert_eq!(rows.0.len(), 4);
        assert!(rows.0[..3].iter().all(|row| row == &[true; 8]));
    }
    let mut rows = Rows::default();
    assert!(
        decode(
            &bits("1 001 11011 00110101 0000110111"),
            &mut rows,
            &mut ctx
        )
        .is_err()
    );
    assert_eq!(rows.0.len(), 2, "invalid row was not partly emitted");
}

#[test]
fn bounded_random_inputs_and_all_short_prefixes_do_not_panic() {
    let mut random = 0x7406_1717_u32;
    for index in 0..20000 {
        let mut data = [0_u8; 32];
        for b in &mut data {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *b = random as u8;
        }
        let mut s = settings(
            match index % 3 {
                0 => EncodingMode::Group4,
                1 => EncodingMode::Group3_1D,
                _ => EncodingMode::Group3_2D { k: 1 },
            },
            index % 7,
            index % 2 == 0,
        );
        s.columns = index % 65;
        s.end_of_line = index % 5 == 0;
        s.rows_are_byte_aligned = index % 7 == 0;
        if let Ok(image) = decode_pdf(&data[..index as usize % 33], s, 2, 128) {
            assert!(image.data.len() <= 128);
            assert_eq!(
                image.data.len(),
                (s.columns as usize).div_ceil(8) * image.height as usize
            );
            assert!(image.consumed <= index as usize % 33);
        }
    }
}
