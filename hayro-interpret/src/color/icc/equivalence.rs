//! Complete byte-consumer bounds for bidirectional RGB matrix/TRC profiles.
use super::*;
use moxcms::ToneReprCurve;

/// Pointwise component error against the corresponding normalized RGB input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IccDeviceRgbBounds {
    /// Maximum error in either direction, including input byte rounding.
    pub max_component_error: f64,
    /// Number of scalar and batched color evaluations used by the proof.
    pub evaluated_colors: u32,
}

/// A failed proof is not permission to substitute a device blending space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IccEquivalenceError {
    /// The profile or conversion is outside the complete proof's subset.
    Unproved,
    /// The caller cancelled the bounded calculation.
    Cancelled,
}

pub(super) fn matrix_tags_only(bytes: &[u8]) -> bool {
    let Some(count) = bytes
        .get(128..132)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_be_bytes)
    else {
        return false;
    };
    let Some(end) = (count as usize)
        .checked_mul(12)
        .and_then(|n| n.checked_add(132))
    else {
        return false;
    };
    let Some(tags) = bytes.get(132..end) else {
        return false;
    };
    // Inspect declarations as well as parsed fields: an unrecognized LUT tag
    // must not disappear into a matrix-only proof. D/B tags are later ICC forms.
    !tags
        .chunks_exact(12)
        .any(|tag| matches!(&tag[..3], b"A2B" | b"B2A" | b"D2B" | b"B2D"))
}

