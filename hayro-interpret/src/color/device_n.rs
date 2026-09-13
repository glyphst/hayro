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

    fn evaluate(&self, input: &[u8]) -> Vec<u8> {
        input
            .chunks_exact(self.num_components as usize)
            .flat_map(|n| {
                let input = n.iter().map(|value| *value as f32 / 255.0).collect();
                let values = self
                    .tint_transform
                    .eval(input)
                    .unwrap_or(self.alternate_space.initial_color());
                self.alternate_space.encode_values(&values)
            })
            .collect()
    }

    fn convert_inner(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        let evaluated = self.evaluate(input);
        self.alternate_space.convert(&evaluated, output)
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
        if self.num_components != 3 {
            return None;
        }

        let evaluated = self.evaluate(input);
        self.alternate_space.convert(&evaluated, input)
    }

    fn is_none(&self) -> bool {
        self.is_none
    }
}
