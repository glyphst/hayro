use crate::color::AlphaColor;
use crate::font::{Glyph, GlyphRun};
use crate::pattern::Pattern;
use crate::{
    ActiveTransferFunction, BlendMode, CacheKey, ClipPath, Context, Device, DrawMode, DrawProps,
    FormInvocation, Image, ImageData, ImageDrawProps, InterpreterCache, InterpreterSettings, Paint,
    SharedInterpreterCache, SoftMask, interpret_page,
};
use hayro_syntax::Pdf;
use kurbo::{Affine, BezPath, Point, Rect};
use std::sync::Arc;

const STATES: &str = "/ExtGState << /Inverse << /TR 5 0 R >> /Square << /TR 6 0 R >> /Identity << /TR /Identity >> /Zero << /ca 0 /CA 0 >> /ShapeZero << /ca 0 /CA 0 /AIS true >> /Alpha << /ca 0.5 /CA 0.75 >> >>";
const PAINT: &str = "0.25 g 0 0 8 8 re f";

fn stream(entries: &str, data: &[u8]) -> Vec<u8> {
    let mut result = format!("<< {entries} /Length {} >>\nstream\n", data.len()).into_bytes();
    result.extend_from_slice(data);
    result.extend_from_slice(b"\nendstream");
    result
}

fn pdf(resources: &str, content: &str, extra: Vec<Vec<u8>>) -> Pdf {
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 32 32] /Resources << {STATES} {resources} >> /Contents 4 0 R >>").into_bytes(),
        stream("", content.as_bytes()),
        b"<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>".to_vec(),
        b"<< /FunctionType 2 /Domain [0 1] /N 2 >>".to_vec(),
    ];
    objects.extend(extra);
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in &offsets[1..] {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    Pdf::new(bytes).expect("independent transfer PDF")
}

#[derive(Clone, Debug)]
struct Mark {
    color: [f32; 4],
    transfer: Option<ActiveTransferFunction>,
    alpha: f32,
    ais: bool,
}

impl Mark {
    fn transferred(&self) -> [f32; 4] {
        self.transfer.as_ref().map_or(self.color, |function| {
            function
                .apply(&AlphaColor::new(self.color))
                .unwrap()
                .components()
        })
    }
}

#[derive(Default)]
struct Capture {
    marks: Vec<Mark>,
    groups: Vec<(f32, bool, bool, BlendMode)>,
    keys: Vec<(&'static str, u128)>,
    image_bytes: Vec<Vec<u8>>,
    gradient_colors: Vec<[f32; 4]>,
}

impl Capture {
    fn paint<'a>(
        &mut self,
        paint: Paint<'a>,
        transfer: Option<ActiveTransferFunction>,
        alpha: f32,
        ais: bool,
        stroke: bool,
    ) {
        match paint {
            Paint::Color(color) => self.marks.push(Mark {
                color: color.to_rgba().components(),
                transfer,
                alpha,
                ais,
            }),
            Paint::Pattern(pattern) => {
                assert!(transfer.is_none(), "pattern owns its selected transfer");
                self.keys.push(("pattern", pattern.cache_key()));
                match *pattern {
                    Pattern::Tiling(pattern) => {
                        pattern.interpret(self, Affine::IDENTITY, stroke).unwrap();
                    }
                    Pattern::Shading(pattern) => {
                        let encoded = pattern.encode();
                        self.marks.push(Mark {
                            color: encoded.sample(Point::new(0.5, 0.5)),
                            transfer: encoded.deferred_transfer_function().cloned(),
                            alpha,
                            ais,
                        });
                        if let crate::encode::EncodedShadingType::RadialAxial(shading) =
                            &encoded.shading_type
                        {
                            let gradient = shading
                                .as_svg_gradient(&encoded, Rect::new(0.0, 0.0, 1.0, 1.0), 0.001)
                                .unwrap();
                            self.gradient_colors
                                .extend(gradient.stops.iter().map(|stop| stop.color));
                        }
                    }
                }
            }
        }
    }
}

