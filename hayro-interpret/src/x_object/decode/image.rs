use super::mask::decode_mask;
use super::{DecodeContext, decode_context, decode_u8_samples, fix_image_length, unpack_samples};
use crate::color::{ColorComponents, ColorSpaceKind, ToLuma, ToRgb};
use crate::interpret::state::ActiveTransferFunction;
use crate::x_object::image::ImageXObject;
use crate::{CmykData, EmbeddedImageAlphaMode, ImageData, LumaData, RgbData};
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::*;
use smallvec::SmallVec;

pub(crate) struct DecodedImage {
    pub(crate) image: ImageData,
    pub(crate) alpha: Option<LumaData>,
}

pub(crate) struct DecodedCmykImage {
    pub(crate) image: CmykData,
    pub(crate) alpha: Option<LumaData>,
}

pub(crate) fn decode_image(
    obj: &ImageXObject<'_>,
    target_dimension: Option<(u32, u32)>,
) -> Option<DecodedImage> {
    ImageDecoder::new(obj, target_dimension)?.decode()
}

pub(crate) fn decode_device_cmyk_image(
    obj: &ImageXObject<'_>,
    target_dimension: Option<(u32, u32)>,
) -> Option<DecodedCmykImage> {
    ImageDecoder::new(obj, target_dimension)?.decode_device_cmyk()
}

struct ImageDecoder<'a, 'b> {
    obj: &'a ImageXObject<'b>,
    ctx: DecodeContext<'b>,
    target_dimension: Option<(u32, u32)>,
    color_key_mask: Option<SmallVec<[u16; 4]>>,
    decoded_color_key_mask: Option<LumaData>,
}

impl<'a, 'b> ImageDecoder<'a, 'b> {
    fn new(obj: &'a ImageXObject<'b>, target_dimension: Option<(u32, u32)>) -> Option<Self> {
        let ctx = decode_context(obj, target_dimension)?;
        let color_key_mask = obj.stream.dict().get::<SmallVec<[u16; 4]>>(MASK);

        Some(Self {
            obj,
            ctx,
            target_dimension,
            color_key_mask,
            decoded_color_key_mask: None,
        })
    }

    fn decode(mut self) -> Option<DecodedImage> {
        if self.has_soft_mask_matte() {
            return super::matte::decode(self.obj, &self.ctx, self.target_dimension);
        }
        let image = self.decode_image()?;
        let alpha = self.decode_alpha_without_matte();

        Some(DecodedImage { image, alpha })
    }

    fn decode_device_cmyk(mut self) -> Option<DecodedCmykImage> {
        if self.ctx.color_space.kind() != ColorSpaceKind::DeviceCmyk
            || self.obj.transfer_function.is_some()
            || self.has_soft_mask_matte()
        {
            return None;
        }
        let data = self.decode_components()?;
        let alpha = self.decode_alpha_without_matte();
        Some(DecodedCmykImage {
            image: CmykData {
                data,
                width: self.ctx.width,
                height: self.ctx.height,
                interpolate: self.obj.interpolate,
                scale_factors: self.ctx.scale_factors,
            },
            alpha,
        })
    }

    fn decode_image(&mut self) -> Option<ImageData> {
        let dict = self.obj.stream.dict();
        if self.ctx.color_space.kind() == ColorSpaceKind::Lab
            && !dict.contains_key(MASK)
            && !dict.contains_key(SMASK)
            && self.obj.embedded_alpha_mode().is_none()
        {
            super::float::validate_decode(self.obj, 3).ok()?;
            let data = super::float::decode_context_rgb(
                &self.ctx,
                crate::x_object::image::uses_jpx_decode(dict),
                u64::MAX,
                || true,
                |value| [(value * 255.0).round() as u8],
            )
            .ok()?;
            let mut rgb = RgbData {
                data,
                width: self.ctx.width,
                height: self.ctx.height,
                interpolate: self.obj.interpolate,
                scale_factors: self.ctx.scale_factors,
            };
            self.apply_transfer_function(&mut rgb);
            return Some(ImageData::Rgb(rgb));
        }
        let mut components = self.decode_components()?;

        if self
            .obj
            .transfer_function
            .as_ref()
            .is_none_or(|t| matches!(t, ActiveTransferFunction::Single(_)))
            && self.ctx.color_space.to_luma(&mut components).is_some()
        {
            if let Some(transfer_function) = &self.obj.transfer_function {
                transfer_function.apply_to(&mut components);
            }

            return Some(ImageData::Luma(LumaData {
                data: components,
                width: self.ctx.width,
                height: self.ctx.height,
                interpolate: self.obj.interpolate,
                scale_factors: self.ctx.scale_factors,
            }));
        }

        let mut rgb_data = self.convert_to_rgb(components)?;

        self.apply_transfer_function(&mut rgb_data);

        Some(ImageData::Rgb(rgb_data))
    }

