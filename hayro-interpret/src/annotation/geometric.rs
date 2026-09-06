//! Bounded default appearances for Square, Circle and Ink annotations.

mod cloudy;

use super::{
    LinkBorderColor, LinkBorderError, LinkBorderStyle, border_style, direct_annotation_color,
    legacy_border, normalized_f32,
};
use crate::color::Color;
use crate::{BlendMode, StrokeProps};
use hayro_syntax::object::{Array, Dict, Name, Number};
use kurbo::{BezPath, PathEl, Point, Rect};
use std::mem::size_of;

/// Remaining aggregate synthesis work for one page. The host derives this
/// from its existing path, draw and decoded-page limits.
#[derive(Clone, Copy, Debug)]
pub struct AnnotationGeometryBudget {
    /// Remaining path building operators, including moves and closes.
    pub verbs: u64,
    /// Remaining independently painted paths (empty paths also consume one).
    pub draws: u64,
    /// Remaining temporary and retained geometry bytes.
    pub bytes: u64,
}

impl Default for AnnotationGeometryBudget {
    fn default() -> Self {
        Self {
            verbs: 65_536,
            draws: 65_536,
            bytes: 64 * 1024 * 1024,
        }
    }
}

impl AnnotationGeometryBudget {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), GeometricAnnotationError> {
        let required = values
            .len()
            .checked_add(additional)
            .ok_or(GeometricAnnotationError::ByteLimit)?;
        if required <= values.capacity() {
            return Ok(());
        }
        let available = self.bytes / (2 * size_of::<T>() as u64);
        let needed = required - values.capacity();
        if needed as u64 > available {
            return Err(GeometricAnnotationError::ByteLimit);
        }
        let grow = needed
            .max(values.capacity().max(8))
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        let capacity = values
            .capacity()
            .checked_add(grow)
            .ok_or(GeometricAnnotationError::ByteLimit)?;
        values
            .try_reserve_exact(capacity - values.len())
            .map_err(|_| GeometricAnnotationError::AllocationFailed)?;
        self.bytes = self
            .bytes
            .checked_sub(2 * grow as u64 * size_of::<T>() as u64)
            .ok_or(GeometricAnnotationError::ByteLimit)?;
        Ok(())
    }
}

/// Validated default appearance in PDF source coordinates. Every Ink path
/// remains a separate paint; filled shapes use a combined fill/stroke object.
#[derive(Clone, Debug)]
pub struct GeometricAnnotation {
    /// Source paths, independent of an inaccurate annotation rectangle.
    pub paths: Vec<BezPath>,
    /// Original direct stroke colour, or transparent.
    pub color: LinkBorderColor,
    /// Original direct interior colour, or transparent (always so for Ink).
    pub interior_color: LinkBorderColor,
    /// Stroke opacity, including zero.
    pub stroke_opacity: f32,
    /// Fill opacity, defaulting to stroke opacity.
    pub fill_opacity: f32,
    /// Explicit normal or standard PDF 2.0 blending.
    pub blend_mode: BlendMode,
    /// Source-space width, dash and join policy.
    pub stroke: StrokeProps,
}

impl GeometricAnnotation {
    /// The visible stroke paint. Zero width never becomes a PDF hairline.
    #[must_use]
    pub fn stroke_color(&self) -> Option<Color> {
        (self.stroke.line_width > 0.0 && self.stroke_opacity > 0.0)
            .then(|| self.color.to_color_with_opacity(self.stroke_opacity))
            .flatten()
    }

    /// The visible interior paint.
    #[must_use]
    pub fn fill_color(&self) -> Option<Color> {
        (self.fill_opacity > 0.0)
            .then(|| self.interior_color.to_color_with_opacity(self.fill_opacity))
            .flatten()
    }
}

