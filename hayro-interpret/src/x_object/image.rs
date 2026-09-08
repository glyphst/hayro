use super::decode::{
    DecodedCmykImage, DecodedImage, DecodedMask, decode_device_cmyk_image, decode_image,
    decode_mask,
};
use super::xobject_oc;
use crate::WarningSinkFn;
use crate::cache::Cache;
use crate::color::ColorSpace;
use crate::context::Context;
use crate::device::Device;
use crate::interpret::state::ActiveTransferFunction;
use crate::{
    BlendMode, CacheKey, EmbeddedImageAlphaMode, Image, ImageColorSpaceProperties, ImageDrawProps,
    RasterImage, StencilImage,
};
use hayro_syntax::object::dict::keys::*;
use hayro_syntax::object::{Array, Dict, Name, Object, Stream};
use hayro_syntax::page::Resources;
use kurbo::Affine;

#[derive(Clone)]
pub(crate) struct ImageXObject<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) color_space: Option<ColorSpace>,
    pub(crate) color_space_properties: ImageColorSpaceProperties,
    pub(crate) cache: Cache,
    pub(crate) interpolate: bool,
    pub(crate) kind: ImageKind,
    pub(crate) stream: Stream<'a>,
    pub(crate) transfer_function: Option<ActiveTransferFunction>,
    pub(crate) warning_sink: WarningSinkFn,
    pub(crate) rendering_intent: crate::color::RenderingIntent,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageKind {
    Image,
    Mask,
    StencilMask,
}

impl ImageKind {
    fn is_mask(self) -> bool {
        self != Self::Image
    }
}

impl<'a> ImageXObject<'a> {
    pub(crate) fn new(
        stream: &Stream<'a>,
        resources: &Resources<'a>,
        warning_sink: &WarningSinkFn,
        cache: &Cache,
        transfer_function: Option<ActiveTransferFunction>,
    ) -> Option<Self> {
        Self::new_inner(
            stream,
            Some(resources),
            warning_sink,
            cache,
            ImageKind::Image,
            transfer_function,
        )
    }

    pub(crate) fn new_mask(
        stream: &Stream<'a>,
        warning_sink: &WarningSinkFn,
        cache: &Cache,
    ) -> Option<Self> {
        Self::new_inner(stream, None, warning_sink, cache, ImageKind::Mask, None)
    }

