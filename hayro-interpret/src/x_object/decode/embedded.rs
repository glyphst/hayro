//! T.800 (2002) I-3: recover channel samples before PDF component mapping.
use super::DecodeContext;
use super::color_output::ColorOutput;
use super::image::DecodedImage;
use super::samples::decode_sample;
use crate::x_object::image::ImageXObject;
use crate::{EmbeddedImageAlphaMode, LumaData};
use hayro_syntax::object::stream::JpxSamples;

struct Samples<'a> {
    source: &'a JpxSamples,
    context: &'a DecodeContext<'a>,
    premultiplied: bool,
    count: usize,
}

impl<'a> Samples<'a> {
    fn new(obj: &ImageXObject<'_>, context: &'a DecodeContext<'a>) -> Option<Self> {
        let mode = obj.embedded_alpha_mode()?;
        let source = context.decoded.image_data.as_ref()?.jpx_samples.as_ref()?;
        let count = (context.width as usize).checked_mul(context.height as usize)?;
        let components = usize::from(context.color_space.num_components());
        if count == 0
            || components == 0
            || source.components.len() != components + 1
            || context.decode_arr.len() != components
            || context
                .decode_arr
                .iter()
                .any(|(a, b)| !a.is_finite() || !b.is_finite())
            || source.components.iter().any(|component| {
                component.samples.len() != count
                    || !(1..=16).contains(&component.bits_per_component)
            })
        {
            return None;
        }
        Some(Self {
            source,
            context,
            count,
            premultiplied: mode == EmbeddedImageAlphaMode::Premultiplied
                && !source.premultiplication_removed,
        })
    }

    fn read(&self, pixel: usize, values: &mut [f64]) -> Option<()> {
        let (alpha, color) = self.source.components.split_last()?;
        let alpha_max = f64::from((1_u32 << alpha.bits_per_component) - 1);
        let alpha_sample = f64::from(*alpha.samples.get(pixel)?);
        if !alpha_sample.is_finite() {
            return None;
        }
        let alpha_sample = alpha_sample.clamp(0.0, alpha_max);
        for ((value, component), &(low, high)) in
            values.iter_mut().zip(color).zip(&self.context.decode_arr)
        {
            let maximum = f64::from((1_u32 << component.bits_per_component) - 1);
            let sample = f64::from(*component.samples.get(pixel)?);
            if !sample.is_finite() {
                return None;
            }
            let sample = sample.clamp(0.0, maximum);
            let sample = if self.premultiplied {
                if alpha_sample == 0.0 {
                    0.0
                } else {
                    // Use the native opacity integer, never its normalized byte.
                    // Multiplication before division also preserves exact index halves.
                    (sample * alpha_max / alpha_sample).clamp(0.0, maximum)
                }
            } else {
                sample
            };
            *value = f64::from(low) + sample / maximum * (f64::from(high) - f64::from(low));
        }
        Some(())
    }
}

pub(super) fn decode(
    obj: &ImageXObject<'_>,
    context: &mut DecodeContext<'_>,
) -> Option<DecodedImage> {
    let source = Samples::new(obj, context)?;
    let indexed = context.color_space.indexed();
    let space = indexed.map_or(&context.color_space, |space| space.base());
    let ranges = space.component_ranges();
    let mut output = ColorOutput::new(space, source.count)?;
    let mut values = vec![0.0; usize::from(context.color_space.num_components())];
    let mut palette_values = vec![0.0; usize::from(space.num_components())];
    for pixel in 0..source.count {
        source.read(pixel, &mut values)?;
        if let Some(indexed) = indexed {
            // JPX multiplies indices themselves. PDF's nearest-index selection
            // follows recovery, unlike an external soft-mask Matte operation.
            for ((value, &sample), &(low, high)) in palette_values
                .iter_mut()
                .zip(indexed.entry(values[0]))
                .zip(&ranges)
            {
                *value = decode_sample(u32::from(sample), 255, low, high);
            }
            output.push(&palette_values)?;
        } else {
            output.push(&values)?;
        }
    }
    let image = output.finish(obj, context)?;
    let alpha = context.decoded.image_data.as_mut()?.alpha.take()?;
    Some(DecodedImage {
        image,
        alpha: Some(LumaData {
            data: alpha,
            width: context.width,
            height: context.height,
            interpolate: obj.interpolate,
            scale_factors: context.scale_factors,
        }),
    })
}

pub(super) fn components(obj: &ImageXObject<'_>, context: &DecodeContext<'_>) -> Option<Vec<u8>> {
    let source = Samples::new(obj, context)?;
    let components = usize::from(context.color_space.num_components());
    let mut data = Vec::new();
    data.try_reserve_exact(source.count.checked_mul(components)?)
        .ok()?;
    let mut values = vec![0.0; components];
    for pixel in 0..source.count {
        source.read(pixel, &mut values)?;
        data.extend(
            values
                .iter()
                .zip(context.color_space.component_ranges())
                .map(|(&value, (low, high))| {
                    super::color_output::encode_component(value, low, high)
                }),
        );
    }
    Some(data)
}

#[cfg(test)]
mod tests;