    fn decode_components(&mut self) -> Option<Vec<u8>> {
        let num_components = self.ctx.color_space.num_components() as usize;

        // To prevent a panic when calling the `chunks` method.
        if num_components == 0 {
            return None;
        }

        let component_ranges = self.ctx.color_space.component_ranges();
        let default_decode = self
            .ctx
            .color_space
            .default_decode_arr(self.ctx.bits_per_component as f32);
        let inverted_default_decode = self
            .ctx
            .color_space
            .inverted_default_decode_arr(self.ctx.bits_per_component as f32);
        let inverted_component_ranges = component_ranges
            .iter()
            .map(|(min, max)| (*max, *min))
            .collect::<SmallVec<[(f32, f32); 4]>>();
        let is_indexed = self.ctx.color_space.is_indexed();

        let direct_invert = if self.ctx.bits_per_component == 8
            && (self.ctx.decode_arr == component_ranges
                || is_indexed && self.ctx.decode_arr == default_decode)
        {
            Some(false)
        } else if self.ctx.bits_per_component == 8
            && (self.ctx.decode_arr == inverted_component_ranges
                || is_indexed && self.ctx.decode_arr == inverted_default_decode)
        {
            Some(true)
        } else {
            None
        };

        let mut components = if let Some(invert) = direct_invert {
            // This is actually the most common case, where the PDF is embedded
            // in such a way where we don't need to decode. In this case,
            // we can use the raw decoded component values directly.
            fix_image_length(
                self.ctx.decoded.data.to_mut(),
                self.ctx.width,
                &mut self.ctx.height,
                0,
                num_components,
            )?;

            if self.color_key_mask.is_some() {
                self.decoded_color_key_mask = self.decode_color_key_mask();
            }

            if invert {
                for value in self.ctx.decoded.data.to_mut() {
                    *value = 255 - *value;
                }
            }

            core::mem::take(&mut self.ctx.decoded.data).into_owned()
        } else {
            let mut components = decode_u8_samples(
                &self.ctx.decoded.data,
                self.ctx.width,
                self.ctx.height,
                &self.ctx.color_space,
                self.ctx.bits_per_component,
                &self.ctx.decode_arr,
            )?;

            fix_image_length(
                &mut components,
                self.ctx.width,
                &mut self.ctx.height,
                0,
                num_components,
            )?;

            components
        };

        if self.obj.embedded_alpha_mode() == Some(EmbeddedImageAlphaMode::Premultiplied) {
            self.undo_embedded_preblend(&mut components, num_components)?;
        }

        Some(components)
    }

    fn undo_embedded_preblend(&self, components: &mut [u8], num_components: usize) -> Option<()> {
        let alpha = self.ctx.decoded.image_data.as_ref()?.alpha.as_deref()?;
        let dict = self.obj.stream.dict();
        let matte = if dict.contains_key(MATTE) {
            let values = dict.get::<ColorComponents>(MATTE)?;
            if values.len() != num_components || values.iter().any(|value| !value.is_finite()) {
                return None;
            }
            self.ctx.color_space.encode_values(&values)
        } else {
            let zero_matte = SmallVec::<[f32; 4]>::from_elem(0.0, num_components);
            self.ctx.color_space.encode_values(&zero_matte)
        };

        unpremultiply_components(components, alpha, &matte)
    }

    fn convert_to_rgb(&self, mut decoded: Vec<u8>) -> Option<RgbData> {
        if self
            .ctx
            .color_space
            .convert_in_place(&mut decoded)
            .is_none()
        {
            let mut output = vec![0; self.ctx.width as usize * self.ctx.height as usize * 3];
            self.ctx.color_space.convert(&decoded, &mut output)?;
            decoded = output;
        }

        Some(RgbData {
            data: decoded,
            width: self.ctx.width,
            height: self.ctx.height,
            interpolate: self.obj.interpolate,
            scale_factors: self.ctx.scale_factors,
        })
    }

    fn apply_transfer_function(&self, rgb_data: &mut RgbData) {
        if let Some(transfer_function) = &self.obj.transfer_function {
            transfer_function.apply_to(&mut rgb_data.data);
        }
    }