/// A missing appearance cannot be synthesized without losing semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GeometricAnnotationError {
    /// Missing, non-finite or malformed rectangle/insets.
    InvalidRectangle,
    /// Invalid direct device colour.
    InvalidColor,
    /// Invalid constant opacity.
    InvalidOpacity,
    /// Invalid or unknown annotation blend mode.
    InvalidBlendMode,
    /// Invalid border style, width or dash array.
    InvalidBorder(LinkBorderError),
    /// A border style other than solid/dashed needs another appearance policy.
    UnsupportedBorder,
    /// Invalid border effect or intensity.
    InvalidEffect,
    /// Missing or malformed InkList/Path, including unresolved array entries.
    InvalidInk,
    /// Non-finite derived geometry or an unmet bounded subdivision tolerance.
    InvalidGeometry,
    /// Aggregate generated path operator budget exceeded.
    PathLimit,
    /// Aggregate independently painted path budget exceeded.
    DrawLimit,
    /// Aggregate geometry byte budget exceeded.
    ByteLimit,
    /// Fallible allocation failed.
    AllocationFailed,
    /// The host cancelled resolution.
    Cancelled,
}

type Error = GeometricAnnotationError;

/// Resolve an absent Square, Circle or Ink appearance. Explicit `/AP` always
/// takes precedence. Budgets are consumed across annotations, even if a later
/// field is invalid; callers must not use a partially resolved page.
pub fn resolve_geometric_annotation(
    annotation: &Dict<'_>,
    budget: &mut AnnotationGeometryBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<GeometricAnnotation>, Error> {
    if annotation.contains_key(b"AP") {
        return Ok(None);
    }
    let subtype = annotation.get::<Name<'_>>(b"Subtype");
    let kind = match subtype.as_ref().map(Name::as_ref) {
        Some(b"Square") => 0,
        Some(b"Circle") => 1,
        Some(b"Ink") => 2,
        _ => return Ok(None),
    };
    if cancelled() {
        return Err(Error::Cancelled);
    }
    let rect_values = annotation
        .get::<Array<'_>>(b"Rect")
        .ok_or(Error::InvalidRectangle)?;
    let [x0, y0, x1, y1] = coordinates::<4>(&rect_values).map_err(|_| Error::InvalidRectangle)?;
    let rect = Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
    let max_dash_items = usize::try_from(budget.bytes / (3 * size_of::<f32>() as u64))
        .unwrap_or(usize::MAX)
        .min(super::MAX_LINK_BORDER_DASH_ITEMS);
    let (width, style) = if annotation.contains_key(b"BS") {
        border_style(annotation, max_dash_items).map_err(Error::InvalidBorder)?
    } else {
        let border = legacy_border(annotation, max_dash_items).map_err(Error::InvalidBorder)?;
        // Legacy corner radii apply to the generic annotation border. A
        // rounded Square needs an explicit appearance; Circle/Ink geometry
        // comes from the subtype rather than that generic rectangle.
        if kind == 0
            && border.width > 0.0
            && (border.horizontal_radius > 0.0 || border.vertical_radius > 0.0)
        {
            return Err(Error::UnsupportedBorder);
        }
        (
            border.width,
            match border.dash_array {
                Some(dash_array) => LinkBorderStyle::Dashed { dash_array },
                None => LinkBorderStyle::Solid,
            },
        )
    };
    let dash_array = match style {
        LinkBorderStyle::Solid => Vec::new(),
        LinkBorderStyle::Dashed { dash_array } => dash_array,
        _ => return Err(Error::UnsupportedBorder),
    };
    budget.bytes = budget
        .bytes
        .checked_sub(2 * dash_array.capacity() as u64 * size_of::<f32>() as u64)
        .ok_or(Error::ByteLimit)?;
    let color = direct_annotation_color(annotation, b"C", LinkBorderColor::Gray([0.0]))
        .map_err(|_| Error::InvalidColor)?;
    let interior_color = if kind == 2 {
        LinkBorderColor::Transparent
    } else {
        direct_annotation_color(annotation, b"IC", LinkBorderColor::Transparent)
            .map_err(|_| Error::InvalidColor)?
    };
    let stroke_opacity = opacity(annotation, b"CA", 1.0)?;
    let fill_opacity = opacity(annotation, b"ca", stroke_opacity)?;
    let blend_mode = match annotation.get::<Name<'_>>(b"BM") {
        Some(name) => crate::interpret::state::convert_blend_mode(name.as_str())
            .ok_or(Error::InvalidBlendMode)?,
        None if annotation.contains_key(b"BM") => return Err(Error::InvalidBlendMode),
        None => BlendMode::Normal,
    };
    let mut result = GeometricAnnotation {
        paths: Vec::new(),
        color,
        interior_color,
        stroke_opacity,
        fill_opacity,
        blend_mode,
        stroke: StrokeProps {
            line_width: width,
            dash_array: dash_array.into(),
            ..StrokeProps::default()
        },
    };
    if kind == 2 {
        if annotation.contains_key(b"Path") {
            let array = annotation
                .get::<Array<'_>>(b"Path")
                .ok_or(Error::InvalidInk)?;
            let mut path = PathBuilder::new(budget, cancelled);
            let mut values = array.flex_iter();
            let mut first = true;
            for _ in array.raw_iter() {
                let operands = values.next::<Array<'_>>().ok_or(Error::InvalidInk)?;
                let count = operands.raw_iter().take(7).count();
                let element = match (first, count) {
                    (true, 2) => {
                        let [x, y] = coordinates(&operands)?;
                        PathEl::MoveTo(Point::new(x, y))
                    }
                    (false, 2) => {
                        let [x, y] = coordinates(&operands)?;
                        PathEl::LineTo(Point::new(x, y))
                    }
                    (false, 6) => {
                        let [a, b, c, d, e, f] = coordinates(&operands)?;
                        PathEl::CurveTo(Point::new(a, b), Point::new(c, d), Point::new(e, f))
                    }
                    _ => return Err(Error::InvalidInk),
                };
                path.push(element)?;
                first = false;
            }
            if first {
                return Err(Error::InvalidInk);
            }
            let path = path.finish();
            append_path(&mut result, path, budget)?;
        } else {
            let array = annotation
                .get::<Array<'_>>(b"InkList")
                .ok_or(Error::InvalidInk)?;
            let mut lists = array.flex_iter();
            for _ in array.raw_iter() {
                if cancelled() {
                    return Err(Error::Cancelled);
                }
                if budget.draws == 0 {
                    return Err(Error::DrawLimit);
                }
                let operands = lists.next::<Array<'_>>().ok_or(Error::InvalidInk)?;
                let mut numbers = operands.flex_iter();
                let mut raw = operands.raw_iter();
                let mut path = PathBuilder::new(budget, cancelled);
                let mut first = true;
                while raw.next().is_some() {
                    if raw.next().is_none() {
                        return Err(Error::InvalidInk);
                    }
                    let x = coordinate(numbers.next::<Number>().ok_or(Error::InvalidInk)?)?;
                    let y = coordinate(numbers.next::<Number>().ok_or(Error::InvalidInk)?)?;
                    let point = Point::new(x, y);
                    path.push(if first {
                        PathEl::MoveTo(point)
                    } else {
                        PathEl::LineTo(point)
                    })?;
                    first = false;
                }
                let path = path.finish();
                append_path(&mut result, path, budget)?;
            }
        }
    } else {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(Error::InvalidRectangle);
        }
        let rd = if annotation.contains_key(b"RD") {
            coordinates::<4>(
                &annotation
                    .get::<Array<'_>>(b"RD")
                    .ok_or(Error::InvalidRectangle)?,
            )
            .map_err(|_| Error::InvalidRectangle)?
        } else {
            [0.0; 4]
        };
        if rd.iter().any(|x| *x < 0.0)
            || rd[0] + rd[2] >= rect.width()
            || rd[1] + rd[3] >= rect.height()
        {
            return Err(Error::InvalidRectangle);
        }
        let intensity = effect(annotation)?;
        let mut path = PathBuilder::new(budget, cancelled);
        if intensity > 0.0 {
            let min = if kind == 0 {
                f64::from(width) * 0.5
            } else {
                0.0
            };
            let inner = Rect::new(
                rect.x0 + rd[0].max(min),
                rect.y0 + rd[3].max(min),
                rect.x1 - rd[2].max(min),
                rect.y1 - rd[1].max(min),
            );
            if inner.width() <= 0.0 || inner.height() <= 0.0 {
                return Err(Error::InvalidGeometry);
            }
            cloudy::draw(&mut path, inner, kind == 1, intensity, f64::from(width))?;
            result.stroke.line_join = kurbo::Join::Bevel;
        } else {
            let half = f64::from(width) * 0.5;
            let inner = Rect::new(
                rect.x0 + rd[0] + half,
                rect.y0 + rd[3] + half,
                rect.x1 - rd[2] - half,
                rect.y1 - rd[1] - half,
            );
            if inner.width() <= 0.0 || inner.height() <= 0.0 {
                return Err(Error::InvalidGeometry);
            }
            if kind == 0 {
                path.push(PathEl::MoveTo(Point::new(inner.x0, inner.y0)))?;
                for point in [
                    (inner.x1, inner.y0),
                    (inner.x1, inner.y1),
                    (inner.x0, inner.y1),
                ] {
                    path.push(PathEl::LineTo(point.into()))?;
                }
            } else {
                ellipse(&mut path, inner)?;
            }
            path.push(PathEl::ClosePath)?;
        }
        let path = path.finish();
        append_path(&mut result, path, budget)?;
    }
    Ok(Some(result))
}

