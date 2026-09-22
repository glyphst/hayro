//! Real inputs and outputs at the named-color alternate boundary.
use super::{AlphaColor, ColorSpace, ColorSpaceType};
use smallvec::SmallVec;

impl ColorSpace {
    /// The CIE alternate supported by the retained floating-image converter.
    /// ICC alternates have their own bounded, batched conversion path.
    pub(crate) fn image_cie_tint_alternate(&self) -> Option<&Self> {
        if let ColorSpaceType::Indexed(space) = self.0.as_ref() {
            return space.base().image_cie_tint_alternate();
        }
        let (_, alternate) = self.tint_transform()?;
        matches!(
            alternate.0.as_ref(),
            ColorSpaceType::CalGray(_) | ColorSpaceType::CalRgb(_) | ColorSpaceType::Lab(_)
        )
        .then_some(alternate)
    }

    /// Keep the CIE conversion in binary64 after the existing binary32 tint
    /// function boundary. Ordinary byte images retain `image_tint_rgb` below.
    pub(crate) fn image_cie_tint_rgb(&self, input: &[f64]) -> Option<[f64; 3]> {
        self.image_cie_tint_alternate()?;
        if input.len() != usize::from(self.num_components())
            || input.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        if let ColorSpaceType::Indexed(space) = self.0.as_ref() {
            // Select the palette entry before narrowing its index, then keep
            // the same normalized binary32 tint values as the byte converter.
            let values: SmallVec<[f64; 4]> = space
                .entry(input[0])
                .iter()
                .map(|value| f64::from(f32::from(*value) / 255.0))
                .collect();
            return space.base().image_cie_tint_rgb(&values);
        }
        let (function, alternate) = self.tint_transform()?;
        let values = input
            .iter()
            .map(|value| value.clamp(0.0, 1.0) as f32)
            .collect();
        let output: SmallVec<[f64; 4]> =
            function.eval(values)?.into_iter().map(f64::from).collect();
        alternate.image_rgb_f64(&output)
    }

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
