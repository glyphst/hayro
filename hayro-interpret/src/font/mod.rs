//! Interacting with the different kinds of PDF fonts.

use crate::context::Context;
use crate::context::InterpreterCache;
use crate::device::Device;
use crate::font::cid::Type0Font;
use crate::font::generated::{
    glyph_names, mac_expert, mac_os_roman, mac_roman, standard, win_ansi,
};
use crate::font::true_type::TrueTypeFont;
use crate::font::type1::Type1Font;
use crate::font::type3::Type3;
use crate::interpret::state::State;
use crate::{CMapResolverFn, CacheKey, FontResolverFn, InterpreterSettings, Paint};
use bitflags::bitflags;
use hayro_syntax::object::Name;
use hayro_syntax::object::dict::keys::SUBTYPE;
use hayro_syntax::object::dict::keys::*;
use hayro_syntax::object::{Dict, Rect, Stream};
use hayro_syntax::page::Resources;
use hayro_syntax::xref::XRef;
use kurbo::{Affine, BezPath, Vec2};
use outline::OutlineFont;
use skrifa::GlyphId;
use std::borrow::Cow;
use std::fmt::Debug;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;

mod blob;
mod cid;
mod generated;
mod glyph_simulator;
pub(crate) mod outline;
mod standard_font;
mod true_type;
mod type1;
pub(crate) mod type3;

pub(crate) const UNITS_PER_EM: f32 = 1000.0;

/// Resolve nominal vertical text metrics in PDF's 1,000-unit glyph space.
///
/// Explicit descriptor ascent/descent values describe the source font and take
/// precedence. Metrics from the selected font program are the next-best proven
/// source, while the descriptor font box is a conservative final source. No
/// source is clamped or synthesized when its values are invalid.
pub(crate) fn resolve_font_text_metrics(
    descriptor: &Dict<'_>,
    font_metrics: Option<(f32, f32)>,
) -> Option<(f32, f32)> {
    descriptor
        .get::<f32>(ASCENT)
        .zip(descriptor.get::<f32>(DESCENT))
        .and_then(|(ascent, descent)| validated_font_text_metrics(ascent, descent))
        .or_else(|| {
            font_metrics.and_then(|(ascent, descent)| validated_font_text_metrics(ascent, descent))
        })
        .or_else(|| {
            let bounds = descriptor.get::<Rect>(FONT_BBOX)?;
            validated_font_text_metrics(bounds.y1 as f32, bounds.y0 as f32)
        })
}

fn validated_font_text_metrics(ascent: f32, descent: f32) -> Option<(f32, f32)> {
    (ascent.is_finite()
        && descent.is_finite()
        && ascent > 0.0
        && descent <= 0.0
        && ascent > descent)
        .then_some((ascent, descent))
}

#[cfg(test)]
mod text_metrics_tests {
    use super::{resolve_font_text_metrics, validated_font_text_metrics};
    use hayro_syntax::object::{Dict, FromBytes};

    #[test]
    fn descriptor_ascent_and_descent_take_precedence() {
        let descriptor =
            Dict::from_bytes(b"<< /Ascent 894 /Descent -246 /FontBBox [0 -10 711 698] >>").unwrap();

        assert_eq!(
            resolve_font_text_metrics(&descriptor, Some((700.0, -200.0))),
            Some((894.0, -246.0))
        );
    }

    #[test]
    fn font_program_metrics_precede_the_descriptor_box() {
        let descriptor = Dict::from_bytes(b"<< /FontBBox [0 -10 711 698] >>").unwrap();

        assert_eq!(
            resolve_font_text_metrics(&descriptor, Some((700.0, -200.0))),
            Some((700.0, -200.0))
        );
    }

    #[test]
    fn descriptor_box_is_a_conservative_final_source() {
        let descriptor = Dict::from_bytes(b"<< /FontBBox [0 -10 711 698] >>").unwrap();

        assert_eq!(
            resolve_font_text_metrics(&descriptor, None),
            Some((698.0, -10.0))
        );
    }

