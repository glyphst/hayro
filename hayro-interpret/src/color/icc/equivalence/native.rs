//! Complete forward bounds for the separate native-component image consumer.
use super::*;
use moxcms::{PointeeSizeExpressible, TransformF64Executor};

const INPUT_BINS: usize = 65_536;
const BATCH_PIXELS: usize = 1024;

impl ICCProfile {
    pub(in crate::color) fn image_device_rgb_bounds(
        &self,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<IccDeviceRgbBounds, IccEquivalenceError> {
        checkpoint(&mut cancelled)?;
        let cache = &self.data.image_equivalence[self.intent as usize];
        if let Some(result) = cache.get() {
            return *result;
        }
        let result = self.calculate_image_device_rgb_bounds(&mut cancelled);
        if !matches!(
            result,
            Err(IccEquivalenceError::Cancelled | IccEquivalenceError::MemoryLimit)
        ) {
            let _ = cache.set(result);
        }
        result
    }

    fn calculate_image_device_rgb_bounds(
        &self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<IccDeviceRgbBounds, IccEquivalenceError> {
        if self.number_components() != 3 || !self.data.matrix_tags_only {
            return Err(IccEquivalenceError::Unproved);
        }
        // This covers profile clones and the complete input/output validation
        // tables. The real image executor retains its own charged ownership.
        let _proof_memory = self
            .reserve_memory(memory::native_construction_bytes(
                &self.data.src_profile,
                memory::native_transform_bytes(&self.data.src_profile, self.intent),
            ))
            .map_err(|_| IccEquivalenceError::MemoryLimit)?;
        let (source, destination, options) = self
            .conversion_profiles()
            .ok_or(IccEquivalenceError::Unproved)?;
        validate_matrix_shaper(&source)?;
        validate_matrix_shaper(&destination)?;
        // The pinned provider's optional extended-range evaluator defaults to
        // false, including if another consumer enables that Cargo feature.
        // conversion_profiles preserves that default and disables CICP.
        if options.allow_use_cicp_transfer
            || options.pcs_xyz_adjustment.is_some()
            || f64::NOT_FINITE_LINEAR_TABLE_SIZE != INPUT_BINS
            || f64::NOT_FINITE_GAMMA_TABLE_SIZE != INPUT_BINS
            || source
                .transform_matrix(&destination)
                .to_f32()
                .v
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 3.0)
        {
            return Err(IccEquivalenceError::Unproved);
        }
        for channel in 0..3 {
            checkpoint(cancelled)?;
            let linear = match channel {
                0 => source.build_r_linearize_table::<f64, INPUT_BINS, 1>(false),
                1 => source.build_g_linearize_table::<f64, INPUT_BINS, 1>(false),
                _ => source.build_b_linearize_table::<f64, INPUT_BINS, 1>(false),
            }
            .map_err(|_| IccEquivalenceError::Unproved)?;
            if linear
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                || linear.windows(2).any(|p| p[0] > p[1])
            {
                return Err(IccEquivalenceError::Unproved);
            }
            let curve = [
                &destination.red_trc,
                &destination.green_trc,
                &destination.blue_trc,
            ][channel];
            let gamma = destination
                .build_gamma_table::<f64, INPUT_BINS, INPUT_BINS, 1>(curve, false)
                .map_err(|_| IccEquivalenceError::Unproved)?;
            if gamma
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                || gamma.windows(2).any(|p| p[0] > p[1])
            {
                return Err(IccEquivalenceError::Unproved);
            }
        }
        checkpoint(cancelled)?;
        let transform = self.native_transform().map_err(|error| match error {
            ColorConversionError::IccMemoryLimit => IccEquivalenceError::MemoryLimit,
            _ => IccEquivalenceError::Unproved,
        })?;
        bound_native_direction(transform.executor.as_ref(), cancelled)
    }
}

fn bound_native_direction(
    transform: &TransformF64Executor,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<IccDeviceRgbBounds, IccEquivalenceError> {
    // The pinned f64 matrix executor indexes complete input tables with
    // PointeeSizeExpressible::_as_usize. Monotone tables, a finite matrix and
    // monotone output encoding enclose each held-channel result at the four
    // endpoint combinations of the other channels. Every input bin is covered,
    // including both binary64 preimage endpoints, for scalar and batch paths.
    // LUT, analytic extended-range and native inverse consumers are excluded.
    // The caller's proof reservation covers these bounded heap buffers.
    let mut input = Vec::new();
    input
        .try_reserve_exact(BATCH_PIXELS * 3)
        .map_err(|_| IccEquivalenceError::MemoryLimit)?;
    input.resize(BATCH_PIXELS * 3, 0.0_f64);
    let mut output = Vec::new();
    output
        .try_reserve_exact(BATCH_PIXELS * 3)
        .map_err(|_| IccEquivalenceError::MemoryLimit)?;
    output.resize(BATCH_PIXELS * 3, 0.0_f64);
    let mut intervals = [[0.0_f64; 2]; BATCH_PIXELS];
    let mut result = IccDeviceRgbBounds {
        max_component_error: 0.0,
        evaluated_colors: 0,
    };
    for channel in 0..3 {
        for corner in 0..4 {
            for start in (0..INPUT_BINS).step_by(BATCH_PIXELS) {
                checkpoint(cancelled)?;
                for (index, (pixel, interval)) in input
                    .chunks_exact_mut(3)
                    .zip(intervals.iter_mut())
                    .enumerate()
                {
                    *interval = native_preimage((start + index) as u16)
                        .ok_or(IccEquivalenceError::Unproved)?;
                    pixel[channel] = interval[0];
                    pixel[(channel + 1) % 3] = if corner & 1 == 0 { 0.0 } else { 1.0 };
                    pixel[(channel + 2) % 3] = if corner & 2 == 0 { 0.0 } else { 1.0 };
                }
                transform
                    .transform(&input, &mut output)
                    .map_err(|_| IccEquivalenceError::Unproved)?;
                for ((pixel, converted), &[low, high]) in input
                    .chunks_exact(3)
                    .zip(output.chunks_exact(3))
                    .zip(&intervals)
                {
                    let mut scalar = [0.0_f64; 3];
                    transform
                        .transform(pixel, &mut scalar)
                        .map_err(|_| IccEquivalenceError::Unproved)?;
                    for value in [converted[channel], scalar[channel]] {
                        let byte = super::super::native::encode_component(value)
                            .ok_or(IccEquivalenceError::Unproved)?;
                        let actual = f64::from(f32::from(byte) / 255.0);
                        result.max_component_error = result
                            .max_component_error
                            .max((actual - low).abs())
                            .max((actual - high).abs());
                    }
                    result.evaluated_colors += 2;
                }
            }
        }
    }
    checkpoint(cancelled)?;
    Ok(result)
}

fn first_native_bin(bin: u32) -> Option<f64> {
    if bin == 0 {
        return Some(0.0);
    }
    let mut value = f64::from(bin) / 65535.0;
    // Division gives only a starting point. Establish the boundary with the
    // actual provider quantizer, including multiply rounding on adjacent f64s.
    // A changed quantizer that cannot be established in this bound is refused.
    for _ in 0..8 {
        if value._as_usize() < bin as usize {
            value = value.next_up();
        } else if value.next_down()._as_usize() >= bin as usize {
            value = value.next_down();
        } else {
            return Some(value);
        }
    }
    None
}

fn native_preimage(bin: u16) -> Option<[f64; 2]> {
    let low = first_native_bin(u32::from(bin))?;
    let high = if bin == u16::MAX {
        1.0
    } else {
        first_native_bin(u32::from(bin) + 1)?.next_down()
    };
    (low <= high && low._as_usize() == usize::from(bin) && high._as_usize() == usize::from(bin))
        .then_some([low, high])
}

#[cfg(test)]
mod tests;
