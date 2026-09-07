use crate::function::{Clamper, TupleVec, Values};
use hayro_syntax::bit_reader::BitReader;
use hayro_syntax::object::{Array, Dict, Number, Object, Stream};
use smallvec::{SmallVec, smallvec};

#[cfg(test)]
mod tests;

// Bound owned sample storage and worst-case scalar interpolation contributions
// before decoding or allocation. These do not bound the general stream decoder.
const MAX_COMPONENTS: usize = 64;
const MAX_SAMPLES: usize = 16 * 1024 * 1024;
const MAX_CONTRIBUTIONS: usize = 65_536;

/// A type 0 function (sampled function).
#[derive(Debug)]
pub(crate) struct Type0 {
    sizes: SmallVec<[usize; 4]>,
    strides: Vec<usize>,
    table: Vec<u32>,
    clamper: Clamper,
    maximum: f64,
    cubic: bool,
    encode: TupleVec,
    decode: TupleVec,
}

impl Type0 {
    pub(crate) fn new(stream: &Stream<'_>) -> Option<Self> {
        let dict = stream.dict();
        let bits = integer(dict, b"BitsPerSample")?;
        if !matches!(bits, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
            return None;
        }
        let order = if dict.contains_key(b"Order") {
            integer(dict, b"Order")?
        } else {
            1
        };
        if !matches!(order, 1 | 3) {
            return None;
        }
        let domain = pairs(dict, b"Domain")?;
        let range = pairs(dict, b"Range")?;
        if domain.iter().any(|&(a, b)| a > b) || range.iter().any(|&(a, b)| a > b) {
            return None;
        }
        let sizes = objects(dict, b"Size", MAX_COMPONENTS)?
            .into_iter()
            .map(|value| {
                let size = Number::try_from(value).ok()?.as_i64_exact()?;
                let size = usize::try_from(size).ok()?;
                (size > 0).then_some(size)
            })
            .collect::<Option<SmallVec<[_; 4]>>>()?;
        if sizes.len() != domain.len() {
            return None;
        }
        // Equal Domain endpoints are legal, but their encoding denominator is
        // undefined unless Size 1 supplies the mandated constant table index.
        if sizes
            .iter()
            .zip(&domain)
            .any(|(&size, &(low, high))| size != 1 && low == high)
        {
            return None;
        }
        let mut entries = range.len();
        let mut contributions = range.len();
        let mut strides = Vec::with_capacity(sizes.len());
        for &size in &sizes {
            strides.push(entries);
            entries = entries.checked_mul(size)?;
            // PDF 1.7 7.10.2 ignores Order 3 in dimensions smaller than 4.
            let neighbors = if order == 3 && size >= 4 {
                4
            } else {
                size.min(2)
            };
            contributions = contributions.checked_mul(neighbors)?;
            if entries > MAX_SAMPLES || contributions > MAX_CONTRIBUTIONS {
                return None;
            }
        }
        let encode = if dict.contains_key(b"Encode") {
            pairs(dict, b"Encode")?
        } else {
            sizes.iter().map(|size| (0.0, (size - 1) as f32)).collect()
        };
        let decode = if dict.contains_key(b"Decode") {
            pairs(dict, b"Decode")?
        } else {
            range.clone()
        };
        if encode.len() != domain.len() || decode.len() != range.len() {
            return None;
        }
        // Bound binary64 interpolation error before the final binary32 output
        // rounding. Each cubic axis has L1 weight norm below 2. The factor 32
        // covers input division, basis arithmetic, accumulation and decoding;
        // checked contribution counts keep the usual n*epsilon bound small.
        // Large Decode amplification can reveal offsets lost when a sample
        // coordinate is rounded, even when every intermediate remains finite.
        let cubic_axes = sizes
            .iter()
            .filter(|&&size| order == 3 && size >= 4)
            .count();
        let numerical_scale = 32.0
            * f64::EPSILON
            * (sizes.iter().sum::<usize>() + contributions) as f64
            * (1_u64 << cubic_axes) as f64;
        if decode.iter().any(|&(low, high)| {
            low != high
                && numerical_scale * (f64::from(low).abs() + f64::from(high).abs()).max(1.0)
                    > 1.0e-6
        }) {
            return None;
        }
        let decoded = stream.decoded().ok()?;
        let bytes = entries.checked_mul(bits as usize)?.div_ceil(8);
        if decoded.len() < bytes {
            return None;
        }
        // Read precisely the declared samples; byte padding and trailing stream
        // data are not sample entries and must not inflate the owned table.
        let mut reader = BitReader::new(&decoded[..bytes]);
        let table = (0..entries)
            .map(|_| reader.read(bits as u8))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            sizes,
            strides,
            table,
            clamper: Clamper {
                domain,
                range: Some(range),
            },
            maximum: ((1_u64 << bits) - 1) as f64,
            cubic: order == 3,
            encode,
            decode,
        })
    }

    pub(crate) fn eval(&self, input: Values) -> Option<Values> {
        if input.len() != self.sizes.len() || input.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let mut axes = SmallVec::<[Axis; 4]>::new();
        for (index, value) in input.into_iter().enumerate() {
            if self.sizes[index] == 1 {
                axes.push(Axis::new(0.0, 1, self.cubic));
                continue;
            }
            let (low, high) = self.clamper.domain[index];
            let (start, end) = self.encode[index];
            // All four products are exact in binary64 because their operands
            // are binary32. Compensated summation preserves small inputs when
            // large opposing Domain/Encode bounds cancel (including identity).
            let x = f64::from(value.clamp(low, high));
            let (low, high) = (f64::from(low), f64::from(high));
            let (start, end) = (f64::from(start), f64::from(end));
            let key = (sum_products([start * high, -start * x, end * x, -end * low])
                / (high - low))
                .clamp(0.0, (self.sizes[index] - 1) as f64);
            axes.push(Axis::new(key, self.sizes[index], self.cubic));
        }
        let mut samples: SmallVec<[f64; 4]> = smallvec![0.0_f64; self.decode.len()];
        self.interpolate(&axes, 0, 0, 1.0, &mut samples);
        let range = self.clamper.range.as_ref()?;
        Some(
            samples
                .into_iter()
                .enumerate()
                .map(|(index, sample)| {
                    let (start, end) = self.decode[index];
                    let value = f64::from(start)
                        + (sample / self.maximum) * (f64::from(end) - f64::from(start));
                    value.clamp(f64::from(range[index].0), f64::from(range[index].1)) as f32
                })
                .collect(),
        )
    }

    // First input dimension varies fastest, with output components adjacent.
    // The previous multilinear traversal was based on PDFBox at
    // https://github.com/apache/pdfbox/blob/bb778d4784f354c36ce032e91a0cee2169a4c598/pdfbox/src/main/java/org/apache/pdfbox/pdmodel/common/function/PDFunctionType0.java#L252
    fn interpolate(
        &self,
        axes: &[Axis],
        dimension: usize,
        offset: usize,
        weight: f64,
        out: &mut [f64],
    ) {
        if dimension == axes.len() {
            for (output, sample) in out.iter_mut().zip(&self.table[offset..]) {
                *output += weight * f64::from(*sample);
            }
            return;
        }
        for &(index, factor) in &axes[dimension].0 {
            self.interpolate(
                axes,
                dimension + 1,
                offset + index * self.strides[dimension],
                weight * factor,
                out,
            );
        }
    }
}

