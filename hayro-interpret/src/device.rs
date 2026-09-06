use crate::font::GlyphRun;
use crate::x_object::soft_mask::SoftMask;
use crate::{BlendMode, ClipPath, FillRule, Image};
use crate::{DrawMode, DrawProps, ImageDrawProps};
use crate::{FormInvocation, LinkBorder, TextMarkup};
use kurbo::{Affine, BezPath, Rect, Shape};

/// Owned metadata stream attached to a marked-content property list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkedContentMetadata {
    /// Raw `/Subtype` name. The currently proven subset is `XML`.
    pub subtype: Vec<u8>,
    /// Decoded, decrypted metadata stream bytes.
    pub data: Vec<u8>,
}

/// One additional entry retained from a marked-content property dictionary.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkedContentProperty {
    /// Raw, decoded PDF name bytes used as the dictionary key.
    pub key: Vec<u8>,
    /// Resolved semantic value of the entry.
    pub value: MarkedContentPropertyValue,
}

/// A bounded, lifetime-independent PDF value retained for producer-defined
/// marked-content properties.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkedContentPropertyValue {
    /// The PDF null object.
    Null,
    /// A PDF boolean.
    Boolean(bool),
    /// A PDF integer preserved without floating-point conversion.
    Integer(i64),
    /// A finite PDF real number.
    Real(f64),
    /// Decrypted PDF string bytes.
    String(Vec<u8>),
    /// Raw, decoded PDF name bytes.
    Name(Vec<u8>),
    /// An ordered PDF array whose indirect entries have been resolved.
    Array(Vec<Self>),
    /// A key-sorted PDF dictionary whose indirect entries have been resolved.
    Dictionary(Vec<MarkedContentProperty>),
}

/// Resolved properties attached to a marked-content sequence.
///
/// Text strings remain as decoded PDF-string bytes. Consumers can apply the
/// PDF text-string encoding rules without retaining syntax-layer lifetimes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkedContentProperties {
    /// Raw name of a named property-list resource used by BDC.
    pub property_list_name: Option<Vec<u8>>,
    /// Whether the named or inline property-list operand resolved to a
    /// dictionary. A missing named resource remains observable to retaining
    /// devices so callers can apply and diagnose deterministic repair.
    pub property_list_resolved: bool,
    /// Marked-content identifier from the properties dictionary.
    pub mcid: Option<i32>,
    /// Replacement text used by accessibility and text extraction.
    pub actual_text: Option<Vec<u8>>,
    /// Alternate description associated with the marked content.
    pub alternate_text: Option<Vec<u8>>,
    /// Language override associated with the marked content.
    pub language: Option<Vec<u8>>,
    /// Raw `/Type` name from the properties dictionary.
    pub property_type: Option<Vec<u8>>,
    /// Raw `/Subtype` name identifying the application or extension that
    /// defines the property list.
    pub property_subtype: Option<Vec<u8>>,
    /// Raw `/Name` text string from the properties dictionary.
    pub name: Option<Vec<u8>>,
    /// Raw attribute-owner `/O` name, when present.
    pub owner: Option<Vec<u8>>,
    /// Finite four-number `/BBox` property in its declared coordinate space.
    pub bounding_box: Option<[f64; 4]>,
    /// Exact decoded XML metadata attached to the marked-content sequence.
    pub metadata: Option<MarkedContentMetadata>,
    /// Additional producer-defined entries, sorted by raw PDF name bytes.
    pub additional_properties: Vec<MarkedContentProperty>,
    /// Property keys that were present but could not be exposed exactly.
    pub unavailable_keys: Vec<Vec<u8>>,
}

/// Stable identity and document-default visibility of one optional-content group.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OptionalContentGroup {
    /// PDF object number of the indirect OCG dictionary.
    pub object_number: i32,
    /// PDF generation number of the indirect OCG dictionary.
    pub generation_number: i32,
    /// Whether the document's default optional-content configuration enables the group.
    pub initially_visible: bool,
}

/// Owned boolean visibility expression for an optional-content scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionalContentExpression {
    /// Visibility follows one optional-content group.
    Group(OptionalContentGroup),
    /// Every operand must be visible. An empty conjunction is visible.
    All(Vec<Self>),
    /// At least one operand must be visible. An empty disjunction is hidden.
    Any(Vec<Self>),
    /// Invert one operand.
    Not(Box<Self>),
}

