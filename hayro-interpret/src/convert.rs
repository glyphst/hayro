use hayro_syntax::content::ops::Transform;

pub(crate) fn convert_transform(t: Transform) -> kurbo::Affine {
    kurbo::Affine::new([
        t.0.as_f64(),
        t.1.as_f64(),
        t.2.as_f64(),
        t.3.as_f64(),
        t.4.as_f64(),
        t.5.as_f64(),
    ])
}
