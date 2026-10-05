use crate::font::GlyphRun;
use crate::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};
use hayro_syntax::Pdf;
use kurbo::{Affine, BezPath, PathEl, Rect, Shape};

#[derive(Default)]
struct Recorder {
    paths: Vec<BezPath>,
    rectangles: usize,
    compound: usize,
}
impl<'a> Device<'a> for Recorder {
    fn draw_path(&mut self, path: &BezPath, _: DrawProps<'a>, _: &DrawMode) {
        self.paths.push(path.clone());
    }
    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, mode: &DrawMode) {
        self.rectangles += 1;
        self.draw_path(&rect.to_path(0.1), props, mode);
    }
    fn begin_combined_fill_stroke(&mut self) {
        self.compound += 1;
    }
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}
fn record(content: &str) -> Recorder {
    let bytes = format!("%PDF-1.7\n1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<<>>/Contents 4 0 R>> endobj\n4 0 obj <</Length {}>> stream\n{content}\nendstream endobj\ntrailer <</Root 1 0 R>>\n%%EOF",content.len()).into_bytes();
    let pdf = Pdf::new(bytes).expect("contour PDF");
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        Affine::IDENTITY,
        Rect::new(0.0, 0.0, 100.0, 100.0),
        &cache,
        pdf.xref(),
        InterpreterSettings::default(),
    );
    let mut recorder = Recorder::default();
    interpret_page(&pdf.pages()[0], &mut context, &mut recorder);
    recorder
}
#[test]
fn rectangle_strokes_preserve_source_traversal_and_explicit_closure() {
    for closed in [false, true] {
        let mut expected = BezPath::new();
        expected.move_to((80.0, 68.0));
        expected.line_to((80.0, 20.0));
        expected.line_to((20.0, 20.0));
        expected.line_to((20.0, 68.0));
        let ending = if closed {
            expected.close_path();
            "h"
        } else {
            expected.line_to((80.0, 68.0));
            "80 68 l"
        };
        for paint in ["S", "B", "B*"] {
            let recorder = record(&format!(
                "[11 5 3 7] 2.5 d 6 w 80 68 m 80 20 l 20 20 l 20 68 l {ending} {paint}"
            ));
            assert_eq!(recorder.rectangles, 0);
            assert_eq!(recorder.compound, usize::from(paint != "S"));
            assert_eq!(recorder.paths.len(), if paint == "S" { 1 } else { 2 });
            for path in recorder.paths {
                assert_eq!(path.elements(), expected.elements());
            }
        }
    }
}
#[test]
fn signed_rectangle_operators_keep_their_start_and_orientation_when_stroked() {
    for (width, height) in [(60, 48), (-60, 48), (60, -48), (-60, -48)] {
        let recorder = record(&format!("[3 7] 1 d 80 68 {width} {height} re S"));
        assert_eq!(recorder.rectangles, 0);
        assert_eq!(
            recorder.paths[0].elements(),
            &[
                PathEl::MoveTo((80.0, 68.0).into()),
                PathEl::LineTo((f64::from(80 + width), 68.0).into()),
                PathEl::LineTo((f64::from(80 + width), f64::from(68 + height)).into()),
                PathEl::LineTo((80.0, f64::from(68 + height)).into()),
                PathEl::ClosePath,
            ]
        );
    }
}
#[test]
fn fill_only_rectangles_keep_the_rectangle_device_shortcut() {
    let recorder = record("80 68 m 80 20 l 20 20 l 20 68 l h f");
    assert_eq!(recorder.rectangles, 1);
    assert_eq!(recorder.compound, 0);
}