/// A trait for a device that can be used to process PDF drawing instructions.
pub trait Device<'a> {
    /// Draw a path.
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode);
    /// Push a new clip path to the clip stack.
    fn push_clip_path(&mut self, clip_path: &ClipPath);
    /// Push a rectangular clip to the clip stack.
    fn push_clip_rect(&mut self, rect: &Rect) {
        self.push_clip_path(&ClipPath {
            path: rect.to_path(0.1),
            fill: FillRule::NonZero,
        });
    }
    /// Push a new transparency group to the blend stack.
    fn push_transparency_group(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'a>>,
        blend_mode: BlendMode,
    );
    /// Push a transparency group while preserving the PDF alpha-source flag.
    ///
    /// The default keeps existing devices source-compatible. Retaining devices
    /// should override this callback because `/AIS true` changes alpha
    /// constants and soft masks from opacity inputs into shape inputs.
    fn push_transparency_group_with_alpha_source(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'a>>,
        blend_mode: BlendMode,
        _alpha_is_shape: bool,
    ) {
        self.push_transparency_group(opacity, mask, blend_mode);
    }
    /// Draw a run of positioned glyphs.
    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        draw_mode: &DrawMode,
    );
    /// Begin one PDF text object (`BT`).
    ///
    /// `text_knockout` is the graphics-state `/TK` value captured at the text
    /// object's boundary. When true, every glyph in the matching text object
    /// forms one implicit non-isolated knockout group.
    fn begin_text_object(&mut self, _text_knockout: bool) {}
    /// End the current PDF text object (`ET`).
    fn end_text_object(&mut self) {}
    /// Begin the implicit graphics object shared by a combined fill-and-stroke
    /// path or glyph.
    ///
    /// PDF transparency treats the fill and stroke as one non-isolated
    /// knockout group, even though devices still receive the two paints
    /// separately so their colors and alpha constants remain independent.
    fn begin_combined_fill_stroke(&mut self) {}
    /// End a combined fill-and-stroke graphics object.
    fn end_combined_fill_stroke(&mut self) {}
    /// Record one semantic PDF text run before its visual paint callbacks.
    ///
    /// This is called exactly once for each interpreted text run, including
    /// invisible and clipping-only text. A fill-and-stroke run can produce two
    /// [`Self::draw_glyph_run`] calls, so text extraction must use this callback
    /// instead of inferring semantic runs from paint operations.
    fn record_glyph_run(
        &mut self,
        _glyph_run: &GlyphRun<'_, 'a>,
        _transform: Affine,
        _visible: bool,
    ) {
    }
    /// Draw an image.
    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>);
    /// Draw one Form `XObject` invocation.
    ///
    /// The default preserves Hayro's immediate interpretation behavior. Devices
    /// that retain scene structure can instead interpret the invocation in its
    /// normalized local coordinate system and instance it at
    /// [`FormInvocation::instance_transform`].
    fn draw_form(&mut self, form: &FormInvocation<'a>)
    where
        Self: Sized,
    {
        form.interpret(self);
    }
    /// Draw a synthesized Link annotation border at its exact position in the
    /// page annotation array.
    ///
    /// The border geometry and stroke units remain in PDF default user space;
    /// `transform` maps that space into the device's page coordinate system.
    /// An explicit annotation appearance stream takes precedence, so this
    /// callback is emitted only when no `/AP` entry is present.
    fn draw_link_border(&mut self, _border: &LinkBorder, _transform: Affine) {}
    /// Draw text markup without an explicit appearance at its position in the
    /// annotation array. Geometry remains in PDF default user space, mapped
    /// to page coordinates by `transform`. Existing devices retain their
    /// previous behaviour through the default no-op.
    fn draw_text_markup(&mut self, _markup: &TextMarkup, _transform: Affine) {}
    /// Draw a bounded Square, Circle or Ink default appearance at its exact
    /// annotation-array position. Coordinates are mapped by `transform`.
    /// Existing devices keep their previous behavior through this default.
    fn draw_geometric_annotation(
        &mut self,
        _appearance: &crate::GeometricAnnotation,
        _transform: Affine,
    ) {
    }
    /// Optional host remainder before allocating annotation geometry. This
    /// intersects the page synthesis budget with already retained content.
    fn remaining_annotation_geometry_budget(&self) -> Option<crate::AnnotationGeometryBudget> {
        None
    }
    /// Pop the last clip path or clip rectangle from the clip stack.
    fn pop_clip(&mut self);
    /// Pop the last transparency group from the blend stack.
    fn pop_transparency_group(&mut self);
    /// Draw a rectangle directly, without going through the general path pipeline.
    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, draw_mode: &DrawMode) {
        self.draw_path(&rect.to_path(0.1), props, draw_mode);
    }
    /// Called at the beginning of a marked content sequence (BMC/BDC).
    ///
    /// The tag is the marked content tag (e.g. b"P", b"Span"). The mcid is
    /// the marked content identifier from the properties dict, if present.
    fn begin_marked_content(&mut self, _tag: &[u8], _mcid: Option<i32>) {}
    /// Called for BDC sequences after resolving inline or named properties.
    ///
    /// The default preserves compatibility with devices that only consume an
    /// MCID. New devices should override this method to retain rich semantic
    /// properties such as `ActualText`.
    fn begin_marked_content_with_properties(
        &mut self,
        tag: &[u8],
        properties: MarkedContentProperties,
    ) {
        self.begin_marked_content(tag, properties.mcid);
    }
    /// Called before content controlled by an OCG or OCMD visibility expression.
    ///
    /// This callback is emitted only when
    /// [`InterpreterSettings::preserve_optional_content`](crate::InterpreterSettings::preserve_optional_content)
    /// is enabled. It may nest inside a marked-content callback or directly
    /// surround an image or Form `XObject` invocation.
    fn begin_optional_content(&mut self, _expression: &OptionalContentExpression) {}
    /// Called after a matching [`Self::begin_optional_content`] callback.
    fn end_optional_content(&mut self) {}
    /// Called at the end of a marked content sequence (EMC).
    fn end_marked_content(&mut self) {}
    /// Return true to stop interpretation at the next operator boundary.
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// A device that discards all drawing operations.
pub struct DummyDevice;

impl Device<'_> for DummyDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'_>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'_>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, '_>, _: DrawProps<'_>, _: &DrawMode) {}
    fn draw_image(&mut self, _: Image<'_, '_>, _: ImageDrawProps<'_>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}
