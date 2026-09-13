use crate::CMapResolverFn;
use crate::context::Context;
use crate::device::Device;
use crate::font::glyph_simulator::GlyphSimulator;
use crate::font::true_type::read_encoding;
use crate::font::{
    Encoding, GlyphRun, Type3Glyph, UNITS_PER_EM, glyph_name_to_bf_string, read_to_unicode,
};
use crate::interpret::state::TextState;
use crate::x_object::soft_mask::SoftMask;
use crate::{BlendMode, interpret};
use crate::{CacheKey, ClipPath, DrawMode, DrawProps, ImageDrawProps};
use crate::{Image, Paint};
use hayro_cmap::{BfString, CMap};
use hayro_syntax::content::TypedIter;
use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::{
    CHAR_PROCS, FIRST_CHAR, FONT_BBOX, FONT_MATRIX, LAST_CHAR, RESOURCES, WIDTHS,
};
use hayro_syntax::object::{Array, Dict, Number};
use hayro_syntax::page::Resources;
use kurbo::{Affine, BezPath, Point, Rect};
use rustc_hash::FxHashMap;
use skrifa::GlyphId;

#[derive(Debug)]
pub(crate) struct Type3<'a> {
    widths: [f32; 256],
    encoding: Encoding,
    encodings: FxHashMap<u8, String>,
    dict: Dict<'a>,
    char_procs: FxHashMap<String, Stream<'a>>,
    glyph_simulator: GlyphSimulator,
    font_bbox: Rect,
    matrix: Affine,
    to_unicode: Option<CMap>,
}

impl<'a> Type3<'a> {
    pub(crate) fn new(dict: &Dict<'a>, cmap_resolver: &CMapResolverFn) -> Option<Self> {
        let (encoding, encodings) = read_encoding(dict);
        let widths = read_type3_widths(dict)?;
        let bbox = dict.get::<[f64; 4]>(FONT_BBOX)?;
        let coefficients = dict.get::<[f64; 6]>(FONT_MATRIX)?;
        if !bbox
            .iter()
            .chain(coefficients.iter())
            .all(|value| value.is_finite())
        {
            return None;
        }
        let font_bbox = Rect::new(
            bbox[0].min(bbox[2]),
            bbox[1].min(bbox[3]),
            bbox[0].max(bbox[2]),
            bbox[1].max(bbox[3]),
        );
        let matrix = Affine::new(coefficients);

        let char_procs = {
            let mut procs = FxHashMap::default();
            let dict = dict.get::<Dict<'_>>(CHAR_PROCS).unwrap_or_default();

            for name in dict.keys() {
                if let Some(prog) = dict.get::<Stream<'_>>(name.as_ref()) {
                    procs.insert(name.as_str().to_string(), prog);
                }
            }

            procs
        };

        let to_unicode = read_to_unicode(dict, cmap_resolver);

