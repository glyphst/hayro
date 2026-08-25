use crate::font::blob::{CffFontBlob, Type1FontBlob};
use crate::font::standard_font::{StandardFont, StandardKind, select_standard_font};
use crate::font::true_type::{Width, read_encoding, read_widths};
use crate::font::{
    Encoding, FallbackFontQuery, glyph_name_to_unicode, normalized_glyph_name, read_to_unicode,
    resolve_font_text_metrics, strip_subset_prefix,
};
use crate::{CMapResolverFn, CacheKey, FontResolverFn};
use hayro_cmap::{BfString, CMap};
use hayro_syntax::object::dict::keys::{BASE_FONT, FONT_DESC, FONT_FILE, FONT_FILE3, FONT_NAME};
use hayro_syntax::object::{Dict, Name, Stream};
use kurbo::BezPath;
use rustc_hash::FxHashMap;
use skrifa::GlyphId;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct Type1Font {
    cache_key: u128,
    kind: Kind,
    to_unicode: Option<CMap>,
    text_metrics: Option<(f32, f32)>,
    base_font_name: Option<String>,
    descriptor_font_name: Option<String>,
}

impl Type1Font {
    pub(crate) fn new(
        dict: &Dict<'_>,
        resolver: &FontResolverFn,
        cmap_resolver: &CMapResolverFn,
    ) -> Option<Self> {
        let cache_key = dict.cache_key();
        let to_unicode = read_to_unicode(dict, cmap_resolver);
        let descriptor = dict.get::<Dict<'_>>(FONT_DESC).unwrap_or_default();
        let base_font_name = dict
            .get::<Name<'_>>(BASE_FONT)
            .map(|name| strip_subset_prefix(name.as_str()).to_string());
        let descriptor_font_name = descriptor
            .get::<Name<'_>>(FONT_NAME)
            .map(|name| strip_subset_prefix(name.as_str()).to_string());
        let finish = |kind: Kind, to_unicode: Option<CMap>| {
            let text_metrics = resolve_font_text_metrics(&descriptor, kind.text_metrics());
            Self {
                cache_key,
                kind,
                to_unicode,
                text_metrics,
                base_font_name: base_font_name.clone(),
                descriptor_font_name: descriptor_font_name.clone(),
            }
        };

        let fallback = || {
            // TODO: Actually use fallback fonts
            let fallback_query = FallbackFontQuery::new(dict);
            let standard_font = fallback_query.pick_standard_font();

            warn!(
                "unable to load font {}, falling back to {}",
                fallback_query
                    .post_script_name
                    .unwrap_or("(no name)".to_string()),
                standard_font.as_str()
            );

            Some(finish(
                Kind::Standard(StandardKind::new_with_standard(
                    dict,
                    standard_font,
                    true,
                    resolver,
                )?),
                to_unicode.clone(),
            ))
        };

        let inner = if is_cff(dict) {
            if let Some(cff) = CffKind::new(dict) {
                finish(Kind::Cff(cff), to_unicode)
            } else {
                return fallback();
            }
        } else if is_type1(dict) {
            if let Some(f) = Type1Kind::new(dict) {
                finish(Kind::Type1(f), to_unicode)
            } else {
                return fallback();
            }
        } else if let Some(standard) = StandardKind::new(dict, resolver) {
            finish(Kind::Standard(standard), to_unicode)
        } else {
            return fallback();
        };

        Some(inner)
    }

    pub(crate) fn new_standard(font: StandardFont, resolver: &FontResolverFn) -> Option<Self> {
        let dict = Dict::default();
        let standard = StandardKind::new_with_standard(&dict, font, true, resolver)?;
        let text_metrics = resolve_font_text_metrics(&dict, standard.text_metrics());

        Some(Self {
            cache_key: 0,
            kind: Kind::Standard(standard),
            to_unicode: None,
            text_metrics,
            base_font_name: Some(font.as_str().to_string()),
            descriptor_font_name: None,
        })
    }

    pub(crate) fn map_code(&self, code: u8) -> GlyphId {
        match &self.kind {
            Kind::Standard(s) => s.map_code(code),
            Kind::Type1(s) => s.map_code(code),
            Kind::Cff(c) => c.map_code(code),
        }
    }

    pub(crate) fn outline_glyph(&self, glyph: GlyphId) -> BezPath {
        match &self.kind {
            Kind::Standard(s) => s.outline_glyph(glyph),
            Kind::Cff(c) => c.outline_glyph(glyph),
            Kind::Type1(t) => t.outline_glyph(glyph),
        }
    }

