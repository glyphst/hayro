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
    BlendMode, CacheKey, Image, ImageColorSpaceProperties, ImageDrawProps, RasterImage,
    StencilImage,
};
use hayro_syntax::object::dict::keys::*;
use hayro_syntax::object::{Name, Object, Stream};
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
        resolve_cs: impl FnMut(&Name<'_>) -> Option<Object<'a>>,
        warning_sink: &WarningSinkFn,
        cache: &Cache,
        transfer_function: Option<ActiveTransferFunction>,
    ) -> Option<Self> {
        Self::new_inner(
            stream,
            resolve_cs,
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
        Self::new_inner(stream, |_| None, warning_sink, cache, ImageKind::Mask, None)
    }

    fn new_inner(
        stream: &Stream<'a>,
        mut resolve_cs: impl FnMut(&Name<'_>) -> Option<Object<'a>>,
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
            let declared_name = cs_obj.clone().and_then(Object::into_name);
            let declared = cs_obj
                .clone()
                .and_then(|object| ColorSpace::new_preserving_icc(object, cache))
                .or_else(|| {
                    declared_name
                        .as_ref()
                        .and_then(&mut resolve_cs)
                        .and_then(|object| ColorSpace::new_preserving_icc(object, cache))
                });
            let declared_kind = declared.as_ref().map(ColorSpace::kind);
            let default_name = declared_kind.and_then(default_device_resource_name);
            let (effective, default_overridden) = if let Some(default_name) = default_name {
                let default_name = Name::new_unescaped(default_name);
                if let Some(default) = resolve_cs(&default_name)
                    .and_then(|object| ColorSpace::new_preserving_icc(object, cache))
                {
                    (Some(default), true)
                } else {
                    (declared, false)
                }
            } else {
                (declared, false)
            };
            let properties = ImageColorSpaceProperties {
                has_color_space: cs_obj.is_some(),
                declared_color_space_kind: declared_kind,
                color_space_kind: effective.as_ref().map(ColorSpace::kind),
                color_space_components: effective.as_ref().map(ColorSpace::component_count),
                color_space_default_overridden: default_overridden,
            };
            (effective, properties)
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
            Image::Raster(RasterImage(self.clone()))
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

    pub(crate) fn stream(&self) -> &Stream<'a> {
        &self.stream
    }

    fn has_mask(&self) -> bool {
        let dict = self.stream.dict();

        smask_in_data_has_alpha(dict.get::<u8>(SMASK_IN_DATA))
            || dict.contains_key(SMASK)
            || dict.contains_key(MASK)
    }
}

fn default_device_resource_name(kind: crate::color::ColorSpaceKind) -> Option<&'static [u8]> {
    match kind {
        crate::color::ColorSpaceKind::DeviceGray => Some(DEFAULT_GRAY),
        crate::color::ColorSpaceKind::DeviceRgb => Some(DEFAULT_RGB),
        crate::color::ColorSpaceKind::DeviceCmyk => Some(DEFAULT_CMYK),
        _ => None,
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

fn smask_in_data_has_alpha(value: Option<u8>) -> bool {
    matches!(value, Some(1 | 2))
}

impl CacheKey for ImageXObject<'_> {
    fn cache_key(&self) -> u128 {
        self.stream.cache_key()
    }
}

#[cfg(test)]
mod tests {
    use super::smask_in_data_has_alpha;

    #[test]
    fn explicit_zero_smask_in_data_does_not_discard_graphics_state_mask() {
        assert!(!smask_in_data_has_alpha(None));
        assert!(!smask_in_data_has_alpha(Some(0)));
        assert!(smask_in_data_has_alpha(Some(1)));
        assert!(smask_in_data_has_alpha(Some(2)));
        assert!(!smask_in_data_has_alpha(Some(3)));
    }
}
