//! Full-precision decoded image colors for retained transfer execution.
use super::{DecodeContext, decode_context};
use crate::color::ColorSpaceKind;
use crate::x_object::image::{ImageKind, ImageXObject, uses_jpx_decode};
use crate::{FloatImageError as Error, RgbF32Data, RgbF64Data};
use hayro_syntax::bit_reader::BitReader;
use hayro_syntax::object::{Array, Object};

pub(crate) fn decode_rgb_f32(
    obj: &ImageXObject<'_>,
    max_bytes: u64,
    checkpoint: impl FnMut() -> bool,
) -> Result<RgbF32Data, Error> {
    let decoded = decode_rgb(obj, max_bytes, checkpoint, |v| (v as f32).to_le_bytes())?;
    Ok(RgbF32Data {
        data: decoded.data,
        width: decoded.width,
        height: decoded.height,
        interpolate: decoded.interpolate,
        scale_factors: decoded.scale_factors,
    })
}

pub(crate) fn decode_rgb_f64(
    obj: &ImageXObject<'_>,
    max_bytes: u64,
    checkpoint: impl FnMut() -> bool,
) -> Result<RgbF64Data, Error> {
    let decoded = decode_rgb(obj, max_bytes, checkpoint, f64::to_le_bytes)?;
    Ok(RgbF64Data {
        data: decoded.data,
        width: decoded.width,
        height: decoded.height,
        interpolate: decoded.interpolate,
        scale_factors: decoded.scale_factors,
    })
}

struct DecodedFloat {
    data: Vec<u8>,
    width: u32,
    height: u32,
    interpolate: bool,
    scale_factors: (f32, f32),
}

fn decode_rgb<const BYTES: usize>(
    obj: &ImageXObject<'_>,
    max_bytes: u64,
    mut checkpoint: impl FnMut() -> bool,
    encode: impl Fn(f64) -> [u8; BYTES],
) -> Result<DecodedFloat, Error> {
    if !checkpoint() {
        return Err(Error::Cancelled);
    }
    let dict = obj.stream.dict();
    let jpx = uses_jpx_decode(dict);
    let components = match obj.color_space.as_ref().map(|space| space.kind()) {
        Some(ColorSpaceKind::DeviceGray | ColorSpaceKind::CalGray) => 1,
        Some(ColorSpaceKind::DeviceRgb | ColorSpaceKind::CalRgb | ColorSpaceKind::Lab) => 3,
        _ => return Err(Error::Unsupported),
    };
    if obj.kind != ImageKind::Image
        || obj.transfer_function.is_some()
        || dict.contains_key(b"Mask")
        || dict.contains_key(b"SMask")
        || obj.embedded_alpha_mode().is_some()
    {
        return Err(Error::Unsupported);
    }
    validate_decode(obj, components)?;
    output_bytes(obj.width, obj.height, BYTES, max_bytes)?;
    let context = decode_context(obj, None).ok_or(Error::Decode)?;
    if !checkpoint() {
        return Err(Error::Cancelled);
    }
    if context.color_space.num_components() as usize != components {
        return Err(Error::Unsupported);
    }
    let data = decode_context_rgb(&context, jpx, max_bytes, checkpoint, encode)?;
    Ok(DecodedFloat {
        data,
        width: context.width,
        height: context.height,
        interpolate: obj.interpolate,
        scale_factors: context.scale_factors,
    })
}

pub(super) fn validate_decode(obj: &ImageXObject<'_>, components: usize) -> Result<(), Error> {
    let dict = obj.stream.dict();
    // Do not let typed array iteration silently discard malformed members.
    for key in [b"D".as_slice(), b"Decode".as_slice()]
        .into_iter()
        .filter(|_| !uses_jpx_decode(dict))
    {
        match dict.get::<Object<'_>>(key) {
            None | Some(Object::Null(_)) => {}
            Some(Object::Array(array)) if valid_decode(&array, components) => {}
            _ => return Err(Error::Unsupported),
        }
    }
    Ok(())
}