    #[test]
    fn invalid_sources_are_rejected_without_synthesis() {
        let descriptor =
            Dict::from_bytes(b"<< /Ascent -20 /Descent 40 /FontBBox [0 10 711 10] >>").unwrap();

        assert_eq!(resolve_font_text_metrics(&descriptor, None), None);
        assert_eq!(validated_font_text_metrics(f32::INFINITY, -200.0), None);
        assert_eq!(validated_font_text_metrics(700.0, f32::NAN), None);
        assert_eq!(validated_font_text_metrics(700.0, 20.0), None);
        assert_eq!(validated_font_text_metrics(0.0, -200.0), None);
    }

    #[test]
    fn invalid_descriptor_pair_falls_through_to_font_metrics() {
        let descriptor =
            Dict::from_bytes(b"<< /Ascent 0 /Descent 0 /FontBBox [0 -10 711 698] >>").unwrap();

        assert_eq!(
            resolve_font_text_metrics(&descriptor, Some((700.0, -200.0))),
            Some((700.0, -200.0))
        );
    }
}

pub(crate) fn stretch_glyph(path: BezPath, expected_width: f32, actual_width: f32) -> BezPath {
    if actual_width != 0.0 && actual_width != expected_width {
        let stretch_factor = expected_width / actual_width;
        Affine::scale_non_uniform(stretch_factor as f64, 1.0) * path
    } else {
        path
    }
}

/// A container for the bytes of a PDF file.
pub type FontData = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// Strip the 6-character subset prefix from a PostScript font name.
///
/// PDF subset fonts use names like "ABCDEF+TimesNewRoman". This function
/// returns `TimesNewRoman` from such a name, or the original name if no
/// valid prefix is found.
pub(crate) fn strip_subset_prefix(name: &str) -> &str {
    match name.split_once('+') {
        Some((prefix, rest)) if prefix.len() == 6 => rest,
        _ => name,
    }
}

use crate::util::hash128;
use hayro_cmap::{BfString, CMap, CMapName, CharacterCollection};
pub use outline::OutlineFontData;
pub use standard_font::StandardFont;

/// Direction used by a PDF font's writing mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritingMode {
    /// Glyph advances run along the text-space x axis.
    Horizontal,
    /// Glyph advances run along the text-space y axis.
    Vertical,
}

/// Owned metrics required to reconstruct reader text geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextGlyphMetrics {
    /// Advance in the font's 1,000-unit glyph space.
    pub advance: [f32; 2],
    /// Point in the positioned glyph's 1,000-unit coordinate system that maps
    /// back to the current writing origin.
    ///
    /// This is `[0, 0]` for horizontal writing. For a vertical `CIDFont` it is
    /// the exact `v1` position vector from `W2`, or the specification-defined
    /// `DW2`/horizontal-width default. The glyph transform already contains
    /// the inverse displacement, so downstream readers use this point to
    /// recover the logical baseline without applying `v1` twice.
    pub writing_origin: [f32; 2],
    /// Nominal ascent in the font's 1,000-unit glyph space.
    pub ascent: Option<f32>,
    /// Nominal descent in the font's 1,000-unit glyph space.
    pub descent: Option<f32>,
    /// Horizontal or vertical writing mode.
    pub writing_mode: WritingMode,
    /// Font weight when it can be determined without guessing.
    pub weight: Option<u32>,
    /// Italic/oblique metadata when it can be determined without guessing.
    pub is_italic: Option<bool>,
}

/// A glyph that can be drawn.
pub enum Glyph<'a> {
    /// A glyph defined by an outline.
    Outline(OutlineGlyph),
    /// A type3 glyph, defined by PDF drawing instructions.
    Type3(Box<Type3Glyph<'a>>),
}

