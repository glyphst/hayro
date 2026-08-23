use crate::RectExt;
use crate::color::{Color, ColorSpace};
use crate::x_object::FormXObject;
use hayro_syntax::object::dict::keys::{
    AP, AS, BORDER, BS, C, CA, D, F, FORMTYPE, N, QUADPOINTS, RECT, RESOURCES, S, SUBTYPE, TYPE, W,
};
use hayro_syntax::object::{Array, Dict, Name, Number, Object, Rect, Stream};
use kurbo::{Affine, Shape};
use smallvec::smallvec;

const MAX_LINK_BORDER_DASH_ITEMS: usize = 65_536;
const DEFAULT_MAX_LINK_QUADS: usize = 65_536;

/// A reason why a Link annotation's `/QuadPoints` entry could not be retained
/// exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LinkQuadPointsError {
    /// `/QuadPoints` is present but is not a resolvable array.
    InvalidArray,
    /// The array is empty or its length is not a multiple of eight.
    InvalidLength,
    /// A coordinate is not a finite number representable by the retained
    /// geometry model.
    InvalidCoordinate,
    /// A quadrilateral is degenerate, clockwise, or has unsupported edge
    /// topology.
    InvalidGeometry,
    /// The number of quadrilaterals exceeds the caller-provided bound.
    ItemLimit,
    /// `/Rect` is missing, malformed, non-finite, or empty.
    InvalidRectangle,
}

/// One Link activation quadrilateral in ISO 32000 source-space order.
///
/// ISO 32000 defines the first edge as the bottom edge and the four vertices
/// as counter-clockwise. The resolver also recognizes Adobe's deployed
/// cross-wise `top-left, top-right, bottom-left, bottom-right` order, but
/// normalizes it into this representation before returning it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkQuadrilateral {
    /// First point of the bottom edge.
    pub bottom_left: kurbo::Point,
    /// Second point of the bottom edge.
    pub bottom_right: kurbo::Point,
    /// Point connected to `bottom_right` along the right edge.
    pub top_right: kurbo::Point,
    /// Point connected to `top_right` and `bottom_left`.
    pub top_left: kurbo::Point,
}

/// A reason why a visible annotation's normal appearance could not be
/// selected or mapped exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AnnotationAppearanceError {
    /// The annotation's `/AP` entry is not a resolvable dictionary.
    InvalidAppearanceDictionary,
    /// The appearance dictionary has no resolvable `/N` entry.
    MissingNormalAppearance,
    /// A normal-appearance state dictionary is present without a valid `/AS`
    /// name on the annotation.
    MissingAppearanceState,
    /// The selected `/AS` name is absent from the normal-appearance state
    /// dictionary.
    UnknownAppearanceState,
    /// The selected normal appearance is not a stream.
    InvalidNormalAppearance,
    /// The selected appearance stream does not declare `/Subtype /Form`.
    InvalidFormSubtype,
    /// The selected appearance has an invalid `/Type` or `/FormType` entry.
    InvalidFormType,
    /// The selected appearance has a malformed `/Resources` entry.
    InvalidFormResources,
    /// The selected Form stream cannot be decoded or has invalid required
    /// geometry.
    InvalidFormStream,
    /// The annotation has no valid, non-degenerate `/Rect`.
    InvalidAnnotationRectangle,
    /// The appearance Form cannot be mapped to the annotation rectangle with
    /// a finite, non-degenerate transform.
    InvalidAppearanceMapping,
}

/// A reason why a synthesized Link annotation border could not be retained
/// exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LinkBorderError {
    /// The annotation rectangle is missing, malformed, non-finite, or empty.
    InvalidRectangle,
    /// The `/BS` entry is not a resolvable dictionary.
    InvalidStyleDictionary,
    /// A `/BS /Type` entry is present but is not `/Border`.
    InvalidStyleType,
    /// The border width is malformed, negative, non-finite, or outside the
    /// retained `f32` range.
    InvalidWidth,
    /// A `/BS /S` entry is malformed or names an unknown border style.
    InvalidStyle,
    /// A legacy `/Border` entry is malformed.
    InvalidLegacyBorder,
    /// A dash array is malformed, contains invalid values, or is all zero.
    InvalidDashArray,
    /// A dash array exceeds the bounded retained-item limit.
    DashArrayLimit,
    /// The annotation `/C` entry is malformed or has invalid components.
    InvalidColor,
    /// The annotation contains `/CA`, whose opacity semantics are defined for
    /// markup annotations rather than Link annotations.
    UnsupportedOpacity,
    /// A `/BS` border depends on malformed or over-limit `/QuadPoints`.
    InvalidQuadPoints(LinkQuadPointsError),
}

