use super::{Decoder, DecoderContext, Image};
use crate::error::{DecodeError, FormatError};
use crate::reference_lifetime_samples::{CASES, INVALID};
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

fn expected(shift: bool) -> Vec<u8> {
    (0_u32..7)
        .flat_map(|y| {
            (0_u32..9).map(move |x| {
                x.checked_sub(u32::from(shift))
                    .map_or(0, |x| u8::from((x * 3 + y * 5 + x * y) % 7 < 3))
            })
        })
        .collect()
}

#[test]
fn reference_lifetimes_preserve_bitmaps_and_shared_globals() {
    let mut context = DecoderContext::default();
    for case in CASES.iter().chain(CASES.iter().rev()) {
        for (data, shift) in [
            (case.data, case.first_shift),
            (case.second, case.second_shift),
        ] {
            if data.is_empty() {
                continue;
            }
            let image = Image::new_embedded_pdf(data, Some(case.globals)).expect(case.name);
            let mut pixels = Pixels::default();
            image
                .decode_with(&mut pixels, &mut context)
                .expect(case.name);
            assert_eq!(pixels.0, expected(shift), "{}", case.name);
        }
    }
}

#[test]
fn reference_lifetimes_require_live_targets_and_valid_attachments() {
    let mut context = DecoderContext::default();
    for case in INVALID {
        assert_eq!(
            Image::new_embedded_pdf(case.data, Some(case.globals)).map(|_| ()),
            Err(DecodeError::Format(FormatError::InvalidPdfEmbedding)),
            "{}",
            case.name,
        );
        let recovery = CASES.last().unwrap();
        for (data, shift) in [
            (recovery.data, recovery.first_shift),
            (recovery.second, recovery.second_shift),
        ] {
            let mut pixels = Pixels::default();
            Image::new_embedded_pdf(data, Some(recovery.globals))
                .unwrap()
                .decode_with(&mut pixels, &mut context)
                .unwrap();
            assert_eq!(pixels.0, expected(shift), "after {}", case.name);
        }
    }
}