impl Glyph<'_> {
    /// Returns the Unicode code point for this glyph, if available.
    ///
    /// This method attempts to determine the Unicode character that this glyph
    /// represents. The exact fallback chain depends on the font type:
    ///
    /// **For Outline Fonts (Type1, TrueType, CFF):**
    /// 1. `ToUnicode` cmap
    /// 2. Glyph name → Unicode (via Adobe Glyph List)
    /// 3. Unicode naming conventions (e.g., "uni0041", "u0041")
    /// 4. The dedicated Symbol/ZapfDingbats mapping for those standard fonts
    ///
    /// **For CID Fonts (Type0):**
    /// 1. `ToUnicode` cmap
    /// 2. An unambiguous reverse mapping through an embedded, non-symbol
    ///    OpenType cmap after applying the PDF's code → CID → GID mapping
    ///
    /// **For Type3 Fonts:**
    /// 1. `ToUnicode` cmap
    /// 2. Encoding glyph name via the Adobe glyph-name mapping algorithm
    ///
    /// Returns `None` if the Unicode value could not be determined.
    ///
    /// Please note that this method is still somewhat experimental and might
    /// not work reliably in all cases.
    pub fn as_unicode(&self) -> Option<BfString> {
        match self {
            Glyph::Outline(g) => g.as_unicode(),
            Glyph::Type3(g) => g.as_unicode(),
        }
    }
}

/// A glyph and its position within a [`GlyphRun`].
pub struct PositionedGlyph<'a> {
    pub(crate) glyph: Glyph<'a>,
    pub(crate) transform: Affine,
}

impl PositionedGlyph<'_> {
    /// Return the transform that places this glyph in text space.
    pub fn transform(&self) -> Affine {
        self.transform
    }
}

impl<'a> Deref for PositionedGlyph<'a> {
    type Target = Glyph<'a>;

    fn deref(&self) -> &Self::Target {
        &self.glyph
    }
}

/// A fully materialized run of positioned glyphs.
pub struct GlyphRun<'run, 'a> {
    pub(crate) glyphs: &'run [PositionedGlyph<'a>],
}

impl<'run, 'a> GlyphRun<'run, 'a> {
    /// Return the glyphs in the run.
    pub fn glyphs(&self) -> &'run [PositionedGlyph<'a>] {
        self.glyphs
    }
}

/// An identifier that uniquely identifies a glyph, for caching purposes.
#[derive(Clone, Debug)]
pub struct GlyphIdentifier {
    id: GlyphId,
    font: OutlineFont,
}

impl CacheKey for GlyphIdentifier {
    fn cache_key(&self) -> u128 {
        hash128(&(self.id, self.font.cache_key()))
    }
}

/// A glyph defined by an outline.
#[derive(Clone, Debug)]
pub struct OutlineGlyph {
    pub(crate) id: GlyphId,
    pub(crate) font: OutlineFont,
    pub(crate) char_code: u32,
}

impl OutlineGlyph {
    /// Return the outline of the glyph, assuming an upem value of 1000.
    pub fn outline(&self) -> BezPath {
        self.font.outline_glyph(self.id, self.char_code)
    }

    /// Return the identifier of the glyph. You can use this to calculate the cache key
    /// for the glyph.
    ///
    /// Note that the `glyph_transform` attribute is not considered in the cache key of
    /// the identifier, only the glyph ID and the font.
    pub fn identifier(&self) -> GlyphIdentifier {
        GlyphIdentifier {
            id: self.id,
            font: self.font.clone(),
        }
    }

    /// Returns the Unicode code point for this glyph, if available.
    ///
    /// See [`Glyph::as_unicode`] for details on the fallback chain used.
    pub fn as_unicode(&self) -> Option<BfString> {
        self.font.char_code_to_unicode(self.char_code)
    }

    /// Get raw font bytes and metadata for downstream use.
    ///
    /// Returns `None` for Type1 fonts.
    pub fn font_data(&self) -> Option<OutlineFontData> {
        self.font.font_data()
    }

    /// Get the glyph ID within the font.
    pub fn glyph_id(&self) -> GlyphId {
        self.id
    }

    /// Get the advance width for this glyph.
    ///
    /// The advance width is how far to move horizontally after drawing
    /// this glyph before drawing the next one.
    pub fn advance_width(&self) -> Option<f32> {
        self.font.glyph_advance_width(self.char_code)
    }

    /// Metrics needed for nominal selection boxes and semantic text layout.
    pub fn text_metrics(&self) -> TextGlyphMetrics {
        let advance = self.font.glyph_advance(self.char_code);
        let writing_origin = self.font.writing_origin(self.char_code);
        let (ascent, descent) = self
            .font
            .text_metrics()
            .map_or((None, None), |(ascent, descent)| {
                (Some(ascent), Some(descent))
            });
        TextGlyphMetrics {
            advance: [advance.x as f32, advance.y as f32],
            writing_origin: [writing_origin.x as f32, writing_origin.y as f32],
            ascent,
            descent,
            writing_mode: if self.font.is_horizontal() {
                WritingMode::Horizontal
            } else {
                WritingMode::Vertical
            },
            weight: self.font.weight(),
            is_italic: self.font.is_italic(),
        }
    }

