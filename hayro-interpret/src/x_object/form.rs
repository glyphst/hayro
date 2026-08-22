use super::xobject_oc;
use crate::cache::CacheKey;
use crate::color::ColorSpaceKind;
use crate::context::Context;
use crate::device::Device;
use crate::interpret::state::State;
use crate::util::hash128;
use crate::{ClipPath, FillRule, interpret};
use crate::{InterpreterCache, InterpreterSettings};
use hayro_syntax::content::TypedIter;
use hayro_syntax::object::Dict;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::*;
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

    /// Conservatively classified explicit blending color space.
    ///
    /// `None` means either that the group omits `/CS`, that the color space is
    /// not a direct device-space name, or that a `DefaultGray`, `DefaultRGB`,
    /// or `DefaultCMYK` resource remaps that direct name.
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
        let group_properties = form.group_properties.map(|mut properties| {
            let default_name = match properties.color_space_kind {
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
            device.push_transparency_group(
                context.get().graphics_state.non_stroke_alpha,
                std::mem::take(&mut context.get_mut().graphics_state.soft_mask),
                std::mem::take(&mut context.get_mut().graphics_state.blend_mode),
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
        BlendMode, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterSettings,
        InterpreterWarning, MarkedContentProperties, MarkedContentProperty,
        MarkedContentPropertyValue, MaskType, SoftMask, interpret_page,
    };
    use hayro_syntax::Pdf;
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

    fn luminosity_soft_mask_pdf() -> Vec<u8> {
        luminosity_soft_mask_pdf_with_backdrop("/BC[0.1 0.2 0.3]")
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
        marked_content_metadata_pdf_with_stream("")
    }

    fn marked_content_metadata_pdf_with_stream(stream_entries: &str) -> Vec<u8> {
        let page_stream = b"/Artifact /Layout BDC 0 0 10 10 re f EMC";
        let metadata = b"<x/>";
        format!(
            "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</Properties<</Layout 5 0 R>>>>/Contents 4 0 R>> endobj\n\
             4 0 obj <</Length {}>> stream\n{}\nendstream endobj\n\
             5 0 obj <</O/Layout/Subtype/Glyphst/BBox[-0.25 1.5 20 30]/Metadata 6 0 R>> endobj\n\
             6 0 obj <</Type/Metadata/Subtype/XML{stream_entries}/Length {}>> stream\n{}\nendstream endobj\n\
             trailer <</Root 1 0 R>>\n%%EOF",
            page_stream.len(),
            String::from_utf8_lossy(page_stream),
            metadata.len(),
            String::from_utf8_lossy(metadata),
        )
        .into_bytes()
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
        group_properties: Vec<Option<FormGroupProperties>>,
        soft_masks: Vec<(MaskType, ColorSpaceKind, bool, Vec<f32>)>,
        marked_properties: Vec<MarkedContentProperties>,
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
        fn draw_path(&mut self, _: &BezPath, mut props: DrawProps<'a>, _: &DrawMode) {
            self.path_transforms.push(props.transform);
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

        fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

        fn draw_form(&mut self, form: &FormInvocation<'a>) {
            self.form_keys.push(form.cache_key());
            self.instance_transforms.push(form.instance_transform());
            self.group_properties.push(form.group_properties());
            if self.retained {
                form.interpret_local(self);
            } else {
                form.interpret(self);
            }
        }

        fn pop_clip(&mut self) {}

        fn pop_transparency_group(&mut self) {}

        fn begin_marked_content_with_properties(
            &mut self,
            _: &[u8],
            properties: MarkedContentProperties,
        ) {
            self.marked_properties.push(properties);
        }
    }

    fn interpret(retained: bool) -> RecordingDevice {
        interpret_bytes(form_pdf(), retained)
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
    fn marked_content_exposes_layout_bounds_owner_and_unfiltered_metadata() {
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
            marked_content_metadata_pdf_with_stream("/Filter/FlateDecode"),
            false,
        );
        let properties = &filtered.marked_properties[0];
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
}
