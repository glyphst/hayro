use crate::font::blob::{CffFontBlob, OpenTypeFontBlob};
use crate::font::generated::{glyph_names, mac_os_roman, mac_roman, standard};
use crate::font::standard_font::{StandardKind, select_standard_font};
use crate::font::{
    Encoding, FallbackFontQuery, FontFlags, glyph_name_to_bf_string, read_to_unicode,
    resolve_font_text_metrics, strip_subset_prefix, unicode_from_name,
};
use crate::util::OptionLog;
use crate::{CMapResolverFn, CacheKey, FontResolverFn};
use hayro_cmap::{BfString, CMap};
use hayro_syntax::object::Array;
use hayro_syntax::object::Dict;
use hayro_syntax::object::Name;
use hayro_syntax::object::Number;
use hayro_syntax::object::Object;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::*;
use kurbo::BezPath;
use rustc_hash::FxHashMap;
use skrifa::attribute::Style;
use skrifa::raw::TableProvider;
use skrifa::raw::tables::cmap::PlatformId;
use skrifa::{GlyphId, MetadataProvider};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) const MAX_EMBEDDED_UNICODE_CMAP_MAPPINGS: usize = 262_144;
const UNMAPPED_GLYPH: u32 = u32::MAX;

#[derive(Debug)]
pub(crate) struct TrueTypeFont {
    cache_key: u128,
    kind: Kind,
    to_unicode: Option<CMap>,
    text_metrics: Option<(f32, f32)>,
}

#[derive(Debug)]
enum Kind {
    Embedded(EmbeddedKind),
    Standard(StandardKind),
}

impl TrueTypeFont {
    pub(crate) fn new(
        dict: &Dict<'_>,
        font_resolver: &FontResolverFn,
        cmap_resolver: &CMapResolverFn,
    ) -> Option<Self> {
        let cache_key = dict.cache_key();
        let to_unicode = read_to_unicode(dict, cmap_resolver);
        let descriptor = dict.get::<Dict<'_>>(FONT_DESC).unwrap_or_default();

        if let Some(embedded) = EmbeddedKind::new(dict) {
            let text_metrics =
                resolve_font_text_metrics(&descriptor, embedded.base_font.text_metrics());
            return Some(Self {
                cache_key,
                kind: Kind::Embedded(embedded),
                to_unicode,
                text_metrics,
            });
        }

        let fallback = || {
            let fallback_query = FallbackFontQuery::new(dict);
            let standard_font = fallback_query.pick_standard_font();

            warn!(
                "unable to load TrueType font {}, falling back to {}",
                fallback_query
                    .post_script_name
                    .unwrap_or("(no name)".to_string()),
                standard_font.as_str()
            );

            let standard =
                StandardKind::new_with_standard(dict, standard_font, true, font_resolver)?;
            let text_metrics = resolve_font_text_metrics(&descriptor, standard.text_metrics());
            Some(Self {
                cache_key,
                kind: Kind::Standard(standard),
                to_unicode: to_unicode.clone(),
                text_metrics,
            })
        };

        if let Some(standard) = StandardKind::new(dict, font_resolver) {
            let text_metrics = resolve_font_text_metrics(&descriptor, standard.text_metrics());
            Some(Self {
                cache_key,
                kind: Kind::Standard(standard),
                to_unicode,
                text_metrics,
            })
        } else {
            fallback()
        }
    }

    pub(crate) fn outline_glyph(&self, glyph: GlyphId, code: u8) -> BezPath {
        match &self.kind {
            Kind::Embedded(e) => e.outline_glyph(glyph),
            Kind::Standard(s) => s.outline_glyph(glyph, code),
        }
    }

    pub(crate) fn font_data(&self) -> Option<crate::font::FontData> {
        match &self.kind {
            Kind::Embedded(e) => Some(e.base_font.font_data()),
            Kind::Standard(_) => None,
        }
    }

