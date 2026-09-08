use crate::bit_reader::BitWriter;
use crate::filter::FilterResult;
use crate::object::stream::{ImageColorSpace, ImageData, ImageDecodeParams};
use alloc::borrow::Cow;
use alloc::vec::Vec;
use hayro_jpeg2000::{ColorSpace, ComponentData, DecodeSettings};

impl ImageColorSpace {
    fn num_components(&self) -> u8 {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
            Self::Unknown(num) => *num,
        }
    }
}

pub(crate) fn decode(data: &[u8], params: &ImageDecodeParams) -> Option<FilterResult<'static>> {
    use crate::object::stream::ImageColorSpace;

    let settings = DecodeSettings {
        resolve_palette_indices: params.num_components.is_none(),
        strict: false,
        target_resolution: params.target_dimension,
    };

    let image = hayro_jpeg2000::Image::new(data, &settings).ok()?;

    let width = image.width();
    let height = image.height();
    // PDF 1.7 Table 89: BitsPerComponent is ignored for JPXDecode. A
    // full-resolution image dictionary must also describe the encoded extent.
    if params.target_dimension.is_none()
        && ((params.width != 0 && params.width != width)
            || (params.height != 0 && params.height != height))
    {
        return None;
    }
    if let Some(components) = params.num_components
        && image.component_bit_depths().len()
            != usize::from(components) + usize::from(image.has_alpha())
    {
        return None;
    }
    let cs = match image.color_space() {
        ColorSpace::Gray => ImageColorSpace::Gray,
        ColorSpace::RGB => ImageColorSpace::Rgb,
        ColorSpace::CMYK => ImageColorSpace::Cmyk,
        ColorSpace::Unknown { num_channels } => ImageColorSpace::Unknown(*num_channels),
        ColorSpace::Icc {
            num_channels: num_components,
            ..
        } => match num_components {
            1 => ImageColorSpace::Gray,
            3 => ImageColorSpace::Rgb,
            4 => ImageColorSpace::Cmyk,
            _ => return None,
        },
    };
    let icc_profile = match image.color_space() {
        ColorSpace::Icc { profile, .. } => Some(profile.clone()),
        _ => None,
    };
    let has_alpha = image.has_alpha();
    let mut decoder_context = hayro_jpeg2000::DecoderContext::default();
    let decoded = if params.num_components.is_some() {
        // An explicit PDF ColorSpace overrides the JP2 colr box (7.4.9).
        image.decode_unconverted(&mut decoder_context).ok()?
    } else {
        image.decode(&mut decoder_context).ok()?
    };
    let components = decoded.components();
    let color_count = usize::from(params.num_components.unwrap_or(cs.num_components()));
    if components.len() != color_count + usize::from(has_alpha) {
        return None;
    }
    let color = components.get(..color_count)?;
    let bpc = color.first()?.bit_depth();
    if !(1..=31).contains(&bpc) || color.iter().any(|c| c.bit_depth() != bpc) {
        // FilterResult describes one packed depth. Mixed colour depths need a
        // different representation; never silently normalize them through u8.
        return None;
    }
    let data = pack_components(color, width, height, bpc)?;
    let alpha = if has_alpha {
        let component = components.last()?;
        let maximum = sample_maximum(component.bit_depth())?;
        let count = (width as usize).checked_mul(height as usize)?;
        if component.samples().len() != count {
            return None;
        }
        let mut alpha = Vec::new();
        alpha.try_reserve_exact(count).ok()?;
        for &sample in component.samples() {
            if !sample.is_finite() {
                return None;
            }
            alpha.push(
                (f64::from(sample).clamp(0.0, f64::from(maximum)) * 255.0 / f64::from(maximum)
                    + 0.5) as u8,
            );
        }
        Some(alpha)
    } else {
        None
    };

    Some(FilterResult {
        data: Cow::Owned(data),
        image_data: Some(ImageData {
            icc_profile,
            alpha,
            color_space: Some(cs),
            bits_per_component: bpc,
            width,
            height,
        }),
    })
}

fn sample_maximum(bits: u8) -> Option<u32> {
    (1..=31).contains(&bits).then(|| (1_u32 << bits) - 1)
}

fn pack_components(
    components: &[ComponentData],
    width: u32,
    height: u32,
    bits: u8,
) -> Option<Vec<u8>> {
    let maximum = sample_maximum(bits)?;
    let count = (width as usize).checked_mul(height as usize)?;
    if components.iter().any(|c| c.samples().len() != count) {
        return None;
    }
    let row_bits = (width as usize)
        .checked_mul(components.len())?
        .checked_mul(usize::from(bits))?;
    let bytes = row_bits.div_ceil(8).checked_mul(height as usize)?;
    let mut data = Vec::new();
    data.try_reserve_exact(bytes).ok()?;
    data.resize(bytes, 0);
    let mut writer = BitWriter::new(&mut data, bits)?;
    for y in 0..height as usize {
        for x in 0..width as usize {
            for component in components {
                let sample = component.samples()[y * width as usize + x];
                if !sample.is_finite() {
                    return None;
                }
                // The decoder's component values are in their original sample
                // units. Round at that depth, never at an intermediate 8 bits.
                let sample = ((f64::from(sample).max(0.0) + 0.5) as u32).min(maximum);
                writer.write(sample)?;
            }
        }
        writer.align();
    }
    Some(data)
}

#[cfg(test)]
#[allow(dead_code)] // Shared controls also exercise the interpreter's color path.
#[path = "jpx_fixtures.rs"]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bit_reader::BitReader;

    #[test]
    fn native_sample_depths_and_row_padding_survive_jpx_decoding() {
        for (bits, data) in [
            (1, fixtures::DEPTH_1),
            (2, fixtures::DEPTH_2),
            (3, fixtures::DEPTH_3),
            (4, fixtures::DEPTH_4),
            (5, fixtures::DEPTH_5),
            (6, fixtures::DEPTH_6),
            (7, fixtures::DEPTH_7),
            (8, fixtures::DEPTH_8),
            (9, fixtures::DEPTH_9),
            (10, fixtures::DEPTH_10),
            (11, fixtures::DEPTH_11),
            (12, fixtures::DEPTH_12),
            (13, fixtures::DEPTH_13),
            (14, fixtures::DEPTH_14),
            (15, fixtures::DEPTH_15),
            (16, fixtures::DEPTH_16),
            (17, fixtures::DEPTH_17),
        ] {
            let params = ImageDecodeParams {
                width: 3,
                height: 2,
                bpc: Some(1),
                num_components: Some(1),
                ..Default::default()
            };
            let decoded = decode(data, &params).unwrap();
            assert_eq!(decoded.image_data.unwrap().bits_per_component, bits);
            let maximum = (1_u32 << bits) - 1;
            let expected = [0, maximum / 2, maximum, 1, maximum - 1, maximum.div_ceil(2)];
            let mut reader = BitReader::new(&decoded.data);
            for row in expected.chunks_exact(3) {
                for &sample in row {
                    assert_eq!(reader.read(bits), Some(sample), "depth {bits}");
                }
                reader.align();
            }
            assert!(
                decode(
                    data,
                    &ImageDecodeParams {
                        width: 1,
                        ..params.clone()
                    }
                )
                .is_none()
            );
            assert!(
                decode(
                    data,
                    &ImageDecodeParams {
                        num_components: Some(3),
                        ..params
                    }
                )
                .is_none()
            );
        }
    }
}
