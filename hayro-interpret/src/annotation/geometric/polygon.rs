//! Polygon contours and endpoint-anchored `PolyLine` decorations.

use super::{
    AnnotationGeometryBudget, Error, GeometricAnnotation, PathBuilder, append_path, cloudy, effect,
    ellipse, operator_path, vertex_path,
};
use hayro_syntax::object::{Array, Dict, Name};
use kurbo::{BezPath, PathEl, Point, Rect, Vec2};

pub(super) fn resolve(
    annotation: &Dict<'_>,
    closed: bool,
    result: &mut GeometricAnnotation,
    budget: &mut AnnotationGeometryBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), Error> {
    if budget.draws == 0 {
        return Err(Error::DrawLimit);
    }
    let source = if annotation.contains_key(b"Path") {
        let array = annotation
            .get::<Array<'_>>(b"Path")
            .ok_or(Error::InvalidPath)?;
        operator_path(&array, budget, cancelled)?
    } else {
        let array = annotation
            .get::<Array<'_>>(b"Vertices")
            .ok_or(Error::InvalidVertices)?;
        let path = vertex_path(&array, budget, cancelled).map_err(|error| match error {
            Error::InvalidInk => Error::InvalidVertices,
            other => other,
        })?;
        if path.elements().is_empty() {
            return Err(Error::InvalidVertices);
        }
        path
    };
    if closed {
        let intensity = effect(annotation)?;
        let path = if intensity > 0.0 {
            let mut out = PathBuilder::new(budget, cancelled);
            cloudy::polygon(
                &mut out,
                &source,
                intensity,
                f64::from(result.stroke.line_width),
            )?;
            result.stroke.line_join = kurbo::Join::Bevel;
            out.finish()
        } else {
            let mut out = PathBuilder {
                elements: source.into_elements(),
                budget,
                cancelled,
            };
            out.push(PathEl::ClosePath)?;
            out.finish()
        };
        append_path(result, path, true, budget)?;
    } else {
        let styles = endings(annotation)?;
        let ends = endpoints(&source, cancelled)?;
        append_path(result, source, false, budget)?;
        // Symbols scale with the border width; zero width cannot create hairlines.
        if result.stroke.line_width > 0.0 {
            for (i, (style, (point, tangent))) in styles.into_iter().zip(ends).enumerate() {
                if style == Ending::None {
                    continue;
                }
                let mut out = PathBuilder::new(budget, cancelled);
                let fill = decoration(
                    &mut out,
                    style,
                    point,
                    tangent,
                    i == 1,
                    f64::from(result.stroke.line_width),
                )?;
                let path = out.finish();
                append_path(result, path, fill, budget)?;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    None,
    Square,
    Circle,
    Diamond,
    OpenArrow,
    ClosedArrow,
    Butt,
    ROpenArrow,
    RClosedArrow,
    Slash,
}

fn endings(annotation: &Dict<'_>) -> Result<[Ending; 2], Error> {
    if !annotation.contains_key(b"LE") {
        return Ok([Ending::None; 2]);
    }
    let array = annotation
        .get::<Array<'_>>(b"LE")
        .ok_or(Error::InvalidLineEnding)?;
    if array.raw_iter().take(3).count() != 2 {
        return Err(Error::InvalidLineEnding);
    }
    let mut names = array.flex_iter();
    let mut result = [Ending::None; 2];
    for value in &mut result {
        *value = match names.next::<Name<'_>>().as_ref().map(Name::as_ref) {
            Some(b"None") => Ending::None,
            Some(b"Square") => Ending::Square,
            Some(b"Circle") => Ending::Circle,
            Some(b"Diamond") => Ending::Diamond,
            Some(b"OpenArrow") => Ending::OpenArrow,
            Some(b"ClosedArrow") => Ending::ClosedArrow,
            Some(b"Butt") => Ending::Butt,
            Some(b"ROpenArrow") => Ending::ROpenArrow,
            Some(b"RClosedArrow") => Ending::RClosedArrow,
            Some(b"Slash") => Ending::Slash,
            _ => return Err(Error::InvalidLineEnding),
        };
    }
    Ok(result)
}

fn direction(vectors: &[Vec2]) -> Option<Vec2> {
    vectors.iter().find_map(|v| {
        let length = v.hypot();
        (length > 0.0).then(|| *v / length)
    })
}

fn endpoints(
    path: &BezPath,
    cancelled: &dyn Fn() -> bool,
) -> Result<[(Point, Option<Vec2>); 2], Error> {
    let Some(PathEl::MoveTo(start)) = path.elements().first().copied() else {
        return Err(Error::InvalidGeometry);
    };
    let mut end = start;
    let mut first = None;
    let mut last = None;
    for element in &path.elements()[1..] {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let (next, a, b) = match *element {
            PathEl::LineTo(p) => (p, direction(&[p - end]), direction(&[p - end])),
            PathEl::CurveTo(a, b, p) => (
                p,
                direction(&[a - end, b - end, p - end]),
                direction(&[p - b, p - a, p - end]),
            ),
            _ => return Err(Error::InvalidGeometry),
        };
        first = first.or(a);
        last = b.or(last);
        end = next;
    }
    Ok([(start, first), (end, last)])
}

fn decoration(
    out: &mut PathBuilder<'_>,
    style: Ending,
    point: Point,
    tangent: Option<Vec2>,
    end: bool,
    width: f64,
) -> Result<bool, Error> {
    // Width proportions follow PDFBox 3.0.8 PDAbstractAppearanceHandler.
    // This viewer policy keeps tips at the declared endpoint and the body
    // intact. Circles use the same cubic arcs as our Circle annotations.
    let fill = matches!(
        style,
        Ending::Square
            | Ending::Circle
            | Ending::Diamond
            | Ending::ClosedArrow
            | Ending::RClosedArrow
    );
    let axis = if matches!(style, Ending::Square | Ending::Circle | Ending::Diamond) {
        Vec2::new(1.0, 0.0)
    } else {
        tangent.ok_or(Error::InvalidGeometry)?
    };
    let map = |x: f64, y: f64| point + axis * x + Vec2::new(-axis.y, axis.x) * y;
    let r = 3.0 * width;
    let (points, count) = match style {
        Ending::Circle => {
            ellipse(
                out,
                Rect::new(point.x - r, point.y - r, point.x + r, point.y + r),
            )?;
            out.push(PathEl::ClosePath)?;
            return Ok(true);
        }
        Ending::Square => ([map(-r, -r), map(r, -r), map(r, r), map(-r, r)], 4),
        Ending::Diamond => ([map(-r, 0.0), map(0.0, r), map(r, 0.0), map(0.0, -r)], 4),
        Ending::Butt => ([map(0.0, -r), map(0.0, r), point, point], 2),
        Ending::Slash => {
            let x = 4.5 * width;
            let y = 9.0 * width * (std::f64::consts::PI / 3.0).sin();
            ([map(x, y), map(-x, -y), point, point], 2)
        }
        Ending::OpenArrow | Ending::ClosedArrow | Ending::ROpenArrow | Ending::RClosedArrow => {
            let reverse = matches!(style, Ending::ROpenArrow | Ending::RClosedArrow);
            let sign = if end ^ reverse { -1.0 } else { 1.0 };
            let x = sign * 9.0 * width * (std::f64::consts::PI / 6.0).cos();
            let y = 4.5 * width;
            ([map(x, y), point, map(x, -y), point], 3)
        }
        Ending::None => return Ok(false),
    };
    out.push(PathEl::MoveTo(points[0]))?;
    for p in &points[1..count] {
        out.push(PathEl::LineTo(*p))?;
    }
    if fill {
        out.push(PathEl::ClosePath)?;
    }
    Ok(fill)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve_geometric_annotation;
    use hayro_syntax::object::FromBytes;

    fn resolve(entries: &str) -> Result<GeometricAnnotation, Error> {
        let source = format!("<</Rect[500 500 501 501]{entries}>>");
        let dict = Dict::from_bytes(source.as_bytes()).unwrap();
        resolve_geometric_annotation(&dict, &mut AnnotationGeometryBudget::default(), &|| false)
            .map(Option::unwrap)
    }

    #[test]
    fn contours_and_all_endings_preserve_body_and_fill_policy() {
        let polygon =
            resolve("/Subtype/Polygon/Vertices[10 20 90 20 60 70]/IC[1 0 0]/LE false").unwrap();
        assert!(polygon.paths[0].fill);
        assert_eq!(
            polygon.paths[0].path.elements(),
            &[
                PathEl::MoveTo((10.0, 20.0).into()),
                PathEl::LineTo((90.0, 20.0).into()),
                PathEl::LineTo((60.0, 70.0).into()),
                PathEl::ClosePath,
            ]
        );
        for (style, fill, count) in [
            ("None", false, 1),
            ("Square", true, 3),
            ("Circle", true, 3),
            ("Diamond", true, 3),
            ("OpenArrow", false, 3),
            ("ClosedArrow", true, 3),
            ("Butt", false, 3),
            ("ROpenArrow", false, 3),
            ("RClosedArrow", true, 3),
            ("Slash", false, 3),
        ] {
            let line = resolve(&format!("/Subtype/PolyLine/Vertices[10 20 90 20 60 70]/IC[1 0 0]/LE[/{style}/{style}]/BE false")).unwrap();
            assert_eq!(line.paths.len(), count, "{style}");
            assert!(!line.paths[0].fill);
            assert_eq!(
                line.paths[0].path.elements(),
                &polygon.paths[0].path.elements()[..3]
            );
            assert!(line.paths[1..].iter().all(|path| path.fill == fill));
        }
        let curve = resolve("/Subtype/PolyLine/Vertices false/Path[[10 20][10 20][10 20 10 40 10 60][10 60]]/LE[/OpenArrow/ClosedArrow]/BS<</W 2>>").unwrap();
        // The degenerate controls and repeated endpoints do not invent an angle.
        assert_eq!(
            curve.paths[1].path.elements()[1],
            PathEl::LineTo((10.0, 20.0).into())
        );
        assert_eq!(
            curve.paths[2].path.elements()[1],
            PathEl::LineTo((10.0, 60.0).into())
        );
        let PathEl::MoveTo(arm) = curve.paths[1].path.elements()[0] else {
            panic!()
        };
        assert!((arm.x - 1.0).abs() < 1e-12 && arm.y > 35.0);
        let empty =
            resolve("/Subtype/PolyLine/Vertices[1 2]/LE[/OpenArrow/ClosedArrow]/BS<</W 0>>")
                .unwrap();
        assert_eq!(empty.paths.len(), 1);
        assert!(empty.stroke_color().is_none());
    }

    #[test]
    fn invalid_sources_and_undefined_directions_fail_closed() {
        for (entries, expected) in [
            ("/Subtype/Polygon/Vertices[]", Error::InvalidVertices),
            ("/Subtype/Polygon/Vertices[1 2 3]", Error::InvalidVertices),
            (
                "/Subtype/Polygon/Vertices[1 false 3 4]",
                Error::InvalidVertices,
            ),
            (
                "/Subtype/PolyLine/Vertices[1 2 3 4]/Path false",
                Error::InvalidPath,
            ),
            ("/Subtype/Polygon/Path[[1 2 3 4 5 6]]", Error::InvalidPath),
            ("/Subtype/Polygon/Path[[1 2][3 true]]", Error::InvalidPath),
            (
                "/Subtype/PolyLine/Vertices[1 2 3 4]/LE[/None]",
                Error::InvalidLineEnding,
            ),
            (
                "/Subtype/PolyLine/Vertices[1 2 3 4]/LE[/None 9 0 R]",
                Error::InvalidLineEnding,
            ),
            (
                "/Subtype/PolyLine/Vertices[1 2 3 4]/LE[/None/Unknown]",
                Error::InvalidLineEnding,
            ),
            (
                "/Subtype/PolyLine/Vertices[1 2 1 2]/LE[/Butt/None]",
                Error::InvalidGeometry,
            ),
            (
                "/Subtype/Polygon/Vertices[0 0 100 100 0 100 100 0]/BE<</S/C/I 1>>",
                Error::InvalidGeometry,
            ),
            (
                "/Subtype/Polygon/Vertices[0 0 100 0 50 0 0 100]/BE<</S/C/I 1>>",
                Error::InvalidGeometry,
            ),
            (
                "/Subtype/Polygon/Vertices[0 0 0.1 0.1 0 0.1 0.1 0 80 0 80 60 0 60]/BE<</S/C/I 1>>",
                Error::InvalidGeometry,
            ),
        ] {
            assert!(
                matches!(resolve(entries), Err(error) if error == expected),
                "{entries}"
            );
        }
        assert!(resolve("/Subtype/PolyLine/Vertices[1 2 1 2]/LE[/Circle/Square]").is_ok());
    }

    #[test]
    fn cloudy_contours_share_bounded_expansion_and_preserve_curves() {
        let a =
            resolve("/Subtype/Polygon/Vertices[0 0 80 0 80 60 40 30 0 60]/BE<</S/C/I 1>>").unwrap();
        let b =
            resolve("/Subtype/Polygon/Vertices[0 0 0 60 40 30 80 60 80 0]/BE<</S/C/I 1>>").unwrap();
        assert_eq!(a.paths[0].path, b.paths[0].path);
        let curved =
            resolve("/Subtype/Polygon/Path[[0 0][80 0][100 20 100 40 80 60][0 60]]/BE<</S/C/I 1>>")
                .unwrap();
        let chord =
            resolve("/Subtype/Polygon/Vertices[0 0 80 0 80 60 0 60]/BE<</S/C/I 1>>").unwrap();
        assert_ne!(curved.paths[0].path, chord.paths[0].path);
        let dict = Dict::from_bytes(
            b"<</Subtype/Polygon/Rect[0 0 1 1]/Vertices[0 0 80 0 80 60 0 60]/BE<</S/C/I 1>>>>",
        )
        .unwrap();
        let mut budget = AnnotationGeometryBudget {
            intersections: 3,
            ..AnnotationGeometryBudget::default()
        };
        resolve_geometric_annotation(&dict, &mut budget, &|| false).unwrap();
        assert_eq!(budget.intersections, 1);
        assert!(matches!(
            resolve_geometric_annotation(&dict, &mut budget, &|| false),
            Err(Error::WorkLimit)
        ));
        let calls = std::cell::Cell::new(0);
        assert!(matches!(
            resolve_geometric_annotation(&dict, &mut AnnotationGeometryBudget::default(), &|| {
                calls.set(calls.get() + 1);
                calls.get() > 20
            }),
            Err(Error::Cancelled)
        ));
    }
}
