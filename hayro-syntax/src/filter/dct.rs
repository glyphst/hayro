use crate::object::dict::keys::COLOR_TRANSFORM;
use crate::object::stream::{FilterResult, ImageColorSpace, ImageData, ImageDecodeParams};
use crate::object::{Dict, Object};
use alloc::borrow::Cow;
use core::num::NonZeroU32;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

pub(crate) fn decode(
    data: &[u8],
    params: &Dict<'_>,
    image_params: &ImageDecodeParams,
) -> Option<FilterResult<'static>> {
    if image_params.width > u16::MAX as u32 || image_params.height > u16::MAX as u32 {
        return None;
    }

    if image_params.bpc.is_some_and(|bits| bits != 8) {
        return None;
    }
    let (sof_offset, adobe_transform) = header_metadata(data)?;

    // Some PDFs have weird JPEGs where the JPEG metadata is completely wrong
    // (for example indicating that one of the dimensions is u16::MAX), but the
    // metadata in the PDF image dictionary is correct. Therefore, we first
    // validate the JPEG metadata and patch the data if any of the dimensions
    // are too large (if they are too small, they will just be padded later on).
    let data = maybe_patch_jpeg_dimensions(data, image_params, sof_offset)?;

    let options = DecoderOptions::default()
        .set_max_width(u16::MAX as usize)
        .set_max_height(u16::MAX as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&*data), options);
    decoder.decode_headers().ok()?;

    let components = decoder.info()?.components;
    if !(1..=4).contains(&components)
        || image_params
            .num_components
            .is_some_and(|count| count != components)
    {
        return None;
    }
    // PDF 1.7 Table 13: Adobe APP14 wins over DecodeParms; otherwise the
    // default is a transform for three components and none for four. JPEG
    // component IDs and JFIF metadata do not select the PDF transform.
    let transform = color_transform(params, adobe_transform, components)?;
    let input = decoder.input_colorspace()?;
    let raw_space = match components {
        1 => ColorSpace::Luma,
        2 => ColorSpace::MultiBand(NonZeroU32::new(2)?),
        3 => match input {
            // zune identifies Adobe transform 0 as CMYK until MCU setup.
            ColorSpace::CMYK | ColorSpace::RGB => ColorSpace::RGB,
            ColorSpace::YCbCr => ColorSpace::YCbCr,
            _ => return None,
        },
        4 if matches!(input, ColorSpace::CMYK | ColorSpace::YCCK) => input,
        _ => return None,
    };
    // Keep the existing optimized conversion for ordinary three-channel JPEGs.
    // Other cases decode their original channels and apply the selected PDF
    // transform before the image's Decode array and PDF color conversion.
    let decoder_transforms = transform && components == 3 && raw_space == ColorSpace::YCbCr;
    let output = if decoder_transforms {
        ColorSpace::RGB
    } else {
        raw_space
    };
    decoder.set_options(options.jpeg_set_out_colorspace(output));
    let mut decoded = decoder.decode().ok()?;
    let (width, height) = decoder.dimensions()?;
    if decoded.len()
        != width
            .checked_mul(height)?
            .checked_mul(usize::from(components))?
    {
        return None;
    }
    if transform && !decoder_transforms {
        transform_components(&mut decoded, components);
    }

    let width = width as u32;
    let height = height as u32;

    let image_data = ImageData {
        icc_profile: None,
        alpha: None,
        color_space: Some(match components {
            1 => ImageColorSpace::Gray,
            3 => ImageColorSpace::Rgb,
            4 => ImageColorSpace::Cmyk,
            _ => ImageColorSpace::Unknown(components),
        }),
        bits_per_component: 8,
        width,
        height,
    };

    Some(FilterResult {
        data: Cow::Owned(decoded),
        image_data: Some(image_data),
    })
}

