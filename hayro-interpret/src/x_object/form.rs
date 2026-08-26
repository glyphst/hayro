use super::xobject_oc;
use crate::cache::CacheKey;
use crate::color::{ColorSpace, ColorSpaceKind};
use crate::context::Context;
use crate::device::Device;
use crate::interpret::state::State;
use crate::util::hash128;
use crate::{ClipPath, FillRule, interpret};
use crate::{InterpreterCache, InterpreterSettings};
use hayro_syntax::content::TypedIter;
use hayro_syntax::object::dict::keys::*;
use hayro_syntax::object::{Dict, Object, Stream};
use hayro_syntax::page::Resources;
use hayro_syntax::xref::XRef;
use kurbo::{Affine, Rect, Shape};
use std::borrow::Cow;
use std::fmt;

#[derive(Clone)]
pub(crate) struct FormXObject<'a> {
    pub(crate) decoded: Cow<'a, [u8]>,
    pub(crate) matrix: Affine,
    pub(crate) bbox: [f32; 4],
    pub(crate) group_properties: Option<FormGroupProperties>,
    pub(crate) dict: Dict<'a>,
    resources: Dict<'a>,
}

/// Typed properties declared by a Form `XObject` `/Group` dictionary.
///
/// Consumers that require exact transparency semantics must additionally
/// require [`Self::is_transparency`] before relying on the remaining values.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FormGroupProperties {
    is_transparency: bool,
    isolated: bool,
    knockout: bool,
    has_color_space: bool,
    declared_color_space_kind: Option<ColorSpaceKind>,
    color_space_kind: Option<ColorSpaceKind>,
    color_space_default_overridden: bool,
}

impl FormGroupProperties {
    /// Whether `/S /Transparency` identifies a transparency group.
    pub fn is_transparency(self) -> bool {
        self.is_transparency
    }

    /// Whether the group declares `/I true` isolation.
    pub fn isolated(self) -> bool {
        self.isolated
    }

    /// Whether the group declares `/K true` knockout behavior.
    pub fn knockout(self) -> bool {
        self.knockout
    }

    /// Whether the group explicitly declares a blending color space.
    pub fn has_color_space(self) -> bool {
        self.has_color_space
    }

    /// Direct device-space family named by `/CS` before a default resource is
    /// applied.
    ///
    /// This remains available when [`Self::color_space_kind`] is `None`
    /// because `DefaultGray`, `DefaultRGB`, or `DefaultCMYK` remaps the name.
    pub fn declared_color_space_kind(self) -> Option<ColorSpaceKind> {
        self.declared_color_space_kind
    }

    /// Parsed family of the explicit blending color space.
    ///
    /// `None` means either that the group omits `/CS`, that the color space
    /// could not be parsed, or that a `DefaultGray`, `DefaultRGB`, or
    /// `DefaultCMYK` resource remaps a direct device-space name.
    pub fn color_space_kind(self) -> Option<ColorSpaceKind> {
        self.color_space_kind
    }

    /// Whether a default device color-space resource remaps the declared `/CS`.
    pub fn color_space_is_default_overridden(self) -> bool {
        self.color_space_default_overridden
    }
}

/// One invocation of a PDF Form `XObject` with its inherited graphics state.
///
/// This value is valid only during the synchronous device callback. It can be
/// interpreted with the original page transform through [`Self::interpret`],
/// or normalized into form-local coordinates through [`Self::interpret_local`]
/// for an owned retained scene.
pub struct FormInvocation<'a> {
    form: FormXObject<'a>,
    resources: Resources<'a>,
    state: State<'a>,
    instance_transform: Affine,
    settings: InterpreterSettings,
    cache: InterpreterCache<'a>,
    xref: &'a XRef,
    nesting_depth: u32,
    retained_key: u128,
    group_properties: Option<FormGroupProperties>,
    group_color_space: Option<ColorSpace>,
}

impl fmt::Debug for FormInvocation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FormInvocation")
            .field("local_bounds", &self.local_bounds())
            .field("instance_transform", &self.instance_transform)
            .field("transparency_group", &self.is_transparency_group())
            .field("retained_key", &self.retained_key)
            .finish_non_exhaustive()
    }
}

impl CacheKey for FormInvocation<'_> {
    fn cache_key(&self) -> u128 {
        self.retained_key
    }
}