    pub(crate) fn postscript_name(&self) -> Option<&str> {
        match &self.kind {
            Kind::Embedded(e) => e.postscript_name.as_deref(),
            Kind::Standard(_) => None,
        }
    }

    pub(crate) fn weight(&self) -> Option<u32> {
        match &self.kind {
            Kind::Embedded(e) => {
                let weight = e.base_font.font_ref().attributes().weight.value().round() as u32;
                if weight > 0 { Some(weight) } else { None }
            }
            Kind::Standard(s) => Some(if s.is_bold() { 700 } else { 400 }),
        }
    }

    pub(crate) fn is_italic(&self) -> bool {
        match &self.kind {
            Kind::Embedded(e) => {
                // Check PDF font flags first
                if let Some(flags) = &e.font_flags
                    && flags.contains(FontFlags::ITALIC)
                {
                    return true;
                }
                // Check skrifa font attributes
                e.base_font.font_ref().attributes().style != Style::Normal
            }
            Kind::Standard(s) => s.is_italic(),
        }
    }

    pub(crate) fn is_serif(&self) -> bool {
        match &self.kind {
            Kind::Embedded(e) => e
                .font_flags
                .as_ref()
                .is_some_and(|f| f.contains(FontFlags::SERIF)),
            Kind::Standard(s) => s.is_serif(),
        }
    }

    pub(crate) fn is_monospace(&self) -> bool {
        match &self.kind {
            Kind::Embedded(e) => {
                // Check PDF font flags first
                if let Some(flags) = &e.font_flags
                    && flags.contains(FontFlags::FIXED_PITCH)
                {
                    return true;
                }
                // Check skrifa font metrics
                e.base_font
                    .font_ref()
                    .metrics(
                        skrifa::instance::Size::unscaled(),
                        skrifa::instance::LocationRef::default(),
                    )
                    .is_monospace
            }
            Kind::Standard(s) => s.is_monospace(),
        }
    }

    pub(crate) fn text_metrics(&self) -> Option<(f32, f32)> {
        self.text_metrics
    }

    pub(crate) fn map_code(&self, code: u8) -> GlyphId {
        match &self.kind {
            Kind::Embedded(e) => e.map_code(code),
            Kind::Standard(s) => s.map_code(code),
        }
    }

    pub(crate) fn glyph_width(&self, code: u8) -> f32 {
        match &self.kind {
            Kind::Embedded(e) => e.glyph_width(code),
            Kind::Standard(s) => s.glyph_width(code).unwrap_or(0.0),
        }
    }

    pub(crate) fn char_code_to_unicode(&self, code: u32) -> Option<BfString> {
        if let Some(to_unicode) = &self.to_unicode
            && let Some(c) = to_unicode.lookup_bf_string(code)
        {
            return Some(c);
        }

        match &self.kind {
            Kind::Embedded(e) => e.char_code_to_unicode(code as u8),
            Kind::Standard(s) => s.char_code_to_unicode(code as u8).map(BfString::Char),
        }
    }
}

#[derive(Debug)]
struct EmbeddedKind {
    base_font: OpenTypeFontBlob,
    widths: Vec<Width>,
    missing_width: f32,
    font_flags: Option<FontFlags>,
    glyph_names: FxHashMap<String, GlyphId>,
    embedded_unicodes: FxHashMap<GlyphId, Option<BfString>>,
    encoding: Encoding,
    // Only used for PDFs that mistakenly embed a
    // CFF font.
    cff_blob: Option<CffFontBlob>,
    differences: FxHashMap<u8, String>,
    cached_mappings: Box<[AtomicU32; 256]>,
    /// PostScript name from the PDF.
    postscript_name: Option<String>,
}

