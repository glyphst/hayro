use crate::CMapResolverFn;
use crate::context::Context;
use crate::device::Device;
use crate::font::glyph_simulator::GlyphSimulator;
use crate::font::true_type::{Width, read_encoding, read_widths};
use crate::font::{
    Encoding, GlyphRun, Type3Glyph, UNITS_PER_EM, glyph_name_to_bf_string, read_to_unicode,
};
use crate::interpret::state::TextState;
use crate::util::RectExt;
use crate::x_object::soft_mask::SoftMask;
use crate::{BlendMode, interpret};
use crate::{CacheKey, ClipPath, DrawMode, DrawProps, ImageDrawProps};
use crate::{Image, Paint};
use hayro_cmap::{BfString, CMap};
use hayro_syntax::content::TypedIter;
use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::object::Dict;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::{CHAR_PROCS, FONT_BBOX, FONT_MATRIX, RESOURCES};
use hayro_syntax::page::Resources;
use kurbo::{Affine, BezPath, Point, Rect};
use rustc_hash::FxHashMap;
use skrifa::GlyphId;

#[derive(Debug)]
pub(crate) struct Type3<'a> {
    widths: Vec<Width>,
    missing_width: f32,
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
        let (widths, missing_width) = read_widths(dict, dict)?;
        let font_bbox = dict
            .get::<hayro_syntax::object::Rect>(FONT_BBOX)
            .unwrap_or(hayro_syntax::object::Rect::ZERO)
            .to_kurbo();

        let matrix = Affine::new(
            dict.get::<[f64; 6]>(FONT_MATRIX)
                .unwrap_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]),
        );

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
            missing_width,
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
        let w = match self.widths.get(code as usize).copied() {
            Some(Width::Value(w)) => w,
            _ => self.missing_width,
        };
        (w * self.matrix.as_coeffs()[0] as f32) * UNITS_PER_EM
    }

    pub(crate) fn glyph_nominal_quad(&self) -> [Point; 4] {
        let transform = self.matrix * Affine::scale(UNITS_PER_EM as f64);
        [
            transform * Point::new(self.font_bbox.x0, self.font_bbox.y0),
            transform * Point::new(self.font_bbox.x1, self.font_bbox.y0),
            transform * Point::new(self.font_bbox.x1, self.font_bbox.y1),
            transform * Point::new(self.font_bbox.x0, self.font_bbox.y1),
        ]
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
            transform * glyph_transform * self.matrix * Affine::scale(UNITS_PER_EM as f64);
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

            is_shape_glyph
        };

        let mut context = Context::new_with(
            state.ctm,
            self.font_bbox,
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

// Only filling, stroking of paths and stencil masks are allowed.
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
        self.inner.draw_glyph_run(glyph_run, props, draw_mode);
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
    use super::encoded_glyph_name;
    use crate::font::Encoding;
    use rustc_hash::FxHashMap;

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