    pub(crate) fn glyph_width(&self, code: u8) -> Option<f32> {
        match &self.kind {
            Kind::Standard(s) => s.glyph_width(code),
            Kind::Cff(c) => c.glyph_width(code),
            Kind::Type1(t) => t.glyph_width(code),
        }
    }

    pub(crate) fn char_code_to_unicode(&self, char_code: u32) -> Option<BfString> {
        if let Some(to_unicode) = &self.to_unicode
            && let Some(c) = to_unicode.lookup_bf_string(char_code)
        {
            // Skip null character mappings and fall back to glyph name
            // lookup. Some PDFs have incorrect ToUnicode mappings that map
            // to U+0000.
            if c != BfString::Char('\0') {
                return Some(c);
            }
        }

        let code = char_code as u8;
        let mapped = match &self.kind {
            Kind::Standard(s) => s.char_code_to_unicode(code).map(BfString::Char),
            Kind::Cff(c) => c.char_code_to_unicode(code).map(BfString::Char),
            Kind::Type1(t) => t.char_code_to_unicode(code).map(BfString::Char),
        };
        mapped.or_else(|| {
            // PDF dictionary names alone are not sufficient proof: require
            // them to agree with the embedded Type 1/CFF program name.
            let font = self.verified_embedded_simple_postscript_name()?;
            let glyph = self.source_glyph_name(char_code)?;
            known_math_font_glyph_to_unicode(font, glyph).map(BfString::Char)
        })
    }

    pub(crate) fn source_glyph_name(&self, char_code: u32) -> Option<&str> {
        let code = u8::try_from(char_code).ok()?;
        match &self.kind {
            Kind::Standard(font) => font.code_to_ps_name(code),
            Kind::Cff(font) => font.code_to_ps_name(code),
            Kind::Type1(font) => font.code_to_ps_name(code),
        }
    }

    pub(crate) fn postscript_name(&self) -> Option<&str> {
        self.embedded_postscript_name()
            .or(self.base_font_name.as_deref())
            .or(self.descriptor_font_name.as_deref())
    }

    pub(crate) fn embedded_postscript_name(&self) -> Option<&str> {
        let name = match &self.kind {
            Kind::Type1(font) => font.font.table().name(),
            Kind::Cff(font) => font.font.postscript_name(),
            Kind::Standard(_) => None,
        }?;
        Some(strip_subset_prefix(name))
    }

    pub(crate) fn base_font_name(&self) -> Option<&str> {
        self.base_font_name.as_deref()
    }

    pub(crate) fn descriptor_font_name(&self) -> Option<&str> {
        self.descriptor_font_name.as_deref()
    }

    fn verified_embedded_simple_postscript_name(&self) -> Option<&str> {
        matching_embedded_font_name(
            self.embedded_postscript_name(),
            self.base_font_name.as_deref(),
            self.descriptor_font_name.as_deref(),
        )
    }

    pub(crate) fn text_metrics(&self) -> Option<(f32, f32)> {
        self.text_metrics
    }

    pub(crate) fn weight(&self) -> Option<u32> {
        match &self.kind {
            Kind::Standard(font) => Some(if font.is_bold() { 700 } else { 400 }),
            Kind::Cff(_) | Kind::Type1(_) => None,
        }
    }

    pub(crate) fn is_italic(&self) -> Option<bool> {
        match &self.kind {
            Kind::Standard(font) => Some(font.is_italic()),
            Kind::Cff(_) | Kind::Type1(_) => None,
        }
    }
}

impl CacheKey for Type1Font {
    fn cache_key(&self) -> u128 {
        self.cache_key
    }
}

#[derive(Debug)]
enum Kind {
    Standard(StandardKind),
    Cff(CffKind),
    Type1(Type1Kind),
}

impl Kind {
    fn text_metrics(&self) -> Option<(f32, f32)> {
        match self {
            Self::Standard(font) => font.text_metrics(),
            Self::Cff(font) => font.font.text_metrics(),
            Self::Type1(font) => font.font.text_metrics(),
        }
    }
}

/// Exact producer-font mappings that are not expressible through the Adobe
/// Glyph List because the Type 1 program uses control-code glyph names.
///
/// `MathTime Professional 2` assigns its LMP2 symbol family to the `MT2SY*`
/// programs. LaTeX's `symbols` slot 2 is `\times`, whose Unicode scalar is
/// U+00D7. Those programs expose that glyph under the built-in name `STX`.
/// Requiring both the published program family and the source glyph name keeps
/// this distinct from a general interpretation of the ASCII control name.
fn known_math_font_glyph_to_unicode(font: &str, glyph: &str) -> Option<char> {
    let is_mtpro2_symbol = matches!(
        font,
        "MT2SYF"
            | "MT2SYS"
            | "MT2SYT"
            | "MT2BSYF"
            | "MT2BSYS"
            | "MT2BSYT"
            | "MT2HSYF"
            | "MT2HSYS"
            | "MT2HSYT"
    );
    (is_mtpro2_symbol && glyph == "STX").then_some('\u{00D7}')
}