impl EmbeddedKind {
    fn new(dict: &Dict<'_>) -> Option<Self> {
        let descriptor = dict.get::<Dict<'_>>(FONT_DESC).unwrap_or_default();

        let font_flags = descriptor.get::<u32>(FLAGS).and_then(FontFlags::from_bits);

        let (widths, missing_width) = read_widths(dict, &descriptor)?;
        let (encoding, differences) = read_encoding(dict);
        let base_font = descriptor
            .get::<Stream<'_>>(FONT_FILE2)
            .and_then(|s| s.decoded().ok())
            .and_then(|d| OpenTypeFontBlob::new(Arc::new(d.to_vec()), 0))?;

        let glyph_names = base_font.glyph_names();
        let mut embedded_unicodes = FxHashMap::default();
        for (name, glyph) in &glyph_names {
            if let Some(unicode) = glyph_name_to_bf_string(name) {
                retain_unique_unicode(&mut embedded_unicodes, *glyph, unicode);
            }
        }

        let charmap = base_font.font_ref().charmap();
        if !charmap.is_symbol()
            && let Some(cmap_unicodes) =
                collect_cmap_unicodes(charmap.mappings(), MAX_EMBEDDED_UNICODE_CMAP_MAPPINGS)
        {
            for (glyph, unicode) in cmap_unicodes {
                match unicode {
                    Some(unicode) => {
                        retain_unique_unicode(&mut embedded_unicodes, glyph, unicode);
                    }
                    None => {
                        embedded_unicodes.insert(glyph, None);
                    }
                }
            }
        }

        let cff_font_blob = base_font
            .font_ref()
            .cff()
            .ok()
            .and_then(|cff| CffFontBlob::new(Arc::new(cff.offset_data().as_ref().to_vec())));

        let postscript_name = dict
            .get::<Name<'_>>(BASE_FONT)
            .map(|n| strip_subset_prefix(n.as_str()).to_string());

        Some(Self {
            base_font,
            differences,
            cff_blob: cff_font_blob,
            widths,
            missing_width,
            glyph_names,
            embedded_unicodes,
            font_flags,
            encoding,
            cached_mappings: Box::new(std::array::from_fn(|_| AtomicU32::new(UNMAPPED_GLYPH))),
            postscript_name,
        })
    }

    fn is_non_symbolic(&self) -> bool {
        self.font_flags
            .as_ref()
            .map(|f| f.contains(FontFlags::NON_SYMBOLIC))
            .unwrap_or(false)
    }

    fn code_to_name(&self, code: u8) -> Option<&str> {
        self.differences
            .get(&code)
            .map(|s| s.as_str())
            .or_else(|| self.encoding.map_code(code))
            // See PDFJS-6410 - PDF has no base encoding, so let's fallback to
            // standard here.
            .or_else(|| standard::get(code))
    }

    fn char_code_to_unicode(&self, code: u8) -> Option<BfString> {
        let declared_name = self
            .differences
            .get(&code)
            .map(String::as_str)
            .or_else(|| self.encoding.map_code(code));
        if let Some(unicode) = declared_name.and_then(glyph_name_to_bf_string) {
            return Some(unicode);
        }

        let glyph = self.map_code(code);
        if glyph != GlyphId::NOTDEF
            && let Some(unicode) = self.embedded_unicodes.get(&glyph).and_then(Option::as_ref)
        {
            return Some(unicode.clone());
        }

        self.is_non_symbolic()
            .then(|| standard::get(code))
            .flatten()
            .and_then(glyph_name_to_bf_string)
    }

    fn outline_glyph(&self, glyph: GlyphId) -> BezPath {
        self.base_font.outline_glyph(glyph)
    }