// Share native component decoding with ordinary unmasked Lab images. The
// caller retains decoding ownership and chooses the final RGB storage width.
pub(super) fn decode_context_rgb<const BYTES: usize>(
    context: &DecodeContext<'_>,
    jpx: bool,
    max_bytes: u64,
    mut checkpoint: impl FnMut() -> bool,
    encode: impl Fn(f64) -> [u8; BYTES],
) -> Result<Vec<u8>, Error> {
    let components = match context.color_space.kind() {
        ColorSpaceKind::DeviceGray | ColorSpaceKind::CalGray => 1,
        ColorSpaceKind::DeviceRgb | ColorSpaceKind::CalRgb | ColorSpaceKind::Lab => 3,
        _ => return Err(Error::Unsupported),
    };
    if !(if jpx {
        (1..=16).contains(&context.bits_per_component)
    } else {
        matches!(context.bits_per_component, 1 | 2 | 4 | 8 | 16)
    }) || context.color_space.num_components() as usize != components
        || context.decode_arr.len() != components
        || context
            .decode_arr
            .iter()
            .any(|(a, b)| !a.is_finite() || !b.is_finite())
        || context
            .decoded
            .image_data
            .as_ref()
            .is_some_and(|data| data.alpha.is_some())
    {
        return Err(Error::Unsupported);
    }
    let capacity = output_bytes(context.width, context.height, BYTES, max_bytes)?;
    let row_bits = u64::from(context.width)
        .checked_mul(components as u64)
        .and_then(|value| value.checked_mul(u64::from(context.bits_per_component)))
        .ok_or(Error::Limit {
            requested: u64::MAX,
        })?;
    let required = row_bits
        .div_ceil(8)
        .checked_mul(u64::from(context.height))
        .ok_or(Error::Limit {
            requested: u64::MAX,
        })?;
    if required > context.decoded.data.len() as u64 {
        // Incomplete high-precision samples must never become padded colors.
        return Err(Error::Decode);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Allocation)?;
    let maximum = (1_u32 << context.bits_per_component) - 1;
    let mut reader = BitReader::new(&context.decoded.data);
    let mut index = 0;
    for _ in 0..context.height {
        for _ in 0..context.width {
            if index % 1024 == 0 && !checkpoint() {
                return Err(Error::Cancelled);
            }
            index += 1;
            let mut values = [0.0_f64; 3];
            for (channel, value) in values.iter_mut().take(components).enumerate() {
                let sample = reader
                    .read(context.bits_per_component)
                    .ok_or(Error::Decode)?;
                let (low, high) = context.decode_arr[channel];
                *value = decode_sample(sample, maximum, low, high);
            }
            let rgb = context
                .color_space
                .image_rgb_f64(&values[..components])
                .ok_or(Error::Decode)?;
            for value in rgb {
                output.extend_from_slice(&encode(value));
            }
        }
        reader.align();
    }
    if !checkpoint() {
        return Err(Error::Cancelled);
    }
    Ok(output)
}

fn output_bytes(
    width: u32,
    height: u32,
    component_bytes: usize,
    limit: u64,
) -> Result<usize, Error> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|value| value.checked_mul(3 * component_bytes as u64))
        .ok_or(Error::Limit {
            requested: u64::MAX,
        })?;
    if bytes > limit {
        return Err(Error::Limit { requested: bytes });
    }
    usize::try_from(bytes).map_err(|_| Error::Limit { requested: bytes })
}

fn valid_decode(array: &Array<'_>, components: usize) -> bool {
    array.raw_iter().take(components * 2 + 1).count() == components * 2
        && array.iter::<f32>().count() == components * 2
        && array.iter::<f32>().all(f32::is_finite)
}

fn decode_sample(sample: u32, maximum: u32, low: f32, high: f32) -> f64 {
    // A binary32 coefficient times a <=16-bit integer is exact in binary64.
    // Compensate their sum before division, retaining opposing Decode bounds.
    let a = f64::from(low) * f64::from(maximum - sample);
    let b = f64::from(high) * f64::from(sample);
    let sum = a + b;
    let virtual_b = sum - a;
    let error = (a - (sum - virtual_b)) + (b - virtual_b);
    sum / f64::from(maximum) + error / f64::from(maximum)
}

#[cfg(test)]
mod tests;