/// Geometry used to synthesize a Link annotation border in default user
/// space.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkBorderGeometry {
    /// A legacy rounded rectangle from `/Border`.
    RoundedRectangle {
        /// Normalized annotation rectangle.
        rect: kurbo::Rect,
        /// Horizontal outer corner radius in default user-space units.
        horizontal_radius: f32,
        /// Vertical outer corner radius in default user-space units.
        vertical_radius: f32,
    },
    /// A square-cornered rectangle used by `/BS`.
    Rectangle {
        /// Normalized annotation rectangle.
        rect: kurbo::Rect,
    },
    /// One or more quadrilaterals used by a `/BS` border.
    Quadrilaterals {
        /// Normalized annotation rectangle. Every retained point is inside it.
        rect: kurbo::Rect,
        /// ISO-order quadrilaterals whose first edge is the bottom edge.
        quads: Vec<LinkQuadrilateral>,
    },
}

impl LinkBorderGeometry {
    /// Return the normalized source-space annotation rectangle.
    #[must_use]
    pub const fn rect(&self) -> kurbo::Rect {
        match self {
            Self::RoundedRectangle { rect, .. }
            | Self::Rectangle { rect }
            | Self::Quadrilaterals { rect, .. } => *rect,
        }
    }
}

/// Standard PDF border style for a synthesized Link appearance.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkBorderStyle {
    /// Solid rectangle.
    Solid,
    /// Dashed rectangle with phase zero.
    Dashed {
        /// Alternating dash and gap lengths in default user-space units.
        dash_array: Vec<f32>,
    },
    /// Simulated raised rectangle.
    Beveled,
    /// Simulated recessed rectangle.
    Inset,
    /// Single line along the bottom of the annotation geometry.
    Underline,
}

/// Direct device color used by a synthesized annotation border.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinkBorderColor {
    /// No color; painting is transparent.
    Transparent,
    /// One `DeviceGray` component.
    Gray([f32; 1]),
    /// Three `DeviceRGB` components.
    Rgb([f32; 3]),
    /// Four `DeviceCMYK` components.
    Cmyk([f32; 4]),
}

impl LinkBorderColor {
    /// Create Hayro's typed color while retaining the original PDF device
    /// color family and components.
    #[must_use]
    pub fn to_color(self) -> Option<Color> {
        match self {
            Self::Transparent => None,
            Self::Gray(components) => Some(Color::new(
                ColorSpace::device_gray(),
                smallvec![components[0]],
                1.0,
            )),
            Self::Rgb(components) => Some(Color::new(
                ColorSpace::device_rgb(),
                smallvec![components[0], components[1], components[2]],
                1.0,
            )),
            Self::Cmyk(components) => Some(Color::new(
                ColorSpace::device_cmyk(),
                smallvec![components[0], components[1], components[2], components[3]],
                1.0,
            )),
        }
    }
}

/// Fully resolved visual parameters for a synthesized Link border.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkBorder {
    /// Source-space border geometry.
    pub geometry: LinkBorderGeometry,
    /// Border width in default user-space units. Zero means no paint.
    pub width: f32,
    /// Standard border style.
    pub style: LinkBorderStyle,
    /// Original direct device color. Transparent means no paint.
    pub color: LinkBorderColor,
}

impl LinkBorder {
    /// Whether this border contributes any page pixels.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.width > 0.0 && !matches!(self.color, LinkBorderColor::Transparent)
    }
}