    fn map_code(&self, code: u8) -> GlyphId {
        let cached = self.cached_mappings[code as usize].load(Ordering::Relaxed);
        if cached != UNMAPPED_GLYPH {
            return GlyphId::new(cached);
        }

        if let Some(blob) = self.cff_blob.as_ref() {
            return self
                .code_to_name(code)
                .and_then(|name| blob.glyph_index_by_name(name))
                .unwrap_or(GlyphId::NOTDEF);
        }

        let mut glyph = None;

        if self.is_non_symbolic() {
            let Some(lookup) = self.code_to_name(code) else {
                return GlyphId::NOTDEF;
            };

            if let Ok(cmap) = self.base_font.font_ref().cmap() {
                for record in cmap.encoding_records() {
                    if record.platform_id() == PlatformId::Windows
                        && record.encoding_id() == 1
                        && let Ok(subtable) = record.subtable(cmap.offset_data())
                    {
                        glyph = glyph.or_else(|| {
                            glyph_names::get(lookup)
                                .map(|n| n.to_string())
                                .or_else(|| unicode_from_name(lookup).map(|n| n.to_string()))
                                .and_then(|n| n.chars().next())
                                .and_then(|c| subtable.map_codepoint(c))
                                .filter(|g| *g != GlyphId::NOTDEF)
                        });
                    }
                }

                for record in cmap.encoding_records() {
                    if record.platform_id() == PlatformId::Macintosh
                        && record.encoding_id() == 0
                        && let Ok(subtable) = record.subtable(cmap.offset_data())
                    {
                        glyph = glyph.or_else(|| {
                            mac_os_roman::get_inverse(lookup)
                                .or_else(|| mac_roman::get_inverse(lookup))
                                .and_then(|c| subtable.map_codepoint(c))
                                .filter(|g| *g != GlyphId::NOTDEF)
                        });
                    }
                }
            }

            if glyph.is_none() {
                if let Some(gid) = self.glyph_names.get(lookup) {
                    glyph = Some(*gid);
                } else if let Some(gid) = glyph_num_string(lookup) {
                    glyph = Some(GlyphId::new(gid));
                }
            }
        } else if let Ok(cmap) = self.base_font.font_ref().cmap() {
            for record in cmap.encoding_records() {
                if record.platform_id() == PlatformId::Windows
                    && matches!(record.encoding_id(), 0 | 1)
                {
                    if let Ok(subtable) = record.subtable(cmap.offset_data()) {
                        for offset in [0x0000_u32, 0xF000, 0xF100, 0xF200] {
                            glyph = glyph
                                .or_else(|| subtable.map_codepoint(code as u32 + offset))
                                .filter(|g| *g != GlyphId::NOTDEF);
                        }
                    }
                } else if matches!(
                    record.platform_id(),
                    PlatformId::Macintosh | PlatformId::Unicode
                ) && record.encoding_id() == 0
                    && let Ok(subtable) = record.subtable(cmap.offset_data())
                {
                    glyph = glyph
                        .or_else(|| subtable.map_codepoint(code))
                        .filter(|g| *g != GlyphId::NOTDEF);
                }
            }
        }

        let glyph = glyph.unwrap_or(GlyphId::NOTDEF);
        self.cached_mappings[code as usize].store(glyph.to_u32(), Ordering::Relaxed);

        glyph
    }

    fn glyph_width(&self, code: u8) -> f32 {
        match self.widths.get(code as usize).copied() {
            Some(Width::Value(w)) => w,
            Some(Width::Missing) => self.missing_width,
            _ => self
                .base_font
                .glyph_metrics()
                .advance_width(self.map_code(code))
                .warn_none(&format!("failed to find advance width for code {code}"))
                .unwrap_or(0.0),
        }
    }
}

fn retain_unique_unicode(
    mappings: &mut FxHashMap<GlyphId, Option<BfString>>,
    glyph: GlyphId,
    unicode: BfString,
) {
    match mappings.entry(glyph) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(Some(unicode));
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if entry.get().as_ref() != Some(&unicode) {
                entry.insert(None);
            }
        }
    }
}

pub(crate) fn collect_cmap_unicodes(
    mappings: impl IntoIterator<Item = (u32, GlyphId)>,
    limit: usize,
) -> Option<FxHashMap<GlyphId, Option<BfString>>> {
    let mut unicodes = FxHashMap::default();
    for (index, (scalar, glyph)) in mappings.into_iter().enumerate() {
        if index >= limit {
            return None;
        }
        if let Some(character) = char::from_u32(scalar)
            && is_extractable_cmap_character(character)
        {
            retain_unique_unicode(&mut unicodes, glyph, BfString::Char(character));
        }
    }
    Some(unicodes)
}