    /// Get the cache key for this glyph's font.
    ///
    /// This identifies the font uniquely, even when `font_data()` returns `None`
    /// (e.g., for Type1 fonts). Useful for grouping glyphs by font.
    pub fn font_cache_key(&self) -> u128 {
        self.font.cache_key()
    }
}

/// A type3 glyph.
#[derive(Clone)]
pub struct Type3Glyph<'a> {
    pub(crate) font: Rc<Type3<'a>>,
    pub(crate) glyph_id: GlyphId,
    pub(crate) state: State<'a>,
    pub(crate) parent_resources: Resources<'a>,
    pub(crate) cache: InterpreterCache<'a>,
    pub(crate) xref: &'a XRef,
    pub(crate) settings: InterpreterSettings,
    pub(crate) nesting_depth: u32,
    pub(crate) char_code: u32,
}

/// Owned nominal metrics for a Type 3 glyph in Hayro's normalized 1000-unit
/// text coordinate system.
///
/// The quadrilateral is derived from the font's `/FontBBox` and
/// `/FontMatrix`; it is intentionally nominal selection geometry rather than
/// an ink bound computed by interpreting the `CharProc`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Type3GlyphMetrics {
    /// Text advance applied by the interpreter after this glyph.
    pub advance: [f32; 2],
    /// Nominal font bounding quadrilateral in glyph text coordinates.
    pub nominal_quad: [kurbo::Point; 4],
}

/// A glyph defined by PDF drawing instructions.
impl<'a> Type3Glyph<'a> {
    /// Draw the type3 glyph to the given device.
    pub fn interpret(
        &self,
        device: &mut impl Device<'a>,
        transform: Affine,
        glyph_transform: Affine,
        paint: &Paint<'a>,
    ) {
        self.font
            .render_glyph(self, transform, glyph_transform, paint, device);
    }

    /// Returns the Unicode code point for this glyph, if available.
    ///
    /// A `ToUnicode` cmap takes precedence. When it has no entry, a Type 3
    /// font's Encoding glyph name is mapped using the Adobe glyph-name rules.
    pub fn as_unicode(&self) -> Option<BfString> {
        self.font.char_code_to_unicode(self.char_code)
    }

    /// Return owned metrics suitable for text selection and search geometry.
    pub fn text_metrics(&self) -> Type3GlyphMetrics {
        debug_assert!(self.char_code <= u8::MAX as u32);
        Type3GlyphMetrics {
            advance: [self.font.glyph_width(self.char_code as u8), 0.0],
            nominal_quad: self.font.glyph_nominal_quad(),
        }
    }
}

impl CacheKey for Type3Glyph<'_> {
    fn cache_key(&self) -> u128 {
        hash128(&(self.font.cache_key(), self.glyph_id))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Font<'a>(u128, FontType<'a>);

impl<'a> Font<'a> {
    pub(crate) fn new(
        dict: &Dict<'a>,
        font_resolver: &FontResolverFn,
        cmap_resolver: &CMapResolverFn,
    ) -> Option<Self> {
        let f_type = match dict.get::<Name<'_>>(SUBTYPE)?.deref() {
            TYPE1 | MM_TYPE1 => {
                FontType::Type1(Rc::new(Type1Font::new(dict, font_resolver, cmap_resolver)?))
            }
            // PDFBOX-5463: PDF viewers seem to accept OpenType as well.
            TRUE_TYPE | OPEN_TYPE => FontType::TrueType(Rc::new(TrueTypeFont::new(
                dict,
                font_resolver,
                cmap_resolver,
            )?)),
            TYPE0 => FontType::Type0(Rc::new(Type0Font::new(dict, font_resolver, cmap_resolver)?)),
            TYPE3 => FontType::Type3(Rc::new(Type3::new(dict, cmap_resolver)?)),
            f => {
                warn!(
                    "unimplemented font type {:?}",
                    std::str::from_utf8(f).unwrap_or("unknown type")
                );

                return None;
            }
        };

