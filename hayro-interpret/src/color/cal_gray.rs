use super::cal_rgb::CalRgb;
use super::{ToLuma, ToRgb, U8Lookup};
use hayro_syntax::object::Dict;

#[derive(Debug, Clone)]
pub(crate) struct CalGray {
    transform: CalRgb,
    lookup: U8Lookup<[u8; 3]>,
    luma_lookup: U8Lookup<u8>,
}

impl CalGray {
    pub(super) fn new(dict: &Dict<'_>) -> Option<Self> {
        Some(Self {
            transform: CalRgb::for_gray(dict)?,
            lookup: U8Lookup::default(),
            luma_lookup: U8Lookup::default(),
        })
    }

    fn u8_lookup(&self) -> Option<&[[u8; 3]; 256]> {
        self.lookup.get_or_init_with(|input| {
            Some(Box::new(core::array::from_fn(|index| {
                self.transform.convert_pixel([input[index]; 3])
            })))
        })
    }

    pub(super) fn convert_value(&self, input: f32) -> [u8; 3] {
        self.transform.convert_components([f64::from(input); 3])
    }
}

impl ToRgb for CalGray {
    fn convert(&self, input: &[u8], output: &mut [u8]) -> Option<()> {
        let lookup = self.u8_lookup()?;
        for (input, output) in input.iter().zip(output.chunks_exact_mut(3)) {
            output.copy_from_slice(&lookup[*input as usize]);
        }
        Some(())
    }
}

impl ToLuma for CalGray {
    fn to_luma(&self, input: &mut [u8]) -> Option<()> {
        let lookup = self.luma_lookup.get_or_init_with(|_| {
            let rgb = self.u8_lookup()?;
            if rgb
                .iter()
                .any(|pixel| pixel[0] != pixel[1] || pixel[0] != pixel[2])
            {
                return None;
            }
            Some(Box::new(core::array::from_fn(|i| rgb[i][0])))
        })?;
        // Failure above leaves samples intact for the decoder's RGB route.
        for value in input {
            *value = lookup[*value as usize];
        }
        Some(())
    }
}