pub(crate) fn is_extractable_cmap_character(character: char) -> bool {
    let scalar = character as u32;
    !character.is_control()
        && !(0xE000..=0xF8FF).contains(&scalar)
        && !(0xF0000..=0xFFFFD).contains(&scalar)
        && !(0x100000..=0x10FFFD).contains(&scalar)
        && !(0xFDD0..=0xFDEF).contains(&scalar)
        && scalar & 0xFFFF < 0xFFFE
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Width {
    Value(f32),
    Missing,
}

// Table 111 requires all four declarations, except for a standard-14 Type 1
// font where all four may be absent. Substituting a similar font must not grant
// that exception to an arbitrary font name or an incomplete declaration.
pub(crate) fn validate_simple_metrics(dict: &Dict<'_>) -> Option<()> {
    let present =
        [FIRST_CHAR, LAST_CHAR, WIDTHS, FONT_DESC].map(|key| !dict.is_null_or_absent(key));
    if present.into_iter().all(|value| value) {
        dict.get::<Dict<'_>>(FONT_DESC)?;
        return Some(());
    }
    if present.into_iter().any(|value| value) || dict.get::<Name<'_>>(SUBTYPE)?.as_ref() != TYPE1 {
        return None;
    }
    let base = dict.get::<Name<'_>>(BASE_FONT)?;
    let (font, _) = select_standard_font(dict, &Dict::default())?;
    (base.as_str() == font.postscript_name()).then_some(())
}

pub(crate) fn read_widths(dict: &Dict<'_>, descriptor: &Dict<'_>) -> Option<(Vec<Width>, f32)> {
    let missing_width = if descriptor.is_null_or_absent(MISSING_WIDTH) {
        0.0
    } else {
        let value = descriptor.get::<f32>(MISSING_WIDTH)?;
        value.is_finite().then_some(value)?
    };
    // An empty table selects standard metrics, including the internal diagnostic
    // fallback font. External dictionaries first pass validate_simple_metrics.
    if [FIRST_CHAR, LAST_CHAR, WIDTHS]
        .into_iter()
        .all(|key| dict.is_null_or_absent(key))
    {
        return Some((Vec::new(), missing_width));
    }
    let first = usize::from(u8::try_from(dict.get::<Number>(FIRST_CHAR)?.as_i64_exact()?).ok()?);
    let last = usize::from(u8::try_from(dict.get::<Number>(LAST_CHAR)?.as_i64_exact()?).ok()?);
    let count = last.checked_sub(first)?.checked_add(1)?;
    let array = dict.get::<Array<'_>>(WIDTHS)?;
    // Bound work and storage before reading values, retaining malformed slots
    // and rejecting both missing entries and tails after a valid prefix.
    if array.raw_iter().take(count + 1).count() != count {
        return None;
    }
    let values = Vec::<f32>::try_from(array).ok()?;
    if values.len() != count || !values.iter().all(|value| value.is_finite()) {
        return None;
    }
    let mut widths = vec![Width::Missing; 256];
    for (slot, value) in widths[first..=last].iter_mut().zip(values) {
        *slot = Width::Value(value);
    }
    Some((widths, missing_width))
}

fn glyph_num_string(s: &str) -> Option<u32> {
    if !s.starts_with('g') || s.len() < 2 {
        return None;
    }

    s[1..].parse::<u32>().ok()
}

impl CacheKey for TrueTypeFont {
    fn cache_key(&self) -> u128 {
        self.cache_key
    }
}

