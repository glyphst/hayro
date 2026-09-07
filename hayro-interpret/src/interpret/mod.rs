use crate::FillRule;
use crate::annotation::{
    annotation_is_visible_on_screen, resolve_annotation_appearance, resolve_geometric_annotation,
    resolve_link_border_with_limit, resolve_text_markup,
};
use crate::color::ColorSpace;
use crate::context::Context;
use crate::convert::{convert_line_cap, convert_line_join};
use crate::device::{
    Device, MarkedContentMetadata, MarkedContentProperties, MarkedContentProperty,
    MarkedContentPropertyValue,
};
use crate::font::{Font, FontData, FontQuery, StandardFont};
use crate::interpret::path::{
    close_path, fill_path, fill_path_impl, fill_stroke_path, stroke_path,
};
use crate::interpret::state::{TextStateFont, handle_gs};
use crate::interpret::text::TextRenderingMode;
use crate::pattern::{Pattern, ShadingPattern};
use crate::shading::Shading;
use crate::util::OptionLog;
use crate::x_object::{ImageXObject, XObject};
use hayro_syntax::content::TypedIter;
use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::object::dict::keys::{
    ACTUAL_TEXT, ALT, ANNOTS, BBOX, LANG, MCID, METADATA, NAME, O, OC, OCG, OCMD, SUBTYPE, TYPE,
};
use hayro_syntax::object::{
    Array, Dict, Name, Object, ObjectIdentifier, Stream, String as PdfString, dict_or_stream,
};
use hayro_syntax::page::{Page, Resources};
use kurbo::{Affine, Point, Shape};
use rustc_hash::FxHashMap;
use smallvec::smallvec;
use std::mem::size_of;
use std::sync::Arc;

pub(crate) mod path;
pub(crate) mod state;
pub(crate) mod text;

pub use state::ActiveTransferFunction;

const KNOWN_MARKED_CONTENT_KEYS: [&[u8]; 11] = [
    MCID,
    ACTUAL_TEXT,
    ALT,
    LANG,
    TYPE,
    SUBTYPE,
    NAME,
    O,
    BBOX,
    METADATA,
    OC,
];

struct MarkedPropertyBudget {
    remaining_bytes: u64,
    max_depth: u32,
}

impl MarkedPropertyBudget {
    fn new(max_bytes: u64, max_depth: u32) -> Self {
        Self {
            remaining_bytes: max_bytes,
            max_depth,
        }
    }

    fn charge(&mut self, bytes: usize) -> Option<()> {
        let bytes = u64::try_from(bytes).ok()?;
        self.remaining_bytes = self.remaining_bytes.checked_sub(bytes)?;
        Some(())
    }
}

fn marked_property_value(
    object: Object<'_>,
    depth: u32,
    budget: &mut MarkedPropertyBudget,
) -> Option<MarkedContentPropertyValue> {
    if depth > budget.max_depth {
        return None;
    }
    budget.charge(size_of::<MarkedContentPropertyValue>())?;
    match object {
        Object::Null(_) => Some(MarkedContentPropertyValue::Null),
        Object::Boolean(value) => Some(MarkedContentPropertyValue::Boolean(value)),
        Object::Number(value) => {
            if let Some(value) = value.as_i64_exact() {
                Some(MarkedContentPropertyValue::Integer(value))
            } else {
                let value = value.as_f64();
                value
                    .is_finite()
                    .then_some(MarkedContentPropertyValue::Real(value))
            }
        }
        Object::String(value) => {
            budget.charge(value.as_bytes().len())?;
            Some(MarkedContentPropertyValue::String(
                value.as_bytes().to_vec(),
            ))
        }
        Object::Name(value) => {
            budget.charge(value.as_ref().len())?;
            Some(MarkedContentPropertyValue::Name(value.as_ref().to_vec()))
        }
        Object::Array(array) => {
            let expected = array.raw_iter().count();
            let mut values = Vec::with_capacity(expected.min(1024));
            let mut resolved = array.iter::<Object<'_>>();
            for _ in 0..expected {
                values.push(marked_property_value(
                    resolved.next()?,
                    depth.saturating_add(1),
                    budget,
                )?);
            }
            Some(MarkedContentPropertyValue::Array(values))
        }
        Object::Dict(dict) => marked_property_dictionary(&dict, depth.saturating_add(1), budget)
            .map(MarkedContentPropertyValue::Dictionary),
        // Known metadata streams are retained separately. Arbitrary streams
        // remain unavailable until their filter and external-file semantics
        // can be preserved and bounded as a complete value.
        Object::Stream(_) => None,
    }
}

fn marked_property_dictionary(
    dict: &Dict<'_>,
    depth: u32,
    budget: &mut MarkedPropertyBudget,
) -> Option<Vec<MarkedContentProperty>> {
    if depth > budget.max_depth {
        return None;
    }
    let mut keys = dict.keys().collect::<Vec<_>>();
    keys.sort();
    let mut entries = Vec::with_capacity(keys.len().min(1024));
    for key in keys {
        budget.charge(size_of::<MarkedContentProperty>())?;
        budget.charge(key.as_ref().len())?;
        let value = dict.get::<Object<'_>>(key.as_ref())?;
        entries.push(MarkedContentProperty {
            key: key.as_ref().to_vec(),
            value: marked_property_value(value, depth, budget)?,
        });
    }
    Some(entries)
}