impl<'a> FormInvocation<'a> {
    fn new(
        form: &FormXObject<'a>,
        parent_resources: &Resources<'a>,
        context: &Context<'a>,
    ) -> Self {
        let resources = Resources::from_parent(form.resources.clone(), parent_resources.clone());
        let group_color_space = form
            .dict
            .get::<Dict<'_>>(GROUP)
            .and_then(|group| group.get::<Object<'_>>(CS))
            .and_then(|object| {
                ColorSpace::new(object.clone(), &context.interpreter_cache.object_cache).or_else(
                    || {
                        object
                            .into_name()
                            .and_then(|name| resources.get_color_space(&name))
                            .and_then(|resolved| {
                                ColorSpace::new(resolved, &context.interpreter_cache.object_cache)
                            })
                    },
                )
            });
        let group_properties = form.group_properties.map(|mut properties| {
            properties.color_space_kind = group_color_space.as_ref().map(ColorSpace::kind);
            let default_name = match properties.declared_color_space_kind {
                Some(ColorSpaceKind::DeviceGray) => Some(DEFAULT_GRAY),
                Some(ColorSpaceKind::DeviceRgb) => Some(DEFAULT_RGB),
                Some(ColorSpaceKind::DeviceCmyk) => Some(DEFAULT_CMYK),
                _ => None,
            };
            if default_name.is_some_and(|name| resources_contain_color_space(&resources, name)) {
                properties.color_space_kind = None;
                properties.color_space_default_overridden = true;
            }
            properties
        });
        let state = context.get().clone();
        let instance_transform = state.ctm;
        let mut normalized_state = state.clone();
        normalized_state.ctm = Affine::IDENTITY;
        normalized_state.clips.clear();
        let form_key = hash128(&(
            hash128(form.decoded.as_ref()),
            form.dict.cache_key(),
            resources_cache_key(&resources),
        ));
        // The complete inherited state determines whether two invocations can
        // share one locally interpreted visual subscene. Debug formatting is
        // deterministic for these value types and deliberately excludes the
        // page-space CTM and external clip stack normalized above.
        let retained_key = hash128(&(form_key, format!("{normalized_state:?}")));
        Self {
            form: form.clone(),
            resources,
            state,
            instance_transform,
            settings: context.settings.clone(),
            cache: context.interpreter_cache.clone(),
            xref: context.xref,
            nesting_depth: context.nesting_depth(),
            retained_key,
            group_properties,
            group_color_space,
        }
    }

    /// Transform that places the normalized form subscene in its page.
    pub fn instance_transform(&self) -> Affine {
        self.instance_transform
    }

    /// Bounds of the normalized form after applying its `/Matrix` and `/BBox`.
    pub fn local_bounds(&self) -> Rect {
        transformed_bbox(self.form.matrix, self.form.bbox)
    }

    /// Whether the form dictionary declares a transparency group.
    pub fn is_transparency_group(&self) -> bool {
        self.group_properties.is_some()
    }

    /// Typed properties from the form's `/Group` dictionary, when present.
    pub fn group_properties(&self) -> Option<FormGroupProperties> {
        self.group_properties
    }

    /// Parsed blending color space declared by the Form group, when valid.
    ///
    /// The handle is owned and remains valid for the duration of this
    /// invocation callback. Retained renderers can sample or classify it
    /// before deep-copying their own representation.
    pub fn group_color_space(&self) -> Option<&ColorSpace> {
        self.group_color_space.as_ref()
    }

    /// Whether the invocation inherits a soft mask whose root transform cannot
    /// yet be normalized independently of the page instance.
    pub fn has_inherited_soft_mask(&self) -> bool {
        self.state.graphics_state.soft_mask.is_some()
    }

    /// Interpret with the original page-space transform.
    pub fn interpret(&self, device: &mut impl Device<'a>) {
        self.interpret_with_root(device, self.instance_transform);
    }

    /// Interpret into normalized form-local coordinates.
    ///
    /// The resulting device callbacks include the form `/Matrix` but exclude
    /// the invocation's outer page transform. Apply [`Self::instance_transform`]
    /// exactly once when instancing the retained result.
    pub fn interpret_local(&self, device: &mut impl Device<'a>) {
        self.interpret_with_root(device, Affine::IDENTITY);
    }

    fn interpret_with_root(&self, device: &mut impl Device<'a>, root: Affine) {
        let mut state = self.state.clone();
        state.ctm = root * self.form.matrix;
        // The caller's clip stack remains active around the form invocation;
        // only clips established inside the form belong to its local replay.
        state.clips.clear();
        let bounds = transformed_bbox(state.ctm, self.form.bbox);
        let mut context = Context::new_with(
            state.ctm,
            bounds,
            &self.cache,
            self.xref,
            self.settings.clone(),
            state,
            self.nesting_depth,
        );

        if self.group_properties.is_some() {
            device.push_transparency_group_with_alpha_source(
                context.get().graphics_state.non_stroke_alpha,
                std::mem::take(&mut context.get_mut().graphics_state.soft_mask),
                std::mem::take(&mut context.get_mut().graphics_state.blend_mode),
                context.get().graphics_state.alpha_is_shape,
            );
            context.get_mut().graphics_state.non_stroke_alpha = 1.0;
            context.get_mut().graphics_state.stroke_alpha = 1.0;
        }

        device.push_clip_path(&ClipPath {
            path: context.get().ctm
                * Rect::new(
                    self.form.bbox[0] as f64,
                    self.form.bbox[1] as f64,
                    self.form.bbox[2] as f64,
                    self.form.bbox[3] as f64,
                )
                .to_path(0.1),
            fill: FillRule::NonZero,
        });
        interpret(
            TypedIter::new(self.form.decoded.as_ref()),
            &self.resources,
            &mut context,
            device,
        );
        device.pop_clip();

        if self.group_properties.is_some() {
            device.pop_transparency_group();
        }
    }
}

impl<'a> FormXObject<'a> {
    pub(crate) fn new(stream: &Stream<'a>) -> Option<Self> {
        let dict = stream.dict();

        let decoded = stream.decoded().ok()?;
        let resources = dict.get::<Dict<'_>>(RESOURCES).unwrap_or_default();

        let matrix = Affine::new(
            dict.get::<[f64; 6]>(MATRIX)
                .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        );
        let bbox = dict.get::<[f32; 4]>(BBOX)?;
        let group_properties = dict.get::<Dict<'_>>(GROUP).map(|group| {
            let is_transparency = group
                .get::<hayro_syntax::object::Name<'_>>(S)
                .is_some_and(|name| name.as_ref() == TRANSPARENCY);
            let color_space_kind =
                group
                    .get::<hayro_syntax::object::Name<'_>>(CS)
                    .and_then(|name| match name.as_ref() {
                        DEVICE_GRAY | G => Some(ColorSpaceKind::DeviceGray),
                        DEVICE_RGB | RGB => Some(ColorSpaceKind::DeviceRgb),
                        DEVICE_CMYK | CMYK => Some(ColorSpaceKind::DeviceCmyk),
                        _ => None,
                    });
            FormGroupProperties {
                is_transparency,
                isolated: group.get::<bool>(I).unwrap_or(false),
                knockout: group.get::<bool>(K).unwrap_or(false),
                has_color_space: group.contains_key(CS),
                declared_color_space_kind: color_space_kind,
                color_space_kind,
                color_space_default_overridden: false,
            }
        });

        Some(Self {
            decoded,
            matrix,
            group_properties,
            bbox,
            dict: dict.clone(),
            resources,
        })
    }

    pub(crate) fn draw(
        &self,
        resources: &Resources<'a>,
        context: &mut Context<'a>,
        device: &mut impl Device<'a>,
    ) {
        if !context.ocg_state.is_visible() {
            return;
        }

        if !context.begin_nested_interpretation() {
            return;
        }

        let has_oc = xobject_oc(&self.dict, context, device);
        if !context.ocg_state.is_visible() {
            if has_oc && context.ocg_state.end_marked_content() {
                device.end_optional_content();
            }
            context.end_nested_interpretation();
            return;
        }

        context.path_mut().truncate(0);
        let invocation = FormInvocation::new(self, resources, context);
        device.draw_form(&invocation);

        if has_oc && context.ocg_state.end_marked_content() {
            device.end_optional_content();
        }

        context.end_nested_interpretation();
    }
}

pub(super) fn resources_contain_color_space(resources: &Resources<'_>, name: &[u8]) -> bool {
    resources.color_spaces.contains_key(name)
        || resources
            .parent()
            .is_some_and(|parent| resources_contain_color_space(parent, name))
}

fn transformed_bbox(transform: Affine, bbox: [f32; 4]) -> Rect {
    (transform
        * Rect::new(
            bbox[0] as f64,
            bbox[1] as f64,
            bbox[2] as f64,
            bbox[3] as f64,
        )
        .to_path(0.1))
    .bounding_box()
}