        Some(Self {
            glyph_simulator: GlyphSimulator::new(),
            encoding,
            font_bbox,
            char_procs,
            widths,
            encodings,
            matrix,
            dict: dict.clone(),
            to_unicode,
        })
    }

    pub(crate) fn map_code(&self, code: u8) -> GlyphId {
        encoded_glyph_name(&self.encoding, &self.encodings, code)
            .map(|g| self.glyph_simulator.string_to_glyph(g))
            .unwrap_or(GlyphId::NOTDEF)
    }

    pub(crate) fn glyph_width(&self, code: u8) -> f32 {
        let w = self.widths[code as usize];
        // Table 112 projects a Type 3 width onto the text-space x axis even
        // when FontMatrix rotates or skews the glyph. Translation is excluded.
        (w * self.matrix.as_coeffs()[0] as f32) * UNITS_PER_EM
    }

    pub(crate) fn glyph_nominal_quad(&self) -> [Point; 4] {
        let transform = Affine::scale(UNITS_PER_EM as f64) * self.matrix;
        [
            transform * Point::new(self.font_bbox.x0, self.font_bbox.y0),
            transform * Point::new(self.font_bbox.x1, self.font_bbox.y0),
            transform * Point::new(self.font_bbox.x1, self.font_bbox.y1),
            transform * Point::new(self.font_bbox.x0, self.font_bbox.y1),
        ]
    }

    pub(crate) fn has_known_bounds(&self) -> bool {
        self.font_bbox != Rect::ZERO
    }

    pub(crate) fn char_code_to_unicode(&self, char_code: u32) -> Option<BfString> {
        if let Some(unicode) = self
            .to_unicode
            .as_ref()
            .and_then(|t| t.lookup_bf_string(char_code))
        {
            return Some(unicode);
        }

        let code = u8::try_from(char_code).ok()?;
        encoded_glyph_name(&self.encoding, &self.encodings, code).and_then(glyph_name_to_bf_string)
    }

    pub(crate) fn render_glyph(
        &self,
        glyph: &Type3Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        paint: &Paint<'a>,
        device: &mut impl Device<'a>,
    ) -> Option<()> {
        let mut state = glyph.state.clone();
        let root_transform =
            transform * glyph_transform * Affine::scale(UNITS_PER_EM as f64) * self.matrix;
        state.ctm = root_transform;

        // Not sure if this is mentioned anywhere, but I do think we need to reset the text state
        // (though the graphics state itself should be preserved).
        state.text_state = TextState::default();

        let name = self.glyph_simulator.glyph_to_string(glyph.glyph_id)?;
        let program = self.char_procs.get(&name)?;
        let decoded = program.decoded().ok()?;
        let iter = TypedIter::new(decoded.as_ref());

        let is_shape_glyph = {
            let mut iter = iter.clone();
            let mut is_shape_glyph = true;

            while let Some(op) = iter.next() {
                match op {
                    TypedInstruction::ShapeGlyph(_) => {
                        break;
                    }
                    TypedInstruction::ColorGlyph(_) => {
                        is_shape_glyph = false;
                        break;
                    }
                    _ => {}
                }
            }

            is_shape_glyph || state.type3_shape_only
        };
        state.type3_shape_only = is_shape_glyph;

        // Context bounds are in the device coordinate system, including the
        // FontMatrix. An all-zero FontBBox is explicitly unknown (Table 112):
        // keep the inherited clip domain, including when a retaining device
        // interprets this instance in normalized glyph coordinates.
        let bounds = if self.has_known_bounds() {
            root_transform.transform_rect_bbox(self.font_bbox)
        } else {
            let outer = transform * glyph_transform;
            if outer == glyph.instance_transform {
                glyph.inherited_clip
            } else {
                let mapping = outer * glyph.instance_transform.inverse();
                if !mapping.as_coeffs().iter().all(|value| value.is_finite()) {
                    (glyph.settings.warning_sink)(crate::InterpreterWarning::UnsupportedFont);
                    return None;
                }
                mapping.transform_rect_bbox(glyph.inherited_clip)
            }
        };
        if ![bounds.x0, bounds.y0, bounds.x1, bounds.y1]
            .iter()
            .all(|value| value.is_finite())
        {
            (glyph.settings.warning_sink)(crate::InterpreterWarning::UnsupportedFont);
            return None;
        }
        let mut context = Context::new_with(
            state.ctm,
            bounds,
            &glyph.cache,
            glyph.xref,
            glyph.settings.clone(),
            state,
            glyph.nesting_depth,
        );

        let mut resources = Resources::from_parent(
            self.dict.get(RESOURCES).unwrap_or_default(),
            glyph.parent_resources.clone(),
        );

        // Technically not valid, but also support by Adobe Acrobat. See PDFBOX-5294.
        if let Some(procs_resources) = program.dict().get::<Dict<'_>>(RESOURCES) {
            resources = Resources::from_parent(procs_resources, resources);
        }

        if is_shape_glyph {
            let deferred_transfer_function = glyph
                .settings
                .defer_transfer_functions
                .then(|| glyph.state.graphics_state.transfer_function.clone())
                .flatten();
            let mut device =
                Type3ShapeGlyphDevice::new(device, paint.clone(), deferred_transfer_function);
            interpret(iter, &resources, &mut context, &mut device);
        } else {
            interpret(iter, &resources, &mut context, device);
        }

        Some(())
    }
}

fn read_type3_widths(dict: &Dict<'_>) -> Option<[f32; 256]> {
    let first = usize::from(u8::try_from(dict.get::<Number>(FIRST_CHAR)?.as_i64_exact()?).ok()?);
    let last = usize::from(u8::try_from(dict.get::<Number>(LAST_CHAR)?.as_i64_exact()?).ok()?);
    let count = last.checked_sub(first)?.checked_add(1)?;
    let array = dict.get::<Array<'_>>(WIDTHS)?;
    // Bound work and storage before decoding; a valid prefix never supplies
    // defaults for missing, unreadable, or trailing entries.
    if array.raw_iter().take(count + 1).count() != count {
        return None;
    }
    let values = Vec::<f32>::try_from(array).ok()?;
    if values.len() != count || !values.iter().all(|value| value.is_finite()) {
        return None;
    }
    // Table 112 defines zero outside FirstChar..=LastChar. MissingWidth is a
    // metric for other simple fonts and does not override this Type 3 rule.
    let mut widths = [0.0; 256];
    widths[first..=last].copy_from_slice(&values);
    Some(widths)
}

