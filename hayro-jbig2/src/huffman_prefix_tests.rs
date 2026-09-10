use super::{
    Decoder, DecoderContext, Image,
    huffman_prefix_samples::{CASES, INVALID},
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

fn expected() -> Vec<u8> {
    (0..7)
        .flat_map(|y| (0..9).map(move |x| u8::from((x * 3 + y * 5 + x * y) % 7 < 3)))
        .collect()
}

#[test]
fn long_huffman_prefixes_preserve_authored_pixels() {
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
fn long_huffman_prefixes_invalid_trees_and_codes_require_errors() {
    let mut context = DecoderContext::default();
    for case in INVALID {
        let result = Image::new_embedded_pdf(case.data, None)
            .and_then(|image| image.decode_with(&mut Pixels::default(), &mut context));
        assert!(result.is_err(), "{}", case.name);
        let mut recovered = Pixels::default();
        Image::new_embedded_pdf(CASES[0].data, None)
            .unwrap()
            .decode_with(&mut recovered, &mut context)
            .unwrap();
        assert_eq!(recovered.0, expected(), "after {}", case.name);
    }
}