/// Resolve the default visual border for a Link annotation without an
/// explicit appearance stream.
///
/// `/AP` takes precedence and produces `None`. `/BS` takes precedence over the
/// legacy `/Border` array. The returned values remain in default user space so
/// a retained device can apply the page root transform without losing source
/// units used by widths and dash patterns.
pub fn resolve_link_border(annotation: &Dict<'_>) -> Result<Option<LinkBorder>, LinkBorderError> {
    resolve_link_border_with_limit(annotation, DEFAULT_MAX_LINK_QUADS)
}

/// Resolve a Link border while bounding the number of retained activation
/// quadrilaterals.
pub fn resolve_link_border_with_limit(
    annotation: &Dict<'_>,
    max_quads: usize,
) -> Result<Option<LinkBorder>, LinkBorderError> {
    if annotation
        .get::<Name<'_>>(SUBTYPE)
        .as_ref()
        .is_none_or(|subtype| subtype.as_ref() != b"Link")
        || annotation.contains_key(AP)
    {
        return Ok(None);
    }

    let rect = annotation
        .get::<Rect>(RECT)
        .ok_or(LinkBorderError::InvalidRectangle)?
        .to_kurbo();
    if !finite_rect(rect) || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(LinkBorderError::InvalidRectangle);
    }
    // `/CA` is an additional entry specific to markup annotations in PDF
    // 32000-1. Link annotations are not markup annotations, so assigning it an
    // opacity meaning here would be an undocumented approximation. Keep the
    // adapter conservative until an extension specification can be identified.
    if annotation.contains_key(CA) {
        return Err(LinkBorderError::UnsupportedOpacity);
    }
    let color = annotation_color(annotation)?;

    let (geometry, width, style) = if annotation.contains_key(BS) {
        let style_dict = annotation
            .get::<Dict<'_>>(BS)
            .ok_or(LinkBorderError::InvalidStyleDictionary)?;
        if style_dict.contains_key(TYPE)
            && style_dict
                .get::<Name<'_>>(TYPE)
                .as_ref()
                .is_none_or(|kind| kind.as_ref() != b"Border")
        {
            return Err(LinkBorderError::InvalidStyleType);
        }
        let width =
            optional_nonnegative_f32(&style_dict, W, 1.0).ok_or(LinkBorderError::InvalidWidth)?;
        if style_dict.contains_key(D) && style_dict.get::<Array<'_>>(D).is_none() {
            return Err(LinkBorderError::InvalidDashArray);
        }
        let style = match style_dict.get::<Name<'_>>(S) {
            Some(style) if style.as_ref() == b"S" => {
                validate_unused_dash(&style_dict)?;
                LinkBorderStyle::Solid
            }
            Some(style) if style.as_ref() == b"D" => LinkBorderStyle::Dashed {
                dash_array: match style_dict.get::<Array<'_>>(D) {
                    Some(dash) => dash_array(&dash)?,
                    None => vec![3.0],
                },
            },
            Some(style) if style.as_ref() == b"B" => {
                validate_unused_dash(&style_dict)?;
                LinkBorderStyle::Beveled
            }
            Some(style) if style.as_ref() == b"I" => {
                validate_unused_dash(&style_dict)?;
                LinkBorderStyle::Inset
            }
            Some(style) if style.as_ref() == b"U" => {
                validate_unused_dash(&style_dict)?;
                LinkBorderStyle::Underline
            }
            Some(_) => return Err(LinkBorderError::InvalidStyle),
            None if style_dict.contains_key(S) => return Err(LinkBorderError::InvalidStyle),
            None => {
                validate_unused_dash(&style_dict)?;
                LinkBorderStyle::Solid
            }
        };
        let geometry = match resolve_link_quad_points(annotation, max_quads)
            .map_err(LinkBorderError::InvalidQuadPoints)?
        {
            Some(quads) => LinkBorderGeometry::Quadrilaterals { rect, quads },
            None => LinkBorderGeometry::Rectangle { rect },
        };
        (geometry, width, style)
    } else {
        let border = legacy_border(annotation)?;
        let style = match border.dash_array {
            Some(dash_array) => LinkBorderStyle::Dashed { dash_array },
            None => LinkBorderStyle::Solid,
        };
        (
            LinkBorderGeometry::RoundedRectangle {
                rect,
                horizontal_radius: border.horizontal_radius,
                vertical_radius: border.vertical_radius,
            },
            border.width,
            style,
        )
    };

    Ok(Some(LinkBorder {
        geometry,
        width,
        style,
        color,
    }))
}