impl<'a> Device<'a> for Capture {
    fn draw_path(&mut self, _: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        self.paint(
            props.paint,
            props.deferred_transfer_function,
            props.alpha_constant,
            props.alpha_is_shape,
            matches!(mode, DrawMode::Stroke(_)),
        );
    }
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn pop_clip(&mut self) {}
    fn push_transparency_group(
        &mut self,
        alpha: f32,
        mask: Option<SoftMask<'a>>,
        blend: BlendMode,
    ) {
        self.push_transparency_group_with_alpha_source(alpha, mask, blend, false);
    }
    fn push_transparency_group_with_alpha_source(
        &mut self,
        alpha: f32,
        mask: Option<SoftMask<'a>>,
        blend: BlendMode,
        ais: bool,
    ) {
        if let Some(mask) = &mask {
            self.keys.push(("soft-mask", mask.cache_key()));
        }
        self.groups.push((alpha, ais, mask.is_some(), blend));
    }
    fn pop_transparency_group(&mut self) {}
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for positioned in run.glyphs() {
            match &**positioned {
                Glyph::Type3(glyph) => {
                    self.keys.push(("glyph", glyph.cache_key()));
                    glyph.interpret(self, props.transform, positioned.transform(), &props.paint);
                }
                Glyph::Outline(_) => self.paint(
                    props.paint.clone(),
                    props.deferred_transfer_function.clone(),
                    props.alpha_constant,
                    props.alpha_is_shape,
                    false,
                ),
            }
        }
    }
    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        match image {
            Image::Raster(image) => {
                self.keys.push(("image", image.cache_key()));
                image.with_rgba(
                    |data, _| {
                        let bytes = match data {
                            ImageData::Rgb(data) => data.data,
                            ImageData::Luma(data) => data.data,
                        };
                        self.marks.push(Mark {
                            color: [f32::from(bytes[0]) / 255.0; 4],
                            transfer: props.deferred_transfer_function.clone(),
                            alpha: 1.0,
                            ais: props.alpha_is_shape,
                        });
                        self.image_bytes.push(bytes);
                    },
                    None,
                );
            }
            Image::Stencil(image) => image.with_stencil(
                |_, paint| {
                    self.paint(
                        paint.clone(),
                        props.deferred_transfer_function,
                        1.0,
                        props.alpha_is_shape,
                        false,
                    );
                },
                None,
            ),
        }
    }
    fn draw_form(&mut self, form: &FormInvocation<'a>) {
        self.keys.push(("form", form.cache_key()));
        form.interpret_local(self);
    }
}

fn capture(pdf: &Pdf, deferred: bool, shared: &SharedInterpreterCache) -> Capture {
    let cache = InterpreterCache::from_shared(shared);
    let mut context = Context::new(
        Affine::IDENTITY,
        Rect::new(0.0, 0.0, 32.0, 32.0),
        &cache,
        pdf.xref(),
        InterpreterSettings {
            defer_transfer_functions: deferred,
            warning_sink: Arc::new(|warning| panic!("{warning:?}")),
            ..InterpreterSettings::default()
        },
    );
    let mut result = Capture::default();
    interpret_page(&pdf.pages()[0], &mut context, &mut result);
    result
}

fn colors(marks: &[Mark]) -> Vec<f32> {
    marks.iter().map(|mark| mark.color[0]).collect()
}
fn transferred(marks: &[Mark]) -> Vec<f32> {
    marks.iter().map(|mark| mark.transferred()[0]).collect()
}

#[test]
fn raw_paints_keep_function_state_save_restore_and_zero_opacity() {
    let pdf = pdf(
        "",
        &format!(
            "/Inverse gs {PAINT} q /Identity gs {PAINT} Q {PAINT} /Square gs {PAINT} /Zero gs {PAINT} /ShapeZero gs {PAINT}"
        ),
        vec![],
    );
    let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
    let normal = capture(&pdf, false, &shared);
    let deferred = capture(&pdf, true, &shared);
    assert_eq!(
        colors(&normal.marks),
        [0.75, 0.25, 0.75, 0.0625, 0.0625, 0.0625]
    );
    assert_eq!(colors(&deferred.marks), [0.25; 6]);
    assert_eq!(transferred(&deferred.marks), colors(&normal.marks));
    assert!(normal.marks.iter().all(|mark| mark.transfer.is_none()));
    assert_eq!(deferred.marks[4].alpha, 0.0);
    assert!(!deferred.marks[4].ais);
    assert_eq!(deferred.marks[5].alpha, 0.0);
    assert!(deferred.marks[5].ais);
    drop(pdf);
    assert_eq!(
        deferred.marks[0].transferred()[0],
        0.75,
        "function outlives parser and cache handles"
    );
}

