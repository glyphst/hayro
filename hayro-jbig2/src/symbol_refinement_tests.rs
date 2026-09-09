use super::{Decoder, DecoderContext, Image, symbol_refinement_samples::CASES};
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

#[test]
fn symbol_and_text_refinement_preserve_shifted_reference_pixels() {
    let mut context = DecoderContext::default();
    for case in CASES.iter().chain(CASES.iter().rev()) {
        let image = Image::new_embedded_pdf(case.data, None).unwrap();
        let mut actual = Pixels::default();
        image
            .decode_with(&mut actual, &mut context)
            .expect(case.name);
        let expected: Vec<_> = (0..case.height)
            .flat_map(|y| {
                (0..case.width).map(move |x| {
                    u8::from(if case.name == "base-symbol-text" {
                        (x * 7 + y * 3 + x * y) % 5 != 0
                    } else {
                        (x * 3 + y * 5 + x * y) % 7 < 3
                    })
                })
            })
            .collect();
        assert_eq!(actual.0, expected, "{}", case.name);
    }
}