fn matching_pdf_font_name<'a>(base: Option<&'a str>, descriptor: Option<&str>) -> Option<&'a str> {
    base.zip(descriptor)
        .filter(|(base, descriptor)| base == descriptor)
        .map(|(base, _)| base)
}

fn matching_embedded_font_name<'a>(
    embedded: Option<&str>,
    base: Option<&'a str>,
    descriptor: Option<&str>,
) -> Option<&'a str> {
    let embedded = embedded?;
    let declared = matching_pdf_font_name(base, descriptor)?;
    (embedded == declared).then_some(declared)
}

#[cfg(test)]
mod known_math_font_tests {
    use super::{
        known_math_font_glyph_to_unicode, matching_embedded_font_name, matching_pdf_font_name,
    };

    #[test]
    fn maps_only_the_published_mtpro2_times_glyph() {
        for font in [
            "MT2SYF", "MT2SYS", "MT2SYT", "MT2BSYF", "MT2BSYS", "MT2BSYT", "MT2HSYF", "MT2HSYS",
            "MT2HSYT",
        ] {
            assert_eq!(known_math_font_glyph_to_unicode(font, "STX"), Some('×'));
        }
        assert_eq!(known_math_font_glyph_to_unicode("MT2SYT", "ETX"), None);
        assert_eq!(
            known_math_font_glyph_to_unicode("UnrelatedFont", "STX"),
            None
        );
    }

    #[test]
    fn requires_matching_base_and_descriptor_names() {
        assert_eq!(
            matching_pdf_font_name(Some("MT2SYT"), Some("MT2SYT")),
            Some("MT2SYT")
        );
        assert_eq!(matching_pdf_font_name(Some("MT2SYT"), None), None);
        assert_eq!(matching_pdf_font_name(None, Some("MT2SYT")), None);
        assert_eq!(
            matching_pdf_font_name(Some("MT2SYT"), Some("OtherFont")),
            None
        );
    }

    #[test]
    fn requires_the_embedded_program_name_to_match() {
        assert_eq!(
            matching_embedded_font_name(Some("MT2SYT"), Some("MT2SYT"), Some("MT2SYT")),
            Some("MT2SYT")
        );
        assert_eq!(
            matching_embedded_font_name(Some("OtherFont"), Some("MT2SYT"), Some("MT2SYT")),
            None
        );
        assert_eq!(
            matching_embedded_font_name(None, Some("MT2SYT"), Some("MT2SYT")),
            None
        );
    }
}

fn is_cff(dict: &Dict<'_>) -> bool {
    dict.get::<Dict<'_>>(FONT_DESC)
        .map(|dict| dict.contains_key(FONT_FILE3))
        .unwrap_or(false)
}

fn is_type1(dict: &Dict<'_>) -> bool {
    dict.get::<Dict<'_>>(FONT_DESC)
        .map(|dict| dict.contains_key(FONT_FILE))
        .unwrap_or(false)
}

#[derive(Debug)]
struct Type1Kind {
    font: Type1FontBlob,
    encoding: Encoding,
    widths: Vec<Width>,
    missing_width: f32,
    encodings: FxHashMap<u8, String>,
    name_to_gid: FxHashMap<String, GlyphId>,
    standard_font: Option<StandardFont>,
}

impl Type1Kind {
    fn new(dict: &Dict<'_>) -> Option<Self> {
        let descriptor = dict.get::<Dict<'_>>(FONT_DESC)?;
        let data = descriptor.get::<Stream<'_>>(FONT_FILE)?;
        let font = Type1FontBlob::new(Arc::new(data.decoded().ok()?.to_vec()))?;

        let (encoding, encodings) = read_encoding(dict);
        let (widths, missing_width) = read_widths(dict, &descriptor)?;
        let standard_font = select_standard_font(dict, &descriptor).map(|(f, _)| f);

        let name_to_gid: FxHashMap<String, GlyphId> = font
            .table()
            .glyph_names()
            .map(|(gid, name)| (name.to_string(), gid))
            .collect();

        Some(Self {
            font,
            encoding,
            widths,
            missing_width,
            encodings,
            name_to_gid,
            standard_font,
        })
    }

    fn name_to_glyph(&self, name: &str) -> Option<GlyphId> {
        self.name_to_gid.get(name).copied()
    }