fn append_path(
    result: &mut GeometricAnnotation,
    path: BezPath,
    budget: &mut AnnotationGeometryBudget,
) -> Result<(), Error> {
    let paints =
        u64::from(result.stroke_color().is_some()) + u64::from(result.fill_color().is_some());
    budget.draws = budget
        .draws
        .checked_sub(paints.max(1))
        .ok_or(Error::DrawLimit)?;
    budget.reserve(&mut result.paths, 1)?;
    result.paths.push(path);
    Ok(())
}

fn opacity(annotation: &Dict<'_>, key: &[u8], default: f32) -> Result<f32, Error> {
    match annotation.get::<Number>(key) {
        Some(value) => normalized_f32(value.as_f64()).ok_or(Error::InvalidOpacity),
        None if annotation.contains_key(key) => Err(Error::InvalidOpacity),
        None => Ok(default),
    }
}

fn effect(annotation: &Dict<'_>) -> Result<f64, Error> {
    if !annotation.contains_key(b"BE") {
        return Ok(0.0);
    }
    let effect = annotation
        .get::<Dict<'_>>(b"BE")
        .ok_or(Error::InvalidEffect)?;
    match effect.get::<Name<'_>>(b"S").as_ref().map(Name::as_ref) {
        Some(b"S") => return Ok(0.0),
        Some(b"C") => {}
        None if !effect.contains_key(b"S") => return Ok(0.0),
        _ => return Err(Error::InvalidEffect),
    }
    let value = match effect.get::<Number>(b"I") {
        Some(value) => value.as_f64(),
        None if effect.contains_key(b"I") => return Err(Error::InvalidEffect),
        None => 0.0,
    };
    if !value.is_finite() || !(0.0..=2.0).contains(&value) {
        return Err(Error::InvalidEffect);
    }
    Ok(value)
}