fn encoded_glyph_name<'a>(
    encoding: &Encoding,
    differences: &'a FxHashMap<u8, String>,
    code: u8,
) -> Option<&'a str> {
    differences
        .get(&code)
        .map(String::as_str)
        .or_else(|| encoding.map_code(code))
}

impl CacheKey for Type3<'_> {
    fn cache_key(&self) -> u128 {
        self.dict.cache_key()
    }
}

struct Type3ShapeGlyphDevice<'a, 'b, T: Device<'a>> {
    inner: &'b mut T,
    paint: Paint<'a>,
    deferred_transfer_function: Option<crate::ActiveTransferFunction>,
}

impl<'a, 'b, T: Device<'a>> Type3ShapeGlyphDevice<'a, 'b, T> {
    pub(crate) fn new(
        device: &'b mut T,
        paint: Paint<'a>,
        deferred_transfer_function: Option<crate::ActiveTransferFunction>,
    ) -> Self {
        Self {
            inner: device,
            paint,
            deferred_transfer_function,
        }
    }
}

// Every mark in an uncolored program uses the selecting text paint. Nested
// Type 3 invocations also retain type3_shape_only in their interpreter state.
impl<'a, T: Device<'a>> Device<'a> for Type3ShapeGlyphDevice<'a, '_, T> {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        let props = DrawProps {
            paint: self.paint.clone(),
            deferred_transfer_function: self.deferred_transfer_function.clone(),
            ..props
        };
        self.inner.draw_path(path, props, draw_mode);
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        self.inner.push_clip_path(clip_path);
    }

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'_>>, _: BlendMode) {}

    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        draw_mode: &DrawMode,
    ) {
        let props = DrawProps {
            paint: self.paint.clone(),
            deferred_transfer_function: self.deferred_transfer_function.clone(),
            ..props
        };
        self.inner.draw_glyph_run(glyph_run, props, draw_mode);
    }

    fn record_glyph_run(&mut self, glyph_run: &GlyphRun<'_, 'a>, transform: Affine, visible: bool) {
        self.inner.record_glyph_run(glyph_run, transform, visible);
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn begin_marked_content(&mut self, tag: &[u8], mcid: Option<i32>) {
        self.inner.begin_marked_content(tag, mcid);
    }

    fn begin_marked_content_with_properties(
        &mut self,
        tag: &[u8],
        properties: crate::MarkedContentProperties,
    ) {
        self.inner
            .begin_marked_content_with_properties(tag, properties);
    }

    fn end_marked_content(&mut self) {
        self.inner.end_marked_content();
    }

    fn begin_optional_content(&mut self, expression: &crate::OptionalContentExpression) {
        self.inner.begin_optional_content(expression);
    }

    fn end_optional_content(&mut self) {
        self.inner.end_optional_content();
    }

    fn begin_text_object(&mut self, text_knockout: bool) {
        self.inner.begin_text_object(text_knockout);
    }

    fn end_text_object(&mut self) {
        self.inner.end_text_object();
    }

    fn begin_combined_fill_stroke(&mut self) {
        self.inner.begin_combined_fill_stroke();
    }

    fn end_combined_fill_stroke(&mut self) {
        self.inner.end_combined_fill_stroke();
    }

    fn pop_clip(&mut self) {
        self.inner.pop_clip();
    }

    fn pop_transparency_group(&mut self) {}

    fn draw_image(&mut self, image: Image<'a, '_>, mut props: ImageDrawProps<'a>) {
        if let Image::Stencil(mut s) = image {
            s.paint = self.paint.clone();
            props.deferred_transfer_function = self.deferred_transfer_function.clone();
            self.inner.draw_image(Image::Stencil(s), props);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Type3, encoded_glyph_name, read_type3_widths};
    use crate::CMapResolverFn;
    use crate::font::Encoding;
    use hayro_syntax::object::{Array, Dict, FromBytes};
    use rustc_hash::FxHashMap;
    use std::sync::Arc;

    #[test]
    fn graphics_state_font_pairs_require_complete_reads() {
        for bytes in [b"[<< >> 12]".as_slice(), b"[<< >> -1.5]", b"[<< >> 0]"] {
            assert!(
                crate::font::read_ext_g_state_font(Array::from_bytes(bytes).unwrap()).is_some()
            );
        }
        for bytes in [
            b"[<< >> 12 /Extra]".as_slice(),
            b"[<< >> 12 null]",
            b"[<< >>]",
            b"[null 12]",
            b"[999 0 R 12]",
            b"[<< >> null]",
            b"[<< >> 999 0 R]",
            b"[<< >> /Bad]",
            b"[<< >> 1000000000000000000000000000000000000000]",
        ] {
            assert!(
                crate::font::read_ext_g_state_font(Array::from_bytes(bytes).unwrap()).is_none(),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn type3_widths_are_complete_bounded_and_zero_outside_the_range() {
        let dict = Dict::from_bytes(
            b"<< /FirstChar 65 /LastChar 66 /Widths [600.25 -900.5] /MissingWidth 1234 >>",
        )
        .unwrap();
        let widths = read_type3_widths(&dict).unwrap();
        assert_eq!(widths[65..=66], [600.25, -900.5]);
        assert_eq!(widths[64], 0.0);
        assert_eq!(widths[67], 0.0);
        for bytes in [
            b"<< /FirstChar 65 /LastChar 66 /Widths [600] >>".as_slice(),
            b"<< /FirstChar 65 /LastChar 66 /Widths [600 900 /Bad] >>",
            b"<< /FirstChar 65 /LastChar 66 /Widths [600 /Bad] >>",
            b"<< /FirstChar 65 /LastChar 66 /Widths [600 null] >>",
            b"<< /FirstChar 65 /LastChar 66 /Widths [600 999 0 R] >>",
            b"<< /FirstChar 256 /LastChar 256 /Widths [600] >>",
            b"<< /FirstChar 4294967295 /LastChar 4294967295 /Widths [600] >>",
            b"<< /FirstChar -1 /LastChar 0 /Widths [600 900] >>",
            b"<< /FirstChar 66 /LastChar 65 /Widths [] >>",
            b"<< /FirstChar 65.5 /LastChar 65 /Widths [600] >>",
            b"<< /FirstChar 65 /LastChar 66 >>",
        ] {
            assert!(
                read_type3_widths(&Dict::from_bytes(bytes).unwrap()).is_none(),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn affine_type3_geometry_keeps_horizontal_projected_advances() {
        let resolver: CMapResolverFn = Arc::new(|_| None);
        let dict = Dict::from_bytes(b"<< /FirstChar 65 /LastChar 65 /Widths [600] /FontBBox [600 700 0 0] /FontMatrix [.0006 .0008 -.0008 .0006 .1 -.2] >>").unwrap();
        let font = Type3::new(&dict, &resolver).unwrap();
        assert!((font.glyph_width(65) - 360.0).abs() < 0.0001);
        assert_eq!(font.glyph_width(66), 0.0);
        for (actual, expected) in font.glyph_nominal_quad().iter().zip([
            (100.0, -200.0),
            (460.0, 280.0),
            (-100.0, 700.0),
            (-460.0, 220.0),
        ]) {
            assert!((actual.x - expected.0).abs() < 1e-9);
            assert!((actual.y - expected.1).abs() < 1e-9);
        }
    }

    #[test]
    fn type3_geometry_never_defaults_after_failed_required_reads() {
        let resolver: CMapResolverFn = Arc::new(|_| None);
        for geometry in [
            "/FontBBox [0 0 600 700]",
            "/FontMatrix [.001 0 0 .001 0 0]",
            "/FontBBox [0 0 600 700 /Bad] /FontMatrix [.001 0 0 .001 0 0]",
            "/FontBBox [0 0 600 700] /FontMatrix [.001 0 0 .001 0 0 /Bad]",
            "/FontBBox [0 0 600 700] /FontMatrix null",
        ] {
            let bytes = format!("<< /FirstChar 65 /LastChar 65 /Widths [600] {geometry} >>");
            assert!(Type3::new(&Dict::from_bytes(bytes.as_bytes()).unwrap(), &resolver).is_none());
        }
    }

    #[test]
    fn differences_override_the_base_encoding() {
        let mut differences = FxHashMap::default();
        differences.insert(65, "B".to_string());

        assert_eq!(
            encoded_glyph_name(&Encoding::WinAnsi, &differences, 65),
            Some("B")
        );
        assert_eq!(
            encoded_glyph_name(&Encoding::WinAnsi, &differences, 32),
            Some("space")
        );
    }
}
