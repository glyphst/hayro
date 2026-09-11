//! PDF 1.7, 7.4.4.4: TIFF component prediction and PNG byte prediction.

use crate::filter::png::{self, BytesPerPixel, RowFilter};
use crate::object::dict::keys::{BITS_PER_COMPONENT, COLORS, COLUMNS, PREDICTOR};
use crate::object::{Dict, Object};
use alloc::vec::Vec;

pub(super) fn integer_param(dict: &Dict<'_>, key: &[u8], default: usize) -> Option<usize> {
    match dict.get::<Object<'_>>(key) {
        None | Some(Object::Null(_)) => Some(default),
        Some(Object::Number(number)) => usize::try_from(number.as_i64_exact()?).ok(),
        _ => None,
    }
}

pub(super) struct PredictorParams {
    pub(super) predictor: u8,
    pub(super) colors: usize,
    pub(super) bits_per_component: u8,
    pub(super) columns: usize,
}

impl PredictorParams {
    pub(super) fn from_params(dict: &Dict<'_>) -> Option<Self> {
        let predictor = u8::try_from(integer_param(dict, PREDICTOR, 1)?).ok()?;
        // These three parameters are used only with prediction (Table 8).
        let params = if predictor == 1 {
            Self {
                predictor,
                colors: 1,
                bits_per_component: 8,
                columns: 1,
            }
        } else {
            Self {
                predictor,
                colors: integer_param(dict, COLORS, 1)?,
                bits_per_component: u8::try_from(integer_param(dict, BITS_PER_COMPONENT, 8)?)
                    .ok()?,
                columns: integer_param(dict, COLUMNS, 1)?,
            }
        };
        params.geometry()?;
        Some(params)
    }

    // Return component count, whole-byte row length, and PNG byte stride.
    fn geometry(&self) -> Option<(usize, usize, usize)> {
        if !matches!(self.predictor, 1 | 2 | 10..=15)
            || !matches!(self.bits_per_component, 1 | 2 | 4 | 8 | 16)
            || self.colors == 0
            || self.columns == 0
        {
            return None;
        }
        let samples = self.columns.checked_mul(self.colors)?;
        let row_bits = samples.checked_mul(usize::from(self.bits_per_component))?;
        let pixel_bits = self
            .colors
            .checked_mul(usize::from(self.bits_per_component))?;
        Some((samples, row_bits.div_ceil(8), pixel_bits.div_ceil(8)))
    }
}

pub(super) fn apply_predictor(mut data: Vec<u8>, params: &PredictorParams) -> Option<Vec<u8>> {
    if params.predictor == 1 {
        return Some(data);
    }
    let (samples, row_len, bpp) = params.geometry()?;
    let is_png = params.predictor >= 10;
    let encoded_row_len = row_len.checked_add(usize::from(is_png))?;
    if !data.len().is_multiple_of(encoded_row_len) {
        return None;
    }
    // No row-sized scratch allocation: even enormous valid geometry with no
    // rows stays cheap, and removing PNG tags only shrinks the decoded buffer.
    if !is_png {
        for row in data.chunks_exact_mut(row_len) {
            match params.bits_per_component {
                8 => unfilter_png(RowFilter::Sub, params.colors, &[], row),
                16 => {
                    for i in params.colors..samples {
                        let left = 2 * (i - params.colors);
                        let pos = 2 * i;
                        let value = u16::from_be_bytes([row[pos], row[pos + 1]])
                            .wrapping_add(u16::from_be_bytes([row[left], row[left + 1]]));
                        row[pos..pos + 2].copy_from_slice(&value.to_be_bytes());
                    }
                }
                bits => {
                    let bits = usize::from(bits);
                    let mask = (1_u8 << bits) - 1;
                    for i in params.colors..samples {
                        let pos = i * bits;
                        let left = (i - params.colors) * bits;
                        let shift = 8 - bits - pos % 8;
                        let prior = (row[left / 8] >> (8 - bits - left % 8)) & mask;
                        let value = ((row[pos / 8] >> shift).wrapping_add(prior)) & mask;
                        row[pos / 8] = (row[pos / 8] & !(mask << shift)) | (value << shift);
                    }
                }
            }
        }
        return Some(data);
    }

    let rows = data.len() / encoded_row_len;
    for index in 0..rows {
        let src = index * encoded_row_len;
        let dst = index * row_len;
        // Any PNG dictionary predictor (10..=15) permits all five row tags.
        let filter = RowFilter::from_u8(data[src])?;
        data.copy_within(src + 1..src + encoded_row_len, dst);
        let (done, rest) = data.split_at_mut(dst);
        let previous = done
            .len()
            .checked_sub(row_len)
            .map_or(&[][..], |start| &done[start..]);
        unfilter_png(filter, bpp, previous, &mut rest[..row_len]);
    }
    data.truncate(rows * row_len);
    Some(data)
}

fn unfilter_png(filter: RowFilter, bpp: usize, previous: &[u8], row: &mut [u8]) {
    if let Some(tbpp) = BytesPerPixel::from_row_len(row.len(), bpp) {
        png::unfilter(filter, tbpp, previous, row);
        return;
    }
    // Packed rows need not contain a whole number of byte strides. PDF also
    // permits more color components than the specialized PNG implementations.
    for i in 0..row.len() {
        let left = if i >= bpp { row[i - bpp] } else { 0 };
        let up = previous.get(i).copied().unwrap_or(0);
        let upper_left = i
            .checked_sub(bpp)
            .and_then(|j| previous.get(j))
            .copied()
            .unwrap_or(0);
        let prediction = match filter {
            RowFilter::NoFilter => 0,
            RowFilter::Sub => left,
            RowFilter::Up => up,
            RowFilter::Avg => ((u16::from(left) + u16::from(up)) / 2) as u8,
            RowFilter::Paeth => {
                let p = i16::from(left) + i16::from(up) - i16::from(upper_left);
                let a = (p - i16::from(left)).abs();
                let b = (p - i16::from(up)).abs();
                let c = (p - i16::from(upper_left)).abs();
                if a <= b && a <= c {
                    left
                } else if b <= c {
                    up
                } else {
                    upper_left
                }
            }
        };
        row[i] = row[i].wrapping_add(prediction);
    }
}
