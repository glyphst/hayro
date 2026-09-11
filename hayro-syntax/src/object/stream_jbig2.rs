//! Bilevel image dictionary contracts (PDF 1.7 §§7.4.6–7.4.7 and Table 89).
use super::{DecodeFailure, Dict, Object};
use crate::object::dict::keys::*;

/// Validate the image dictionary for a stream using `JBIG2Decode`.
///
/// Callers must identify the filter first. This does not decode compressed data
/// or resolve the colour space; its component count is checked during decoding.
pub fn validate_jbig2_image_dictionary(dict: &Dict<'_>) -> Result<(), DecodeFailure> {
    validate_bilevel_image_dictionary(dict)
}

/// Validate a CCITT image dictionary before decoding or consulting an image cache.
/// The caller must first identify the CCITT filter; generic streams are exempt.
pub fn validate_ccitt_image_dictionary(dict: &Dict<'_>) -> Result<(), DecodeFailure> {
    validate_bilevel_image_dictionary(dict)
}

fn validate_bilevel_image_dictionary(dict: &Dict<'_>) -> Result<(), DecodeFailure> {
    let entry = |short, long| {
        optional_entry(
            dict,
            if dict.contains_key(short) {
                short
            } else {
                long
            },
        )
    };
    for (short, long) in [(W, WIDTH), (H, HEIGHT)] {
        let Some(Object::Number(number)) = entry(short, long)? else {
            return Err(DecodeFailure::StreamDecode);
        };
        if number
            .as_i64_exact()
            .and_then(|value| u32::try_from(value).ok())
            .is_none_or(|value| value == 0)
        {
            return Err(DecodeFailure::StreamDecode);
        }
    }
    let stencil = match entry(IM, IMAGE_MASK)? {
        None => false,
        Some(Object::Boolean(value)) => value,
        _ => return Err(DecodeFailure::StreamDecode),
    };
    match entry(BPC, BITS_PER_COMPONENT)? {
        None if stencil => {}
        Some(Object::Number(number)) if number.as_i64_exact() == Some(1) => {}
        _ => return Err(DecodeFailure::StreamDecode),
    }
    if entry(CS, COLORSPACE)?.is_some() == stencil {
        return Err(DecodeFailure::StreamDecode);
    }
    if let Some(decode) = entry(D, DECODE)? {
        let Object::Array(array) = decode else {
            return Err(DecodeFailure::StreamDecode);
        };
        if array.raw_iter().count() != 2 {
            return Err(DecodeFailure::StreamDecode);
        }
        let mut numbers = array.iter::<crate::object::Number>();
        let (Some(low), Some(high), None) = (numbers.next(), numbers.next(), numbers.next()) else {
            return Err(DecodeFailure::StreamDecode);
        };
        if stencil && !matches!((low.as_f64(), high.as_f64()), (0.0, 1.0) | (1.0, 0.0)) {
            return Err(DecodeFailure::StreamDecode);
        }
    }
    Ok(())
}

/// Null and undefined references are absent; defined but unreadable references
/// (including ancestry cycles) and malformed values are failures.
pub(crate) fn optional_entry<'a>(
    dict: &Dict<'a>,
    key: &[u8],
) -> Result<Option<Object<'a>>, DecodeFailure> {
    if !dict.contains_key(key) {
        return Ok(None);
    }
    let raw = dict
        .get_raw::<Object<'_>>(key)
        .ok_or(DecodeFailure::StreamDecode)?;
    if raw
        .as_obj_ref()
        .is_some_and(|reference| !dict.ctx().xref().contains_object(reference.into()))
    {
        return Ok(None);
    }
    match raw.resolve(dict.ctx()).ok_or(DecodeFailure::StreamDecode)? {
        Object::Null(_) => Ok(None),
        object => Ok(Some(object)),
    }
}
