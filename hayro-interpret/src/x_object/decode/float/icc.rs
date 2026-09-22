//! ICC image conversion without an intermediate output quantizer.
use super::{DecodeContext, Error, Samples};
use crate::color::ColorSpace;

const BATCH_PIXELS: usize = 1024;

pub(super) fn decode<const BYTES: usize>(
    context: &DecodeContext<'_>,
    icc: &ColorSpace,
    samples: &mut Samples<'_>,
    output: &mut Vec<u8>,
    mut checkpoint: impl FnMut() -> bool,
    encode: impl Fn(f64) -> [u8; BYTES],
) -> Result<(), Error> {
    // The caller has already validated/allocated the retained output capacity.
    // Charge both bounded staging buffers in addition to the CMS's own scratch
    // and retained executor. All reservations are released on error/cancellation.
    let mut remaining = u64::from(context.width) * u64::from(context.height);
    let batch = remaining.min(BATCH_PIXELS as u64) as usize;
    let components = context.color_space.component_count();
    let icc_components = icc.component_count();
    let source_length = batch * icc_components;
    let output_length = batch * 3;
    let _reservation = icc
        .reserve_icc_image_samples(source_length + output_length + components)
        .ok_or(Error::Decode)?;
    let mut values = buffer(components)?;
    let mut source = buffer(source_length)?;
    let mut converted = buffer(output_length)?;
    while remaining != 0 {
        if !checkpoint() {
            return Err(Error::Cancelled);
        }
        let count = remaining.min(BATCH_PIXELS as u64) as usize;
        for pixel in source[..count * icc_components].chunks_exact_mut(icc_components) {
            samples.read(&mut values)?;
            let native = context
                .color_space
                .image_icc_components(&values)
                .ok_or(Error::Decode)?;
            if native.len() != icc_components {
                return Err(Error::Decode);
            }
            pixel.copy_from_slice(&native);
        }
        icc.convert_icc_image_samples_f64(
            &source[..count * icc_components],
            &mut converted[..count * 3],
        )
        .ok_or(Error::Decode)?;
        for &value in &converted[..count * 3] {
            output.extend_from_slice(&encode(value));
        }
        remaining -= count as u64;
    }
    if !checkpoint() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

fn buffer(length: usize) -> Result<Vec<f64>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| Error::Allocation)?;
    values.resize(length, 0.0);
    Ok(values)
}