    fn new_inner(
        stream: &Stream<'a>,
        resources: Option<&Resources<'a>>,
        warning_sink: &WarningSinkFn,
        cache: &Cache,
        mut kind: ImageKind,
        transfer_function: Option<ActiveTransferFunction>,
    ) -> Option<Self> {
        let dict = stream.dict();

        let is_stencil_mask = dict
            .get::<bool>(IM)
            .or_else(|| dict.get::<bool>(IMAGE_MASK))
            .unwrap_or(false);

        if is_stencil_mask {
            kind = ImageKind::StencilMask;
        }

        let (image_cs, color_space_properties) = if kind.is_mask() {
            // Masks are always single-channel.
            (
                Some(ColorSpace::device_gray()),
                mask_color_space_properties(),
            )
        } else {
            let cs_obj = dict
                .get::<Object<'_>>(CS)
                .or_else(|| dict.get::<Object<'_>>(COLORSPACE));
            if cs_obj.is_none() && (dict.contains_key(CS) || dict.contains_key(COLORSPACE)) {
                (warning_sink)(crate::InterpreterWarning::ImageColorSpace(
                    crate::color::ImageColorSpaceError::Invalid,
                ));
                return None;
            }
            if let Some(object) = cs_obj {
                let (space, properties) = ColorSpace::new_image(object, cache, resources?)
                    .map_err(|error| {
                        (warning_sink)(crate::InterpreterWarning::ImageColorSpace(error));
                    })
                    .ok()?;
                (Some(space), properties)
            } else {
                (
                    None,
                    ImageColorSpaceProperties {
                        has_color_space: false,
                        declared_color_space_kind: None,
                        color_space_kind: None,
                        color_space_components: None,
                        color_space_default_overridden: false,
                    },
                )
            }
        };

        let interpolate = dict
            .get::<bool>(I)
            .or_else(|| dict.get::<bool>(INTERPOLATE))
            .unwrap_or(false);

        let width = dict.get::<u32>(W).or_else(|| dict.get::<u32>(WIDTH))?;
        let height = dict.get::<u32>(H).or_else(|| dict.get::<u32>(HEIGHT))?;

        if width == 0 || height == 0 {
            return None;
        }

        Some(Self {
            width,
            cache: cache.clone(),
            height,
            color_space: image_cs,
            color_space_properties,
            warning_sink: warning_sink.clone(),
            rendering_intent: crate::color::RenderingIntent::default(),
            transfer_function,
            interpolate,
            stream: stream.clone(),
            kind,
        })
    }

    pub(crate) fn draw<'b>(&self, context: &mut Context<'b>, device: &mut impl Device<'b>) {
        if !context.ocg_state.is_visible() {
            return;
        }

        let mut bound = self.clone();
        let intent = if self.kind == ImageKind::Image && self.stream.dict().contains_key(b"Intent")
        {
            match crate::color::RenderingIntent::from_object(
                self.stream.dict().get::<Object<'_>>(b"Intent"),
            ) {
                Ok((intent, unknown)) => {
                    if unknown {
                        (self.warning_sink)(crate::InterpreterWarning::RenderingIntentUnknown);
                    }
                    intent
                }
                Err(error) => {
                    (self.warning_sink)(crate::InterpreterWarning::ColorConversion(error));
                    return;
                }
            }
        } else {
            context.get().graphics_state.rendering_intent
        };
        bound.rendering_intent = intent;
        if self.kind == ImageKind::Image
            && let Some(space) = &self.color_space
        {
            match space.with_rendering_intent(intent) {
                Ok(space) => bound.color_space = Some(space),
                Err(error) => {
                    (self.warning_sink)(crate::InterpreterWarning::ColorConversion(error));
                    return;
                }
            }
        }
        let has_oc = xobject_oc(self.stream.dict(), context, device);
        if !context.ocg_state.is_visible() {
            if has_oc && context.ocg_state.end_marked_content() {
                device.end_optional_content();
            }
            return;
        }

        let width = self.width as f64;
        let height = self.height as f64;

        context.save_state();
        context.pre_concat_affine(Affine::new([
            1.0 / width,
            0.0,
            0.0,
            -1.0 / height,
            0.0,
            1.0,
        ]));
        let transform = context.get().ctm;

        let has_alpha = self.has_mask();

        let mut soft_mask = std::mem::take(&mut context.get_mut().graphics_state.soft_mask);
        let blend_mode = std::mem::take(&mut context.get_mut().graphics_state.blend_mode);

        // If image has a soft mask, the soft mask from the graphics state
        // should be discarded.
        if has_alpha {
            soft_mask = None;
        }

        device.push_transparency_group_with_alpha_source(
            context.get().graphics_state.non_stroke_alpha,
            std::mem::take(&mut soft_mask),
            blend_mode,
            context.get().graphics_state.alpha_is_shape,
        );

        let image = if self.kind.is_mask() {
            Image::Stencil(StencilImage {
                paint: context.get_paint(false),
                image_xobject: self.clone(),
            })
        } else {
            Image::Raster(RasterImage(bound))
        };

        device.draw_image(
            image,
            ImageDrawProps {
                transform,
                soft_mask: None,
                blend_mode: BlendMode::default(),
                alpha_is_shape: context.get().graphics_state.alpha_is_shape,
            },
        );
        device.pop_transparency_group();

        context.restore_state(device);

        if has_oc && context.ocg_state.end_marked_content() {
            device.end_optional_content();
        }
    }

    pub(crate) fn decoded_mask(&self, target_dimension: Option<(u32, u32)>) -> Option<DecodedMask> {
        if !self.kind.is_mask() {
            return None;
        }

        decode_mask(self, target_dimension)
    }