fn resources_cache_key(resources: &Resources<'_>) -> u128 {
    let local = hash128(&[
        resources.ext_g_states.cache_key(),
        resources.fonts.cache_key(),
        resources.properties.cache_key(),
        resources.color_spaces.cache_key(),
        resources.x_objects.cache_key(),
        resources.patterns.cache_key(),
        resources.shadings.cache_key(),
    ]);
    hash128(&(
        local,
        resources
            .parent()
            .map(resources_cache_key)
            .unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorSpaceKind;
    use crate::font::GlyphRun;
    use crate::{
        BlendMode, DrawMode, DrawProps, Image, ImageColorSpaceProperties, ImageData,
        ImageDrawProps, InterpreterSettings, InterpreterWarning, LinkBorder, LinkBorderColor,
        LinkBorderGeometry, LinkBorderStyle, MarkedContentProperties, MarkedContentProperty,
        MarkedContentPropertyValue, MaskType, SoftMask, interpret_page,
    };
    use hayro_syntax::Pdf;
    use hayro_syntax::object::Name;
    use kurbo::BezPath;
    use std::sync::{Arc, Mutex};

    fn form_pdf() -> Vec<u8> {
        form_pdf_with_group("")
    }

    fn form_pdf_with_group(group: &str) -> Vec<u8> {
        form_pdf_with_group_and_resources(group, "")
    }

    fn form_pdf_with_group_and_resources(group: &str, resources: &str) -> Vec<u8> {
        let page_stream = b"q 2 0 0 2 10 20 cm /Fm0 Do Q q 3 0 0 3 30 40 cm /Fm0 Do Q";
        let form_stream = b"0 0 10 10 re f";
        format!(
            "%PDF-1.4\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</XObject<</Fm0 5 0 R>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             5 0 obj <</Type/XObject/Subtype/Form/FormType 1/BBox[0 0 10 10]/Matrix[1 0 0 1 3 4]{group}/Resources<<{resources}>>/Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
            form_stream.len(),
            String::from_utf8_lossy(form_stream),
        )
        .into_bytes()
    }

    fn annotation_pdf(annots: &str, objects: &str) -> Vec<u8> {
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 600 700]/Resources<<>>/Annots[{annots}]/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length 0>> stream\n\nendstream endobj\n\
             {objects}\n\
             trailer <</Root 1 0 R>>\n%%EOF"
        )
        .into_bytes()
    }

    fn content_pdf(contents: &str) -> Vec<u8> {
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<<>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            contents.len(),
            contents,
        )
        .into_bytes()
    }

    fn luminosity_soft_mask_pdf() -> Vec<u8> {
        luminosity_soft_mask_pdf_with_backdrop("/BC[0.1 0.2 0.3]")
    }

    fn automatic_stroke_adjustment_pdf() -> Vec<u8> {
        let page_stream = b"/GS1 gs 0 10 m 90 10 l S q /GS0 gs 0 20 m 90 20 l S Q 0 30 m 90 30 l S";
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</ExtGState<</GS0<</SA false>>/GS1<</SA true>>>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
        )
        .into_bytes()
    }

    fn default_rgb_image_pdf() -> Vec<u8> {
        let page_stream = b"q 10 0 0 10 0 0 cm /ImDefault Do Q q 10 0 0 10 20 0 cm /ImExplicit Do Q q 10 0 0 10 40 0 cm /ImAlias Do Q";
        let image_data = "4080C0>";
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<<\
               /ColorSpace<</DefaultRGB [/CalRGB <</WhitePoint[0.9505 1 1.089]/Gamma[2 2 2]>>]/AliasRGB/DeviceRGB>>\
               /XObject<</ImDefault 5 0 R/ImExplicit 6 0 R/ImAlias 7 0 R>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             5 0 obj <</Type/XObject/Subtype/Image/Width 1/Height 1/BitsPerComponent 8\
               /ColorSpace/DeviceRGB/Filter/ASCIIHexDecode/Length {}>> stream\n{}\nendstream endobj\n\
             6 0 obj <</Type/XObject/Subtype/Image/Width 1/Height 1/BitsPerComponent 8\
               /ColorSpace[/CalRGB <</WhitePoint[0.9505 1 1.089]/Gamma[2 2 2]>>]\
               /Filter/ASCIIHexDecode/Length {}>> stream\n{}\nendstream endobj\n\
             7 0 obj <</Type/XObject/Subtype/Image/Width 1/Height 1/BitsPerComponent 8\
               /ColorSpace/AliasRGB/Filter/ASCIIHexDecode/Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
            image_data.len(),
            image_data,
            image_data.len(),
            image_data,
            image_data.len(),
            image_data,
        )
        .into_bytes()
    }

    fn explicit_srgb_icc_image_pdf() -> Vec<u8> {
        let source =
            include_bytes!("../../../hayro-tests/pdfs/custom/xobject_with_fill_opacity.pdf");
        let source = Pdf::new(source.to_vec()).expect("parse ICC source fixture");
        let group = source.pages()[0]
            .raw()
            .get::<Dict<'_>>(GROUP)
            .expect("ICC source page group");
        let color_space = group
            .get::<hayro_syntax::object::Array<'_>>(CS)
            .expect("ICCBased source array");
        let mut color_space = color_space.flex_iter();
        let family = color_space.next::<Name<'_>>().expect("ICCBased family");
        assert_eq!(family.as_ref(), ICC_BASED);
        let profile = color_space
            .next::<Stream<'_>>()
            .expect("ICC profile stream")
            .decoded()
            .expect("decode ICC profile");
        let page_stream = b"q 10 0 0 10 0 0 cm /Im0 Do Q";
        let image_data = "4080C0>";
        let mut pdf = format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<<\
               /ColorSpace<</DefaultRGB/DeviceRGB/Icc[/ICCBased 6 0 R]>>\
               /XObject<</Im0 5 0 R>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             5 0 obj <</Type/XObject/Subtype/Image/Width 1/Height 1/BitsPerComponent 8\
               /ColorSpace/Icc/Filter/ASCIIHexDecode/Length {}>> stream\n{}\nendstream endobj\n\
             6 0 obj <</N 3/Length {}>> stream\n",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
            image_data.len(),
            image_data,
            profile.len(),
        )
        .into_bytes();
        pdf.extend_from_slice(profile.as_ref());
        pdf.extend_from_slice(b"\nendstream endobj\ntrailer <</Root 1 0 R>>\n%%EOF");
        pdf
    }

    fn alpha_and_text_object_semantics_pdf() -> Vec<u8> {
        let page_stream = b"/Shape gs 0 0 10 10 re f /Opacity gs 20 0 10 10 re f BT ET /NoKo gs BT ET 40 0 10 10 re B";
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</ExtGState<</Shape<</AIS true/ca 0.4>>/Opacity<</AIS false/ca 0.7>>/NoKo<</TK false>>>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
        )
        .into_bytes()
    }

    fn luminosity_soft_mask_pdf_with_backdrop(backdrop: &str) -> Vec<u8> {
        luminosity_soft_mask_pdf_with_options(backdrop, "/DeviceRGB", "")
    }

    fn luminosity_soft_mask_pdf_with_options(
        backdrop: &str,
        color_space: &str,
        resources: &str,
    ) -> Vec<u8> {
        let page_stream = b"/GS0 gs 0 0 100 100 re f";
        let mask_stream = b"0.25 0.5 0.75 rg 0 0 100 100 re f";
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</ExtGState<</GS0 5 0 R>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             5 0 obj <</Type/ExtGState/SMask<</S/Luminosity/G 6 0 R{backdrop}>>>> endobj\n\
             6 0 obj <</Type/XObject/Subtype/Form/FormType 1/BBox[0 0 100 100]/Group<</S/Transparency/I true/K false/CS{color_space}>>/Resources<<{resources}>>/Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
            mask_stream.len(),
            String::from_utf8_lossy(mask_stream),
        )
        .into_bytes()
    }

    fn marked_content_metadata_pdf() -> Vec<u8> {
        marked_content_metadata_pdf_with_stream("", b"<x/>")
    }

    fn marked_content_metadata_pdf_with_stream(stream_entries: &str, metadata: &[u8]) -> Vec<u8> {
        let page_stream = b"/Artifact /Layout BDC 0 0 10 10 re f EMC";
        let mut pdf = format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</Properties<</Layout 5 0 R>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             5 0 obj <</O/Layout/Subtype/Glyphst/BBox[-0.25 1.5 20 30]/Metadata 6 0 R>> endobj\n\
             6 0 obj <</Type/Metadata/Subtype/XML{stream_entries}/Length {}>> stream\n",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
            metadata.len(),
        )
        .into_bytes();
        pdf.extend_from_slice(metadata);
        pdf.extend_from_slice(b"\nendstream endobj\ntrailer <</Root 1 0 R>>\n%%EOF");
        pdf
    }

    fn marked_content_private_properties_pdf(entries: &str) -> Vec<u8> {
        let page_stream = b"/Artifact /Layout BDC 0 0 10 10 re f EMC";
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</Properties<</Layout 5 0 R>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             5 0 obj <<{entries}>> endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
        )
        .into_bytes()
    }

    fn missing_marked_content_properties_pdf() -> Vec<u8> {
        let page_stream = b"/Figure /Missing BDC 0 0 10 10 re f EMC";
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<<>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
        )
        .into_bytes()
    }

    fn annotation_form(object: u32, bbox: &str, matrix: &str, contents: &str) -> String {
        format!(
            "{object} 0 obj <</Type/XObject/Subtype/Form/FormType 1/BBox{bbox}/Matrix{matrix}/Resources<<>>/Length {}>> stream\n{contents}\nendstream endobj",
            contents.len()
        )
    }

    #[derive(Default)]
    struct RecordingDevice {
        retained: bool,
        form_keys: Vec<u128>,
        instance_transforms: Vec<Affine>,
        path_transforms: Vec<Affine>,
        draw_modes: Vec<DrawMode>,
        group_properties: Vec<Option<FormGroupProperties>>,
        group_color_spaces: Vec<Option<ColorSpaceKind>>,
        soft_masks: Vec<(MaskType, ColorSpaceKind, bool, Vec<f32>)>,
        marked_properties: Vec<MarkedContentProperties>,
        link_borders: Vec<(LinkBorder, Affine)>,
        events: Vec<&'static str>,
        alpha_is_shape: Vec<bool>,
        alpha_constants: Vec<f32>,
        text_knockout: Vec<bool>,
        raster_images: Vec<(ImageColorSpaceProperties, Vec<u8>)>,
    }

    impl RecordingDevice {
        fn record_soft_mask(&mut self, mask: SoftMask<'_>) {
            self.soft_masks.push((
                mask.mask_type(),
                mask.group_color_space_kind(),
                mask.group_color_space_is_default_overridden(),
                mask.background_color().components().to_vec(),
            ));
        }
    }

    impl<'a> Device<'a> for RecordingDevice {
        fn draw_path(&mut self, _: &BezPath, mut props: DrawProps<'a>, mode: &DrawMode) {
            self.events.push("path");
            self.path_transforms.push(props.transform);
            self.draw_modes.push(mode.clone());
            self.alpha_is_shape.push(props.alpha_is_shape);
            self.alpha_constants.push(props.alpha_constant);
            if let Some(mask) = props.soft_mask.take() {
                self.record_soft_mask(mask);
            }
        }

        fn push_clip_path(&mut self, _: &ClipPath) {}

        fn push_transparency_group(&mut self, _: f32, mask: Option<SoftMask<'a>>, _: BlendMode) {
            if let Some(mask) = mask {
                self.record_soft_mask(mask);
            }
        }

        fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}

        fn begin_text_object(&mut self, text_knockout: bool) {
            self.events.push("begin-text");
            self.text_knockout.push(text_knockout);
        }

        fn end_text_object(&mut self) {
            self.events.push("end-text");
        }

        fn begin_marked_content(&mut self, _: &[u8], _: Option<i32>) {
            self.events.push("begin-marked");
        }

        fn begin_marked_content_with_properties(
            &mut self,
            _: &[u8],
            properties: MarkedContentProperties,
        ) {
            self.events.push("begin-marked");
            self.marked_properties.push(properties);
        }

        fn end_marked_content(&mut self) {
            self.events.push("end-marked");
        }

        fn begin_combined_fill_stroke(&mut self) {
            self.events.push("begin-fill-stroke");
        }

        fn end_combined_fill_stroke(&mut self) {
            self.events.push("end-fill-stroke");
        }

        fn draw_image(&mut self, image: Image<'a, '_>, _: ImageDrawProps<'a>) {
            if let Image::Raster(image) = image {
                let properties = image.color_space_properties();
                image.with_rgba(
                    |data, _| {
                        let bytes = match data {
                            ImageData::Rgb(data) => data.data,
                            ImageData::Luma(data) => data.data,
                        };
                        self.raster_images.push((properties, bytes));
                    },
                    None,
                );
            }
        }

        fn draw_form(&mut self, form: &FormInvocation<'a>) {
            self.events.push("form");
            self.form_keys.push(form.cache_key());
            self.instance_transforms.push(form.instance_transform());
            self.group_properties.push(form.group_properties());
            self.group_color_spaces
                .push(form.group_color_space().map(ColorSpace::kind));
            if self.retained {
                form.interpret_local(self);
            } else {
                form.interpret(self);
            }
        }

        fn draw_link_border(&mut self, border: &LinkBorder, transform: Affine) {
            self.events.push("link-border");
            self.link_borders.push((border.clone(), transform));
        }

        fn pop_clip(&mut self) {}

        fn pop_transparency_group(&mut self) {}
    }

    fn interpret(retained: bool) -> RecordingDevice {
        interpret_bytes(form_pdf(), retained)
    }

    #[test]
    fn automatic_stroke_adjustment_is_retained_and_restored() {
        let device = interpret_bytes(automatic_stroke_adjustment_pdf(), false);
        let adjustment = device
            .draw_modes
            .iter()
            .map(|mode| match mode {
                DrawMode::Stroke(props) => props.automatic_adjustment,
                mode => panic!("expected a stroke, got {mode:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(adjustment, [true, false, true]);
    }

    #[test]
    fn images_apply_default_rgb_before_decoding_and_report_provenance() {
        let device = interpret_bytes(default_rgb_image_pdf(), false);
        assert_eq!(device.raster_images.len(), 3);

        let (default_properties, default_pixels) = &device.raster_images[0];
        assert!(default_properties.has_color_space());
        assert_eq!(
            default_properties.declared_color_space_kind(),
            Some(ColorSpaceKind::DeviceRgb)
        );
        assert_eq!(
            default_properties.color_space_kind(),
            Some(ColorSpaceKind::CalRgb)
        );
        assert_eq!(default_properties.color_space_components(), Some(3));
        assert!(default_properties.color_space_is_default_overridden());

        let (explicit_properties, explicit_pixels) = &device.raster_images[1];
        assert_eq!(
            explicit_properties.declared_color_space_kind(),
            Some(ColorSpaceKind::CalRgb)
        );
        assert_eq!(
            explicit_properties.color_space_kind(),
            Some(ColorSpaceKind::CalRgb)
        );
        assert_eq!(explicit_properties.color_space_components(), Some(3));
        assert!(!explicit_properties.color_space_is_default_overridden());

        assert_eq!(default_pixels, explicit_pixels);
        assert_ne!(default_pixels.as_slice(), [0x40, 0x80, 0xc0]);

        let (alias_properties, alias_pixels) = &device.raster_images[2];
        assert_eq!(
            alias_properties.declared_color_space_kind(),
            Some(ColorSpaceKind::DeviceRgb)
        );
        assert_eq!(
            alias_properties.color_space_kind(),
            Some(ColorSpaceKind::CalRgb)
        );
        assert!(alias_properties.color_space_is_default_overridden());
        assert_eq!(alias_pixels, explicit_pixels);
    }

    #[test]
    fn explicit_srgb_icc_image_is_not_mistaken_for_device_rgb() {
        let device = interpret_bytes(explicit_srgb_icc_image_pdf(), false);
        assert_eq!(device.raster_images.len(), 1);
        let properties = device.raster_images[0].0;
        assert_eq!(
            properties.declared_color_space_kind(),
            Some(ColorSpaceKind::IccBased)
        );
        assert_eq!(
            properties.color_space_kind(),
            Some(ColorSpaceKind::IccBased)
        );
        assert_eq!(properties.color_space_components(), Some(3));
        assert!(!properties.color_space_is_default_overridden());
    }

    #[test]
    fn alpha_source_text_knockout_and_compound_object_boundaries_are_retained() {
        let device = interpret_bytes(alpha_and_text_object_semantics_pdf(), false);
        assert_eq!(device.alpha_is_shape, [true, false, false, false]);
        assert_eq!(device.alpha_constants, [0.4, 0.7, 0.7, 1.0]);
        assert_eq!(device.text_knockout, [true, false]);
        assert_eq!(
            device.events,
            [
                "path",
                "path",
                "begin-text",
                "end-text",
                "begin-text",
                "end-text",
                "begin-fill-stroke",
                "path",
                "path",
                "end-fill-stroke",
            ]
        );
    }

    fn interpret_bytes(bytes: Vec<u8>, retained: bool) -> RecordingDevice {
        interpret_bytes_with_settings(bytes, retained, InterpreterSettings::default())
    }

    fn interpret_bytes_with_settings(
        bytes: Vec<u8>,
        retained: bool,
        settings: InterpreterSettings,
    ) -> RecordingDevice {
        let pdf = Pdf::new(bytes).expect("parse form fixture");
        let cache = InterpreterCache::new();
        let mut context = Context::new(
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 600.0, 700.0),
            &cache,
            pdf.xref(),
            settings,
        );
        let mut device = RecordingDevice {
            retained,
            ..RecordingDevice::default()
        };
        interpret_page(&pdf.pages()[0], &mut context, &mut device);
        device
    }

    #[test]
    fn malformed_text_object_boundaries_are_repaired_and_reported_once() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warning_target = warnings.clone();
        let settings = InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                warning_target.lock().unwrap().push(warning);
            }),
            ..InterpreterSettings::default()
        };
        let device = interpret_bytes_with_settings(
            content_pdf("ET BT BT ET BT 0 0 10 10 re f"),
            false,
            settings,
        );

        assert_eq!(
            device.events,
            [
                "begin-text",
                "end-text",
                "begin-text",
                "end-text",
                "begin-text",
                "end-text",
                "path",
            ]
        );
        let warnings = warnings.lock().unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| matches!(warning, InterpreterWarning::UnmatchedTextObjectEnd))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| matches!(warning, InterpreterWarning::NestedTextObject))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| matches!(warning, InterpreterWarning::UnterminatedTextObject))
        );
    }

    #[test]
    fn malformed_marked_content_boundaries_are_repaired_and_reported_once() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warning_target = warnings.clone();
        let settings = InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                warning_target.lock().unwrap().push(warning);
            }),
            ..InterpreterSettings::default()
        };
        let device = interpret_bytes_with_settings(
            content_pdf("EMC EMC /Span BMC /Inner BMC 0 0 10 10 re f EMC"),
            false,
            settings,
        );

        assert_eq!(
            device.events,
            [
                "begin-marked",
                "begin-marked",
                "path",
                "end-marked",
                "end-marked",
            ]
        );
        let warnings = warnings.lock().unwrap();
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| matches!(warning, InterpreterWarning::UnmatchedMarkedContentEnd))
                .count(),
            1
        );
        assert_eq!(
            warnings
                .iter()
                .filter(|warning| matches!(warning, InterpreterWarning::UnterminatedMarkedContent))
                .count(),
            1
        );
    }

    #[test]
    fn retained_form_invocations_normalize_outer_transforms_and_share_keys() {
        let device = interpret(true);
        assert_eq!(device.form_keys.len(), 2);
        assert_eq!(device.form_keys[0], device.form_keys[1]);
        assert_eq!(
            device.instance_transforms[0].as_coeffs(),
            [2.0, 0.0, 0.0, 2.0, 10.0, 20.0]
        );
        assert_eq!(
            device.instance_transforms[1].as_coeffs(),
            [3.0, 0.0, 0.0, 3.0, 30.0, 40.0]
        );
        assert_eq!(device.path_transforms.len(), 2);
        assert!(
            device
                .path_transforms
                .iter()
                .all(|transform| transform.as_coeffs() == [1.0, 0.0, 0.0, 1.0, 3.0, 4.0])
        );
        assert!(device.group_properties.iter().all(Option::is_none));
    }

    #[test]
    fn form_invocations_expose_typed_transparency_group_properties() {
        let device = interpret_bytes(
            form_pdf_with_group("/Group<</S/Transparency/I true/K false/CS/DeviceRGB>>"),
            true,
        );
        assert_eq!(device.group_properties.len(), 2);
        let properties = device.group_properties[0].expect("group properties");
        assert!(properties.is_transparency());
        assert!(properties.isolated());
        assert!(!properties.knockout());
        assert!(properties.has_color_space());
        assert_eq!(
            properties.color_space_kind(),
            Some(ColorSpaceKind::DeviceRgb)
        );
        assert!(!properties.color_space_is_default_overridden());
        assert!(
            device
                .group_properties
                .iter()
                .all(|candidate| *candidate == Some(properties))
        );
    }

    #[test]
    fn form_group_device_space_reports_default_resource_remapping() {
        let device = interpret_bytes(
            form_pdf_with_group_and_resources(
                "/Group<</S/Transparency/I true/K false/CS/DeviceRGB>>",
                "/ColorSpace<</DefaultRGB/DeviceGray>>",
            ),
            true,
        );
        let properties = device.group_properties[0].expect("group properties");
        assert!(properties.has_color_space());
        assert_eq!(
            properties.declared_color_space_kind(),
            Some(ColorSpaceKind::DeviceRgb)
        );
        assert_eq!(properties.color_space_kind(), None);
        assert!(properties.color_space_is_default_overridden());
    }

    #[test]
    fn form_invocations_expose_parsed_non_device_group_color_spaces() {
        let device = interpret_bytes(
            form_pdf_with_group(
                "/Group<</S/Transparency/I true/K false/CS[/CalRGB<</WhitePoint[0.9505 1 1.089]>>]>>",
            ),
            true,
        );
        let properties = device.group_properties[0].expect("group properties");
        assert_eq!(properties.declared_color_space_kind(), None);
        assert_eq!(properties.color_space_kind(), Some(ColorSpaceKind::CalRgb));
        assert_eq!(device.group_color_spaces[0], Some(ColorSpaceKind::CalRgb));
        assert!(!properties.color_space_is_default_overridden());
    }

    #[test]
    fn soft_masks_expose_group_color_space_and_unconverted_backdrop() {
        let device = interpret_bytes(luminosity_soft_mask_pdf(), false);
        assert_eq!(
            device.soft_masks.len(),
            1,
            "path callbacks: {}",
            device.path_transforms.len()
        );
        assert_eq!(
            device.soft_masks[0],
            (
                MaskType::Luminosity,
                ColorSpaceKind::DeviceRgb,
                false,
                vec![0.1, 0.2, 0.3]
            )
        );

        let device = interpret_bytes(luminosity_soft_mask_pdf_with_backdrop(""), false);
        assert_eq!(device.soft_masks[0].3, vec![0.0, 0.0, 0.0]);

        let device = interpret_bytes(
            luminosity_soft_mask_pdf_with_options(
                "/BC[0.1 0.2 0.3 0.4]",
                "/DeviceCMYK",
                "/ColorSpace<</DefaultCMYK/DeviceRGB>>",
            ),
            false,
        );
        assert_eq!(device.soft_masks[0].1, ColorSpaceKind::DeviceCmyk);
        assert!(device.soft_masks[0].2);
        assert_eq!(device.soft_masks[0].3, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn marked_content_exposes_layout_bounds_owner_and_decoded_metadata() {
        let device = interpret_bytes(marked_content_metadata_pdf(), false);
        assert_eq!(device.marked_properties.len(), 1);
        let properties = &device.marked_properties[0];
        assert_eq!(properties.owner.as_deref(), Some(b"Layout".as_slice()));
        assert_eq!(
            properties.property_subtype.as_deref(),
            Some(b"Glyphst".as_slice())
        );
        assert_eq!(properties.bounding_box, Some([-0.25, 1.5, 20.0, 30.0]));
        let metadata = properties.metadata.as_ref().expect("metadata");
        assert_eq!(metadata.subtype, b"XML");
        assert_eq!(metadata.data, b"<x/>");
        assert!(properties.unavailable_keys.is_empty());

        let filtered = interpret_bytes(
            marked_content_metadata_pdf_with_stream(
                "/Filter/FlateDecode",
                b"\x78\x9c\xb3\xa9\xd0\xb7\x03\x00\x02\xf8\x01\x22",
            ),
            false,
        );
        let properties = &filtered.marked_properties[0];
        let metadata = properties.metadata.as_ref().expect("filtered metadata");
        assert_eq!(metadata.data, b"<x/>");
        assert!(properties.unavailable_keys.is_empty());

        let unsupported_filter = interpret_bytes(
            marked_content_metadata_pdf_with_stream("/Filter/ASCIIHexDecode", b"3c782f3e>"),
            false,
        );
        let properties = &unsupported_filter.marked_properties[0];
        assert!(properties.metadata.is_none());
        assert_eq!(properties.unavailable_keys, [b"Metadata".to_vec()]);

        let limited = interpret_bytes_with_settings(
            marked_content_metadata_pdf(),
            false,
            InterpreterSettings {
                max_marked_content_metadata_bytes: 3,
                ..InterpreterSettings::default()
            },
        );
        let properties = &limited.marked_properties[0];
        assert!(properties.metadata.is_none());
        assert_eq!(properties.unavailable_keys, [b"Metadata".to_vec()]);

        let filtered_limited = interpret_bytes_with_settings(
            marked_content_metadata_pdf_with_stream(
                "/Filter/FlateDecode",
                b"\x78\x9c\xb3\xa9\xd0\xb7\x03\x00\x02\xf8\x01\x22",
            ),
            false,
            InterpreterSettings {
                max_marked_content_metadata_bytes: 3,
                ..InterpreterSettings::default()
            },
        );
        let properties = &filtered_limited.marked_properties[0];
        assert!(properties.metadata.is_none());
        assert_eq!(properties.unavailable_keys, [b"Metadata".to_vec()]);
    }

    #[test]
    fn marked_content_exposes_bounded_producer_defined_values() {
        let device = interpret_bytes(
            marked_content_private_properties_pdf(
                "/PRECISION 5/UNITS(inches)/VP[0.5 -2.25 true null/Glyphst (bytes)]\
                 /Nested<</Count 9007199254740993/Values[1 2 3]>>",
            ),
            false,
        );
        let properties = &device.marked_properties[0];
        assert_eq!(
            properties.additional_properties,
            [
                MarkedContentProperty {
                    key: b"Nested".to_vec(),
                    value: MarkedContentPropertyValue::Dictionary(vec![
                        MarkedContentProperty {
                            key: b"Count".to_vec(),
                            value: MarkedContentPropertyValue::Integer(9_007_199_254_740_993),
                        },
                        MarkedContentProperty {
                            key: b"Values".to_vec(),
                            value: MarkedContentPropertyValue::Array(vec![
                                MarkedContentPropertyValue::Integer(1),
                                MarkedContentPropertyValue::Integer(2),
                                MarkedContentPropertyValue::Integer(3),
                            ]),
                        },
                    ]),
                },
                MarkedContentProperty {
                    key: b"PRECISION".to_vec(),
                    value: MarkedContentPropertyValue::Integer(5),
                },
                MarkedContentProperty {
                    key: b"UNITS".to_vec(),
                    value: MarkedContentPropertyValue::String(b"inches".to_vec()),
                },
                MarkedContentProperty {
                    key: b"VP".to_vec(),
                    value: MarkedContentPropertyValue::Array(vec![
                        MarkedContentPropertyValue::Real(0.5),
                        MarkedContentPropertyValue::Real(-2.25),
                        MarkedContentPropertyValue::Boolean(true),
                        MarkedContentPropertyValue::Null,
                        MarkedContentPropertyValue::Name(b"Glyphst".to_vec()),
                        MarkedContentPropertyValue::String(b"bytes".to_vec()),
                    ]),
                },
            ]
        );
        assert!(properties.unavailable_keys.is_empty());

        let limited = interpret_bytes_with_settings(
            marked_content_private_properties_pdf("/PRECISION 5/UNITS(inches)"),
            false,
            InterpreterSettings {
                max_marked_content_property_bytes: 1,
                ..InterpreterSettings::default()
            },
        );
        let properties = &limited.marked_properties[0];
        assert!(properties.additional_properties.is_empty());
        assert_eq!(
            properties.unavailable_keys,
            [b"PRECISION".to_vec(), b"UNITS".to_vec()]
        );
    }

    #[test]
    fn marked_content_exposes_unresolved_named_property_list_identity() {
        let device = interpret_bytes(missing_marked_content_properties_pdf(), false);
        let properties = &device.marked_properties[0];
        assert_eq!(
            properties.property_list_name.as_deref(),
            Some(b"Missing".as_slice())
        );
        assert!(!properties.property_list_resolved);
        assert!(properties.additional_properties.is_empty());
        assert!(properties.unavailable_keys.is_empty());

        let resolved = interpret_bytes(marked_content_metadata_pdf(), false);
        let properties = &resolved.marked_properties[0];
        assert_eq!(
            properties.property_list_name.as_deref(),
            Some(b"Layout".as_slice())
        );
        assert!(properties.property_list_resolved);
    }

    #[test]
    fn default_form_interpretation_preserves_page_space_output() {
        let device = interpret(false);
        assert_eq!(device.path_transforms.len(), 2);
        assert_eq!(
            device.path_transforms[0].as_coeffs(),
            [2.0, 0.0, 0.0, 2.0, 16.0, 28.0]
        );
        assert_eq!(
            device.path_transforms[1].as_coeffs(),
            [3.0, 0.0, 0.0, 3.0, 39.0, 52.0]
        );
    }

    #[test]
    fn annotation_appearance_maps_a_transformed_nonzero_bbox_exactly() {
        let form = annotation_form(6, "[10 20 50 80]", "[2 0 0 3 7 11]", "10 20 40 60 re f");
        let bytes = annotation_pdf(
            "5 0 R",
            &format!(
                "5 0 obj <</Type/Annot/Subtype/Square/Rect[100 200 300 500]/AP<</N 6 0 R>>>> endobj\n{form}"
            ),
        );
        let device = interpret_bytes(bytes, true);

        assert_eq!(device.instance_transforms.len(), 1);
        let actual = device.instance_transforms[0].as_coeffs();
        let expected = [2.5, 0.0, 0.0, 5.0 / 3.0, 32.5, 245.0 / 3.0];
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-9, "{actual} != {expected}");
        }
        assert_eq!(
            device.path_transforms[0].as_coeffs(),
            [2.0, 0.0, 0.0, 3.0, 7.0, 11.0]
        );
    }

    #[test]
    fn annotation_appearance_state_requires_and_uses_as_exactly() {
        let on = annotation_form(6, "[0 0 10 10]", "[1 0 0 1 0 0]", "0 0 10 10 re f");
        let off = annotation_form(7, "[0 0 10 10]", "[1 0 0 1 50 0]", "0 0 10 10 re f");
        let objects = format!(
            "5 0 obj <</Type/Annot/Subtype/Widget/Rect[10 20 30 40]/AP<</N<</On 6 0 R/Off 7 0 R>>>>/AS/On>> endobj\n{on}\n{off}"
        );
        let device = interpret_bytes(annotation_pdf("5 0 R", &objects), true);
        assert_eq!(device.path_transforms.len(), 1);
        assert_eq!(
            device.path_transforms[0].as_coeffs(),
            Affine::IDENTITY.as_coeffs()
        );

        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warning_target = warnings.clone();
        let settings = InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                warning_target.lock().unwrap().push(warning);
            }),
            ..InterpreterSettings::default()
        };
        let objects_without_as = format!(
            "5 0 obj <</Type/Annot/Subtype/Widget/Rect[10 20 30 40]/AP<</N<</On 6 0 R/Off 7 0 R>>>>>> endobj\n{on}\n{off}"
        );
        let device = interpret_bytes_with_settings(
            annotation_pdf("5 0 R", &objects_without_as),
            true,
            settings,
        );
        assert!(device.form_keys.is_empty());
        assert!(matches!(
            warnings.lock().unwrap().as_slice(),
            [InterpreterWarning::AnnotationAppearanceFailure(
                crate::AnnotationAppearanceError::MissingAppearanceState
            )]
        ));
    }

    #[test]
    fn annotation_visibility_flags_follow_screen_view_rules() {
        let form = annotation_form(8, "[0 0 10 10]", "[1 0 0 1 0 0]", "0 0 10 10 re f");
        let objects = format!(
            "5 0 obj <</Type/Annot/Subtype/Square/Rect[0 0 10 10]/F 1/AP<</N 8 0 R>>>> endobj\n\
             6 0 obj <</Type/Annot/Subtype/Square/Rect[20 0 30 10]/F 2/AP<</N 8 0 R>>>> endobj\n\
             7 0 obj <</Type/Annot/Subtype/Square/Rect[40 0 50 10]/F 32/AP<</N 8 0 R>>>> endobj\n\
             9 0 obj <</Type/Annot/Subtype/Square/Rect[60 0 70 10]/AP<</N 8 0 R>>>> endobj\n\
             10 0 obj <</Type/Annot/Subtype/PrivateThing/Rect[80 0 90 10]/F 1/AP<</N 8 0 R>>>> endobj\n{form}"
        );
        let device = interpret_bytes(
            annotation_pdf("5 0 R 6 0 R 7 0 R 9 0 R 10 0 R", &objects),
            true,
        );
        assert_eq!(device.form_keys.len(), 2);
    }

    #[test]
    fn link_borders_preserve_annotation_order_and_typed_visual_parameters() {
        let appearance = annotation_form(11, "[0 0 10 10]", "[1 0 0 1 0 0]", "0 0 10 10 re f");
        let objects = format!(
            "5 0 obj <</Type/Annot/Subtype/Link/Rect[10 20 110 70]/Border[8 6 2[3 2]]/C[0.2 0.4 0.6]>> endobj\n\
             6 0 obj <</Type/Annot/Subtype/Link/Rect[120 20 220 70]/Border[0 0 9]/BS<</Type/Border/W 3/S/U/D[4]>>/C[0.1]>> endobj\n\
             7 0 obj <</Type/Annot/Subtype/Link/Rect[230 20 330 70]/BS<</W 0/S/B>>/C[1 0 0]>> endobj\n\
             8 0 obj <</Type/Annot/Subtype/Link/Rect[340 20 440 70]/BS<</W 4/S/I>>/C[]>> endobj\n\
             9 0 obj <</Type/Annot/Subtype/Link/Rect[10 90 110 140]/AP<</N 11 0 R>>/BS 17>> endobj\n\
             10 0 obj <</Type/Annot/Subtype/Link/Rect[120 90 220 140]/BS<</W 5/S/B>>/C[0.1 0.2 0.3 0.4]>> endobj\n{appearance}"
        );
        let device = interpret_bytes(
            annotation_pdf("5 0 R 9 0 R 6 0 R 10 0 R 7 0 R 8 0 R", &objects),
            true,
        );

        assert_eq!(
            device.events,
            ["link-border", "form", "path", "link-border", "link-border"]
        );
        assert_eq!(device.link_borders.len(), 3);
        assert_eq!(
            device.link_borders[0],
            (
                LinkBorder {
                    geometry: LinkBorderGeometry::RoundedRectangle {
                        rect: Rect::new(10.0, 20.0, 110.0, 70.0),
                        horizontal_radius: 8.0,
                        vertical_radius: 6.0,
                    },
                    width: 2.0,
                    style: LinkBorderStyle::Dashed {
                        dash_array: vec![3.0, 2.0],
                    },
                    color: LinkBorderColor::Rgb([0.2, 0.4, 0.6]),
                },
                Affine::IDENTITY,
            )
        );
        assert_eq!(
            device.link_borders[1].0,
            LinkBorder {
                geometry: LinkBorderGeometry::Rectangle {
                    rect: Rect::new(120.0, 20.0, 220.0, 70.0),
                },
                width: 3.0,
                style: LinkBorderStyle::Underline,
                color: LinkBorderColor::Gray([0.1]),
            }
        );
        assert_eq!(
            device.link_borders[2].0,
            LinkBorder {
                geometry: LinkBorderGeometry::Rectangle {
                    rect: Rect::new(120.0, 90.0, 220.0, 140.0),
                },
                width: 5.0,
                style: LinkBorderStyle::Beveled,
                color: LinkBorderColor::Cmyk([0.1, 0.2, 0.3, 0.4]),
            }
        );
    }

    #[test]
    fn link_border_normalizes_iso_and_adobe_quadpoint_orders() {
        let objects = "5 0 obj <</Type/Annot/Subtype/Link/Rect[10 20 110 70]/QuadPoints[20 25 100 25 90 60 30 60]/BS<</W 2/S/U>>/C[1 0 0]>> endobj\n\
             6 0 obj <</Type/Annot/Subtype/Link/Rect[120 20 220 70]/QuadPoints[130 60 210 60 140 25 200 25]/BS<</W 2/S/S>>/C[0 0 1]>> endobj\n\
             7 0 obj <</Type/Annot/Subtype/Link/Rect[230 20 330 70]/QuadPoints[220 25 300 25 300 60 240 60]/BS<</W 2/S/S>>>> endobj";
        let device = interpret_bytes(annotation_pdf("5 0 R 6 0 R 7 0 R", objects), true);

        assert_eq!(device.link_borders.len(), 3);
        assert_eq!(
            device.link_borders[0].0.geometry,
            LinkBorderGeometry::Quadrilaterals {
                rect: Rect::new(10.0, 20.0, 110.0, 70.0),
                quads: vec![crate::LinkQuadrilateral {
                    bottom_left: kurbo::Point::new(20.0, 25.0),
                    bottom_right: kurbo::Point::new(100.0, 25.0),
                    top_right: kurbo::Point::new(90.0, 60.0),
                    top_left: kurbo::Point::new(30.0, 60.0),
                }],
            }
        );
        assert_eq!(
            device.link_borders[1].0.geometry,
            LinkBorderGeometry::Quadrilaterals {
                rect: Rect::new(120.0, 20.0, 220.0, 70.0),
                quads: vec![crate::LinkQuadrilateral {
                    bottom_left: kurbo::Point::new(140.0, 25.0),
                    bottom_right: kurbo::Point::new(200.0, 25.0),
                    top_right: kurbo::Point::new(210.0, 60.0),
                    top_left: kurbo::Point::new(130.0, 60.0),
                }],
            }
        );
        assert_eq!(
            device.link_borders[2].0.geometry,
            LinkBorderGeometry::Rectangle {
                rect: Rect::new(230.0, 20.0, 330.0, 70.0),
            }
        );
    }

    #[test]
    fn link_quadpoint_limit_reports_a_structured_warning() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warning_target = warnings.clone();
        let settings = InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                warning_target.lock().unwrap().push(warning);
            }),
            max_link_quads_per_annotation: 1,
            ..InterpreterSettings::default()
        };
        let objects = "5 0 obj <</Type/Annot/Subtype/Link/Rect[10 20 110 70]/QuadPoints[10 20 50 20 50 30 10 30 10 40 50 40 50 50 10 50]/BS<</W 2/S/S>>>> endobj";
        let device =
            interpret_bytes_with_settings(annotation_pdf("5 0 R", objects), true, settings);

        assert!(device.link_borders.is_empty());
        assert!(matches!(
            warnings.lock().unwrap().as_slice(),
            [InterpreterWarning::LinkBorderFailure(
                crate::LinkBorderError::InvalidQuadPoints(crate::LinkQuadPointsError::ItemLimit)
            )]
        ));
    }

    #[test]
    fn malformed_link_border_reports_a_structured_warning() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warning_target = warnings.clone();
        let settings = InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                warning_target.lock().unwrap().push(warning);
            }),
            ..InterpreterSettings::default()
        };
        let objects = "5 0 obj <</Type/Annot/Subtype/Link/Rect[10 20 110 70]/BS<</W 2/S/D/D[0 0]>>/C[1 0 0]>> endobj";
        let device =
            interpret_bytes_with_settings(annotation_pdf("5 0 R", objects), true, settings);

        assert!(device.link_borders.is_empty());
        assert!(matches!(
            warnings.lock().unwrap().as_slice(),
            [InterpreterWarning::LinkBorderFailure(
                crate::LinkBorderError::InvalidDashArray
            )]
        ));
    }

    #[test]
    fn link_border_does_not_assign_markup_opacity_semantics() {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let warning_target = warnings.clone();
        let settings = InterpreterSettings {
            warning_sink: Arc::new(move |warning| {
                warning_target.lock().unwrap().push(warning);
            }),
            ..InterpreterSettings::default()
        };
        let objects =
            "5 0 obj <</Type/Annot/Subtype/Link/Rect[10 20 110 70]/Border[0 0 1]/CA 0.5>> endobj";
        let device =
            interpret_bytes_with_settings(annotation_pdf("5 0 R", objects), true, settings);

        assert!(device.link_borders.is_empty());
        assert!(matches!(
            warnings.lock().unwrap().as_slice(),
            [InterpreterWarning::LinkBorderFailure(
                crate::LinkBorderError::UnsupportedOpacity
            )]
        ));
    }
}
