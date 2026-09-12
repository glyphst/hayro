use super::Function;
use hayro_syntax::object::{Dict, Object};
use smallvec::smallvec;
use std::sync::Arc;

/// Select the effective graphics-state transfer entry. PDF null and undefined
/// optional dictionary entries are absent; a non-null TR2 takes precedence.
pub fn selected_transfer_function<'a>(dict: &Dict<'a>) -> Option<(&'static [u8], Object<'a>)> {
    for key in [b"TR2".as_slice(), b"TR".as_slice()] {
        if let Some(object) = dict.get::<Object<'a>>(key)
            && !matches!(object, Object::Null(_))
        {
            return Some((key, object));
        }
    }
    None
}

/// A one-input, one-output transfer function with an exact byte-input table.
#[derive(Clone)]
pub struct TransferFunction {
    key: u128,
    function: Option<Function>,
    samples: Arc<[u8; 256]>,
    clamp_output: bool,
}

impl TransferFunction {
    /// The full owned function behind this transfer. A channel explicitly
    /// selected as Identity has no function. Call [`Function::definition`] to
    /// retain executable data instead of approximating this with byte samples.
    pub fn function(&self) -> Option<&Function> {
        self.function.as_ref()
    }

    pub(crate) fn new(function: Function) -> Option<Self> {
        Self::new_with_output_policy(function, false)
    }

    /// Table 144 clamps a soft-mask transfer result to the unit interval.
    pub(crate) fn new_soft_mask(function: Function) -> Option<Self> {
        Self::new_with_output_policy(function, true)
    }

    fn new_with_output_policy(function: Function, clamp_output: bool) -> Option<Self> {
        if function.arity() != Some((1, 1)) {
            return None;
        }
        let mut samples = [0; 256];
        for (index, sample) in samples.iter_mut().enumerate() {
            let output = evaluate(&function, index as f32 / 255.0, clamp_output)?;
            *sample = (output * 255.0 + 0.5) as u8;
        }
        Some(Self {
            key: crate::util::hash128(&format!("{function:?}:clamp={clamp_output}")),
            function: Some(function),
            samples: Arc::new(samples),
            clamp_output,
        })
    }

    pub(crate) fn identity() -> Self {
        Self {
            key: crate::util::hash128(&"Identity"),
            function: None,
            clamp_output: false,
            samples: Arc::new(std::array::from_fn(|index| index as u8)),
        }
    }

    /// Apply to components that have already been represented as bytes.
    pub fn apply_to(&self, values: &mut [u8]) {
        self.apply_to_stride(values, 1);
    }

    pub(crate) fn apply_to_stride(&self, values: &mut [u8], stride: usize) {
        for value in values.iter_mut().step_by(stride) {
            *value = self.samples[*value as usize];
        }
    }

    /// Preserve real paint/shading inputs; sampling a byte table first moves
    /// discontinuities and can amplify quantization through nonlinear functions.
    pub fn apply_f32(&self, value: f32) -> Option<f32> {
        if !value.is_finite() {
            return None;
        }
        let value = value.clamp(0.0, 1.0);
        self.function.as_ref().map_or(Some(value), |function| {
            evaluate(function, value, self.clamp_output)
        })
    }
}

fn evaluate(function: &Function, value: f32, clamp_output: bool) -> Option<f32> {
    let result = function.eval(smallvec![value])?;
    let output = *result.first()?;
    if result.len() != 1 || !output.is_finite() {
        return None;
    }
    if clamp_output {
        Some(output.clamp(0.0, 1.0))
    } else {
        (0.0..=1.0).contains(&output).then_some(output)
    }
}

impl std::fmt::Debug for TransferFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TransferFunction").field(&self.key).finish()
    }
}

impl crate::CacheKey for TransferFunction {
    fn cache_key(&self) -> u128 {
        self.key
    }
}