pub(crate) fn read_encoding(dict: &Dict<'_>) -> (Encoding, FxHashMap<u8, String>) {
    fn get_encoding_base(dict: &Dict<'_>, name: Name<'_>) -> Encoding {
        match dict.get::<Name<'_>>(name.as_ref()) {
            Some(n) => match n.deref() {
                WIN_ANSI_ENCODING => Encoding::WinAnsi,
                MAC_ROMAN_ENCODING => Encoding::MacRoman,
                MAC_EXPERT_ENCODING => Encoding::MacExpert,
                STANDARD_ENCODING => Encoding::Standard,
                _ => {
                    warn!("Unknown font encoding {}", name.as_str());

                    Encoding::BuiltIn
                }
            },
            None => Encoding::BuiltIn,
        }
    }

    let mut map = FxHashMap::default();

    if let Some(encoding_dict) = dict.get::<Dict<'_>>(ENCODING) {
        if let Some(differences) = encoding_dict.get::<Array<'_>>(DIFFERENCES) {
            let entries = differences.iter::<Object<'_>>();

            let mut code = 0_i64;

            for obj in entries {
                match obj {
                    Object::Number(num) => {
                        code = num.as_i64();
                    }
                    Object::Name(name) => {
                        if let Ok(code) = code.try_into() {
                            map.insert(code, name.as_str().to_string());
                        }
                        code = code.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }

        (
            get_encoding_base(&encoding_dict, Name::new_unescaped(BASE_ENCODING)),
            map,
        )
    } else {
        (
            get_encoding_base(dict, Name::new_unescaped(ENCODING)),
            FxHashMap::default(),
        )
    }
}

#[cfg(test)]
mod simple_metrics_tests {
    use super::{Width, read_widths, validate_simple_metrics};
    use hayro_syntax::object::{Dict, FromBytes};

    #[test]
    fn simple_metrics_complete_ranges_and_missing_width() {
        let dict = Dict::from_bytes(b"<< /FirstChar 65 /LastChar 67 /Widths [600.25 -800.5 0] >>")
            .unwrap();
        for entry in ["", "/MissingWidth null", "/MissingWidth 123.5"] {
            let bytes = format!("<< {entry} >>");
            let descriptor = Dict::from_bytes(bytes.as_bytes()).unwrap();
            let (widths, missing) = read_widths(&dict, &descriptor).unwrap();
            assert_eq!(widths.len(), 256);
            for (code, expected) in [(65, 600.25), (66, -800.5), (67, 0.0)] {
                assert!(matches!(widths[code], Width::Value(value) if value == expected));
            }
            assert!(matches!(widths[64], Width::Missing));
            assert!(matches!(widths[255], Width::Missing));
            assert_eq!(missing, if entry.ends_with("123.5") { 123.5 } else { 0.0 });
        }
        for code in [0, 255] {
            let bytes = format!("<< /FirstChar {code} /LastChar {code} /Widths [500] >>");
            assert!(
                read_widths(
                    &Dict::from_bytes(bytes.as_bytes()).unwrap(),
                    &Dict::default()
                )
                .is_some()
            );
        }
    }

    #[test]
    fn simple_metrics_reject_partial_wrong_typed_and_unbounded_declarations() {
        for entry in [
            "/FirstChar 65 /LastChar 66 /Widths [600]",
            "/FirstChar 65 /LastChar 65 /Widths [600 /Bad]",
            "/FirstChar 65 /LastChar 66 /Widths [600 null]",
            "/FirstChar 65 /LastChar 66 /Widths [600 /Bad]",
            "/FirstChar 65 /LastChar 66 /Widths [600 999 0 R]",
            "/FirstChar 65 /LastChar 65 /Widths /Bad",
            "/FirstChar 65 /LastChar 65",
            "/FirstChar 65.0 /LastChar 65 /Widths [600]",
            "/FirstChar 65 /LastChar 65.5 /Widths [600]",
            "/FirstChar -1 /LastChar 0 /Widths [600 700]",
            "/FirstChar 66 /LastChar 65 /Widths []",
            "/FirstChar 256 /LastChar 256 /Widths [600]",
            "/FirstChar 4294967295 /LastChar 4294967295 /Widths [600]",
            "/FirstChar 0 /LastChar 4294967295 /Widths [600]",
        ] {
            let bytes = format!("<< {entry} >>");
            assert!(
                read_widths(
                    &Dict::from_bytes(bytes.as_bytes()).unwrap(),
                    &Dict::default()
                )
                .is_none(),
                "{entry}"
            );
        }
        for value in [
            "/Bad",
            "[500]",
            "99999999999999999999999999999999999999999999999999",
        ] {
            let bytes = format!("<< /MissingWidth {value} >>");
            assert!(
                read_widths(
                    &Dict::default(),
                    &Dict::from_bytes(bytes.as_bytes()).unwrap()
                )
                .is_none()
            );
        }
    }

