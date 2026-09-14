//! Device alternate components after named-color conversion, before RGB output.
use super::{Color, ColorComponents, ColorSpace, ColorSpaceKind, ColorSpaceType};
use smallvec::SmallVec;

impl ColorSpace {
    /// Borrow an ordinary Separation/DeviceN function and its alternate space.
    ///
    /// Callers composing a shading must clamp named-color inputs to [0, 1],
    /// evaluate this function, then apply the alternate's component bounds and
    /// conversion. The original function definition retains its Domain, Range,
    /// stitching boundaries and calculator branches. All/None and wrapper
    /// spaces return None; an Indexed palette is not a continuous tint input.
    pub fn tint_transform(&self) -> Option<(&crate::Function, &Self)> {
        if !self.uses_tint_transform() {
            return None;
        }
        match self.0.as_ref() {
            ColorSpaceType::Separation(space) => {
                Some((space.tint_transform(), space.alternate_space()))
            }
            ColorSpaceType::DeviceN(space) => {
                Some((space.tint_transform(), space.alternate_space()))
            }
            _ => None,
        }
    }

    /// The direct device alternate reached by an ordinary tint transform.
    ///
    /// Indexed and uncolored Pattern wrappers preserve the underlying alternate.
    /// Calibrated/ICC alternates and the special All/None colorants return None.
    /// This does not change the original family returned by [`Self::kind`].
    pub fn device_alternate_kind(&self) -> Option<ColorSpaceKind> {
        match self.0.as_ref() {
            ColorSpaceType::Pattern(space) => space.color_space().device_alternate_kind(),
            ColorSpaceType::Indexed(space) => space.base().device_alternate_kind(),
            ColorSpaceType::Separation(_) | ColorSpaceType::DeviceN(_)
                if self.uses_tint_transform() =>
            {
                self.resolved_device_kind()
            }
            _ => None,
        }
    }

    /// Evaluate a named color to its real device alternate components.
    ///
    /// The existing bounded binary32 function arithmetic is preserved. Values
    /// are clipped to the alternate's component bounds before being returned;
    /// no byte quantization or RGB conversion occurs. Unsupported alternates,
    /// invalid inputs and failed function evaluation return None.
    pub fn device_alternate_components(&self, input: &[f32]) -> Option<ColorComponents> {
        let input: SmallVec<[f64; 4]> = input.iter().copied().map(f64::from).collect();
        self.image_device_alternate_components(&input)
    }

    pub(crate) fn image_device_alternate_components(
        &self,
        input: &[f64],
    ) -> Option<ColorComponents> {
        self.device_alternate_kind()?;
        self.device_values(input)
    }

    fn resolved_device_kind(&self) -> Option<ColorSpaceKind> {
        if self.is_non_marking() || self.is_all_colorants() {
            return None;
        }
        match self.0.as_ref() {
            ColorSpaceType::DeviceGray(_)
            | ColorSpaceType::DeviceRgb(_)
            | ColorSpaceType::DeviceCmyk(_) => Some(self.kind()),
            ColorSpaceType::Separation(space) => space.alternate_space().resolved_device_kind(),
            ColorSpaceType::DeviceN(space) => space.alternate_space().resolved_device_kind(),
            ColorSpaceType::Indexed(space) => space.base().resolved_device_kind(),
            ColorSpaceType::Pattern(space) => space.color_space().resolved_device_kind(),
            _ => None,
        }
    }

    fn device_values(&self, input: &[f64]) -> Option<ColorComponents> {
        if input.len() != usize::from(self.num_components())
            || input.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        match self.0.as_ref() {
            ColorSpaceType::Pattern(space) => space.color_space().device_values(input),
            ColorSpaceType::Indexed(space) => {
                // Preserve the original decoded index until nearest-entry lookup.
                let values: SmallVec<[f64; 4]> = space
                    .entry(input[0])
                    .iter()
                    .map(|value| f64::from(*value) / 255.0)
                    .collect();
                space.base().device_values(&values)
            }
            ColorSpaceType::Separation(space) => {
                let values = space.tint_components(&unit_values(input))?;
                let values: SmallVec<[f64; 4]> = values.into_iter().map(f64::from).collect();
                space.alternate_space().device_values(&values)
            }
            ColorSpaceType::DeviceN(space) => {
                let values = space.tint_components(&unit_values(input))?;
                let values: SmallVec<[f64; 4]> = values.into_iter().map(f64::from).collect();
                space.alternate_space().device_values(&values)
            }
            ColorSpaceType::DeviceGray(_)
            | ColorSpaceType::DeviceRgb(_)
            | ColorSpaceType::DeviceCmyk(_) => Some(unit_values(input)),
            _ => None,
        }
    }
}

fn unit_values(values: &[f64]) -> ColorComponents {
    values
        .iter()
        .map(|value| value.clamp(0.0, 1.0) as f32)
        .collect()
}

impl Color {
    /// The direct device alternate of this named color, when supported.
    pub fn device_alternate_kind(&self) -> Option<ColorSpaceKind> {
        self.color_space.device_alternate_kind()
    }

    /// The real alternate components, preserving tint evaluation before RGB output.
    pub fn device_alternate_components(&self) -> Option<ColorComponents> {
        self.color_space
            .device_alternate_components(&self.components)
    }
}