        let cache_key = dict.cache_key();

        Some(Self(cache_key, f_type))
    }

    pub(crate) fn new_standard(
        standard_font: StandardFont,
        font_resolver: &FontResolverFn,
    ) -> Option<Self> {
        let font = Type1Font::new_standard(standard_font, font_resolver)?;

        Some(Self(0, FontType::Type1(Rc::new(font))))
    }

    pub(crate) fn map_code(&self, code: u32) -> GlyphId {
        match &self.1 {
            FontType::Type1(f) => {
                debug_assert!(code <= u8::MAX as u32);

                f.map_code(code as u8)
            }
            FontType::TrueType(t) => {
                debug_assert!(code <= u8::MAX as u32);

                t.map_code(code as u8)
            }
            FontType::Type0(t) => t.map_code(code),
            FontType::Type3(t) => {
                debug_assert!(code <= u8::MAX as u32);

                t.map_code(code as u8)
            }
        }
    }

    pub(crate) fn get_glyph(
        &self,
        glyph: GlyphId,
        char_code: u32,
        ctx: &mut Context<'a>,
        resources: &Resources<'a>,
    ) -> Glyph<'a> {
        match &self.1 {
            FontType::Type1(t) => {
                let font = OutlineFont::Type1(t.clone());
                Glyph::Outline(OutlineGlyph {
                    id: glyph,
                    font,
                    char_code,
                })
            }
            FontType::TrueType(t) => {
                let font = OutlineFont::TrueType(t.clone());
                Glyph::Outline(OutlineGlyph {
                    id: glyph,
                    font,
                    char_code,
                })
            }
            FontType::Type0(t) => {
                let font = OutlineFont::Type0(t.clone());
                Glyph::Outline(OutlineGlyph {
                    id: glyph,
                    font,
                    char_code,
                })
            }
            FontType::Type3(t) => {
                let nesting_depth = ctx.nesting_depth() + 1;
                let shape_glyph = Type3Glyph {
                    font: t.clone(),
                    glyph_id: glyph,
                    state: ctx.get().clone(),
                    parent_resources: resources.clone(),
                    cache: ctx.interpreter_cache.clone(),
                    xref: ctx.xref,
                    settings: ctx.settings.clone(),
                    nesting_depth,
                    char_code,
                };

                Glyph::Type3(Box::new(shape_glyph))
            }
        }
    }

    pub(crate) fn code_advance(&self, code: u32) -> Vec2 {
        match &self.1 {
            FontType::Type1(t) => {
                debug_assert!(code <= u8::MAX as u32);

                Vec2::new(t.glyph_width(code as u8).unwrap_or(0.0) as f64, 0.0)
            }
            FontType::TrueType(t) => {
                debug_assert!(code <= u8::MAX as u32);

                Vec2::new(t.glyph_width(code as u8) as f64, 0.0)
            }
            FontType::Type0(t) => t.code_advance(code),
            FontType::Type3(t) => {
                debug_assert!(code <= u8::MAX as u32);

                Vec2::new(t.glyph_width(code as u8) as f64, 0.0)
            }
        }
    }

    pub(crate) fn origin_displacement(&self, code: u32) -> Vec2 {
        match &self.1 {
            FontType::Type1(_) => Vec2::default(),
            FontType::TrueType(_) => Vec2::default(),
            FontType::Type0(t) => t.origin_displacement(code),
            FontType::Type3(_) => Vec2::default(),
        }
    }

    pub(crate) fn read_code(&self, bytes: &[u8], offset: usize) -> (u32, usize) {
        match &self.1 {
            FontType::Type1(_) => (bytes[offset] as u32, 1),
            FontType::TrueType(_) => (bytes[offset] as u32, 1),
            FontType::Type0(t) => t.read_code(bytes, offset),
            FontType::Type3(_) => (bytes[offset] as u32, 1),
        }
    }

    pub(crate) fn is_horizontal(&self) -> bool {
        match &self.1 {
            FontType::Type1(_) => true,
            FontType::TrueType(_) => true,
            FontType::Type0(t) => t.is_horizontal(),
            FontType::Type3(_) => true,
        }
    }
}