#[test]
fn stroke_and_fill_keep_independent_alpha_with_the_same_raw_transfer() {
    let pdf = pdf(
        "",
        "/Inverse gs /Alpha gs 0.25 g 0.25 G 0 0 8 8 re B",
        vec![],
    );
    let result = capture(
        &pdf,
        true,
        &SharedInterpreterCache::new(crate::InterpreterCacheLimits::default()),
    );
    assert_eq!(colors(&result.marks), [0.25, 0.25]);
    assert_eq!(
        result.marks.iter().map(|m| m.alpha).collect::<Vec<_>>(),
        [0.5, 0.75]
    );
    assert_eq!(transferred(&result.marks), [0.75, 0.75]);
}

#[test]
fn shared_image_reuse_keeps_raw_samples_and_separate_selected_functions() {
    let pdf = pdf(
        "/XObject << /I 7 0 R >>",
        "/Inverse gs /I Do /Identity gs /I Do /Square gs /I Do",
        vec![stream(
            "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Interpolate true",
            &[0, 255],
        )],
    );
    let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
    let normal = capture(&pdf, false, &shared);
    let deferred = capture(&pdf, true, &shared);
    let normal_again = capture(&pdf, false, &shared);
    assert_eq!(
        normal.image_bytes,
        [vec![255, 0], vec![0, 255], vec![0, 255]]
    );
    assert_eq!(normal_again.image_bytes, normal.image_bytes);
    assert_eq!(deferred.image_bytes, vec![vec![0, 255]; 3]);
    let keys = deferred
        .keys
        .iter()
        .filter(|(kind, _)| *kind == "image")
        .map(|(_, key)| *key)
        .collect::<Vec<_>>();
    assert!(
        keys.windows(2).all(|pair| pair[0] == pair[1]),
        "raw bytes may share despite distinct transfer state"
    );
    let midpoint = AlphaColor::new([0.5; 4]);
    assert_eq!(
        deferred.marks[2]
            .transfer
            .as_ref()
            .unwrap()
            .apply(&midpoint)
            .unwrap()
            .components()[0],
        0.25,
        "transfer is available after interpolation"
    );
}

#[test]
fn nested_group_invocation_opacity_is_separate_from_elementary_transfer() {
    let resources = format!("{STATES} /XObject << /Child 8 0 R >>");
    let outer = stream(
        &format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 8 8] /Resources << {resources} >> /Group << /S /Transparency /CS /DeviceRGB /I true >>"
        ),
        b"/Square gs /Child Do",
    );
    let inner = stream(
        &format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 8 8] /Resources << {STATES} >> /Group << /S /Transparency /CS /DeviceRGB /I true >>"
        ),
        format!("/Inverse gs {PAINT}").as_bytes(),
    );
    let pdf = pdf(
        "/XObject << /Outer 7 0 R >>",
        "/Alpha gs /Outer Do",
        vec![outer, inner],
    );
    let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
    let normal = capture(&pdf, false, &shared);
    let deferred = capture(&pdf, true, &shared);
    assert_eq!(
        deferred.groups,
        [
            (0.5, false, false, BlendMode::Normal),
            (1.0, false, false, BlendMode::Normal)
        ]
    );
    assert_eq!(colors(&deferred.marks), [0.25]);
    assert_eq!(transferred(&deferred.marks), [0.75]);
    assert_eq!(deferred.marks[0].alpha, 1.0);
    assert_ne!(
        normal.keys, deferred.keys,
        "subscenes must not alias across transfer modes"
    );
}

