use super::{
    Decoder, DecoderContext, Image,
    segment_retention_samples::{CASES, INVALID},
};
use crate::error::{DecodeError, FormatError, ParseError, SegmentError};
use crate::reader::Reader;
use crate::segment::parse_segment_header;
use alloc::vec::Vec;

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

fn expected() -> Vec<u8> {
    (0..7)
        .flat_map(|y| (0..9).map(move |x| u8::from((x * 3 + y * 5 + x * y) % 7 < 3)))
        .collect()
}

#[test]
fn segment_retentions_preserve_authored_pixels() {
    let mut context = DecoderContext::default();
    for case in CASES.iter().chain(CASES.iter().rev()) {
        let image = Image::new_embedded_pdf(case.data, None).unwrap();
        let mut actual = Pixels::default();
        image
            .decode_with(&mut actual, &mut context)
            .expect(case.name);
        assert_eq!(actual.0, expected(), "{}", case.name);
    }
}

#[test]
fn segment_retentions_invalid_padding_and_truncation_require_errors() {
    let mut context = DecoderContext::default();
    for case in INVALID {
        let result = Image::new_embedded_pdf(case.data, None)
            .and_then(|image| image.decode_with(&mut Pixels::default(), &mut context));
        let expected_error = if case.name.starts_with("truncated-") {
            DecodeError::Parse(ParseError::UnexpectedEof)
        } else if case.name.starts_with("prohibited-") {
            DecodeError::Segment(SegmentError::InvalidReferredCount)
        } else {
            DecodeError::Format(FormatError::ReservedBits)
        };
        assert_eq!(result, Err(expected_error), "{}", case.name);
        assert_eq!(
            Image::new_embedded(case.data, None).err().unwrap(),
            expected_error,
            "{}",
            case.name
        );
        let mut standalone = Vec::from(&b"\x97JB2\r\n\x1a\n\x03"[..]);
        standalone.extend_from_slice(case.data);
        assert_eq!(
            Image::new(&standalone).err().unwrap(),
            expected_error,
            "{}",
            case.name
        );
        let mut recovered = Pixels::default();
        Image::new_embedded_pdf(CASES[0].data, None)
            .unwrap()
            .decode_with(&mut recovered, &mut context)
            .unwrap();
        assert_eq!(recovered.0, expected(), "after {}", case.name);
    }
}

// Header-only controls cover every possible short byte and every final long
// retention byte, including completely occupied bytes at counts 7/15/23/31.
#[test]
fn segment_retentions_short_header_bytes() {
    for count in 0_u8..=4 {
        for flags in 0_u8..32 {
            let mut data = Vec::from(&[0, 0, 1, 0, 62, (count << 5) | flags][..]);
            data.extend(0..count);
            data.extend([1, 0, 0, 0, 0]);
            let mut reader = Reader::new(&data);
            let result = parse_segment_header(&mut reader);
            if u32::from(flags) < (1 << (count + 1)) {
                let header = result.unwrap();
                assert_eq!(
                    header.referred_to_segments,
                    (0..u32::from(count)).collect::<Vec<_>>()
                );
                assert_eq!(header.page_association, 1);
                assert!(reader.at_end());
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    DecodeError::Format(FormatError::ReservedBits)
                );
            }
        }
    }
}

#[test]
fn segment_retentions_long_header_bytes_and_truncation() {
    for count in 5_u32..=65 {
        for last in 0_u8..=255 {
            let mut data = Vec::from(&[0, 1, 0, 1, 126][..]);
            data.extend_from_slice(&(0xE000_0000 | count).to_be_bytes());
            // Write one self flag and count reference flags, then byte padding.
            let mut flags = Vec::new();
            for bit in 0..=count {
                if bit % 8 == 0 {
                    flags.push(0_u8);
                }
                *flags.last_mut().unwrap() |= 1 << (bit % 8);
            }
            *flags.last_mut().unwrap() = last;
            data.extend(flags);
            for number in 0..count {
                data.extend_from_slice(&number.to_be_bytes());
            }
            data.extend([0, 0, 0, 1, 0, 0, 0, 0]);
            let mut reader = Reader::new(&data);
            let result = parse_segment_header(&mut reader);
            let used = (count + 1) % 8;
            if used == 0 || u32::from(last) < (1 << used) {
                let header = result.unwrap();
                assert_eq!(header.referred_to_segments, (0..count).collect::<Vec<_>>());
                assert_eq!(header.page_association, 1);
                assert_eq!(header.data_length, Some(0));
                assert!(reader.at_end());
                if last == 0 {
                    for end in 5..data.len() {
                        assert_eq!(
                            parse_segment_header(&mut Reader::new(&data[..end])).unwrap_err(),
                            DecodeError::Parse(ParseError::UnexpectedEof)
                        );
                    }
                }
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    DecodeError::Format(FormatError::ReservedBits)
                );
            }
        }
    }
    let max_count = [0, 1, 0, 1, 62, 255, 255, 255, 255];
    assert_eq!(
        parse_segment_header(&mut Reader::new(&max_count)).unwrap_err(),
        DecodeError::Parse(ParseError::UnexpectedEof)
    );
}