fn coordinate(number: Number) -> Result<f64, Error> {
    let value = number.as_f32();
    if value.is_finite() {
        Ok(f64::from(value))
    } else {
        Err(Error::InvalidInk)
    }
}

fn coordinates<const N: usize>(array: &Array<'_>) -> Result<[f64; N], Error> {
    if array.raw_iter().take(N + 1).count() != N {
        return Err(Error::InvalidInk);
    }
    let mut numbers = array.flex_iter();
    let mut result = [0.0; N];
    for value in &mut result {
        *value = coordinate(numbers.next::<Number>().ok_or(Error::InvalidInk)?)?;
    }
    Ok(result)
}

struct PathBuilder<'a> {
    elements: Vec<PathEl>,
    budget: &'a mut AnnotationGeometryBudget,
    cancelled: &'a dyn Fn() -> bool,
}

impl<'a> PathBuilder<'a> {
    fn new(budget: &'a mut AnnotationGeometryBudget, cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            elements: Vec::new(),
            budget,
            cancelled,
        }
    }
    fn checkpoint(&self) -> Result<(), Error> {
        if (self.cancelled)() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn push(&mut self, element: PathEl) -> Result<(), Error> {
        self.checkpoint()?;
        let finite = |p: Point| (p.x as f32).is_finite() && (p.y as f32).is_finite();
        if !match element {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => finite(p),
            PathEl::QuadTo(a, b) => finite(a) && finite(b),
            PathEl::CurveTo(a, b, c) => finite(a) && finite(b) && finite(c),
            PathEl::ClosePath => true,
        } {
            return Err(Error::InvalidGeometry);
        }
        self.budget.verbs = self.budget.verbs.checked_sub(1).ok_or(Error::PathLimit)?;
        self.budget.reserve(&mut self.elements, 1)?;
        self.elements.push(element);
        Ok(())
    }
    fn finish(self) -> BezPath {
        BezPath::from_vec(self.elements)
    }
}