fn pattern_pdf(colored: bool, in_form: bool, cell: &str) -> Pdf {
    let selection = if colored {
        "/Pattern cs /P scn"
    } else {
        "/PCS cs 0.25 /P scn"
    };
    let resources =
        format!("{STATES} /Pattern << /P 7 0 R >> /ColorSpace << /PCS [/Pattern /DeviceGray] >>");
    let pattern = stream(
        &format!(
            "/Type /Pattern /PatternType 1 /PaintType {} /TilingType 1 /BBox [0 0 8 8] /XStep 8 /YStep 8 /Resources << {STATES} >>",
            if colored { 1 } else { 2 }
        ),
        cell.as_bytes(),
    );
    if in_form {
        let form = stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 8 8] /Resources << {resources} >>"),
            format!("/Square gs {selection} 0 0 8 8 re f").as_bytes(),
        );
        pdf(
            "/XObject << /F 8 0 R >>",
            "/Inverse gs /F Do",
            vec![pattern, form],
        )
    } else {
        pdf(
            "/Pattern << /P 7 0 R >> /ColorSpace << /PCS [/Pattern /DeviceGray] >>",
            &format!("/Inverse gs {selection} 0 0 8 8 re f /Square gs 0 0 8 8 re f"),
            vec![pattern],
        )
    }
}

#[test]
fn colored_pattern_keeps_parent_initial_transfer_and_uncolored_uses_selection() {
    for (colored, in_form, expected) in [
        (true, false, vec![0.25, 0.25]),
        (true, true, vec![0.75]),
        (false, false, vec![0.75, 0.0625]),
        (false, true, vec![0.0625]),
    ] {
        let pdf = pattern_pdf(
            colored,
            in_form,
            if colored { PAINT } else { "0 0 8 8 re f" },
        );
        let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
        let normal = capture(&pdf, false, &shared);
        let deferred = capture(&pdf, true, &shared);
        assert_eq!(
            colors(&normal.marks),
            expected,
            "default colored={colored} in_form={in_form}"
        );
        assert_eq!(colors(&deferred.marks), vec![0.25; expected.len()]);
        assert_eq!(
            transferred(&deferred.marks),
            expected,
            "deferred colored={colored} in_form={in_form}"
        );
        assert_ne!(normal.keys, deferred.keys);
    }
}

#[test]
fn mixed_pattern_cell_retains_zero_opacity_marks_for_later_eligibility() {
    let pdf = pattern_pdf(true, true, &format!("{PAINT} /Zero gs {PAINT}"));
    let result = capture(
        &pdf,
        true,
        &SharedInterpreterCache::new(crate::InterpreterCacheLimits::default()),
    );
    assert_eq!(colors(&result.marks), [0.25, 0.25]);
    assert_eq!(transferred(&result.marks), [0.75, 0.75]);
    assert_eq!(
        result.marks.iter().map(|m| m.alpha).collect::<Vec<_>>(),
        [1.0, 0.0]
    );
}

#[test]
fn direct_and_pattern_shading_sampling_keeps_raw_colors_and_functions() {
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Extend [true true] /Function << /FunctionType 2 /Domain [0 1] /C0 [0.25] /C1 [0.25] /N 1 >> >>";
    for (resource, content) in [
        (format!("/Shading << /S {shading} >>"), "/Inverse gs /S sh"),
        (
            format!(
                "/Pattern << /P << /PatternType 2 /Shading {shading} /ExtGState << /TR 5 0 R >> >> >>"
            ),
            "/Square gs /Pattern cs /P scn 0 0 8 8 re f",
        ),
    ] {
        let pdf = pdf(&resource, content, vec![]);
        let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
        let normal = capture(&pdf, false, &shared);
        let deferred = capture(&pdf, true, &shared);
        assert_eq!(colors(&normal.marks), [0.75]);
        assert_eq!(colors(&deferred.marks), [0.25]);
        assert_eq!(transferred(&deferred.marks), [0.75]);
        assert!(normal.gradient_colors.iter().all(|color| color[0] == 0.75));
        assert!(
            deferred
                .gradient_colors
                .iter()
                .all(|color| color[0] == 0.25)
        );
        assert_ne!(normal.keys, deferred.keys);
    }
}

#[test]
fn uncolored_stencil_cells_keep_the_selecting_transfer() {
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ImageMask true /BitsPerComponent 1",
        &[0],
    );
    let pattern = stream(
        "/Type /Pattern /PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 8 8] /XStep 8 /YStep 8 /Resources << /XObject << /I 7 0 R >> >>",
        b"/I Do",
    );
    let pdf = pdf(
        "/Pattern << /P 8 0 R >> /ColorSpace << /PCS [/Pattern /DeviceGray] >>",
        "/Inverse gs /PCS cs 0.25 /P scn 0 0 8 8 re f",
        vec![image, pattern],
    );
    let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
    assert_eq!(colors(&capture(&pdf, false, &shared).marks), [0.75]);
    let result = capture(&pdf, true, &shared);
    assert_eq!(colors(&result.marks), [0.25]);
    assert_eq!(transferred(&result.marks), [0.75]);
}