impl ICCProfile {
    pub(in crate::color) fn device_rgb_bounds(
        &self,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<IccDeviceRgbBounds, IccEquivalenceError> {
        checkpoint(&mut cancelled)?;
        let cache = &self.data.equivalence[self.intent as usize];
        if let Some(result) = cache.get() {
            return *result;
        }
        let result = self.calculate_device_rgb_bounds(&mut cancelled);
        // Cancellation belongs to this caller, never to the shared profile.
        // Avoid blocking another caller on an in-progress, cancellable proof.
        if result != Err(IccEquivalenceError::Cancelled) {
            let _ = cache.set(result);
        }
        result
    }

    fn calculate_device_rgb_bounds(
        &self,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<IccDeviceRgbBounds, IccEquivalenceError> {
        if self.number_components() != 3 || !self.data.matrix_tags_only {
            // Gray -> neutral RGB alone does not prove RGB -> DeviceGray;
            // generic ICC destinations and multidimensional LUTs are separate.
            return Err(IccEquivalenceError::Unproved);
        }
        let (source, destination, options) = self
            .conversion_profiles()
            .ok_or(IccEquivalenceError::Unproved)?;
        validate_profile(&source, &mut cancelled)?;
        validate_profile(&destination, &mut cancelled)?;
        for (from, to) in [(&source, &destination), (&destination, &source)] {
            let matrix = from.transform_matrix(to).to_f32();
            if matrix
                .v
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > 3.0)
            {
                return Err(IccEquivalenceError::Unproved);
            }
        }
        let IccTransform::Rgb(forward) = self.transform().ok_or(IccEquivalenceError::Unproved)?
        else {
            return Err(IccEquivalenceError::Unproved);
        };
        // For matrix/TRC profiles, exchanging the actual forward profiles also
        // inverts the explicitly folded absolute-media-white scaling.
        let reverse = destination
            .create_transform_8bit(Layout::Rgb, &source, Layout::Rgb, options)
            .map_err(|_| IccEquivalenceError::Unproved)?;
        let mut result = IccDeviceRgbBounds {
            max_component_error: 0.0,
            evaluated_colors: 0,
        };
        for transform in [forward.as_ref(), reverse.as_ref()] {
            bound_direction(transform, &mut cancelled, &mut result)?;
        }
        Ok(result)
    }
}

fn checkpoint(cancelled: &mut impl FnMut() -> bool) -> Result<(), IccEquivalenceError> {
    if cancelled() {
        Err(IccEquivalenceError::Cancelled)
    } else {
        Ok(())
    }
}

fn validate_profile(
    profile: &ColorProfile,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), IccEquivalenceError> {
    if profile.color_space != DataColorSpace::Rgb
        || profile.pcs != DataColorSpace::Xyz
        || !profile.is_matrix_shaper()
        || profile.lut_a_to_b_perceptual.is_some()
        || profile.lut_a_to_b_colorimetric.is_some()
        || profile.lut_a_to_b_saturation.is_some()
        || profile.lut_b_to_a_perceptual.is_some()
        || profile.lut_b_to_a_colorimetric.is_some()
        || profile.lut_b_to_a_saturation.is_some()
    {
        return Err(IccEquivalenceError::Unproved);
    }
    let [Some(red), Some(green), Some(blue)] =
        [&profile.red_trc, &profile.green_trc, &profile.blue_trc]
    else {
        return Err(IccEquivalenceError::Unproved);
    };
    // moxcms 0.8.1's uniform-TRC shortcut uses OR between adjacent equalities.
    // Do not certify a profile whose unequal curve can be ignored by that path.
    let same_variant = matches!(
        (red, green, blue),
        (
            ToneReprCurve::Lut(_),
            ToneReprCurve::Lut(_),
            ToneReprCurve::Lut(_)
        ) | (
            ToneReprCurve::Parametric(_),
            ToneReprCurve::Parametric(_),
            ToneReprCurve::Parametric(_)
        )
    );
    if same_variant && (red == green || green == blue) && !(red == green && green == blue) {
        return Err(IccEquivalenceError::Unproved);
    }
    // These are the exact 8-bit consumer's input tables, with CICP disabled.
    for channel in 0..3 {
        checkpoint(cancelled)?;
        let linear = match channel {
            0 => profile.build_r_linearize_table::<u8, 256, 8>(false),
            1 => profile.build_g_linearize_table::<u8, 256, 8>(false),
            _ => profile.build_b_linearize_table::<u8, 256, 8>(false),
        }
        .map_err(|_| IccEquivalenceError::Unproved)?;
        if linear
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || linear.windows(2).any(|p| p[0] > p[1])
        {
            return Err(IccEquivalenceError::Unproved);
        }
        let curve = [&profile.red_trc, &profile.green_trc, &profile.blue_trc][channel];
        let gamma = profile
            .build_gamma_table::<u8, 65_536, 4096, 8>(curve, false)
            .map_err(|_| IccEquivalenceError::Unproved)?;
        if gamma[..4096].windows(2).any(|p| p[0] > p[1]) {
            return Err(IccEquivalenceError::Unproved);
        }
    }
    Ok(())
}

fn bound_direction(
    transform: &Transform8BitExecutor,
    cancelled: &mut impl FnMut() -> bool,
    result: &mut IccDeviceRgbBounds,
) -> Result<(), IccEquivalenceError> {
    // With monotone input/output tables, each output channel is monotone in
    // each of the other two inputs (its matrix coefficient determines the sign).
    // Float multiply/add/FMA and byte indexing/clipping are monotone. In the
    // Q2.13 path, normalized linear values and |matrix| <= 3 keep all products
    // and three-term sums inside i32 (the Q1.30 path uses i64 sums). Thus four endpoint combinations enclose
    // every other-byte pair for each of the 256 values of the held component.
    // This property is tied to moxcms 0.8.1's matrix consumer, not general ICC LUTs.
    let mut input = [0_u8; 768];
    let mut output = [0_u8; 768];
    // This table contains no profile data and is shared by all proof calls.
    static PREIMAGES: OnceLock<[[f32; 2]; 256]> = OnceLock::new();
    let preimages = PREIMAGES.get_or_init(|| std::array::from_fn(|i| quantizer_preimage(i as u8)));
    for channel in 0..3 {
        for corner in 0..4 {
            checkpoint(cancelled)?;
            for (value, pixel) in input.chunks_exact_mut(3).enumerate() {
                pixel[channel] = value as u8;
                pixel[(channel + 1) % 3] = if corner & 1 == 0 { 0 } else { 255 };
                pixel[(channel + 2) % 3] = if corner & 2 == 0 { 0 } else { 255 };
            }
            transform
                .transform(&input, &mut output)
                .map_err(|_| IccEquivalenceError::Unproved)?;
            for (value, (input, output)) in input
                .chunks_exact(3)
                .zip(output.chunks_exact(3))
                .enumerate()
            {
                let mut scalar = [0_u8; 3];
                transform
                    .transform(input, &mut scalar)
                    .map_err(|_| IccEquivalenceError::Unproved)?;
                let [low, high] = preimages[value];
                for converted in [output[channel], scalar[channel]] {
                    let actual = f64::from(f32::from(converted) / 255.0);
                    result.max_component_error = result
                        .max_component_error
                        .max((actual - f64::from(low)).abs())
                        .max((actual - f64::from(high)).abs());
                }
                result.evaluated_colors += 2;
            }
        }
    }
    Ok(())
}

fn quantize(value: f32) -> u8 {
    (value * 255.0 + 0.5) as u8
}

fn quantizer_preimage(value: u8) -> [f32; 2] {
    // Positive binary32 bit patterns have numeric order. Binary search the
    // actual multiply/add/cast, rather than assuming exact half-byte thresholds.
    let first = |byte: u16| {
        let (mut low, mut high) = (0_u32, 1.0_f32.to_bits() + 1);
        while low < high {
            let middle = low + (high - low) / 2;
            if u16::from(quantize(f32::from_bits(middle))) >= byte {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        low
    };
    let low = first(u16::from(value));
    let high = if value == 255 {
        1.0_f32.to_bits()
    } else {
        first(u16::from(value) + 1) - 1
    };
    [f32::from_bits(low), f32::from_bits(high)]
}

#[cfg(test)]
mod tests;
