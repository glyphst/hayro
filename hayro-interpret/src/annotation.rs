use crate::RectExt;
use crate::x_object::FormXObject;
use hayro_syntax::object::dict::keys::{AP, AS, F, FORMTYPE, N, RECT, RESOURCES, SUBTYPE, TYPE};
use hayro_syntax::object::{Dict, Name, Object, Rect, Stream};
use kurbo::{Affine, Shape};

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