fn marked_content_properties(
    props: &Dict<'_>,
    max_metadata_bytes: u64,
    max_property_bytes: u64,
    max_property_depth: u32,
) -> MarkedContentProperties {
    let mcid = props.get::<i32>(MCID);
    let actual_text = props
        .get::<PdfString<'_>>(ACTUAL_TEXT)
        .map(|value| value.as_bytes().to_vec());
    let alternate_text = props
        .get::<PdfString<'_>>(ALT)
        .map(|value| value.as_bytes().to_vec());
    let language = props
        .get::<PdfString<'_>>(LANG)
        .map(|value| value.as_bytes().to_vec());
    let property_type = props
        .get::<Name<'_>>(TYPE)
        .map(|value| value.as_ref().to_vec());
    let property_subtype = props
        .get::<Name<'_>>(SUBTYPE)
        .map(|value| value.as_ref().to_vec());
    let name = props
        .get::<PdfString<'_>>(NAME)
        .map(|value| value.as_bytes().to_vec());
    let owner = props
        .get::<Name<'_>>(O)
        .map(|value| value.as_ref().to_vec());
    let bounding_box = props
        .get::<[f64; 4]>(BBOX)
        .filter(|values| values.iter().all(|value| value.is_finite()));
    let metadata = props.get::<Stream<'_>>(METADATA).and_then(|stream| {
        let dict = stream.dict();
        let is_metadata = dict
            .get::<Name<'_>>(TYPE)
            .is_some_and(|value| value.as_ref() == METADATA);
        let subtype = dict.get::<Name<'_>>(SUBTYPE)?;
        if !is_metadata || subtype.as_ref() != b"XML" {
            return None;
        }
        let max_metadata_bytes = usize::try_from(max_metadata_bytes).unwrap_or(usize::MAX);
        let data = stream.decoded_flate_with_limit(max_metadata_bytes).ok()?;
        Some(MarkedContentMetadata {
            subtype: subtype.as_ref().to_vec(),
            data: data.into_owned(),
        })
    });
    let mut unavailable_keys = Vec::new();
    for (key, available) in [
        (MCID, mcid.is_some()),
        (ACTUAL_TEXT, actual_text.is_some()),
        (ALT, alternate_text.is_some()),
        (LANG, language.is_some()),
        (TYPE, property_type.is_some()),
        (SUBTYPE, property_subtype.is_some()),
        (NAME, name.is_some()),
        (O, owner.is_some()),
        (BBOX, bounding_box.is_some()),
        (METADATA, metadata.is_some()),
    ] {
        if props.contains_key(key) && !available {
            unavailable_keys.push(key.to_vec());
        }
    }
    let mut additional_properties = Vec::new();
    let mut property_budget = MarkedPropertyBudget::new(max_property_bytes, max_property_depth);
    let mut keys = props.keys().collect::<Vec<_>>();
    keys.sort();
    for key in keys {
        if KNOWN_MARKED_CONTENT_KEYS
            .iter()
            .any(|known| key.as_ref() == *known)
        {
            continue;
        }
        let retained = property_budget
            .charge(size_of::<MarkedContentProperty>())
            .and_then(|()| property_budget.charge(key.as_ref().len()))
            .and_then(|()| props.get::<Object<'_>>(key.as_ref()))
            .and_then(|value| marked_property_value(value, 0, &mut property_budget));
        if let Some(value) = retained {
            additional_properties.push(MarkedContentProperty {
                key: key.as_ref().to_vec(),
                value,
            });
        } else {
            unavailable_keys.push(key.as_ref().to_vec());
        }
    }
    MarkedContentProperties {
        property_list_name: None,
        property_list_resolved: true,
        mcid,
        actual_text,
        alternate_text,
        language,
        property_type,
        property_subtype,
        name,
        owner,
        bounding_box,
        metadata,
        additional_properties,
        unavailable_keys,
    }
}

