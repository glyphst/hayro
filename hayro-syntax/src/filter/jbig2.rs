use crate::bit_reader::BitWriter;
use crate::object::Dict;
use crate::object::Object;
use crate::object::dict::keys::JBIG2_GLOBALS;
use crate::object::stream::{FilterResult, ImageColorSpace, ImageData, ImageDecodeParams};
use alloc::borrow::Cow;
use alloc::vec;

/// Decode JBIG2 data from a PDF stream.
///
/// The `params` dictionary may contain a `JBIG2Globals` entry pointing to
/// a stream with shared symbol dictionaries.
pub(crate) fn decode(
    data: &[u8],
    params: &Dict<'_>,
    image_params: &ImageDecodeParams,
) -> Option<FilterResult<'static>> {
    let globals = match crate::object::stream::optional_entry(params, JBIG2_GLOBALS).ok()? {
        None => None,
        Some(Object::Stream(stream)) => Some(stream.decoded().ok()?),
        _ => return None,
    };
    let image = hayro_jbig2::Image::new_embedded_pdf(data, globals.as_deref()).ok()?;
    if image_params.bpc.is_some_and(|bpc| bpc != 1)
        || image_params.num_components.is_some_and(|count| count != 1)
        || (image_params.width != 0 && image_params.width != image.width())
        || (image_params.height != 0 && image_params.height != image.height())
    {
        return None;
    }

    // JBIG2Decode always returns packed, row-padded one-bit PDF samples. Generic
    // streams (including compressed JBIG2Globals) consume these exact bytes.
    // JBIG2 black=1 is the opposite of PDF black=0.
    let row_bytes = (image.width() as usize).div_ceil(8);
    let mut packed = vec![0_u8; row_bytes.checked_mul(image.height() as usize)?];
    struct BitWriterDecoder<'a> {
        writer: BitWriter<'a>,
    }
    impl hayro_jbig2::Decoder for BitWriterDecoder<'_> {
        fn push_pixel(&mut self, black: bool) {
            let _ = self.writer.write(u32::from(!black));
        }
        fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
            let _ = self
                .writer
                .fill_bytes(if black { 0 } else { 255 }, chunk_count as usize);
        }
        fn next_line(&mut self) {
            self.writer.align();
        }
    }
    let mut decoder = BitWriterDecoder {
        writer: BitWriter::new(&mut packed, 1)?,
    };
    image.decode(&mut decoder).ok()?;

    Some(FilterResult {
        data: Cow::Owned(packed),
        image_data: Some(ImageData {
            jpx_samples: None,
            icc_profile: None,
            alpha: None,
            color_space: Some(ImageColorSpace::Gray),
            bits_per_component: 1,
            width: image.width(),
            height: image.height(),
        }),
    })
}
