use super::{ColorSpace, ToRgb, U8Lookup};
use crate::function::Function;
use hayro_syntax::object::{Array, Name, Object};
use smallvec::smallvec;

#[derive(Debug, Clone)]
pub(crate) struct Separation {
    alternate_space: ColorSpace,
    tint_transform: Function,
    is_none_separation: bool,
    is_all_separation: bool,
    lookup: U8Lookup<[u8; 3]>,
}

impl Separation {
    pub(super) fn with_intent(
        &self,
        intent: super::RenderingIntent,
    ) -> Result<Self, super::ColorConversionError> {
        let mut result = self.clone();
        if self.is_all_separation {
            return Ok(result);
        }
        result.alternate_space = self.alternate_space.with_rendering_intent(intent)?;
        result.lookup = U8Lookup::default();
        Ok(result)
    }

    pub(super) fn new<'a>(
        array: &Array<'a>,
        resolve: &mut dyn FnMut(Object<'a>) -> Option<ColorSpace>,
    ) -> Option<Self> {
        if array.raw_iter().take(5).count() != 4 {
            return None;
        }
        let mut iter = array.flex_iter();
        // Skip `/Separation`
        let _ = iter.next::<Name<'_>>()?;
        let name = iter.next::<Name<'_>>()?;
        let alternate_space = resolve(iter.next::<Object<'_>>()?)?;
        let tint_transform = Function::new(&iter.next::<Object<'_>>()?)?;
        // All and None ignore tint evaluation, but still require a valid
        // device/CIE alternate and a function with matching components.
        if tint_transform.arity()? != (1, alternate_space.component_count()) {
            return None;
        }
        let is_none_separation = name.as_str() == "None";
        let is_all_separation = name.as_str() == "All";

        Some(Self {
            alternate_space,
            tint_transform,
            is_none_separation,
            is_all_separation,
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
        let [tint] = input else { return None };
        if !tint.is_finite() {
            return None;
        }
        self.tint_transform.eval(smallvec![tint.clamp(0.0, 1.0)])
    }

    fn convert_inner(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        if output.len() != input.len().checked_mul(3)? {
            return None;
        }
        for (tint, pixel) in input.iter().zip(output.chunks_exact_mut(3)) {
            let color = self.convert_components(&[f32::from(*tint) / 255.0], 1.0)?;
            pixel.copy_from_slice(&color.to_rgba8()[..3]);
        }
        Some(())
    }

    fn u8_lookup(&self) -> Option<&[[u8; 3]; 256]> {
        self.lookup
            .get_or_init(|input, output| self.convert_inner(input, output))
    }

    pub(super) fn is_all(&self) -> bool {
        self.is_all_separation
    }
}

impl ToRgb for Separation {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        if self.is_all_separation {
            for (tint, pixel) in input.iter().zip(output.chunks_exact_mut(3)) {
                pixel.fill(255 - tint);
            }
            return Some(());
        }
        let lookup = self.u8_lookup()?;
        for (input, output) in input.iter().zip(output.chunks_exact_mut(3)) {
            output.copy_from_slice(&lookup[*input as usize]);
        }

        Some(())
    }

    fn is_none(&self) -> bool {
        self.is_none_separation
    }
}