/// Resolve a Link annotation's activation quadrilaterals.
///
/// An absent entry, or an entry containing any coordinate outside `/Rect`,
/// returns `None`, which instructs callers to use `/Rect` as required by ISO
/// 32000. Malformed and over-limit entries return an error so callers never
/// silently substitute a different hit region.
pub fn resolve_link_quad_points(
    annotation: &Dict<'_>,
    max_quads: usize,
) -> Result<Option<Vec<LinkQuadrilateral>>, LinkQuadPointsError> {
    if !annotation.contains_key(QUADPOINTS) {
        return Ok(None);
    }
    let rect = annotation
        .get::<Rect>(RECT)
        .ok_or(LinkQuadPointsError::InvalidRectangle)?
        .to_kurbo();
    if !finite_rect(rect) || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(LinkQuadPointsError::InvalidRectangle);
    }
    let array = annotation
        .get::<Array<'_>>(QUADPOINTS)
        .ok_or(LinkQuadPointsError::InvalidArray)?;
    let item_count = array.raw_iter().count();
    if item_count == 0 || !item_count.is_multiple_of(8) {
        return Err(LinkQuadPointsError::InvalidLength);
    }
    let quad_count = item_count / 8;
    if quad_count > max_quads {
        return Err(LinkQuadPointsError::ItemLimit);
    }

    let mut values = array.flex_iter();
    let mut raw_quads = Vec::with_capacity(quad_count);
    let mut outside_rect = false;
    for _ in 0..quad_count {
        let mut points = [kurbo::Point::ZERO; 4];
        for point in &mut points {
            let x = values
                .next::<Number>()
                .map(|number| number.as_f64())
                .filter(|value| value.is_finite())
                .ok_or(LinkQuadPointsError::InvalidCoordinate)?;
            let y = values
                .next::<Number>()
                .map(|number| number.as_f64())
                .filter(|value| value.is_finite())
                .ok_or(LinkQuadPointsError::InvalidCoordinate)?;
            *point = kurbo::Point::new(x, y);
            outside_rect |= x < rect.x0 || x > rect.x1 || y < rect.y0 || y > rect.y1;
        }
        raw_quads.push(points);
    }
    if outside_rect {
        return Ok(None);
    }

    let mut quads = Vec::with_capacity(quad_count);
    for points in raw_quads {
        let normalized = if valid_counter_clockwise_quad(points) {
            points
        } else if quad_self_intersects(points) {
            let adobe_z_order = [points[2], points[3], points[1], points[0]];
            if !valid_counter_clockwise_quad(adobe_z_order) {
                return Err(LinkQuadPointsError::InvalidGeometry);
            }
            adobe_z_order
        } else {
            return Err(LinkQuadPointsError::InvalidGeometry);
        };
        quads.push(LinkQuadrilateral {
            bottom_left: normalized[0],
            bottom_right: normalized[1],
            top_right: normalized[2],
            top_left: normalized[3],
        });
    }
    Ok(Some(quads))
}

fn valid_counter_clockwise_quad(points: [kurbo::Point; 4]) -> bool {
    let scale = points
        .iter()
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let epsilon = f64::EPSILON * scale * scale * 64.0;
    !quad_self_intersects(points) && signed_double_area(points) > epsilon
}

fn signed_double_area(points: [kurbo::Point; 4]) -> f64 {
    points
        .into_iter()
        .zip(points.into_iter().cycle().skip(1))
        .take(4)
        .map(|(first, second)| first.x * second.y - first.y * second.x)
        .sum()
}

fn quad_self_intersects(points: [kurbo::Point; 4]) -> bool {
    segments_intersect(points[0], points[1], points[2], points[3])
        || segments_intersect(points[1], points[2], points[3], points[0])
}