/// A callback function for resolving font queries.
///
/// The first argument is the raw data, the second argument is the index in case the font
/// is a TTC, otherwise it should be 0.
pub type FontResolverFn = Arc<dyn Fn(&FontQuery) -> Option<(FontData, u32)> + Send + Sync>;
/// A callback function for resolving cmap names to their files.
pub type CMapResolverFn =
    Arc<dyn Fn(hayro_cmap::CMapName<'_>) -> Option<&'static [u8]> + Send + Sync>;
/// A callback function for resolving warnings during interpretation.
pub type WarningSinkFn = Arc<dyn Fn(InterpreterWarning) + Send + Sync>;

#[derive(Clone)]
/// Settings that should be applied during the interpretation process.
pub struct InterpreterSettings {
    /// Nearly every PDF contains text. In most cases, PDF files embed the fonts they use, and
    /// hayro can therefore read the font files and do all the processing needed. However, there
    /// are two problems:
    /// - Fonts don't _have_ to be embedded, it's possible that the PDF file only defines the basic
    ///   metadata of the font, like its name, but relies on the PDF processor to find that font
    ///   in its environment.
    /// - The PDF specification requires a list of 14 fonts that should always be available to a
    ///   PDF processor. These include:
    ///   - Times New Roman (Normal, Bold, Italic, `BoldItalic`)
    ///   - Courier (Normal, Bold, Italic, `BoldItalic`)
    ///   - Helvetica (Normal, Bold, Italic, `BoldItalic`)
    ///   - `ZapfDingBats`
    ///   - Symbol
    ///
    /// Because of this, if any of the above situations occurs, this callback will be called, which
    /// expects the data of an appropriate font to be returned, if available. If no such font is
    /// provided, the text will most likely fail to render.
    ///
    /// For the font data, there are two different formats that are accepted:
    /// - Any valid TTF/OTF font.
    /// - A valid CFF font program.
    ///
    /// The following recommendations are given for the implementation of this callback function.
    ///
    /// For the standard fonts, in case the original fonts are available on the system, you should
    /// just return those. Otherwise, for Helvetica, Courier and Times New Roman, the best alternative
    /// are the corresponding fonts of the [Liberation font family](https://github.com/liberationfonts/liberation-fonts).
    /// If you prefer smaller fonts, you can use the [Foxit CFF fonts](https://github.com/LaurenzV/hayro/tree/master/assets/standard_fonts),
    /// which are much smaller but are missing glyphs for certain scripts.
    ///
    /// For the `Symbol` and `ZapfDingBats` fonts, you should also prefer the system fonts, and if
    /// not available to you, you can, similarly to above, use the corresponding fonts from Foxit.
    ///
    /// If you don't want having to deal with this, you can just enable the `embed-fonts` feature
    /// and use the default implementation of the callback.
    pub font_resolver: FontResolverFn,
    /// A callback for resolving cmaps that aren't embedded.
    ///
    /// When the PDF requires using a cmap that is not directly embedded in the PDF,
    /// this callback will be called to attempt fetching the data of the file.
    ///
    /// When the `embed-cmaps` feature is enabled, this uses `load_embedded`
    /// method from `hayro-cmap` by default, which embeds the cmap files for
    /// all 61 predefined cmaps
    /// that the PDF specification requires to be readily available on a system.
    /// Otherwise, you can implement your custom logic for lazily fetching the
    /// data. If you are fine not supporting such PDFs, you can simply pass a closure
    /// that always returns `None`.
    pub cmap_resolver: CMapResolverFn,
    /// In certain cases, `hayro` will emit a warning in case an issue was encountered while interpreting
    /// the PDF file. Providing a callback allows you to catch those warnings and handle them, if desired.
    pub warning_sink: WarningSinkFn,
    /// Whether annotations should be rendered as well.
    ///
    /// Note that this feature is currently not fully implemented yet, so some
    /// annotations might be missing.
    pub render_annotations: bool,
    /// Preserve every optional-content branch and emit owned visibility-expression callbacks.
    ///
    /// The default remains `false`, which applies the document's default OCG
    /// configuration during interpretation for compatibility with existing
    /// raster devices. Retained-scene devices should enable this and evaluate
    /// the emitted expressions themselves.
    pub preserve_optional_content: bool,
    /// Maximum decoded metadata bytes copied from one marked-content property
    /// list.
    ///
    /// Values above this limit remain present in `unavailable_keys`, allowing
    /// retained-scene devices to fail closed before allocating the copy.
    pub max_marked_content_metadata_bytes: u64,
    /// Maximum bytes retained for producer-defined entries in one
    /// marked-content property list, including structural accounting.
    pub max_marked_content_property_bytes: u64,
    /// Maximum nested array and dictionary depth retained for one
    /// marked-content property value.
    pub max_marked_content_property_depth: u32,
    /// Maximum activation quadrilaterals retained from one Link annotation.
    pub max_link_quads_per_annotation: usize,
    /// Maximum aggregate text-markup quadrilaterals resolved for one page.
    pub max_text_markup_quads_per_page: usize,
    /// Aggregate geometry budget for synthesized geometric annotation appearances.
    pub annotation_geometry_budget: crate::AnnotationGeometryBudget,
    /// Aggregate owned optional-content expression budget for annotations.
    pub annotation_optional_content_budget: crate::OptionalContentBudget,
}

impl Default for InterpreterSettings {
    fn default() -> Self {
        Self {
            #[cfg(not(feature = "embed-fonts"))]
            font_resolver: Arc::new(|_| None),
            #[cfg(feature = "embed-fonts")]
            font_resolver: Arc::new(|query| match query {
                FontQuery::Standard(s) => Some(s.get_font_data()),
                FontQuery::Fallback(f) => Some(f.pick_standard_font().get_font_data()),
            }),
            #[cfg(feature = "embed-cmaps")]
            cmap_resolver: Arc::new(hayro_cmap::load_embedded),
            #[cfg(not(feature = "embed-cmaps"))]
            cmap_resolver: Arc::new(|_| None),
            warning_sink: Arc::new(|_| {}),
            render_annotations: true,
            preserve_optional_content: false,
            max_marked_content_metadata_bytes: 64 * 1024 * 1024,
            max_marked_content_property_bytes: 64 * 1024 * 1024,
            max_marked_content_property_depth: 64,
            max_link_quads_per_annotation: 65_536,
            max_text_markup_quads_per_page: 65_536,
            annotation_geometry_budget: crate::AnnotationGeometryBudget::default(),
            annotation_optional_content_budget: crate::OptionalContentBudget::default(),
        }
    }
}

#[derive(Copy, Clone, Debug)]
/// Warnings that can occur while interpreting a PDF file.
pub enum InterpreterWarning {
    /// An unsupported font kind was encountered.
    ///
    /// Currently, only CID fonts with non-identity encoding are unsupported.
    UnsupportedFont,
    /// An image failed to decode.
    ImageDecodeFailure,
    /// An image color-space declaration or selected default could not be resolved.
    ImageColorSpace(crate::color::ImageColorSpaceError),
    /// A selected paint or shading color space could not be resolved exactly.
    ColorSpaceFailure,
    /// An unknown intent name was replaced with the PDF relative default.
    RenderingIntentUnknown,
    /// An explicit color conversion could not be represented exactly.
    ColorConversion(crate::color::ColorConversionError),
    /// An optional-content membership dictionary could not be converted into
    /// an owned visibility expression.
    OptionalContentExpressionFailure,
    /// An annotation membership failed bounded expression resolution.
    AnnotationOptionalContentFailure(crate::OptionalContentError),
    /// A visible annotation's normal appearance could not be selected or
    /// mapped exactly.
    AnnotationAppearanceFailure(crate::AnnotationAppearanceError),
    /// A Link annotation's synthesized border could not be resolved exactly.
    LinkBorderFailure(crate::LinkBorderError),
    /// A synthesized text-markup appearance could not be resolved exactly.
    TextMarkupFailure(crate::TextMarkupError),
    /// A synthesized geometric annotation could not be resolved exactly.
    GeometricAnnotationFailure(crate::GeometricAnnotationError),
    /// An `EMC` operator had no matching `BMC` or `BDC` in this content scope
    /// and was ignored.
    UnmatchedMarkedContentEnd,
    /// A marked-content sequence reached the end of its content scope without
    /// an `EMC` and was closed there.
    UnterminatedMarkedContent,
    /// An `ET` operator had no matching `BT` in this content scope and was
    /// ignored.
    UnmatchedTextObjectEnd,
    /// A text object reached the next graphics object or the end of its
    /// content scope without an `ET` and was closed at that boundary.
    UnterminatedTextObject,
    /// A nested `BT` closed the already active text object before beginning a
    /// new one.
    NestedTextObject,
}

/// interpret the contents of the page and render them into the device.
pub fn interpret_page<'a>(
    page: &Page<'a>,
    context: &mut Context<'a>,
    device: &mut impl Device<'a>,
) {
    let resources = page.resources();
    interpret(page.typed_operations(), resources, context, device);

    if context.settings.render_annotations
        && let Some(annot_arr) = page.raw().get::<Array<'_>>(ANNOTS)
    {
        let mut remaining_markup_quads = context.settings.max_text_markup_quads_per_page;
        let mut geometry_budget = context.settings.annotation_geometry_budget;
        let mut optional_budget = context.settings.annotation_optional_content_budget;
        for annot in annot_arr.iter::<Dict<'_>>() {
            if device.is_cancelled() {
                break;
            }
            if let Some(remaining) = device.remaining_annotation_geometry_budget() {
                geometry_budget.verbs = geometry_budget.verbs.min(remaining.verbs);
                geometry_budget.draws = geometry_budget.draws.min(remaining.draws);
                geometry_budget.bytes = geometry_budget.bytes.min(remaining.bytes);
                geometry_budget.intersections =
                    geometry_budget.intersections.min(remaining.intersections);
            }
            // Print is intentionally irrelevant for this screen device.
            if !annotation_is_visible_on_screen(&annot) {
                continue;
            }

            let has_oc = match crate::resolve_optional_content(
                &annot,
                context.xref,
                &mut optional_budget,
                &|| device.is_cancelled(),
            ) {
                Ok(Some(expression)) => {
                    let expression = context.ocg_state.begin_resolved(expression);
                    if context.settings.preserve_optional_content {
                        device.begin_optional_content(&expression);
                    }
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    (context.settings.warning_sink)(
                        InterpreterWarning::AnnotationOptionalContentFailure(error),
                    );
                    continue;
                }
            };
            if !context.ocg_state.is_visible() {
                if has_oc && context.ocg_state.end_marked_content() {
                    device.end_optional_content();
                }
                continue;
            }

            match resolve_annotation_appearance(&annot) {
                Ok(Some(appearance)) => {
                    // ISO 32000-2, 12.5.5: concatenate the appearance Form's
                    // Matrix with the transform mapping its transformed BBox
                    // onto the annotation Rect. The Form retains its Matrix;
                    // this outer transform is the annotation instance.
                    context.save_state();
                    context.pre_concat_affine(appearance.transform);
                    context.push_root_transform();

                    appearance.form.draw(resources, context, device);
                    context.pop_root_transform();
                    context.restore_state(device);
                }
                Ok(None) => match resolve_link_border_with_limit(
                    &annot,
                    context.settings.max_link_quads_per_annotation,
                ) {
                    Ok(Some(border)) if border.is_visible() => {
                        device.draw_link_border(&border, context.root_transform());
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => match resolve_text_markup(&annot, remaining_markup_quads) {
                        Ok(Some(markup)) => {
                            remaining_markup_quads -= markup.quads.len();
                            device.draw_text_markup(&markup, context.root_transform());
                        }
                        Ok(None) => match resolve_geometric_annotation(
                            &annot,
                            &mut geometry_budget,
                            &|| device.is_cancelled(),
                        ) {
                            Ok(Some(appearance)) => device
                                .draw_geometric_annotation(&appearance, context.root_transform()),
                            Ok(None) => {}
                            Err(error) => (context.settings.warning_sink)(
                                InterpreterWarning::GeometricAnnotationFailure(error),
                            ),
                        },
                        Err(error) => (context.settings.warning_sink)(
                            InterpreterWarning::TextMarkupFailure(error),
                        ),
                    },
                    Err(error) => {
                        (context.settings.warning_sink)(InterpreterWarning::LinkBorderFailure(
                            error,
                        ));
                    }
                },
                Err(error) => (context.settings.warning_sink)(
                    InterpreterWarning::AnnotationAppearanceFailure(error),
                ),
            }
            if has_oc && context.ocg_state.end_marked_content() {
                device.end_optional_content();
            }
        }
    }
}