impl CacheKey for Font<'_> {
    fn cache_key(&self) -> u128 {
        self.0
    }
}

#[derive(Clone, Debug)]
enum FontType<'a> {
    Type1(Rc<Type1Font>),
    TrueType(Rc<TrueTypeFont>),
    Type0(Rc<Type0Font>),
    Type3(Rc<Type3<'a>>),
}

#[derive(Debug)]
enum Encoding {
    Standard,
    MacRoman,
    WinAnsi,
    MacExpert,
    BuiltIn,
}

impl Encoding {
    fn map_code(&self, code: u8) -> Option<&'static str> {
        if code == 0 {
            return Some(".notdef");
        }
        match self {
            Self::Standard => standard::get(code),
            Self::MacRoman => mac_roman::get(code).or_else(|| mac_os_roman::get(code)),
            Self::WinAnsi => win_ansi::get(code),
            Self::MacExpert => mac_expert::get(code),
            Self::BuiltIn => None,
        }
    }
}

/// The font stretch.
#[derive(Debug, Copy, Clone)]
pub enum FontStretch {
    /// Normal.
    Normal,
    /// Ultra condensed.
    UltraCondensed,
    /// Extra condensed.
    ExtraCondensed,
    /// Condensed.
    Condensed,
    /// Semi condensed.
    SemiCondensed,
    /// Semi expanded.
    SemiExpanded,
    /// Expanded.
    Expanded,
    /// Extra expanded.
    ExtraExpanded,
    /// Ultra expanded.
    UltraExpanded,
}

impl FontStretch {
    fn from_string(s: &str) -> Self {
        match s {
            "UltraCondensed" => Self::UltraCondensed,
            "ExtraCondensed" => Self::ExtraCondensed,
            "Condensed" => Self::Condensed,
            "SemiCondensed" => Self::SemiCondensed,
            "SemiExpanded" => Self::SemiExpanded,
            "Expanded" => Self::Expanded,
            "ExtraExpanded" => Self::ExtraExpanded,
            "UltraExpanded" => Self::UltraExpanded,
            _ => Self::Normal,
        }
    }
}

bitflags! {
    /// Bitflags describing various characteristics of fonts.
    #[derive(Debug)]
    pub(crate) struct FontFlags: u32 {
        const FIXED_PITCH = 1 << 0;
        const SERIF = 1 << 1;
        const SYMBOLIC = 1 << 2;
        const SCRIPT = 1 << 3;
        const NON_SYMBOLIC = 1 << 5;
        const ITALIC = 1 << 6;
        const ALL_CAP = 1 << 16;
        const SMALL_CAP = 1 << 17;
        const FORCE_BOLD = 1 << 18;
    }
}

/// A query for a font.
pub enum FontQuery {
    /// A query for one of the 14 PDF standard fonts.
    Standard(StandardFont),
    /// A query for a font that is not embedded in the PDF file.
    ///
    /// Note that this type of query is currently not supported,
    /// but will be implemented in the future.
    Fallback(FallbackFontQuery),
}

/// A query for a font with specific properties.
#[derive(Debug, Clone)]
pub struct FallbackFontQuery {
    /// The postscript name of the font.
    pub post_script_name: Option<String>,
    /// The name of the font.
    pub font_name: Option<String>,
    /// The family of the font.
    pub font_family: Option<String>,
    /// The stretch of the font.
    pub font_stretch: FontStretch,
    /// The weight of the font.
    pub font_weight: u32,
    /// Whether the font is monospaced.
    pub is_fixed_pitch: bool,
    /// Whether the font is serif.
    pub is_serif: bool,
    /// Whether the font is italic.
    pub is_italic: bool,
    /// Whether the font is bold.
    pub is_bold: bool,
    /// Whether the font is small cap.
    pub is_small_cap: bool,
    /// The character collection (registry/ordering) if this is a CID font.
    pub character_collection: Option<CharacterCollection>,
}

