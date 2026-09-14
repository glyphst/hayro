use super::{ColorSpace, ToRgb, U8Lookup};
use crate::function::Function;
use hayro_syntax::object::{Array, Dict, Name, Object};

#[derive(Debug, Clone)]
pub(crate) struct DeviceN {
    alternate_space: ColorSpace,
    pub(super) num_components: u8,
    tint_transform: Function,
    is_none: bool,
    lookup: U8Lookup<[u8; 3]>,
}

impl DeviceN {
    pub(super) fn with_intent(
        &self,
        intent: super::RenderingIntent,
    ) -> Result<Self, super::ColorConversionError> {
        let mut result = self.clone();
        result.alternate_space = self.alternate_space.with_rendering_intent(intent)?;
        result.lookup = U8Lookup::default();
        Ok(result)
    }

    pub(super) fn new<'a>(
        array: &Array<'a>,
        resolve: &mut dyn FnMut(Object<'a>) -> Option<ColorSpace>,
    ) -> Option<Self> {
        let count = array.raw_iter().take(6).count();
        if !matches!(count, 4 | 5) {
            return None;
        }
        let mut iter = array.flex_iter();
        // Skip `/DeviceN`
        let _ = iter.next::<Name<'_>>()?;
        let names = iter.next::<Array<'_>>()?;
        let num_components = u8::try_from(names.raw_iter().take(256).count()).ok()?;
        let mut names_iter = names.flex_iter();
        // Read every slot: a malformed tail must not turn a prefix of /None
        // components into a valid non-marking space.
        let names = (0..num_components)
            .map(|_| names_iter.next::<Name<'_>>())
            .collect::<Option<Vec<_>>>()?;
        if names.iter().enumerate().any(|(index, name)| {
            name.as_str() == "All" || (name.as_str() != "None" && names[..index].contains(name))
        }) {
            return None;
        }
        let all_none = names.iter().all(|n| n.as_str() == "None");
        let alternate_space = resolve(iter.next::<Object<'_>>()?)?;
        let tint_transform = Function::new(&iter.next::<Object<'_>>()?)?;
        if tint_transform.arity()?
            != (
                usize::from(num_components),
                alternate_space.component_count(),
            )
        {
            return None;
        }
        if count == 5 {
            let attributes = iter.next::<Dict<'_>>()?;
            if attributes
                .get::<Name<'_>>(b"Subtype")
                .is_some_and(|n| n.as_str() == "NChannel")
                && names.iter().any(|n| n.as_str() == "None")
            {
                return None;
            }
        }

        if num_components == 0 {
            return None;
        }

        Some(Self {
            alternate_space,
            num_components,
            tint_transform,
            is_none: all_none,
            lookup: U8Lookup::default(),
        })
    }

    pub(super) fn convert_components(
        &self,
        input: &[f32],
        opacity: f32,
    ) -> Option<super::AlphaColor> {
        let values = self.tint_components(input)?;
        Some(self.alternate_space.to_rgba(&values, opacity))
    }

    pub(super) fn alternate_space(&self) -> &ColorSpace {
        &self.alternate_space
    }

    pub(super) fn tint_transform(&self) -> &Function {
        &self.tint_transform
    }

    pub(super) fn tint_components(&self, input: &[f32]) -> Option<super::ColorComponents> {
        if input.len() != usize::from(self.num_components)
            || input.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        self.tint_transform
            .eval(input.iter().map(|value| value.clamp(0.0, 1.0)).collect())
    }

    fn convert_inner(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        let components = usize::from(self.num_components);
        if !input.len().is_multiple_of(components)
            || output.len() != (input.len() / components).checked_mul(3)?
        {
            return None;
        }
        for (source, pixel) in input
            .chunks_exact(components)
            .zip(output.chunks_exact_mut(3))
        {
            let values: smallvec::SmallVec<[f32; 4]> = source
                .iter()
                .map(|value| f32::from(*value) / 255.0)
                .collect();
            let color = self.convert_components(&values, 1.0)?;
            pixel.copy_from_slice(&color.to_rgba8()[..3]);
        }
        Some(())
    }

    fn u8_lookup(&self) -> Option<&[[u8; 3]; 256]> {
        self.lookup
            .get_or_init(|input, output| self.convert_inner(input, output))
    }
}

impl ToRgb for DeviceN {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        if self.num_components == 1 {
            let lookup = self.u8_lookup()?;
            for (input, output) in input.iter().zip(output.chunks_exact_mut(3)) {
                output.copy_from_slice(&lookup[*input as usize]);
            }

            Some(())
        } else {
            self.convert_inner(input, output)
        }
    }

    fn convert_in_place(&self, input: &mut [u8]) -> Option<()> {
        if self.num_components != 3 || !input.len().is_multiple_of(3) {
            return None;
        }

        for pixel in input.chunks_exact_mut(3) {
            let values = [pixel[0], pixel[1], pixel[2]].map(|value| f32::from(value) / 255.0);
            let color = self.convert_components(&values, 1.0)?;
            pixel.copy_from_slice(&color.to_rgba8()[..3]);
        }
        Some(())
    }

    fn is_none(&self) -> bool {
        self.is_none
    }
}