    fn decode_alpha_without_matte(&mut self) -> Option<LumaData> {
        // If the alpha channel is invalid, return no alpha so the main image can
        // still be returned (see PDFJS-19611).
        let dict = self.obj.stream.dict();

        if self.obj.embedded_alpha_mode().is_some() {
            let mut data = self
                .ctx
                .decoded
                .image_data
                .as_mut()
                .and_then(|image| image.alpha.take())?;
            fix_image_length(&mut data, self.ctx.width, &mut self.ctx.height, 0, 1)?;

            Some(LumaData {
                data,
                width: self.ctx.width,
                height: self.ctx.height,
                interpolate: self.obj.interpolate,
                scale_factors: self.ctx.scale_factors,
            })
            // Note: `SMASK` field takes precedence over `MASK`, so order matters here.
        } else if let Some(s_mask) = dict
            .get::<Stream<'_>>(SMASK)
            .or_else(|| dict.get::<Stream<'_>>(MASK))
        {
            let obj = ImageXObject::new_mask(&s_mask, &self.obj.warning_sink, &self.obj.cache)?;

            decode_mask(&obj, self.target_dimension).map(|decoded| decoded.luma)
        } else if self.color_key_mask.is_some() {
            self.decoded_color_key_mask
                .take()
                .or_else(|| self.decode_color_key_mask())
        } else {
            None
        }
    }

    fn has_soft_mask_matte(&self) -> bool {
        self.obj.embedded_alpha_mode().is_none()
            && self
                .obj
                .stream
                .dict()
                .get::<Stream<'_>>(SMASK)
                .is_some_and(|mask| mask.dict().contains_key(MATTE))
    }

    fn decode_color_key_mask(&self) -> Option<LumaData> {
        let color_key_mask = self.color_key_mask.as_deref()?;
        let num_components = self.ctx.color_space.num_components() as usize;
        let components = unpack_samples(
            &self.ctx.decoded.data,
            self.ctx.width,
            self.ctx.height,
            num_components,
            self.ctx.bits_per_component,
        )?;
        let mut data = Vec::with_capacity(self.ctx.width as usize * self.ctx.height as usize);

        for pixel in components.chunks_exact(num_components) {
            let mut value = 0;

            for (component, min_max) in pixel.iter().zip(color_key_mask.chunks_exact(2)) {
                if *component > min_max[1] || *component < min_max[0] {
                    value = 255;
                }
            }

            data.push(value);
        }

        let mut height = self.ctx.height;
        fix_image_length(&mut data, self.ctx.width, &mut height, 0, 1)?;

        Some(LumaData {
            data,
            width: self.ctx.width,
            height,
            interpolate: self.obj.interpolate,
            scale_factors: self.ctx.scale_factors,
        })
    }
}

fn unpremultiply_components(components: &mut [u8], alpha: &[u8], matte: &[u8]) -> Option<()> {
    if matte.is_empty() || !components.len().is_multiple_of(matte.len()) {
        return None;
    }
    if components.len() / matte.len() != alpha.len() {
        return None;
    }

    for (pixel, &a) in components.chunks_exact_mut(matte.len()).zip(alpha) {
        if a == 0 {
            // The original color is mathematically unrecoverable and has no
            // contribution at zero opacity. Preserve the encoded matte sample.
            continue;
        }
        let inverse_alpha = 255.0 / f32::from(a);
        for (component, &matte_component) in pixel.iter_mut().zip(matte) {
            let matte_component = f32::from(matte_component);
            let value = matte_component + (f32::from(*component) - matte_component) * inverse_alpha;
            *component = value.round_ties_even().clamp(0.0, 255.0) as u8;
        }
    }

    Some(())
}

#[cfg(test)]
mod tests {
    use super::unpremultiply_components;

    #[test]
    fn embedded_preblend_is_reversed_per_source_component() {
        let mut rgb = vec![64, 0, 0, 255, 128, 0];
        assert_eq!(
            unpremultiply_components(&mut rgb, &[64, 128], &[0, 0, 0]),
            Some(())
        );
        assert_eq!(rgb, [255, 0, 0, 255, 255, 0]);

        let mut cmyk = vec![128, 64, 32, 191];
        assert_eq!(
            unpremultiply_components(&mut cmyk, &[128], &[0, 0, 0, 255]),
            Some(())
        );
        assert_eq!(cmyk, [255, 128, 64, 128]);
    }

    #[test]
    fn embedded_preblend_preserves_zero_alpha_and_rejects_mismatched_planes() {
        let mut color = vec![10, 20, 30];
        assert_eq!(
            unpremultiply_components(&mut color, &[0], &[10, 20, 30]),
            Some(())
        );
        assert_eq!(color, [10, 20, 30]);

        assert_eq!(unpremultiply_components(&mut color, &[], &[0, 0, 0]), None);
        assert_eq!(unpremultiply_components(&mut color, &[255], &[]), None);
    }
}