fn segments_intersect(
    first_start: kurbo::Point,
    first_end: kurbo::Point,
    second_start: kurbo::Point,
    second_end: kurbo::Point,
) -> bool {
    let first_a = orientation(first_start, first_end, second_start);
    let first_b = orientation(first_start, first_end, second_end);
    let second_a = orientation(second_start, second_end, first_start);
    let second_b = orientation(second_start, second_end, first_end);
    let scale = [first_start, first_end, second_start, second_end]
        .into_iter()
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let epsilon = f64::EPSILON * scale * scale * 64.0;
    if first_a.abs() <= epsilon
        || first_b.abs() <= epsilon
        || second_a.abs() <= epsilon
        || second_b.abs() <= epsilon
    {
        return true;
    }
    first_a.is_sign_positive() != first_b.is_sign_positive()
        && second_a.is_sign_positive() != second_b.is_sign_positive()
}

fn orientation(start: kurbo::Point, end: kurbo::Point, point: kurbo::Point) -> f64 {
    (end.x - start.x) * (point.y - start.y) - (end.y - start.y) * (point.x - start.x)
}

struct LegacyBorder {
    horizontal_radius: f32,
    vertical_radius: f32,
    width: f32,
    dash_array: Option<Vec<f32>>,
}

fn legacy_border(annotation: &Dict<'_>) -> Result<LegacyBorder, LinkBorderError> {
    if !annotation.contains_key(BORDER) {
        return Ok(LegacyBorder {
            horizontal_radius: 0.0,
            vertical_radius: 0.0,
            width: 1.0,
            dash_array: None,
        });
    }
    let border = annotation
        .get::<Array<'_>>(BORDER)
        .ok_or(LinkBorderError::InvalidLegacyBorder)?;
    let count = border.raw_iter().count();
    if !matches!(count, 3 | 4) {
        return Err(LinkBorderError::InvalidLegacyBorder);
    }
    let mut values = border.flex_iter();
    let horizontal_radius = values
        .next::<Number>()
        .and_then(|value| finite_nonnegative_f32(value.as_f64()))
        .ok_or(LinkBorderError::InvalidLegacyBorder)?;
    let vertical_radius = values
        .next::<Number>()
        .and_then(|value| finite_nonnegative_f32(value.as_f64()))
        .ok_or(LinkBorderError::InvalidLegacyBorder)?;
    let width = values
        .next::<Number>()
        .and_then(|value| finite_nonnegative_f32(value.as_f64()))
        .ok_or(LinkBorderError::InvalidLegacyBorder)?;
    let dash = if count == 4 {
        let dash = values
            .next::<Array<'_>>()
            .ok_or(LinkBorderError::InvalidDashArray)?;
        Some(dash_array(&dash)?)
    } else {
        None
    };
    Ok(LegacyBorder {
        horizontal_radius,
        vertical_radius,
        width,
        dash_array: dash,
    })
}

fn validate_unused_dash(style: &Dict<'_>) -> Result<(), LinkBorderError> {
    if let Some(dash) = style.get::<Array<'_>>(D) {
        dash_array(&dash)?;
    }
    Ok(())
}

fn dash_array(array: &Array<'_>) -> Result<Vec<f32>, LinkBorderError> {
    let count = array.raw_iter().count();
    if count > MAX_LINK_BORDER_DASH_ITEMS {
        return Err(LinkBorderError::DashArrayLimit);
    }
    let numbers = array.iter::<Number>().collect::<Vec<_>>();
    if numbers.len() != count {
        return Err(LinkBorderError::InvalidDashArray);
    }
    let mut values = Vec::with_capacity(numbers.len());
    let mut any_positive = false;
    for number in numbers {
        let value =
            finite_nonnegative_f32(number.as_f64()).ok_or(LinkBorderError::InvalidDashArray)?;
        any_positive |= value > 0.0;
        values.push(value);
    }
    if !values.is_empty() && !any_positive {
        return Err(LinkBorderError::InvalidDashArray);
    }
    Ok(values)
}