#[test]
fn pattern_colored_stencil_uses_cell_transfer_not_the_current_selection() {
    let image = stream(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ImageMask true /BitsPerComponent 1",
        &[0],
    );
    let pattern = stream(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 8 8] /XStep 8 /YStep 8 /Resources << >>",
        PAINT.as_bytes(),
    );
    let pdf = pdf(
        "/XObject << /I 7 0 R >> /Pattern << /P 8 0 R >>",
        "/Inverse gs /Pattern cs /P scn /I Do",
        vec![image, pattern],
    );
    let result = capture(
        &pdf,
        true,
        &SharedInterpreterCache::new(crate::InterpreterCacheLimits::default()),
    );
    assert_eq!(colors(&result.marks), [0.25]);
    assert!(result.marks[0].transfer.is_none());
}

#[test]
fn type3_color_and_shape_glyphs_retain_raw_paint_and_selected_transfer() {
    for shape in [false, true] {
        let font = b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 8 8] /FontMatrix [1 0 0 1 0 0] /FirstChar 65 /LastChar 65 /Widths [8] /Encoding << /Type /Encoding /Differences [65 /A] >> /CharProcs << /A 8 0 R >> /Resources << >> >>".to_vec();
        let program = stream(
            "",
            if shape {
                b"8 0 0 0 8 8 d1 0 0 8 8 re f"
            } else {
                b"8 0 d0 0.25 g 0 0 8 8 re f"
            },
        );
        let pdf = pdf(
            "/Font << /F 7 0 R >>",
            "/Inverse gs 0.25 g BT /F 1 Tf (A) Tj ET /Square gs BT /F 1 Tf (A) Tj ET",
            vec![font, program],
        );
        let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
        let normal = capture(&pdf, false, &shared);
        let deferred = capture(&pdf, true, &shared);
        assert_eq!(colors(&normal.marks), [0.75, 0.0625]);
        assert_eq!(colors(&deferred.marks), [0.25, 0.25]);
        assert_eq!(transferred(&deferred.marks), [0.75, 0.0625]);
        assert_ne!(normal.keys, deferred.keys);
    }
}

#[test]
fn direct_color_conversion_preserves_a_non_byte_aligned_transfer_boundary() {
    let function = "<< /FunctionType 3 /Domain [0 1] /Range [0 1] /Functions [<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >> << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [1] /N 1 >>] /Bounds [0.499] /Encode [0 1 0 1] >>";
    for color in ["0.4995 g", "0.4995 0.4995 0.4995 rg"] {
        // The old color conversion rounded this input to 127/255, crossing the
        // 0.499 boundary before the real-valued transfer evaluator saw it.
        let pdf = pdf(
            "/XObject << /F 7 0 R >>",
            "/F Do",
            vec![stream(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 8 8] /Resources << /ExtGState << /T << /TR {function} >> >> >>"
                ),
                format!("/T gs {color} 0 0 8 8 re f").as_bytes(),
            )],
        );
        let shared = SharedInterpreterCache::new(crate::InterpreterCacheLimits::default());
        let normal = capture(&pdf, false, &shared);
        let deferred = capture(&pdf, true, &shared);
        assert_eq!(colors(&normal.marks), [1.0]);
        assert_eq!(colors(&deferred.marks), [0.4995]);
        assert_eq!(transferred(&deferred.marks), [1.0]);
    }
}

#[test]
fn inline_images_preserve_selected_transfer_and_raw_samples() {
    let pdf = pdf(
        "",
        "/Square gs BI /W 2 /H 1 /BPC 8 /CS /G /F /AHx /I true ID 00FF> EI",
        vec![],
    );
    let result = capture(
        &pdf,
        true,
        &SharedInterpreterCache::new(crate::InterpreterCacheLimits::default()),
    );
    assert_eq!(result.image_bytes, [vec![0, 255]]);
    assert_eq!(
        result.marks[0]
            .transfer
            .as_ref()
            .unwrap()
            .apply(&AlphaColor::new([0.5; 4]))
            .unwrap()
            .components()[0],
        0.25
    );
}