fn maybe_patch_jpeg_dimensions<'a>(
    data: &'a [u8],
    image_params: &ImageDecodeParams,
    sof_offset: usize,
) -> Option<Cow<'a, [u8]>> {
    let height_offset = sof_offset.checked_add(5)?;
    let width_offset = sof_offset.checked_add(7)?;

    let jpeg_height = u16::from_be_bytes([
        *data.get(height_offset)?,
        *data.get(height_offset.checked_add(1)?)?,
    ]);
    let jpeg_width = u16::from_be_bytes([
        *data.get(width_offset)?,
        *data.get(width_offset.checked_add(1)?)?,
    ]);

    let jpeg_area = (jpeg_width as usize).checked_mul(jpeg_height as usize)?;
    let image_area = (image_params.width as usize).checked_mul(image_params.height as usize)?;
    let need_patch = image_area != 0 && jpeg_area > image_area;

    if !need_patch {
        return Some(Cow::Borrowed(data));
    }

    let target_w = (image_params.width as u16).to_be_bytes();
    let target_h = (image_params.height as u16).to_be_bytes();

    let mut patched = data.to_vec();
    patched[height_offset..height_offset.checked_add(2)?].copy_from_slice(&target_h);
    patched[width_offset..width_offset.checked_add(2)?].copy_from_slice(&target_w);

    Some(Cow::Owned(patched))
}

fn color_transform(params: &Dict<'_>, adobe: Option<u8>, components: u8) -> Option<bool> {
    if components <= 2 {
        return Some(false);
    }
    if let Some(adobe) = adobe {
        return match (adobe, components) {
            (0, _) => Some(false),
            (1, 3) | (2, 4) => Some(true),
            _ => None,
        };
    }
    match params.get::<Object<'_>>(COLOR_TRANSFORM) {
        None | Some(Object::Null(_)) => Some(components == 3),
        Some(Object::Number(value)) => match value.as_i64_exact()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        _ => None,
    }
}

fn transform_components(data: &mut [u8], components: u8) {
    // Adobe Technical Note 5116, sections 13.1–13.2:
    // https://pdfa.org/norm-refs/5116.DCT_Filter.pdf
    for pixel in data.chunks_exact_mut(usize::from(components)) {
        let y = f64::from(pixel[0]);
        let cb = f64::from(pixel[1]) - 128.0;
        let cr = f64::from(pixel[2]) - 128.0;
        let rgb = [
            y + 1.402 * cr,
            y - 0.3441363 * cb - 0.71413636 * cr,
            y + 1.772 * cb,
        ];
        for (value, rgb) in pixel[..3].iter_mut().zip(rgb) {
            // Four-channel YUVK transforms to subtractive CMYK; K is unchanged.
            let converted = if components == 4 { 255.0 - rgb } else { rgb };
            *value = (converted.clamp(0.0, 255.0) + 0.5) as u8;
        }
    }
}

/// Read only marker segments before the first scan. Segment lengths are checked
/// before slicing; marker-looking bytes inside a payload are never interpreted.
fn header_metadata(data: &[u8]) -> Option<(usize, Option<u8>)> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut i = 2_usize;
    let mut sof = None;
    let mut adobe = None;
    loop {
        let marker_start = i;
        if *data.get(i)? != 0xff {
            return None;
        }
        while *data.get(i)? == 0xff {
            i = i.checked_add(1)?;
        }
        let marker = *data.get(i)?;
        i = i.checked_add(1)?;
        match marker {
            0x01 => continue, // TEM is standalone.
            0x00 | 0xd0..=0xd9 => return None,
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*data.get(i)?, *data.get(i + 1)?]));
        if length < 2 {
            return None;
        }
        let end = i.checked_add(length)?;
        let payload = data.get(i + 2..end)?;
        match marker {
            0xda => return Some((sof?, adobe)),
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                if sof.is_some() {
                    return None;
                }
                // The final padding FF, directly before the marker code.
                sof = Some(i - 2);
            }
            0xee if payload.starts_with(b"Adobe") => {
                let transform = *payload.get(11)?;
                if transform > 2 || adobe.is_some_and(|previous| previous != transform) {
                    return None;
                }
                adobe = Some(transform);
            }
            _ => {}
        }
        debug_assert!(end > marker_start);
        i = end;
    }
}

#[cfg(test)]
#[path = "dct_tests.rs"]
mod tests;