    fn map_code(&self, code: u8) -> GlyphId {
        if let Some(entry) = self.encodings.get(&code) {
            self.name_to_glyph(entry)
        } else {
            match self.encoding {
                Encoding::BuiltIn => self.font.table().encoding().and_then(|e| e.map(code)),
                _ => self
                    .encoding
                    .map_code(code)
                    .and_then(|name| self.name_to_glyph(name)),
            }
        }
        .unwrap_or(GlyphId::NOTDEF)
    }

    fn outline_glyph(&self, glyph: GlyphId) -> BezPath {
        self.font.outline_glyph(glyph)
    }

    fn code_to_ps_name(&self, code: u8) -> Option<&str> {
        self.encodings
            .get(&code)
            .map(String::as_str)
            .or_else(|| match self.encoding {
                Encoding::BuiltIn => self
                    .font
                    .table()
                    .encoding()
                    .and_then(|e| e.glyph_name(code)),
                _ => self.encoding.map_code(code),
            })
    }

    fn glyph_width(&self, code: u8) -> Option<f32> {
        match self.widths.get(code as usize).copied() {
            Some(Width::Value(w)) => Some(w),
            Some(Width::Missing) => Some(self.missing_width),
            _ => {
                // If font looks like a standard font, get the width from there.
                let sf = self.standard_font?;
                self.code_to_ps_name(code)
                    .and_then(|name| sf.get_width(name))
            }
        }
    }

    fn char_code_to_unicode(&self, code: u8) -> Option<char> {
        self.code_to_ps_name(code).and_then(glyph_name_to_unicode)
    }
}

#[derive(Debug)]
struct CffKind {
    font: CffFontBlob,
    encoding: Encoding,
    widths: Vec<Width>,
    missing_width: f32,
    encodings: FxHashMap<u8, String>,
    name_to_gid: FxHashMap<String, GlyphId>,
    gid_to_name: Vec<Option<String>>,
    standard_font: Option<StandardFont>,
}

impl CffKind {
    fn new(dict: &Dict<'_>) -> Option<Self> {
        let descriptor = dict.get::<Dict<'_>>(FONT_DESC)?;
        let data = descriptor.get::<Stream<'_>>(FONT_FILE3)?;
        let font = CffFontBlob::new(Arc::new(data.decoded().ok()?.to_vec()))?;

        let (encoding, encodings) = read_encoding(dict);
        let (widths, missing_width) = read_widths(dict, &descriptor)?;
        let standard_font = select_standard_font(dict, &descriptor).map(|(f, _)| f);
        let mut gid_to_name = vec![None; font.num_glyphs() as usize];
        let name_to_gid: FxHashMap<String, GlyphId> = font
            .glyph_names()
            .into_iter()
            .inspect(|(gid, name)| gid_to_name[gid.to_u32() as usize] = Some(name.clone()))
            .map(|(gid, name)| (name, gid))
            .collect();

        Some(Self {
            font,
            encoding,
            widths,
            missing_width,
            encodings,
            name_to_gid,
            gid_to_name,
            standard_font,
        })
    }

    fn map_code(&self, code: u8) -> GlyphId {
        let get_glyph = |entry: &str| {
            self.name_to_gid
                .get(entry)
                .copied()
                .or_else(|| self.name_to_gid.get(normalized_glyph_name(entry)).copied())
        };

        if let Some(entry) = self.encodings.get(&code) {
            get_glyph(entry)
        } else {
            match self.encoding {
                Encoding::BuiltIn => self.font.glyph_index(code),
                _ => self.encoding.map_code(code).and_then(get_glyph),
            }
        }
        .unwrap_or(GlyphId::NOTDEF)
    }

    fn outline_glyph(&self, glyph: GlyphId) -> BezPath {
        self.font.outline_glyph(glyph)
    }

    fn code_to_ps_name(&self, code: u8) -> Option<&str> {
        if let Some(entry) = self.encodings.get(&code) {
            Some(entry.as_str())
        } else {
            match self.encoding {
                Encoding::BuiltIn => self
                    .font
                    .glyph_index(code)
                    .and_then(|gid| self.gid_to_name.get(gid.to_u32() as usize))
                    .and_then(|name| name.as_deref()),
                _ => self.encoding.map_code(code),
            }
        }
    }

    fn glyph_width(&self, code: u8) -> Option<f32> {
        match self.widths.get(code as usize).copied() {
            Some(Width::Value(w)) => Some(w),
            Some(Width::Missing) => Some(self.missing_width),
            _ => {
                // If font looks like a standard font, get the width from there.
                let sf = self.standard_font?;
                self.code_to_ps_name(code)
                    .and_then(|name| sf.get_width(name))
            }
        }
    }

    fn char_code_to_unicode(&self, code: u8) -> Option<char> {
        self.code_to_ps_name(code).and_then(glyph_name_to_unicode)
    }
}
