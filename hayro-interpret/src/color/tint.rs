//! Real inputs and outputs at the named-color alternate boundary.
use super::{AlphaColor, ColorSpace, ColorSpaceType};
use smallvec::SmallVec;

impl ColorSpace {
    pub(crate) fn uses_tint_transform(&self) -> bool {
        if self.is_non_marking() || self.is_all_colorants() {
            return false;
        }
        match self.0.as_ref() {
            ColorSpaceType::Separation(_) | ColorSpaceType::DeviceN(_) => true,
            ColorSpaceType::Indexed(space) => space.base().uses_tint_transform(),
            _ => false,
        }
    }

    pub(super) fn image_tint_rgb(&self, input: &[f64]) -> Option<[f64; 3]> {
        if input.len() != usize::from(self.num_components())
            || input.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        let rgba = if let ColorSpaceType::Indexed(space) = self.0.as_ref() {
            // Keep the original decoded index for nearest-entry selection;
            // narrowing around a half-integer could select another palette row.
            let values: SmallVec<[f32; 4]> = space
                .entry(input[0])
                .iter()
                .map(|value| f32::from(*value) / 255.0)
                .collect();
            space.base().tint_rgba(&values, 1.0)?
        } else {
            let values: SmallVec<[f32; 4]> = input
                .iter()
                .map(|value| value.clamp(0.0, 1.0) as f32)
                .collect();
            self.tint_rgba(&values, 1.0)?
        }
        .components();
        Some([rgba[0], rgba[1], rgba[2]].map(f64::from))
    }

    pub(super) fn tint_rgba(&self, input: &[f32], opacity: f32) -> Option<AlphaColor> {
        match self.0.as_ref() {
            ColorSpaceType::Separation(space) => space.convert_components(input, opacity),
            ColorSpaceType::DeviceN(space) => space.convert_components(input, opacity),
            ColorSpaceType::Indexed(space) => {
                let [index] = input else { return None };
                if !index.is_finite() {
                    return None;
                }
                // A palette over a tint space has normalized byte components.
                // Evaluate the tint before quantizing the alternate color.
                let values: SmallVec<[f32; 4]> = space
                    .entry(f64::from(*index))
                    .iter()
                    .map(|value| f32::from(*value) / 255.0)
                    .collect();
                space.base().tint_rgba(&values, opacity)
            }
            _ => None,
        }
    }
}
