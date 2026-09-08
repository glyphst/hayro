//! Bounded native component reads shared by color conversion and matte recovery.
use super::DecodeContext;
use crate::FloatImageError as Error;
use hayro_syntax::bit_reader::BitReader;

pub(super) struct Samples<'a> {
    reader: BitReader<'a>,
    decode: &'a [(f32, f32)],
    bits: u8,
    maximum: u32,
    width: u32,
    column: u32,
    remaining: u64,
}

impl<'a> Samples<'a> {
    pub(super) fn new(context: &'a DecodeContext<'_>, jpx: bool) -> Result<Self, Error> {
        let components = context.color_space.num_components() as usize;
        if context.width == 0 || context.height == 0 || components == 0 {
            return Err(Error::Decode);
        }
        if !(if jpx {
            (1..=16).contains(&context.bits_per_component)
        } else {
            matches!(context.bits_per_component, 1 | 2 | 4 | 8 | 16)
        }) || context.decode_arr.len() != components
            || context
                .decode_arr
                .iter()
                .any(|(a, b)| !a.is_finite() || !b.is_finite())
        {
            return Err(Error::Unsupported);
        }
        let required = u64::from(context.width)
            .checked_mul(components as u64)
            .and_then(|v| v.checked_mul(u64::from(context.bits_per_component)))
            .map(|v| v.div_ceil(8))
            .and_then(|v| v.checked_mul(u64::from(context.height)))
            .ok_or(Error::Limit {
                requested: u64::MAX,
            })?;
        if required > context.decoded.data.len() as u64 {
            return Err(Error::Decode);
        }
        Ok(Self {
            reader: BitReader::new(&context.decoded.data),
            decode: &context.decode_arr,
            bits: context.bits_per_component,
            maximum: (1_u32 << context.bits_per_component) - 1,
            width: context.width,
            column: 0,
            remaining: u64::from(context.width) * u64::from(context.height),
        })
    }

    pub(super) fn read(&mut self, output: &mut [f64]) -> Result<(), Error> {
        if self.remaining == 0 || output.len() != self.decode.len() {
            return Err(Error::Decode);
        }
        for (value, &(low, high)) in output.iter_mut().zip(self.decode) {
            *value = decode_sample(
                self.reader.read(self.bits).ok_or(Error::Decode)?,
                self.maximum,
                low,
                high,
            );
        }
        self.remaining -= 1;
        self.column += 1;
        if self.column == self.width {
            self.column = 0;
            self.reader.align();
        }
        Ok(())
    }
}

pub(super) fn decode_sample(sample: u32, maximum: u32, low: f32, high: f32) -> f64 {
    // A binary32 coefficient times a <=16-bit integer is exact in binary64.
    // Compensate their sum before division, retaining opposing Decode bounds.
    let a = f64::from(low) * f64::from(maximum - sample);
    let b = f64::from(high) * f64::from(sample);
    let sum = a + b;
    let virtual_b = sum - a;
    let error = (a - (sum - virtual_b)) + (b - virtual_b);
    sum / f64::from(maximum) + error / f64::from(maximum)
}