    #[test]
    fn simple_metrics_standard_exception_requires_all_four_absent() {
        let entries = [
            "/FirstChar 65",
            "/LastChar 65",
            "/Widths [667]",
            "/FontDescriptor << >>",
        ];
        for mask in 0..16 {
            let mut input = String::from("<< /Subtype /Type1 /BaseFont /Helvetica ");
            for (bit, entry) in entries.iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    input.push_str(entry);
                    input.push(' ');
                }
            }
            input.push_str(">>");
            let valid =
                validate_simple_metrics(&Dict::from_bytes(input.as_bytes()).unwrap()).is_some();
            assert_eq!(valid, mask == 0 || mask == 15, "{input}");
        }
        for name in ["Arial", "HelveticaNeue", "ABCDEF+Helvetica"] {
            let input = format!("<< /Subtype /Type1 /BaseFont /{name} >>");
            assert!(
                validate_simple_metrics(&Dict::from_bytes(input.as_bytes()).unwrap()).is_none()
            );
        }
        for subtype in ["TrueType", "MMType1", "OpenType"] {
            let input = format!("<< /Subtype /{subtype} /BaseFont /Helvetica >>");
            assert!(
                validate_simple_metrics(&Dict::from_bytes(input.as_bytes()).unwrap()).is_none()
            );
        }
    }
}

#[cfg(test)]
mod unicode_fallback_tests {
    use super::{collect_cmap_unicodes, is_extractable_cmap_character, retain_unique_unicode};
    use hayro_cmap::BfString;
    use rustc_hash::FxHashMap;
    use skrifa::GlyphId;

    #[test]
    fn embedded_glyph_unicode_must_be_unambiguous() {
        let glyph = GlyphId::new(7);
        let mut mappings = FxHashMap::default();
        retain_unique_unicode(&mut mappings, glyph, BfString::Char('A'));
        retain_unique_unicode(&mut mappings, glyph, BfString::Char('A'));
        assert_eq!(mappings.get(&glyph), Some(&Some(BfString::Char('A'))));

        retain_unique_unicode(&mut mappings, glyph, BfString::Char('B'));
        assert_eq!(mappings.get(&glyph), Some(&None));

        retain_unique_unicode(&mut mappings, glyph, BfString::Char('A'));
        assert_eq!(mappings.get(&glyph), Some(&None));
    }

    #[test]
    fn cmap_fallback_rejects_nonsemantic_scalars() {
        assert!(is_extractable_cmap_character('A'));
        assert!(is_extractable_cmap_character('\u{1F642}'));
        assert!(!is_extractable_cmap_character('\0'));
        assert!(!is_extractable_cmap_character('\u{E000}'));
        assert!(!is_extractable_cmap_character('\u{FDD0}'));
        assert!(!is_extractable_cmap_character('\u{10FFFF}'));
    }

    #[test]
    fn cmap_fallback_is_discarded_when_its_scan_limit_is_exceeded() {
        let glyph = GlyphId::new(7);
        assert!(
            collect_cmap_unicodes([(u32::from('A'), glyph), (u32::from('B'), glyph)], 2).is_some()
        );
        assert!(
            collect_cmap_unicodes([(u32::from('A'), glyph), (u32::from('B'), glyph)], 1).is_none()
        );
    }
}
