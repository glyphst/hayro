//! Uncaptioned Line annotations with signed leader geometry.

use super::{AnnotationGeometryBudget, Error, GeometricAnnotation, PathBuilder, coordinates};
use hayro_syntax::object::{Array, Dict, Number};
use kurbo::{PathEl, Point, Vec2};

pub(super) fn resolve(
    annotation: &Dict<'_>,
    result: &mut GeometricAnnotation,
    budget: &mut AnnotationGeometryBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), Error> {
    match annotation.get::<bool>(b"Cap") {
        Some(true) => return Err(Error::UnsupportedCaption),
        None if annotation.contains_key(b"Cap") => return Err(Error::InvalidLine),
        _ => {}
    }
    let array = annotation
        .get::<Array<'_>>(b"L")
        .ok_or(Error::InvalidLine)?;
    let [x1, y1, x2, y2] = coordinates(&array).map_err(|_| Error::InvalidLine)?;
    let points = [Point::new(x1, y1), Point::new(x2, y2)];
    let [length, extension, offset] =
        [b"LL".as_slice(), b"LLE", b"LLO"].map(|key| match annotation.get::<Number>(key) {
            Some(value) if value.as_f32().is_finite() => Ok(f64::from(value.as_f32())),
            None if !annotation.contains_key(key) => Ok(0.0),
            _ => Err(Error::InvalidLine),
        });
    let (length, extension, offset) = (length?, extension?, offset?);
    if extension < 0.0
        || offset < 0.0
        || (annotation.contains_key(b"LLE") && !annotation.contains_key(b"LL"))
    {
        return Err(Error::InvalidLine);
    }
    let delta = points[1] - points[0];
    let distance = delta.hypot();
    let tangent = (distance > 0.0).then(|| delta / distance);
    let normal = match tangent {
        Some(v) => Vec2::new(-v.y, v.x),
        None if length == 0.0 && extension == 0.0 && offset == 0.0 => Vec2::ZERO,
        None => return Err(Error::InvalidGeometry),
    };
    // PDFBox 3.0.8's signed leader convention: L locates the un-offset
    // leader endpoints, with the body translated along the left normal.
    let sign = if length < 0.0 { -1.0 } else { 1.0 };
    let start = sign * offset;
    let body = length + start;
    let end = body + sign * extension;
    let ends = points.map(|p| (p + normal * body, tangent));
    let mut path = PathBuilder::new(budget, cancelled);
    // One stroke object keeps opacity stable at body/leader intersections.
    if start != end {
        for point in points {
            path.push(PathEl::MoveTo(point + normal * start))?;
            path.push(PathEl::LineTo(point + normal * end))?;
        }
    }
    path.push(PathEl::MoveTo(ends[0].0))?;
    path.push(PathEl::LineTo(ends[1].0))?;
    super::polygon::append_decorated_path(
        annotation,
        path.finish(),
        ends,
        result,
        budget,
        cancelled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve_geometric_annotation;
    use hayro_syntax::object::FromBytes;

    fn resolve(entries: &str) -> Result<GeometricAnnotation, Error> {
        let text = format!("<</Subtype/Line/Rect[0 0 0 0]{entries}>>");
        let dict = Dict::from_bytes(text.as_bytes()).unwrap();
        resolve_geometric_annotation(&dict, &mut AnnotationGeometryBudget::default(), &|| false)
            .map(Option::unwrap)
    }

    #[test]
    fn signed_leaders_share_one_stroke_and_move_endpoint_symbols() {
        for (length, sign) in [(10, 1.0), (-10, -1.0), (0, 1.0)] {
            let line = resolve(&format!(
                "/L[20 30 80 30]/LL {length}/LLE 3/LLO 2/LE[/OpenArrow/ClosedArrow]/IC[1 0 0]"
            ))
            .unwrap();
            let body = f64::from(length) + sign * 2.0;
            assert_eq!(line.paths.len(), 3);
            assert_eq!(
                line.paths[0].path.elements(),
                &[
                    PathEl::MoveTo((20.0, 30.0 + sign * 2.0).into()),
                    PathEl::LineTo((20.0, 30.0 + body + sign * 3.0).into()),
                    PathEl::MoveTo((80.0, 30.0 + sign * 2.0).into()),
                    PathEl::LineTo((80.0, 30.0 + body + sign * 3.0).into()),
                    PathEl::MoveTo((20.0, 30.0 + body).into()),
                    PathEl::LineTo((80.0, 30.0 + body).into()),
                ]
            );
            assert!(!line.paths[0].fill && !line.paths[1].fill && line.paths[2].fill);
            assert_eq!(
                line.paths[1].path.elements()[1],
                PathEl::LineTo((20.0, 30.0 + body).into())
            );
            assert_eq!(
                line.paths[2].path.elements()[1],
                PathEl::LineTo((80.0, 30.0 + body).into())
            );
        }
        let reverse = resolve("/L[80 30 20 30]/LL 10").unwrap();
        assert_eq!(
            reverse.paths[0].path.elements()[4],
            PathEl::MoveTo((80.0, 20.0).into())
        );
        let empty = resolve("/L[20 30 20 30]/LE[/Circle/Square]").unwrap();
        assert_eq!(empty.paths.len(), 3);
        let zero = resolve("/L[20 30 20 30]/BS<</W 0>>/LE[/Butt/Slash]").unwrap();
        assert_eq!(zero.paths.len(), 1);
        assert!(zero.stroke_color().is_none());
    }

    #[test]
    fn line_operands_and_caps_fail_closed_before_invisible_paint() {
        for entries in [
            "",
            "/L false",
            "/L[1 2 3]",
            "/L[1 2 3 4 5]",
            "/L[1 2 3 99 0 R]",
            "/L[1 2 3 4]/LL false",
            "/L[1 2 3 4]/LLE 0",
            "/L[1 2 3 4]/LL 1/LLE -1",
            "/L[1 2 3 4]/LLO -1",
            "/L[1 2 3 4]/Cap /false",
        ] {
            assert!(
                matches!(
                    resolve(&format!("{entries}/C[]/CA 0")),
                    Err(Error::InvalidLine)
                ),
                "{entries}"
            );
        }
        assert!(matches!(
            resolve("/L[1 2 3 4]/Cap true/Contents()"),
            Err(Error::UnsupportedCaption)
        ));
        assert!(resolve("/L[1 2 3 4]/Cap false/CP false/CO false/BE false/Path false").is_ok());
        for controls in ["/LL 1", "/LLO 1", "/LL 0/LLE 1", "/LE[/Butt/None]"] {
            assert!(matches!(
                resolve(&format!("/L[1 2 1 2]{controls}")),
                Err(Error::InvalidGeometry)
            ));
        }
    }

    #[test]
    fn line_synthesis_is_cancellable_and_defers_to_explicit_appearances() {
        let dict = Dict::from_bytes(
            b"<</Subtype/Line/Rect[0 0 0 0]/L[10 20 70 20]/LL 8/LLE 3/LLO 2/LE[/Square/Circle]>>",
        )
        .unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(matches!(
            resolve_geometric_annotation(&dict, &mut AnnotationGeometryBudget::default(), &|| {
                calls.set(calls.get() + 1);
                calls.get() > 4
            }),
            Err(Error::Cancelled)
        ));
        let explicit = Dict::from_bytes(b"<</Subtype/Line/AP null/L false/Cap true>>").unwrap();
        assert!(
            resolve_geometric_annotation(
                &explicit,
                &mut AnnotationGeometryBudget::default(),
                &|| false
            )
            .unwrap()
            .is_none()
        );
    }
}
