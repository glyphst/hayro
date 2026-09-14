//! Preserve sampled tint inputs through Decode before the alternate transform.
use super::{
    DecodeContext,
    color_output::{ColorOutput, ColorTarget},
    samples::Samples,
};
use crate::x_object::image::{ImageXObject, uses_jpx_decode};
use crate::{CmykData, ImageData};

pub(super) fn needs_native_samples(context: &DecodeContext<'_>) -> bool {
    // Existing byte lookup caches are exact for <=8-bit normalized samples
    // and integral default palette indices. Keep that bounded fast path; a
    // changed Decode map or higher source depth must reach the real evaluator.
    if !matches!(context.bits_per_component, 1 | 2 | 4 | 8) {
        return true;
    }
    let ranges = if context.color_space.is_indexed() {
        context
            .color_space
            .default_decode_arr(f32::from(context.bits_per_component))
    } else {
        context.color_space.component_ranges()
    };
    context.decode_arr.len() != ranges.len()
        || context
            .decode_arr
            .iter()
            .zip(ranges)
            .any(|(&(low, high), (start, end))| {
                (low, high) != (start, end) && (low, high) != (end, start)
            })
}

pub(super) fn decode(obj: &ImageXObject<'_>, context: &DecodeContext<'_>) -> Option<ImageData> {
    decode_output(obj, context, ColorTarget::Rgb)?.finish(obj, context)
}

pub(super) fn decode_cmyk(obj: &ImageXObject<'_>, context: &DecodeContext<'_>) -> Option<CmykData> {
    decode_output(obj, context, ColorTarget::Cmyk)?.finish_cmyk(obj, context)
}

fn decode_output<'a>(
    obj: &ImageXObject<'_>,
    context: &'a DecodeContext<'_>,
    target: ColorTarget,
) -> Option<ColorOutput<'a>> {
    let components = usize::from(context.color_space.num_components());
    super::float::validate_decode(obj, components).ok()?;
    let count = usize::try_from(u64::from(context.width) * u64::from(context.height)).ok()?;
    let mut samples = Samples::new(context, uses_jpx_decode(obj.stream.dict())).ok()?;
    let mut output = ColorOutput::with_target(&context.color_space, count, target)?;
    if target == ColorTarget::Cmyk && components == 1 && !needs_native_samples(context) {
        output.cache_byte_tints()?;
    }
    let mut values = Vec::new();
    values.try_reserve_exact(components).ok()?;
    values.resize(components, 0.0);
    for _ in 0..count {
        samples.read(&mut values).ok()?;
        output.push(&values)?;
    }
    Some(output)
}

#[cfg(test)]
mod tests;
