//! Full-precision decoded image colors for retained transfer execution.
use super::decode_context;
use crate::color::ColorSpaceKind;
use crate::x_object::image::{ImageKind, ImageXObject, uses_jpx_decode};
use crate::{FloatImageError as Error, RgbF32Data};
use hayro_syntax::bit_reader::BitReader;
use hayro_syntax::object::{Array, Object};

pub(crate) fn decode_rgb_f32(
    obj: &ImageXObject<'_>,
    max_bytes: u64,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<RgbF32Data, Error> {
    if !checkpoint() {
        return Err(Error::Cancelled);
    }
    let dict = obj.stream.dict();
    let components = match obj.color_space.as_ref().map(|space| space.kind()) {
        Some(ColorSpaceKind::DeviceGray | ColorSpaceKind::CalGray) => 1,
        Some(ColorSpaceKind::DeviceRgb | ColorSpaceKind::CalRgb) => 3,
        _ => return Err(Error::Unsupported),
    };
    if obj.kind != ImageKind::Image
        || obj.transfer_function.is_some()
        || uses_jpx_decode(dict)
        || dict.contains_key(b"Mask")
        || dict.contains_key(b"SMask")
    {
        return Err(Error::Unsupported);
    }
    // Do not let typed array iteration silently discard malformed members.
    for key in [b"D".as_slice(), b"Decode".as_slice()] {
        match dict.get::<Object<'_>>(key) {
            None | Some(Object::Null(_)) => {}
            Some(Object::Array(array)) if valid_decode(&array, components) => {}
            _ => return Err(Error::Unsupported),
        }
    }
    output_bytes(obj.width, obj.height, max_bytes)?;
    let context = decode_context(obj, None).ok_or(Error::Decode)?;
    if !checkpoint() {
        return Err(Error::Cancelled);
    }
    if !matches!(context.bits_per_component, 1 | 2 | 4 | 8 | 16)
        || context.color_space.num_components() as usize != components
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
    let capacity = output_bytes(context.width, context.height, max_bytes)?;
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
                .image_rgb_f32(&values[..components])
                .ok_or(Error::Decode)?;
            for value in rgb {
                output.extend_from_slice(&value.to_le_bytes());
            }
        }
        reader.align();
    }
    if !checkpoint() {
        return Err(Error::Cancelled);
    }
    Ok(RgbF32Data {
        data: output,
        width: context.width,
        height: context.height,
        interpolate: obj.interpolate,
        scale_factors: context.scale_factors,
    })
}

fn output_bytes(width: u32, height: u32, limit: u64) -> Result<usize, Error> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|value| value.checked_mul(12))
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