impl FallbackFontQuery {
    pub(crate) fn new(dict: &Dict<'_>) -> Self {
        let post_script_name = dict
            .get::<Name<'_>>(BASE_FONT)
            .map(|n| strip_subset_prefix(n.as_str()).to_string());

        let mut data = Self {
            post_script_name,
            ..Default::default()
        };

        if let Some(descriptor) = dict.get::<Dict<'_>>(FONT_DESC) {
            data.font_name = dict
                .get::<Name<'_>>(FONT_NAME)
                .map(|n| strip_subset_prefix(n.as_str()).to_string());
            data.font_family = descriptor
                .get::<Name<'_>>(FONT_FAMILY)
                .map(|n| n.as_str().to_string());
            data.font_stretch = descriptor
                .get::<Name<'_>>(FONT_STRETCH)
                .map(|n| FontStretch::from_string(n.as_str()))
                .unwrap_or(FontStretch::Normal);
            data.font_weight = descriptor.get::<u32>(FONT_WEIGHT).unwrap_or(400);

            if let Some(flags) = descriptor
                .get::<u32>(FLAGS)
                .map(FontFlags::from_bits_truncate)
            {
                data.is_serif = flags.contains(FontFlags::SERIF);
                data.is_italic = flags.contains(FontFlags::ITALIC);
                data.is_small_cap = flags.contains(FontFlags::SMALL_CAP);
            }
        }

        data.is_italic |= data
            .post_script_name
            .as_ref()
            .is_some_and(|s| s.contains("Italic"));

        data.is_bold |= data
            .post_script_name
            .as_ref()
            .is_some_and(|s| s.contains("Bold"));

        data
    }

    /// Do a best-effort fallback to the 14 standard fonts based on the query.
    pub fn pick_standard_font(&self) -> StandardFont {
        if self.is_fixed_pitch {
            match (self.is_bold, self.is_italic) {
                (true, true) => StandardFont::CourierBoldOblique,
                (true, false) => StandardFont::CourierBold,
                (false, true) => StandardFont::CourierOblique,
                (false, false) => StandardFont::Courier,
            }
        } else if !self.is_serif {
            match (self.is_bold, self.is_italic) {
                (true, true) => StandardFont::HelveticaBoldOblique,
                (true, false) => StandardFont::HelveticaBold,
                (false, true) => StandardFont::HelveticaOblique,
                (false, false) => StandardFont::Helvetica,
            }
        } else {
            match (self.is_bold, self.is_italic) {
                (true, true) => StandardFont::TimesBoldItalic,
                (true, false) => StandardFont::TimesBold,
                (false, true) => StandardFont::TimesItalic,
                (false, false) => StandardFont::TimesRoman,
            }
        }
    }
}

impl Default for FallbackFontQuery {
    fn default() -> Self {
        Self {
            post_script_name: None,
            font_name: None,
            font_family: None,
            font_stretch: FontStretch::Normal,
            font_weight: 400,
            is_fixed_pitch: false,
            is_serif: false,
            is_italic: false,
            is_bold: false,
            is_small_cap: false,
            character_collection: None,
        }
    }
}

/// Convert a glyph name to a Unicode character, if possible.
/// An incomplete implementation of the Adobe Glyph List Specification
/// <https://github.com/adobe-type-tools/agl-specification>
pub(crate) fn glyph_name_to_unicode(name: &str) -> Option<char> {
    match glyph_name_to_bf_string(name) {
        Some(BfString::Char(character)) => Some(character),
        Some(BfString::String(string)) => string.chars().next(),
        None => None,
    }
    .or_else(|| {
        warn!("failed to map glyph name {} to unicode", name);

        None
    })
}

/// Apply the Adobe glyph-name mapping algorithm without guessing producer-
/// private names.
///
/// A glyph name can carry a suffix after `.`, combine independently mapped
/// components with `_`, or use the `uniXXXX`/`uXXXXX` naming conventions. The
/// returned value retains multi-scalar mappings instead of truncating them.
pub(crate) fn glyph_name_to_bf_string(name: &str) -> Option<BfString> {
    let canonical = name.split('.').next()?;
    if canonical.is_empty() {
        return None;
    }

    let mut unicode = String::new();
    for component in canonical.split('_') {
        if component.is_empty() {
            return None;
        }
        if let Some(mapped) = glyph_names::get(component) {
            unicode.push_str(mapped);
        } else {
            unicode.push_str(&unicode_sequence_from_name(component)?);
        }
    }

    let mut characters = unicode.chars();
    let first = characters.next()?;
    if characters.next().is_none() {
        Some(BfString::Char(first))
    } else {
        Some(BfString::String(unicode))
    }
}

