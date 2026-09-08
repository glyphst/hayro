//! PDF 32000-1 11.6.5.3: undo preblending before source color conversion.
use super::float::validate_decode;
use super::image::DecodedImage;
use super::samples::Samples;
use super::{DecodeContext, decode_context};
use crate::color::{ColorSpaceKind, ToRgb};
use crate::x_object::image::{ImageXObject, uses_jpx_decode};
use crate::{ImageData, LumaData, RgbData};
use hayro_syntax::object::dict::keys::*;
use hayro_syntax::object::{Array, Name, Object, Stream};
use smallvec::SmallVec;

pub(super) fn decode(
    obj: &ImageXObject<'_>,
    context: &DecodeContext<'_>,
    target_dimension: Option<(u32, u32)>,
) -> Option<DecodedImage> {
    let mask = obj.stream.dict().get::<Stream<'_>>(SMASK)?;
    let dict = mask.dict();
    let components = context.color_space.num_components() as usize;
    let array = dict.get::<Array<'_>>(MATTE)?;
    if array.raw_iter().take(components + 1).count() != components {
        return None;
    }
    let matte = array.iter::<f32>().collect::<SmallVec<[f32; 4]>>();
    if !context.color_space.image_matte_is_valid(&matte)
        || dict.get::<Name<'_>>(SUBTYPE)?.as_ref() != b"Image"
        || dict
            .get::<Name<'_>>(COLORSPACE)
            .or_else(|| dict.get::<Name<'_>>(CS))?
            .as_ref()
            != b"DeviceGray"
        || dict.contains_key(MASK)
        || dict.contains_key(SMASK)
        || (dict.contains_key(TYPE) && dict.get::<Name<'_>>(TYPE)?.as_ref() != b"XObject")
    {
        return None;
    }
    for key in [IM, IMAGE_MASK] {
        if !matches!(
            dict.get::<Object<'_>>(key),
            None | Some(Object::Null(_)) | Some(Object::Boolean(false))
        ) {
            return None;
        }
    }
    let mask_obj = ImageXObject::new_mask(&mask, &obj.warning_sink, &obj.cache)?;
    if (mask_obj.width, mask_obj.height) != (obj.width, obj.height)
        || (!uses_jpx_decode(dict)
            && dict
                .get::<u8>(BITS_PER_COMPONENT)
                .or_else(|| dict.get::<u8>(BPC))
                .is_none())
    {
        return None;
    }
    validate_decode(obj, components).ok()?;
    validate_decode(&mask_obj, 1).ok()?;
    let mask_context = decode_context(&mask_obj, target_dimension)?;
    if (context.width, context.height) != (mask_context.width, mask_context.height)
        || context
            .decoded
            .image_data
            .as_ref()
            .is_some_and(|data| data.alpha.is_some())
        || mask_context
            .decoded
            .image_data
            .as_ref()
            .is_some_and(|data| data.alpha.is_some())
    {
        return None;
    }
    let mut source = Samples::new(context, uses_jpx_decode(obj.stream.dict())).ok()?;
    let mut mask_samples = Samples::new(&mask_context, uses_jpx_decode(dict)).ok()?;
    let count = usize::try_from(u64::from(context.width) * u64::from(context.height)).ok()?;
    let gray = context.color_space.kind() == ColorSpaceKind::DeviceGray;
    let native = matches!(
        context.color_space.kind(),
        ColorSpaceKind::DeviceGray
            | ColorSpaceKind::DeviceRgb
            | ColorSpaceKind::CalGray
            | ColorSpaceKind::CalRgb
            | ColorSpaceKind::Lab
    );
    let channels = if gray {
        1
    } else if native {
        3
    } else {
        components
    };
    let mut data = Vec::new();
    data.try_reserve_exact(count.checked_mul(channels)?).ok()?;
    let mut alpha = Vec::new();
    alpha.try_reserve_exact(count).ok()?;
    let mut values = vec![0.0; components];
    let ranges = context.color_space.component_ranges();
    for _ in 0..count {
        source.read(&mut values).ok()?;
        let mut opacity = [0.0];
        mask_samples.read(&mut opacity).ok()?;
        let opacity = opacity[0].clamp(0.0, 1.0);
        alpha.push((opacity * 255.0).round() as u8);
        for ((value, matte), &(low, high)) in values.iter_mut().zip(&matte).zip(&ranges) {
            let matte = f64::from(*matte);
            // At zero opacity the original source is unrecoverable. Its color
            // cannot contribute, so use the declared matte deterministically.
            *value = if opacity == 0.0 {
                matte
            } else {
                matte + (*value - matte) / opacity
            }
            .clamp(f64::from(low), f64::from(high));
        }
        if native {
            let rgb = context.color_space.image_rgb_f64(&values)?;
            data.extend(
                rgb.into_iter()
                    .take(channels)
                    .map(|v| (v * 255.0).round() as u8),
            );
        } else {
            // Keep the existing byte-input ICC/tint policy, but quantize only
            // after recovering the source components. Convert the full batch.
            let values = values
                .iter()
                .map(|v| *v as f32)
                .collect::<SmallVec<[f32; 4]>>();
            data.extend(context.color_space.encode_values(&values));
        }
    }
    if !native && context.color_space.convert_in_place(&mut data).is_none() {
        let mut output = Vec::new();
        output.try_reserve_exact(count.checked_mul(3)?).ok()?;
        output.resize(count * 3, 0);
        context.color_space.convert(&data, &mut output)?;
        data = output;
    }
    let mut gray = gray;
    if gray
        && obj.transfer_function.as_ref().is_some_and(|transfer| {
            !matches!(
                transfer,
                crate::interpret::state::ActiveTransferFunction::Single(_)
            )
        })
    {
        let mut rgb = Vec::new();
        rgb.try_reserve_exact(count.checked_mul(3)?).ok()?;
        for value in data {
            rgb.extend([value; 3]);
        }
        data = rgb;
        gray = false;
    }
    if let Some(transfer) = &obj.transfer_function {
        transfer.apply_to(&mut data);
    }
    let image = if gray {
        ImageData::Luma(LumaData {
            data,
            width: context.width,
            height: context.height,
            interpolate: obj.interpolate,
            scale_factors: context.scale_factors,
        })
    } else {
        ImageData::Rgb(RgbData {
            data,
            width: context.width,
            height: context.height,
            interpolate: obj.interpolate,
            scale_factors: context.scale_factors,
        })
    };
    Some(DecodedImage {
        image,
        alpha: Some(LumaData {
            data: alpha,
            width: mask_context.width,
            height: mask_context.height,
            interpolate: mask_obj.interpolate,
            scale_factors: mask_context.scale_factors,
        }),
    })
}

#[cfg(test)]
mod tests;