fn annotation_color(annotation: &Dict<'_>) -> Result<LinkBorderColor, LinkBorderError> {
    if !annotation.contains_key(C) {
        return Ok(LinkBorderColor::Gray([0.0]));
    }
    let color = annotation
        .get::<Array<'_>>(C)
        .ok_or(LinkBorderError::InvalidColor)?;
    let count = color.raw_iter().count();
    if !matches!(count, 0 | 1 | 3 | 4) {
        return Err(LinkBorderError::InvalidColor);
    }
    let numbers = color.iter::<Number>().collect::<Vec<_>>();
    if numbers.len() != count {
        return Err(LinkBorderError::InvalidColor);
    }
    let values = numbers
        .into_iter()
        .map(|number| normalized_f32(number.as_f64()).ok_or(LinkBorderError::InvalidColor))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match values.as_slice() {
        [] => LinkBorderColor::Transparent,
        [gray] => LinkBorderColor::Gray([*gray]),
        [red, green, blue] => LinkBorderColor::Rgb([*red, *green, *blue]),
        [cyan, magenta, yellow, key] => LinkBorderColor::Cmyk([*cyan, *magenta, *yellow, *key]),
        _ => unreachable!(),
    })
}

fn optional_nonnegative_f32(dict: &Dict<'_>, key: &[u8], default: f32) -> Option<f32> {
    match dict.get::<Number>(key) {
        Some(value) => finite_nonnegative_f32(value.as_f64()),
        None if dict.contains_key(key) => None,
        None => Some(default),
    }
}

fn finite_nonnegative_f32(value: f64) -> Option<f32> {
    let converted = value as f32;
    (value.is_finite() && value >= 0.0 && converted.is_finite()).then_some(converted)
}

fn normalized_f32(value: f64) -> Option<f32> {
    let converted = value as f32;
    (value.is_finite() && (0.0..=1.0).contains(&value) && converted.is_finite())
        .then_some(converted)
}

/// Whether an annotation participates in the normal interactive screen view.
///
/// `Hidden` and `NoView` always suppress an annotation. `Invisible` suppresses
/// only an unknown annotation type for which no standard handler exists, as
/// specified by the PDF annotation-flags table.
pub fn annotation_is_visible_on_screen(annotation: &Dict<'_>) -> bool {
    let flags = annotation.get::<u32>(F).unwrap_or(0);
    if flags & (2 | 32) != 0 {
        return false;
    }
    if flags & 1 == 0 {
        return true;
    }
    annotation
        .get::<Name<'_>>(SUBTYPE)
        .is_some_and(|subtype| is_standard_annotation_subtype(subtype.as_ref()))
}

fn is_standard_annotation_subtype(subtype: &[u8]) -> bool {
    matches!(
        subtype,
        b"Text"
            | b"Link"
            | b"FreeText"
            | b"Line"
            | b"Square"
            | b"Circle"
            | b"Polygon"
            | b"PolyLine"
            | b"Highlight"
            | b"Underline"
            | b"Squiggly"
            | b"StrikeOut"
            | b"Stamp"
            | b"Caret"
            | b"Ink"
            | b"Popup"
            | b"FileAttachment"
            | b"Sound"
            | b"Movie"
            | b"Widget"
            | b"Screen"
            | b"PrinterMark"
            | b"TrapNet"
            | b"Watermark"
            | b"3D"
            | b"Redact"
            | b"Projection"
    )
}