struct Axis(SmallVec<[(usize, f64); 4]>);

impl Axis {
    fn new(key: f64, size: usize, cubic: bool) -> Self {
        let lower = key.floor() as usize;
        let t = key - lower as f64;
        if t == 0.0 {
            return Self(smallvec![(lower, 1.0)]);
        }
        if !cubic || size < 4 {
            return Self(smallvec![(lower, 1.0 - t), (lower + 1, t)]);
        }
        // Cubic Hermite interpolation: endpoint derivatives are half the
        // difference of neighboring samples; repeat boundary samples. Written
        // from the Hermite basis, with Ghostscript 10.07.1 as an external oracle.
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        Self(smallvec![
            (lower.saturating_sub(1), -0.5 * h10),
            (lower, h00 - 0.5 * h11),
            (lower + 1, h01 + 0.5 * h10),
            ((lower + 2).min(size - 1), 0.5 * h11),
        ])
    }
}

fn integer(dict: &Dict<'_>, key: &[u8]) -> Option<i64> {
    dict.get::<Number>(key)?.as_i64_exact()
}

fn objects<'a>(dict: &Dict<'a>, key: &[u8], limit: usize) -> Option<Vec<Object<'a>>> {
    let array = dict.get::<Array<'_>>(key)?;
    let count = array.raw_iter().take(limit + 1).count();
    if count == 0 || count > limit {
        return None;
    }
    let values = array
        .iter::<Object<'_>>()
        .take(limit + 1)
        .collect::<Vec<_>>();
    (values.len() == count).then_some(values)
}

fn pairs(dict: &Dict<'_>, key: &[u8]) -> Option<TupleVec> {
    let values = objects(dict, key, MAX_COMPONENTS * 2)?;
    if !values.len().is_multiple_of(2) {
        return None;
    }
    values
        .chunks_exact(2)
        .map(|pair| {
            let start = f32::try_from(pair[0].clone()).ok()?;
            let end = f32::try_from(pair[1].clone()).ok()?;
            (start.is_finite() && end.is_finite()).then_some((start, end))
        })
        .collect()
}

// Grow a nonoverlapping expansion with error-free TwoSum operations. Four
// binary32 products fit in four binary64 components and cannot overflow or
// underflow binary64, so cancellation does not discard the smaller products.
fn sum_products(terms: [f64; 4]) -> f64 {
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
