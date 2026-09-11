//! Generic template 0 with twelve adaptive pixels (T.88 Amendment 2).

use super::AdaptiveTemplatePixel;
use crate::arithmetic_decoder::{ArithmeticDecoder, ArithmeticDecoderContext};
use crate::bitmap::Bitmap;

pub(super) fn decode(
    bitmap: &mut Bitmap,
    decoder: &mut ArithmeticDecoder<'_>,
    contexts: &mut [ArithmeticDecoderContext],
    tpgdon: bool,
    pixels: &[AdaptiveTemplatePixel; 12],
) {
    // Reading order at the nominal positions in Amendment 2 Figure 3(b),
    // matching the ordinary template-0 context layout. Only four pixels
    // remain fixed: (-1, 0), (-1, -1), (0, -1), and (1, -1).
    const AT_BITS: [u32; 12] = [1, 13, 9, 14, 12, 5, 2, 3, 11, 4, 15, 10];
    let mut ltp = false;
    for y in 0..bitmap.height {
        if tpgdon {
            // Figure 8: moving AT pixels never changes the SLTP context.
            ltp ^= decoder.read_bit(&mut contexts[0x9b25]) != 0;
        }
        if ltp {
            if y > 0 {
                let stride = bitmap.stride as usize;
                let src = (y as usize - 1) * stride;
                bitmap
                    .data
                    .copy_within(src..src + stride, y as usize * stride);
            }
            continue;
        }
        for x in 0..bitmap.width {
            let sample = |dx: i32, dy: i32| {
                // Bitmap dimensions are bounded by u16::MAX. Negative
                // coordinates cast to out-of-bounds u32 and sample zero.
                bitmap.get_pixel((x as i32 + dx) as u32, (y as i32 + dy) as u32) as usize
            };
            let mut context =
                sample(-1, 0) | (sample(-1, -1) << 8) | (sample(0, -1) << 7) | (sample(1, -1) << 6);
            for (pixel, bit) in pixels.iter().zip(AT_BITS) {
                context |= sample(i32::from(pixel.x), i32::from(pixel.y)) << bit;
            }
            let pixel = decoder.read_bit(&mut contexts[context]) as u8;
            if pixel != 0 {
                bitmap.set_pixel(x, y, pixel);
            }
        }
    }
}