/// Select the normal appearance stream for an annotation.
///
/// An `/AP /N` stream is returned directly. If `/N` is an appearance-state
/// dictionary, the annotation's required `/AS` name is used exactly; this
/// function never guesses a state or silently substitutes `/Off`.
pub fn select_annotation_normal_appearance<'a>(
    annotation: &Dict<'a>,
) -> Result<Option<Stream<'a>>, AnnotationAppearanceError> {
    if !annotation.contains_key(AP) {
        return Ok(None);
    }
    let appearances = annotation
        .get::<Dict<'_>>(AP)
        .ok_or(AnnotationAppearanceError::InvalidAppearanceDictionary)?;
    if !appearances.contains_key(N) {
        return Err(AnnotationAppearanceError::MissingNormalAppearance);
    }
    match appearances
        .get::<Object<'_>>(N)
        .ok_or(AnnotationAppearanceError::MissingNormalAppearance)?
    {
        Object::Stream(stream) => Ok(Some(stream)),
        Object::Dict(states) => {
            let state = annotation
                .get::<Name<'_>>(AS)
                .ok_or(AnnotationAppearanceError::MissingAppearanceState)?;
            if !states.contains_key(state.as_ref()) {
                return Err(AnnotationAppearanceError::UnknownAppearanceState);
            }
            states
                .get::<Stream<'_>>(state)
                .map(Some)
                .ok_or(AnnotationAppearanceError::InvalidNormalAppearance)
        }
        _ => Err(AnnotationAppearanceError::InvalidNormalAppearance),
    }
}

pub(crate) struct ResolvedAnnotationAppearance<'a> {
    pub(crate) form: FormXObject<'a>,
    pub(crate) transform: Affine,
}

pub(crate) fn resolve_annotation_appearance<'a>(
    annotation: &Dict<'a>,
) -> Result<Option<ResolvedAnnotationAppearance<'a>>, AnnotationAppearanceError> {
    let Some(stream) = select_annotation_normal_appearance(annotation)? else {
        return Ok(None);
    };
    let dict = stream.dict();
    if dict
        .get::<Name<'_>>(SUBTYPE)
        .as_ref()
        .is_none_or(|subtype| subtype.as_ref() != b"Form")
    {
        return Err(AnnotationAppearanceError::InvalidFormSubtype);
    }
    if dict.contains_key(TYPE)
        && dict
            .get::<Name<'_>>(TYPE)
            .as_ref()
            .is_none_or(|kind| kind.as_ref() != b"XObject")
    {
        return Err(AnnotationAppearanceError::InvalidFormType);
    }
    if dict.contains_key(FORMTYPE) && dict.get::<i32>(FORMTYPE) != Some(1) {
        return Err(AnnotationAppearanceError::InvalidFormType);
    }
    if dict.contains_key(RESOURCES) && dict.get::<Dict<'_>>(RESOURCES).is_none() {
        return Err(AnnotationAppearanceError::InvalidFormResources);
    }

    let form = FormXObject::new(&stream).ok_or(AnnotationAppearanceError::InvalidFormStream)?;
    if form.bbox.iter().any(|value| !value.is_finite())
        || form
            .matrix
            .as_coeffs()
            .iter()
            .any(|value| !value.is_finite())
    {
        return Err(AnnotationAppearanceError::InvalidFormStream);
    }
    let rect = annotation
        .get::<Rect>(RECT)
        .ok_or(AnnotationAppearanceError::InvalidAnnotationRectangle)?
        .to_kurbo();
    if !finite_rect(rect) || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(AnnotationAppearanceError::InvalidAnnotationRectangle);
    }

    let transformed = (form.matrix
        * kurbo::Rect::new(
            form.bbox[0] as f64,
            form.bbox[1] as f64,
            form.bbox[2] as f64,
            form.bbox[3] as f64,
        )
        .to_path(0.1))
    .bounding_box();
    if !finite_rect(transformed) || transformed.width() <= 0.0 || transformed.height() <= 0.0 {
        return Err(AnnotationAppearanceError::InvalidAppearanceMapping);
    }

    let scale_x = rect.width() / transformed.width();
    let scale_y = rect.height() / transformed.height();
    let transform = Affine::new([
        scale_x,
        0.0,
        0.0,
        scale_y,
        rect.x0 - scale_x * transformed.x0,
        rect.y0 - scale_y * transformed.y0,
    ]);
    if transform.as_coeffs().iter().any(|value| !value.is_finite()) {
        return Err(AnnotationAppearanceError::InvalidAppearanceMapping);
    }

    Ok(Some(ResolvedAnnotationAppearance { form, transform }))
}

fn finite_rect(rect: kurbo::Rect) -> bool {
    [rect.x0, rect.y0, rect.x1, rect.y1]
        .into_iter()
        .all(f64::is_finite)
}