fn unicode_sequence_from_name(name: &str) -> Option<String> {
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.is_empty() || !hex.len().is_multiple_of(4) {
            return None;
        }

        let mut unicode = String::new();
        for chunk in hex.as_bytes().chunks_exact(4) {
            let digits = std::str::from_utf8(chunk).ok()?;
            let scalar = u32::from_str_radix(digits, 16).ok()?;
            unicode.push(char::from_u32(scalar)?);
        }
        return Some(unicode);
    }

    let hex = name.strip_prefix('u')?;
    if !(4..=6).contains(&hex.len()) {
        return None;
    }
    let scalar = u32::from_str_radix(hex, 16).ok()?;
    Some(char::from_u32(scalar)?.to_string())
}

pub(crate) fn unicode_from_name(name: &str) -> Option<char> {
    let convert = |input: &str| u32::from_str_radix(input, 16).ok().and_then(char::from_u32);

    name.starts_with("uni")
        .then(|| name.get(3..).and_then(convert))
        .or_else(|| {
            name.starts_with("u")
                .then(|| name.get(1..).and_then(convert))
        })
        .flatten()
}

#[cfg(test)]
mod glyph_name_tests {
    use super::{glyph_name_to_bf_string, glyph_name_to_unicode};
    use hayro_cmap::BfString;

    #[test]
    fn maps_agl_names_and_discards_style_suffixes() {
        assert_eq!(glyph_name_to_bf_string("A"), Some(BfString::Char('A')));
        assert_eq!(
            glyph_name_to_bf_string("space.alt"),
            Some(BfString::Char(' '))
        );
        assert_eq!(
            glyph_name_to_bf_string("ffi"),
            Some(BfString::Char('\u{fb03}'))
        );
    }

    #[test]
    fn retains_composite_and_unicode_name_sequences() {
        assert_eq!(
            glyph_name_to_bf_string("f_f_i"),
            Some(BfString::String("ffi".into()))
        );
        assert_eq!(glyph_name_to_unicode("f_f_i"), Some('f'));
        assert_eq!(
            glyph_name_to_bf_string("uni00410042"),
            Some(BfString::String("AB".into()))
        );
        assert_eq!(
            glyph_name_to_bf_string("u1F642"),
            Some(BfString::Char('\u{1f642}'))
        );
    }

    #[test]
    fn rejects_private_or_invalid_names() {
        assert_eq!(glyph_name_to_bf_string("a17"), None);
        assert_eq!(glyph_name_to_bf_string("x30"), None);
        assert_eq!(glyph_name_to_bf_string(".notdef"), None);
        assert_eq!(glyph_name_to_bf_string("uniD800"), None);
        assert_eq!(glyph_name_to_bf_string("u110000"), None);
        assert_eq!(glyph_name_to_bf_string("A__B"), None);
    }
}

pub(crate) fn read_to_unicode(dict: &Dict<'_>, cmap_resolver: &CMapResolverFn) -> Option<CMap> {
    dict.get::<Stream<'_>>(TO_UNICODE)
        .and_then(|s| s.decoded().ok())
        // See PDFJS-11915, where `Identity-H` is used for `ToUnicode`. I don't
        // believe it's valid, but at least mupdf seems to be able to deal with it.
        .or_else(|| {
            dict.get::<Name<'_>>(TO_UNICODE)
                .and_then(|name| (cmap_resolver)(CMapName::from_bytes(name.as_ref())))
                .map(|d| Cow::Owned(d.to_vec()))
        })
        .and_then(|data| {
            let cmap_resolver = cmap_resolver.clone();
            CMap::parse(&data, move |name| (cmap_resolver)(name))
        })
}

// When mapping to glyphs, some fonts might only have a glyph for the "normalized"
// name.
pub(crate) fn normalized_glyph_name(mut name: &str) -> &str {
    if name == "nbspace" {
        name = "space";
    }

    if name == "sfthyphen" {
        name = "hyphen";
    }

    name
}