/// Interpret the instructions from `ops` and render them into the device.
pub fn interpret<'a>(
    mut ops: TypedIter<'_>,
    resources: &Resources<'a>,
    context: &mut Context<'a>,
    device: &mut impl Device<'a>,
) {
    let num_states = context.num_states();
    let initial_marked_content_depth = context.ocg_state.marked_content_depth();
    let mut font_dict_cache = FxHashMap::<Name<'a>, Dict<'a>>::default();
    let mut text_object_active = false;
    let mut warned_unmatched_marked_content_end = false;
    let mut warned_unterminated_marked_content = false;
    let mut warned_unmatched_text_object_end = false;
    let mut warned_unterminated_text_object = false;
    let mut warned_nested_text_object = false;

    context.save_state();

    while let Some(op) = ops.next() {
        if device.is_cancelled() {
            break;
        }
        if text_object_active && matches!(&op, TypedInstruction::BeginText(_)) {
            if !warned_nested_text_object {
                (context.settings.warning_sink)(InterpreterWarning::NestedTextObject);
                warned_nested_text_object = true;
            }
            close_text_object(context, device);
            text_object_active = false;
        } else if text_object_active && starts_graphics_object(&op) {
            if !warned_unterminated_text_object {
                (context.settings.warning_sink)(InterpreterWarning::UnterminatedTextObject);
                warned_unterminated_text_object = true;
            }
            close_text_object(context, device);
            text_object_active = false;
        }
        match op {
            TypedInstruction::SaveState(_) => context.save_state(),
            TypedInstruction::StrokeColorDeviceRgb(s) => {
                context.get_mut().graphics_state.stroke_cs = ColorSpace::device_rgb();
                context.get_mut().graphics_state.stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32()];
                context.get_mut().graphics_state.stroke_pattern = None;
            }
            TypedInstruction::StrokeColorDeviceGray(s) => {
                context.get_mut().graphics_state.stroke_cs = ColorSpace::device_gray();
                context.get_mut().graphics_state.stroke_color = smallvec![s.0.as_f32()];
                context.get_mut().graphics_state.stroke_pattern = None;
            }
            TypedInstruction::StrokeColorCmyk(s) => {
                context.get_mut().graphics_state.stroke_cs = ColorSpace::device_cmyk();
                context.get_mut().graphics_state.stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32(), s.3.as_f32()];
                context.get_mut().graphics_state.stroke_pattern = None;
            }
            TypedInstruction::LineWidth(w) => {
                context.get_mut().graphics_state.stroke_props.line_width = w.0.as_f32();
            }
            TypedInstruction::LineCap(c) => {
                context.get_mut().graphics_state.stroke_props.line_cap = convert_line_cap(c);
            }
            TypedInstruction::LineJoin(j) => {
                context.get_mut().graphics_state.stroke_props.line_join = convert_line_join(j);
            }
            TypedInstruction::MiterLimit(l) => {
                context.get_mut().graphics_state.stroke_props.miter_limit = l.0.as_f32();
            }
            TypedInstruction::Transform(t) => {
                context.pre_concat_transform(t);
            }
            TypedInstruction::RectPath(r) => {
                let rect = kurbo::Rect::new(
                    r.0.as_f64(),
                    r.1.as_f64(),
                    r.0.as_f64() + r.2.as_f64(),
                    r.1.as_f64() + r.3.as_f64(),
                )
                .to_path(0.1);
                context.path_mut().extend(rect);
            }
            TypedInstruction::MoveTo(m) => {
                let p = Point::new(m.0.as_f64(), m.1.as_f64());
                *(context.last_point_mut()) = p;
                *(context.sub_path_start_mut()) = p;
                context.path_mut().move_to(p);
            }
            TypedInstruction::FillPathEvenOdd(_) => {
                fill_path(context, device, FillRule::EvenOdd);
            }
            TypedInstruction::FillPathNonZero(_) => {
                fill_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::FillPathNonZeroCompatibility(_) => {
                fill_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::FillAndStrokeEvenOdd(_) => {
                fill_stroke_path(context, device, FillRule::EvenOdd);
            }
            TypedInstruction::FillAndStrokeNonZero(_) => {
                fill_stroke_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::CloseAndStrokePath(_) => {
                close_path(context);
                stroke_path(context, device);
            }
            TypedInstruction::CloseFillAndStrokeEvenOdd(_) => {
                close_path(context);
                fill_stroke_path(context, device, FillRule::EvenOdd);
            }
            TypedInstruction::CloseFillAndStrokeNonZero(_) => {
                close_path(context);
                fill_stroke_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::NonStrokeColorDeviceGray(s) => {
                context.get_mut().graphics_state.none_stroke_cs = ColorSpace::device_gray();
                context.get_mut().graphics_state.non_stroke_color = smallvec![s.0.as_f32()];
                context.get_mut().graphics_state.non_stroke_pattern = None;
            }
            TypedInstruction::NonStrokeColorDeviceRgb(s) => {
                context.get_mut().graphics_state.none_stroke_cs = ColorSpace::device_rgb();
                context.get_mut().graphics_state.non_stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32()];
                context.get_mut().graphics_state.non_stroke_pattern = None;
            }
            TypedInstruction::NonStrokeColorCmyk(s) => {
                context.get_mut().graphics_state.none_stroke_cs = ColorSpace::device_cmyk();
                context.get_mut().graphics_state.non_stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32(), s.3.as_f32()];
                context.get_mut().graphics_state.non_stroke_pattern = None;
            }
            TypedInstruction::LineTo(m) => {
                if !context.path().elements().is_empty() {
                    let last_point = *context.last_point();
                    let mut p = Point::new(m.0.as_f64(), m.1.as_f64());
                    *(context.last_point_mut()) = p;
                    if last_point == p {
                        // Add a small delta so that zero width lines can still have a round stroke.
                        p.x += 0.0001;
                    }

                    context.path_mut().line_to(p);
                }
            }
            TypedInstruction::CubicTo(c) => {
                if !context.path().elements().is_empty() {
                    let p1 = Point::new(c.0.as_f64(), c.1.as_f64());
                    let p2 = Point::new(c.2.as_f64(), c.3.as_f64());
                    let p3 = Point::new(c.4.as_f64(), c.5.as_f64());

                    *(context.last_point_mut()) = p3;

                    context.path_mut().curve_to(p1, p2, p3);
                }
            }
            TypedInstruction::CubicStartTo(c) => {
                if !context.path().elements().is_empty() {
                    let p1 = *context.last_point();
                    let p2 = Point::new(c.0.as_f64(), c.1.as_f64());
                    let p3 = Point::new(c.2.as_f64(), c.3.as_f64());

                    *(context.last_point_mut()) = p3;

                    context.path_mut().curve_to(p1, p2, p3);
                }
            }
            TypedInstruction::CubicEndTo(c) => {
                if !context.path().elements().is_empty() {
                    let p2 = Point::new(c.0.as_f64(), c.1.as_f64());
                    let p3 = Point::new(c.2.as_f64(), c.3.as_f64());

                    *(context.last_point_mut()) = p3;

                    context.path_mut().curve_to(p2, p3, p3);
                }
            }
            TypedInstruction::ClosePath(_) => {
                close_path(context);
            }
            TypedInstruction::SetGraphicsState(gs) => {
                if let Some(gs) = resources
                    .get_ext_g_state(gs.0)
                    .warn_none(&format!("failed to get extgstate {}", gs.0.as_str()))
                {
                    handle_gs(&gs, context, resources);
                }
            }
            TypedInstruction::StrokePath(_) => {
                stroke_path(context, device);
            }
            TypedInstruction::EndPath(_) => {
                if let Some(clip) = *context.clip()
                    && !context.path().elements().is_empty()
                {
                    let clip_path = context.get().ctm * context.path().clone();
                    context.push_clip_path(clip_path, clip, device);

                    *(context.clip_mut()) = None;
                }

                context.path_mut().truncate(0);
            }
            TypedInstruction::NonStrokeColor(c) => {
                let gs = &mut context.get_mut().graphics_state;
                gs.non_stroke_color = c.0.into_iter().map(|n| n.as_f32()).collect();
                gs.non_stroke_pattern = None;
            }
            TypedInstruction::StrokeColor(c) => {
                let gs = &mut context.get_mut().graphics_state;
                gs.stroke_color = c.0.into_iter().map(|n| n.as_f32()).collect();
                gs.stroke_pattern = None;
            }
            TypedInstruction::ClipNonZero(_) => {
                *(context.clip_mut()) = Some(FillRule::NonZero);
            }
            TypedInstruction::ClipEvenOdd(_) => {
                *(context.clip_mut()) = Some(FillRule::EvenOdd);
            }
            TypedInstruction::RestoreState(_) => context.restore_state(device),
            TypedInstruction::FlatnessTolerance(_) => {
                // Ignore for now.
            }
            TypedInstruction::ColorSpaceStroke(c) => {
                let cs = if let Some(named) = ColorSpace::new_from_name(c.0) {
                    named
                } else {
                    context
                        .get_color_space(resources, c.0)
                        .unwrap_or(ColorSpace::device_gray())
                };

                if !cs.is_pattern() {
                    context.get_mut().graphics_state.stroke_pattern = None;
                }
                context.get_mut().graphics_state.stroke_color = cs.initial_color();
                context.get_mut().graphics_state.stroke_cs = cs;
            }
            TypedInstruction::ColorSpaceNonStroke(c) => {
                let cs = if let Some(named) = ColorSpace::new_from_name(c.0) {
                    named
                } else {
                    context
                        .get_color_space(resources, c.0)
                        .unwrap_or(ColorSpace::device_gray())
                };

                if !cs.is_pattern() {
                    context.get_mut().graphics_state.non_stroke_pattern = None;
                }
                context.get_mut().graphics_state.non_stroke_color = cs.initial_color();
                context.get_mut().graphics_state.none_stroke_cs = cs;
            }
            TypedInstruction::DashPattern(p) => {
                context.get_mut().graphics_state.stroke_props.dash_offset = p.1.as_f32();
                // kurbo apparently cannot properly deal with offsets that are exactly 0.
                context.get_mut().graphics_state.stroke_props.dash_array =
                    p.0.iter::<f32>()
                        .map(|n| if n == 0.0 { 0.01 } else { n })
                        .collect();
            }
            TypedInstruction::RenderingIntent(value) => {
                let (intent, unknown) = crate::color::RenderingIntent::from_name(value.0);
                context.get_mut().graphics_state.rendering_intent = intent;
                if unknown {
                    (context.settings.warning_sink)(InterpreterWarning::RenderingIntentUnknown);
                }
            }
            TypedInstruction::NonStrokeColorNamed(n) => {
                context.get_mut().graphics_state.non_stroke_color =
                    n.0.into_iter().map(|n| n.as_f32()).collect();
                context.get_mut().graphics_state.non_stroke_pattern = n.1.and_then(|name| {
                    resources
                        .get_pattern(name)
                        .and_then(|d| Pattern::new(d, context, resources))
                });
            }
            TypedInstruction::StrokeColorNamed(n) => {
                context.get_mut().graphics_state.stroke_color =
                    n.0.into_iter().map(|n| n.as_f32()).collect();
                context.get_mut().graphics_state.stroke_pattern = n.1.and_then(|name| {
                    resources
                        .get_pattern(name)
                        .and_then(|d| Pattern::new(d, context, resources))
                });
            }
            TypedInstruction::BeginMarkedContentWithProperties(bdc) => {
                // Properties can be either:
                // 1. A Name that references an entry in the Resources/Properties dictionary
                // 2. An inline dictionary with an OC key

                let property_name = bdc.1.clone().into_name();
                let property_ref = property_name
                    .as_ref()
                    .and_then(|name| resources.properties.get_ref(name.as_ref()))
                    .map(ObjectIdentifier::from);
                let resolved_properties = property_name
                    .as_ref()
                    .and_then(|name| resources.properties.get::<Dict<'_>>(name.as_ref()))
                    .or_else(|| dict_or_stream(bdc.1).map(|(props, _)| props.clone()));

                let mut marked_properties = resolved_properties
                    .as_ref()
                    .map(|properties| {
                        marked_content_properties(
                            properties,
                            context.settings.max_marked_content_metadata_bytes,
                            context.settings.max_marked_content_property_bytes,
                            context.settings.max_marked_content_property_depth,
                        )
                    })
                    .unwrap_or_default();
                marked_properties.property_list_name =
                    property_name.as_ref().map(|name| name.as_ref().to_vec());
                marked_properties.property_list_resolved = resolved_properties.is_some();

                device.begin_marked_content_with_properties(bdc.0, marked_properties);
                let membership = resolved_properties.as_ref().and_then(|props| {
                    if let Some(dict) = props.get::<Dict<'_>>(OC) {
                        Some((dict, props.get_ref(OC).map(ObjectIdentifier::from)))
                    } else if props
                        .get::<Name<'_>>(TYPE)
                        .is_some_and(|kind| kind.as_ref() == OCG || kind.as_ref() == OCMD)
                    {
                        Some((props.clone(), property_ref))
                    } else {
                        None
                    }
                });
                let has_membership = membership.is_some();
                let optional_expression = match membership {
                    Some((dict, Some(reference))) => {
                        context.ocg_state.begin_ocg(&dict, reference, context.xref)
                    }
                    Some((dict, None))
                        if dict
                            .get::<Name<'_>>(TYPE)
                            .is_some_and(|kind| kind.as_ref() == OCMD) =>
                    {
                        context.ocg_state.begin_ocmd(&dict, context.xref)
                    }
                    Some(_) => {
                        context.ocg_state.begin_unresolved_optional_content();
                        None
                    }
                    None => {
                        context.ocg_state.begin_marked_content();
                        None
                    }
                };
                if context.settings.preserve_optional_content {
                    if let Some(expression) = optional_expression.as_ref() {
                        device.begin_optional_content(expression);
                    } else if has_membership {
                        (context.settings.warning_sink)(
                            InterpreterWarning::OptionalContentExpressionFailure,
                        );
                    }
                }
            }
            TypedInstruction::MarkedContentPointWithProperties(_) => {}
            TypedInstruction::EndMarkedContent(_) => {
                if context.ocg_state.marked_content_depth() > initial_marked_content_depth {
                    if context.ocg_state.end_marked_content() {
                        device.end_optional_content();
                    }
                    device.end_marked_content();
                } else if !warned_unmatched_marked_content_end {
                    (context.settings.warning_sink)(InterpreterWarning::UnmatchedMarkedContentEnd);
                    warned_unmatched_marked_content_end = true;
                }
            }
            TypedInstruction::MarkedContentPoint(_) => {}
            TypedInstruction::BeginMarkedContent(bmc) => {
                context.ocg_state.begin_marked_content();
                device.begin_marked_content(bmc.0, None);
            }
            TypedInstruction::BeginText(_) => {
                context.get_mut().text_state.text_matrix = Affine::IDENTITY;
                context.get_mut().text_state.text_line_matrix = Affine::IDENTITY;
                device.begin_text_object(context.get().graphics_state.text_knockout);
                text_object_active = true;
            }
            TypedInstruction::SetTextMatrix(m) => {
                let m = Affine::new([
                    m.0.as_f64(),
                    m.1.as_f64(),
                    m.2.as_f64(),
                    m.3.as_f64(),
                    m.4.as_f64(),
                    m.5.as_f64(),
                ]);
                context.get_mut().text_state.text_line_matrix = m;
                context.get_mut().text_state.text_matrix = m;
            }
            TypedInstruction::EndText(_) => {
                if text_object_active {
                    close_text_object(context, device);
                    text_object_active = false;
                } else if !warned_unmatched_text_object_end {
                    (context.settings.warning_sink)(InterpreterWarning::UnmatchedTextObjectEnd);
                    warned_unmatched_text_object_end = true;
                }
            }
            TypedInstruction::TextFont(t) => {
                let name = t.0;

                // In case we are unable to resolve the font, two scenarios:
                // 1) If the font doesn't exist in the first place in the resource dictionary,
                // assume Helvetica (this seems to be what other PDF viewers do).
                // 2) In case it's `None` because we were unable to resolve the font
                // (for whatever reason), leave it as `None`. Better showing no
                // text at all than garbage text.
                let font = if let Some(font_dict) = font_dict_cache.get(name).cloned() {
                    context.resolve_font(&font_dict)
                } else if let Some(font_dict) = resources.get_font(name) {
                    font_dict_cache.insert(name.clone(), font_dict.clone());
                    context.resolve_font(&font_dict)
                } else {
                    Font::new_standard(StandardFont::Helvetica, &context.settings.font_resolver)
                        .map(TextStateFont::Fallback)
                };

                context.get_mut().text_state.font_size = t.1.as_f32();
                context.get_mut().text_state.font = font;
            }
            TypedInstruction::ShowText(s) => {
                if context.get().text_state.font.is_none() {
                    // Even if no explicit font was set, we try to assume Helvetica. Acrobat
                    // seems to do the same.
                    context.get_mut().text_state.font = Font::new_standard(
                        StandardFont::Helvetica,
                        &context.settings.font_resolver,
                    )
                    .map(TextStateFont::Fallback);
                }

                text::show_text_string(context, device, resources, s.0);
            }
            TypedInstruction::ShowTexts(s) => {
                if context.get().text_state.font.is_none() {
                    // Even if no explicit font was set, we try to assume Helvetica. Acrobat
                    // seems to do the same.
                    context.get_mut().text_state.font = Font::new_standard(
                        StandardFont::Helvetica,
                        &context.settings.font_resolver,
                    )
                    .map(TextStateFont::Fallback);
                }

                text::begin_glyph_run(context);
                for obj in s.0.iter::<Object<'_>>() {
                    match obj {
                        Object::Number(num) => {
                            text::apply_glyph_run_adjustment(context, num.as_f32());
                        }
                        Object::String(text) => {
                            text::append_text_string(context, resources, &text);
                        }
                        _ => {}
                    }
                }
                text::show_glyph_run(context, device);
            }
            TypedInstruction::HorizontalScaling(h) => {
                context.get_mut().text_state.horizontal_scaling = h.0.as_f32();
            }
            TypedInstruction::TextLeading(tl) => {
                context.get_mut().text_state.leading = tl.0.as_f32();
            }
            TypedInstruction::CharacterSpacing(c) => {
                context.get_mut().text_state.char_space = c.0.as_f32();
            }
            TypedInstruction::WordSpacing(w) => {
                context.get_mut().text_state.word_space = w.0.as_f32();
            }
            TypedInstruction::NextLine(n) => {
                let (tx, ty) = (n.0.as_f64(), n.1.as_f64());
                text::next_line(context, tx, ty);
            }
            TypedInstruction::NextLineUsingLeading(_) => {
                text::next_line(context, 0.0, -context.get().text_state.leading as f64);
            }
            TypedInstruction::NextLineAndShowText(n) => {
                text::next_line(context, 0.0, -context.get().text_state.leading as f64);
                text::show_text_string(context, device, resources, n.0);
            }
            TypedInstruction::TextRenderingMode(r) => {
                let mode = match r.0.as_i64() {
                    0 => TextRenderingMode::Fill,
                    1 => TextRenderingMode::Stroke,
                    2 => TextRenderingMode::FillStroke,
                    3 => TextRenderingMode::Invisible,
                    4 => TextRenderingMode::FillAndClip,
                    5 => TextRenderingMode::StrokeAndClip,
                    6 => TextRenderingMode::FillAndStrokeAndClip,
                    7 => TextRenderingMode::Clip,
                    _ => {
                        warn!("unknown text rendering mode {}", r.0.as_i64());

                        TextRenderingMode::Fill
                    }
                };

                context.get_mut().text_state.render_mode = mode;
            }
            TypedInstruction::NextLineAndSetLeading(n) => {
                let (tx, ty) = (n.0.as_f64(), n.1.as_f64());
                context.get_mut().text_state.leading = -ty as f32;
                text::next_line(context, tx, ty);
            }
            TypedInstruction::ShapeGlyph(_) => {}
            TypedInstruction::XObject(x) => {
                let cache = context.interpreter_cache.object_cache.clone();
                let warning_sink = context.settings.warning_sink.clone();
                let transfer_function = context.get().graphics_state.transfer_function.clone();
                if let Some(x_object) = resources.get_x_object(x.0).and_then(|s| {
                    XObject::new(
                        &s,
                        resources,
                        &warning_sink,
                        &cache,
                        transfer_function.clone(),
                    )
                }) {
                    x_object.draw(resources, context, device);
                }
            }
            TypedInstruction::InlineImage(i) => {
                let warning_sink = context.settings.warning_sink.clone();
                let transfer_function = context.get().graphics_state.transfer_function.clone();
                let cache = context.interpreter_cache.object_cache.clone();
                if let Some(x_object) =
                    ImageXObject::new(i.0, resources, &warning_sink, &cache, transfer_function)
                {
                    x_object.draw(context, device);
                }
            }
            TypedInstruction::TextRise(t) => {
                context.get_mut().text_state.rise = t.0.as_f32();
            }
            TypedInstruction::Shading(s) => {
                if !context.ocg_state.is_visible() {
                    continue;
                }

                let transfer_function = context.get().graphics_state.transfer_function.clone();

                if let Some(sp) = resources
                    .get_shading(s.0)
                    .and_then(|o| {
                        let (dict, stream) = dict_or_stream(&o)?;
                        Shading::new(
                            dict,
                            stream,
                            &context.interpreter_cache.object_cache,
                            &context.settings.warning_sink,
                        )
                    })
                    .map(|s| {
                        Pattern::Shading(ShadingPattern {
                            shading: Arc::new(s),
                            matrix: Affine::IDENTITY,
                            opacity: context.get().graphics_state.non_stroke_alpha,
                            transfer_function: transfer_function.clone(),
                            background_applies: false,
                        })
                    })
                {
                    context.save_state();
                    context.push_root_transform();
                    let st = context.get_mut();
                    st.graphics_state.non_stroke_pattern = Some(sp);
                    st.graphics_state.none_stroke_cs = ColorSpace::pattern();

                    let bbox = context.bbox().to_path(0.1);
                    let inverted_bbox = context.get().ctm.inverse() * bbox;
                    fill_path_impl(context, device, FillRule::NonZero, Some(&inverted_bbox));

                    context.pop_root_transform();
                    context.restore_state(device);
                } else {
                    warn!("failed to process shading");
                }
            }
            TypedInstruction::BeginCompatibility(_) => {}
            TypedInstruction::EndCompatibility(_) => {}
            TypedInstruction::ColorGlyph(_) => {}
            TypedInstruction::ShowTextWithParameters(t) => {
                context.get_mut().text_state.word_space = t.0.as_f32();
                context.get_mut().text_state.char_space = t.1.as_f32();
                text::next_line(context, 0.0, -context.get().text_state.leading as f64);
                text::show_text_string(context, device, resources, t.2);
            }
            TypedInstruction::Fallback(operator) if &**operator == b"ri" => {
                (context.settings.warning_sink)(InterpreterWarning::ColorConversion(
                    crate::color::ColorConversionError::InvalidIntent,
                ));
            }
            _ => {
                warn!("failed to read an operator");
            }
        }
    }

    let cancelled = device.is_cancelled();
    if text_object_active {
        if !cancelled && !warned_unterminated_text_object {
            (context.settings.warning_sink)(InterpreterWarning::UnterminatedTextObject);
        }
        close_text_object(context, device);
    }
    while context.ocg_state.marked_content_depth() > initial_marked_content_depth {
        if !cancelled && !warned_unterminated_marked_content {
            (context.settings.warning_sink)(InterpreterWarning::UnterminatedMarkedContent);
            warned_unterminated_marked_content = true;
        }
        if context.ocg_state.end_marked_content() {
            device.end_optional_content();
        }
        device.end_marked_content();
    }

    while context.num_states() > num_states {
        context.restore_state(device);
    }
}

