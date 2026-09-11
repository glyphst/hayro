use super::{Decoder, DecoderContext, Image};
use crate::error::{DecodeError, FormatError, ParseError, RegionError, TemplateError};
use crate::halftone_samples::{CASES, EncodedCase, INVALID};
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

fn verify(case: &EncodedCase, context: &mut DecoderContext) {
    let image = Image::new_embedded_pdf(case.data, Some(case.globals)).expect(case.name);
    assert_eq!((image.width(), image.height()), (case.width, case.height));
    let mut pixels = Pixels::default();
    image.decode_with(&mut pixels, context).expect(case.name);
    assert_eq!(pixels.0.len(), (case.width * case.height) as usize);
    let stride = case.width.div_ceil(8);
    for y in 0..case.height {
        for x in 0..case.width {
            let expected = (case.expected[(y * stride + x / 8) as usize] >> (7 - x % 8)) & 1;
            assert_eq!(
                pixels.0[(y * case.width + x) as usize],
                expected,
                "{} ({x}, {y})",
                case.name
            );
        }
    }
}

#[test]
fn halftone_patterns_bitplanes_and_composition_preserve_authored_pixels() {
    let mut context = DecoderContext::default();
    for case in CASES.iter().chain(CASES.iter().rev()) {
        verify(case, &mut context);
    }
}

#[test]
fn halftone_invalid_flags_and_adaptive_offsets_require_errors() {
    let mut context = DecoderContext::default();
    for case in INVALID {
        let expected = match case.error_kind {
            1 => DecodeError::Format(FormatError::ReservedBits),
            2 => DecodeError::Template(TemplateError::InvalidAtPixel),
            3 => DecodeError::Region(RegionError::InvalidCombinationOperator),
            4 => DecodeError::Parse(ParseError::UnexpectedEof),
            _ => panic!("unknown expected error"),
        };
        let mut pixels = Pixels::default();
        let result = Image::new_embedded_pdf(case.data, Some(case.globals))
            .and_then(|image| image.decode_with(&mut pixels, &mut context));
        assert_eq!(result, Err(expected), "{}", case.name);
        assert!(pixels.0.is_empty(), "{} emitted partial pixels", case.name);
        verify(CASES.last().unwrap(), &mut context);
    }
}
