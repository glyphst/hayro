use super::{
    Decoder, DecoderContext, Image,
    huffman_refinement_samples::{CASES, INVALID},
};
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
fn huffman_text_uses_declared_offset_tables_in_reference_order() {
    let mut context = DecoderContext::default();
    for case in CASES.iter().chain(CASES.iter().rev()) {
        let image = Image::new_embedded_pdf(case.data, None).unwrap();
        let mut actual = Pixels::default();
        image
            .decode_with(&mut actual, &mut context)
            .expect(case.name);
        let expected: Vec<_> = (0..7)
            .flat_map(|y| {
                (0..9).map(move |x| {
                    u8::from(if case.name == "plain-huffman-text" {
                        (x * 7 + y * 3 + x * y) % 5 != 0
                    } else if case.name.starts_with("empty-") {
                        false
                    } else {
                        (x * 3 + y * 5 + x * y) % 7 < 3
                    })
                })
            })
            .collect();
        assert_eq!(actual.0, expected, "{}", case.name);
    }
}

#[test]
fn huffman_text_rejects_invalid_flags_table_counts_and_marker_roles() {
    let mut context = DecoderContext::default();
    for case in INVALID {
        let mut output = Pixels::default();
        let result = Image::new_embedded_pdf(case.data, None)
            .and_then(|image| image.decode_with(&mut output, &mut context));
        assert!(result.is_err(), "{}", case.name);
        let mut valid = Pixels::default();
        Image::new_embedded_pdf(CASES[0].data, None)
            .unwrap()
            .decode_with(&mut valid, &mut context)
            .unwrap();
        let expected: Vec<_> = (0..7)
            .flat_map(|y| (0..9).map(move |x| u8::from((x * 7 + y * 3 + x * y) % 5 != 0)))
            .collect();
        assert_eq!(valid.0, expected, "after {}", case.name);
    }
}