    pub(crate) fn decoded_image(
        &self,
        target_dimension: Option<(u32, u32)>,
    ) -> Option<DecodedImage> {
        if self.kind.is_mask() {
            return None;
        }

        decode_image(self, target_dimension)
    }

    pub(crate) fn decoded_device_cmyk_image(
        &self,
        target_dimension: Option<(u32, u32)>,
    ) -> Option<DecodedCmykImage> {
        if self.kind != ImageKind::Image {
            return None;
        }

        decode_device_cmyk_image(self, target_dimension)
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn embedded_alpha_mode(&self) -> Option<EmbeddedImageAlphaMode> {
        embedded_alpha_mode(self.stream.dict())
    }

    pub(crate) fn stream(&self) -> &Stream<'a> {
        &self.stream
    }

    fn has_mask(&self) -> bool {
        let dict = self.stream.dict();

        embedded_alpha_mode(dict).is_some() || dict.contains_key(SMASK) || dict.contains_key(MASK)
    }
}

fn mask_color_space_properties() -> ImageColorSpaceProperties {
    ImageColorSpaceProperties {
        has_color_space: false,
        declared_color_space_kind: None,
        color_space_kind: Some(crate::color::ColorSpaceKind::DeviceGray),
        color_space_components: Some(1),
        color_space_default_overridden: false,
    }
}

fn embedded_alpha_mode(dict: &Dict<'_>) -> Option<EmbeddedImageAlphaMode> {
    if !uses_jpx_decode(dict) {
        return None;
    }

    match dict.get::<u8>(SMASK_IN_DATA) {
        Some(1) => Some(EmbeddedImageAlphaMode::Unassociated),
        Some(2) => Some(EmbeddedImageAlphaMode::Premultiplied),
        _ => None,
    }
}

fn uses_jpx_decode(dict: &Dict<'_>) -> bool {
    let is_jpx = |name: &Name<'_>| name.as_ref() == JPX_DECODE;

    dict.get::<Name<'_>>(F)
        .or_else(|| dict.get::<Name<'_>>(FILTER))
        .as_ref()
        .is_some_and(is_jpx)
        || dict
            .get::<Array<'_>>(F)
            .or_else(|| dict.get::<Array<'_>>(FILTER))
            .is_some_and(|filters| filters.iter::<Name<'_>>().any(|name| is_jpx(&name)))
}

impl CacheKey for ImageXObject<'_> {
    fn cache_key(&self) -> u128 {
        crate::util::hash128(&(
            self.stream.cache_key(),
            self.rendering_intent,
            self.transfer_function.as_ref().map(CacheKey::cache_key),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::embedded_alpha_mode;
    use crate::EmbeddedImageAlphaMode;
    use hayro_syntax::object::{Dict, FromBytes};

    #[test]
    fn embedded_alpha_requires_jpx_and_a_supported_mode() {
        let mode = |entries: &[u8]| {
            let dict = Dict::from_bytes(entries).expect("image dictionary");
            embedded_alpha_mode(&dict)
        };

        assert_eq!(
            mode(b"<< /Filter /JPXDecode /SMaskInData 1 >>"),
            Some(EmbeddedImageAlphaMode::Unassociated)
        );
        assert_eq!(
            mode(b"<< /Filter [/ASCII85Decode /JPXDecode] /SMaskInData 2 >>"),
            Some(EmbeddedImageAlphaMode::Premultiplied)
        );
        assert_eq!(
            mode(b"<< /F /JPXDecode /SMaskInData 2 >>"),
            Some(EmbeddedImageAlphaMode::Premultiplied)
        );
        assert_eq!(mode(b"<< /Filter /JPXDecode /SMaskInData 0 >>"), None);
        assert_eq!(mode(b"<< /Filter /JPXDecode /SMaskInData 3 >>"), None);
        assert_eq!(mode(b"<< /Filter /FlateDecode /SMaskInData 2 >>"), None);
        assert_eq!(mode(b"<< /SMaskInData 2 >>"), None);
    }
}
