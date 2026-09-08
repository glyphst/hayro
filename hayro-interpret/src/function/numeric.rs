//! Bounded dictionary arrays and cancellation-resistant affine arithmetic.
use super::{TupleVec, Values};
use hayro_syntax::object::{Array, Dict, Object};

pub(super) const MAX_COMPONENTS: usize = 64;

pub(super) fn present(dict: &Dict<'_>, key: &[u8]) -> bool {
    !matches!(dict.get::<Object<'_>>(key), None | Some(Object::Null(_)))
}

pub(super) fn objects<'a>(dict: &Dict<'a>, key: &[u8], limit: usize) -> Option<Vec<Object<'a>>> {
    let array = dict.get::<Array<'_>>(key)?;
    let count = array.raw_iter().take(limit + 1).count();
    if count > limit {
        return None;
    }
    let values = array
        .iter::<Object<'_>>()
        .take(limit + 1)
        .collect::<Vec<_>>();
    (values.len() == count).then_some(values)
}

pub(super) fn values(dict: &Dict<'_>, key: &[u8], limit: usize) -> Option<Values> {
    objects(dict, key, limit)?
        .into_iter()
        .map(|object| {
            let value = f32::try_from(object).ok()?;
            value.is_finite().then_some(value)
        })
        .collect()
}

pub(super) fn pairs(dict: &Dict<'_>, key: &[u8], limit: usize) -> Option<TupleVec> {
    let values = values(dict, key, limit.checked_mul(2)?)?;
    if values.is_empty() || !values.len().is_multiple_of(2) {
        return None;
    }
    Some(
        values
            .chunks_exact(2)
            .map(|pair| (pair[0], pair[1]))
            .collect(),
    )
}

/// All products of two finite binary32 operands are exact and normal in
/// binary64. Preserve their small terms through cancellation before division.
pub(super) fn affine(x: f32, low: f32, high: f32, start: f32, end: f32) -> f64 {
    if x == low {
        return f64::from(start);
    }
    if x == high {
        return f64::from(end);
    }
    let [x, low, high, start, end] = [x, low, high, start, end].map(f64::from);
    sum_products([start * high, -start * x, end * x, -end * low]) / (high - low)
}

// Grow a nonoverlapping expansion with error-free TwoSum operations. Four
// binary32 products fit in four binary64 components and cannot overflow or
// underflow binary64, so cancellation does not discard the smaller products.
pub(super) fn sum_products(terms: [f64; 4]) -> f64 {
    let mut parts = [0.0; 4];
    let mut len = 0;
    for mut value in terms {
        let mut next = 0;
        for index in 0..len {
            let term = parts[index];
            let sum = value + term;
            let virtual_term = sum - value;
            let error = (value - (sum - virtual_term)) + (term - virtual_term);
            if error != 0.0 {
                parts[next] = error;
                next += 1;
            }
            value = sum;
        }
        parts[next] = value;
        len = next + 1;
    }
    parts[..len].iter().sum()
}