fn close_text_object<'a>(context: &mut Context<'a>, device: &mut impl Device<'a>) {
    let has_outline = context
        .get()
        .text_state
        .clip_paths
        .segments()
        .next()
        .is_some();

    // The text object's implicit knockout group contains only its glyph
    // paints. A clipping text mode takes effect after the text object has been
    // evaluated.
    device.end_text_object();

    if has_outline {
        let clip_path = context.get().ctm * context.get().text_state.clip_paths.clone();
        context.push_clip_path(clip_path, FillRule::NonZero, device);
    }

    context.get_mut().text_state.clip_paths.truncate(0);
}

fn starts_graphics_object(instruction: &TypedInstruction<'_, '_>) -> bool {
    matches!(
        instruction,
        TypedInstruction::SaveState(_)
            | TypedInstruction::RestoreState(_)
            | TypedInstruction::Transform(_)
            | TypedInstruction::MoveTo(_)
            | TypedInstruction::LineTo(_)
            | TypedInstruction::CubicTo(_)
            | TypedInstruction::CubicStartTo(_)
            | TypedInstruction::CubicEndTo(_)
            | TypedInstruction::RectPath(_)
            | TypedInstruction::ClosePath(_)
            | TypedInstruction::StrokePath(_)
            | TypedInstruction::CloseAndStrokePath(_)
            | TypedInstruction::FillPathNonZero(_)
            | TypedInstruction::FillPathEvenOdd(_)
            | TypedInstruction::FillPathNonZeroCompatibility(_)
            | TypedInstruction::FillAndStrokeNonZero(_)
            | TypedInstruction::FillAndStrokeEvenOdd(_)
            | TypedInstruction::CloseFillAndStrokeNonZero(_)
            | TypedInstruction::CloseFillAndStrokeEvenOdd(_)
            | TypedInstruction::EndPath(_)
            | TypedInstruction::ClipNonZero(_)
            | TypedInstruction::ClipEvenOdd(_)
            | TypedInstruction::XObject(_)
            | TypedInstruction::InlineImage(_)
            | TypedInstruction::Shading(_)
            | TypedInstruction::ShapeGlyph(_)
            | TypedInstruction::ColorGlyph(_)
    )
}