fn ellipse(path: &mut PathBuilder<'_>, rect: Rect) -> Result<(), Error> {
    let center = rect.center();
    let rx = rect.width() * 0.5;
    let ry = rect.height() * 0.5;
    let k = 4.0 / 3.0 * (std::f64::consts::PI / 8.0).tan();
    path.push(PathEl::MoveTo(Point::new(center.x + rx, center.y)))?;
    for (a, b, c) in [
        ((rx, k * ry), (k * rx, ry), (0.0, ry)),
        ((-k * rx, ry), (-rx, k * ry), (-rx, 0.0)),
        ((-rx, -k * ry), (-k * rx, -ry), (0.0, -ry)),
        ((k * rx, -ry), (rx, -k * ry), (rx, 0.0)),
    ] {
        path.push(PathEl::CurveTo(
            center + kurbo::Vec2::from(a),
            center + kurbo::Vec2::from(b),
            center + kurbo::Vec2::from(c),
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::object::FromBytes;
    use kurbo::Shape;

    fn resolve(source: &str) -> Result<GeometricAnnotation, Error> {
        let dict = Dict::from_bytes(source.as_bytes()).unwrap();
        resolve_geometric_annotation(&dict, &mut AnnotationGeometryBudget::default(), &|| false)
            .map(|value| value.expect("geometric annotation"))
    }

    #[test]
    fn geometry_style_and_precedence() {
        let shape = resolve("<</Subtype/Square/Rect[100 90 10 20]/RD[12 4 6 8]/BS<</W 4/D[3]>>/Border false/C[0 1 0 0]/IC[0.4]/CA 0.5/BM/Multiply>>").unwrap();
        assert_eq!(
            shape.paths[0].bounding_box(),
            Rect::new(24.0, 30.0, 92.0, 84.0)
        );
        assert!(shape.stroke.dash_array.is_empty());
        assert_eq!(shape.color, LinkBorderColor::Cmyk([0.0, 1.0, 0.0, 0.0]));
        assert_eq!(shape.fill_opacity, 0.5);
        assert_eq!(shape.blend_mode, BlendMode::Multiply);
        let ink = resolve("<</Subtype/Ink/Rect[0 0 0 0]/InkList false/Path[[10 20][20 30][30 40 40 50 50 40]]/BS<</S/D>>/CA 0/ca 1>>").unwrap();
        assert_eq!(
            ink.paths[0].elements(),
            &[
                PathEl::MoveTo((10.0, 20.0).into()),
                PathEl::LineTo((20.0, 30.0).into()),
                PathEl::CurveTo(
                    (30.0, 40.0).into(),
                    (40.0, 50.0).into(),
                    (50.0, 40.0).into()
                )
            ]
        );
        assert_eq!(ink.stroke.dash_array.as_slice(), &[3.0]);
        assert!(ink.stroke_color().is_none());
        assert!(ink.fill_color().is_none());
        let ink =
            resolve("<</Subtype/Ink/Rect[3 3 1 1]/InkList[[0 0 0 0 10 10][10 10 20 0]]>>").unwrap();
        assert_eq!(ink.paths.len(), 2);
        assert_eq!(ink.paths[0].elements().len(), 3);
        let shape =
            resolve("<</Subtype/Circle/Rect[0 0 50 30]/Border[0 0 0]/IC[1 0 0]/CA 0/ca 1>>")
                .unwrap();
        assert!(shape.stroke_color().is_none());
        assert!(shape.fill_color().is_some());
        let explicit = Dict::from_bytes(b"<</Subtype/Ink/AP false/Path false>>").unwrap();
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

    #[test]
    fn malformed_geometry_and_controls_fail_closed() {
        for entries in [
            "/Subtype/Ink/InkList[[1 2 3]]",
            "/Subtype/Ink/InkList[[1 true 3 4]]",
            "/Subtype/Ink/InkList[[1 2 3 4] false]",
            "/Subtype/Ink/InkList[[1 2 3 4]]/Path[]",
            "/Subtype/Ink/Path[[1 2 3 4 5 6]]",
            "/Subtype/Ink/Path[[1 2][3 4 5]]",
            "/Subtype/Square/RD[1 2 3]",
            "/Subtype/Square/RD[0 51 0 50]",
            "/Subtype/Square/RD[-1 0 0 0]",
            "/Subtype/Square/BE<</S/C/I 2.1>>",
            "/Subtype/Circle/BE<</S/Unknown>>",
            "/Subtype/Circle/ca 2",
            "/Subtype/Square/BM[ /Multiply ]",
            "/Subtype/Square/C[1 2]",
            "/Subtype/Square/BS<</S/B>>",
            "/Subtype/Square/BS<</S/D/D[0 0]>>",
        ] {
            assert!(
                resolve(&format!("<</Rect[0 0 100 100]{entries}>>")).is_err(),
                "{entries}"
            );
        }
        assert!(resolve("<</Subtype/Square/Rect[0 0 false 100 100]>>").is_err());
    }

    #[test]
    fn expansion_budgets_and_cancellation_are_aggregate() {
        let square = Dict::from_bytes(b"<</Subtype/Square/Rect[0 0 100 100]>>").unwrap();
        let mut budget = AnnotationGeometryBudget {
            verbs: 9,
            ..AnnotationGeometryBudget::default()
        };
        resolve_geometric_annotation(&square, &mut budget, &|| false).unwrap();
        assert!(matches!(
            resolve_geometric_annotation(&square, &mut budget, &|| false),
            Err(Error::PathLimit)
        ));
        let mut budget = AnnotationGeometryBudget {
            bytes: 1,
            ..AnnotationGeometryBudget::default()
        };
        assert!(matches!(
            resolve_geometric_annotation(&square, &mut budget, &|| false),
            Err(Error::ByteLimit)
        ));
        let cloud = Dict::from_bytes(
            b"<</Subtype/Square/Rect[0 0 100000000000000000000 100]/BE<</S/C/I 1>>>>",
        )
        .unwrap();
        assert!(matches!(
            resolve_geometric_annotation(&cloud, &mut AnnotationGeometryBudget::default(), &|| {
                false
            }),
            Err(Error::PathLimit)
        ));
        let calls = std::cell::Cell::new(0);
        let cancel = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        assert!(matches!(
            resolve_geometric_annotation(
                &square,
                &mut AnnotationGeometryBudget::default(),
                &cancel
            ),
            Err(Error::Cancelled)
        ));
        let ink = Dict::from_bytes(b"<</Subtype/Ink/Rect[0 0 0 0]/InkList[[][][]]>>").unwrap();
        let mut budget = AnnotationGeometryBudget {
            draws: 2,
            ..AnnotationGeometryBudget::default()
        };
        assert!(matches!(
            resolve_geometric_annotation(&ink, &mut budget, &|| false),
            Err(Error::DrawLimit)
        ));
    }
}
