use hayro_syntax::object::{Array, Number, Object};
use smallvec::SmallVec;

/// A completely decoded PDF line dash pattern, in user-space units.
#[derive(Clone, Debug, PartialEq)]
pub struct DashPattern {
    /// Alternating dash and gap lengths. Zero lengths are significant.
    pub array: SmallVec<[f32; 4]>,
    /// Distance into the cyclic pattern at each subpath's start.
    pub phase: f32,
}

impl DashPattern {
    /// Validate both operands of the PDF `d` operator without accepting a
    /// successfully decoded prefix of an invalid array.
    pub fn new(array: &Array<'_>, phase: Number) -> Option<Self> {
        let array = SmallVec::<[f32; 4]>::try_from(array.clone()).ok()?;
        let phase = phase.as_f32();
        if !phase.is_finite()
            || array.iter().any(|value| !value.is_finite() || *value < 0.0)
            || (!array.is_empty() && !array.iter().any(|value| *value > 0.0))
        {
            return None;
        }
        Some(Self { array, phase })
    }

    /// Decode the exact two-element array used by an `ExtGState` `D` entry.
    pub fn from_ext_g_state(array: Array<'_>) -> Option<Self> {
        if array.raw_iter().take(3).count() != 2 {
            return None;
        }
        let mut entries = array.iter::<Object<'_>>();
        Self::new(
            &entries.next()?.into_array()?,
            entries.next()?.into_number()?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::object::FromBytes;

    #[test]
    fn dash_declarations_are_complete_and_keep_zero_lengths() {
        for bytes in [b"[[0 2] 0]".as_slice(), b"[[2 0 3] -7]", b"[[] 0]"] {
            assert!(DashPattern::from_ext_g_state(Array::from_bytes(bytes).unwrap()).is_some());
        }
        let pattern =
            DashPattern::from_ext_g_state(Array::from_bytes(b"[[0 2] 0]").unwrap()).unwrap();
        assert_eq!(pattern.array.as_slice(), &[0.0, 2.0]);
        for bytes in [
            b"[[2 /Bad 3] 0]".as_slice(),
            b"[[2 /Bad] 0]",
            b"[[2 null] 0]",
            b"[[2 -1] 0]",
            b"[[0 0] 0]",
            b"[[2] 0 /Bad]",
            b"[[2] null]",
            b"[[2]]",
            b"[[2 999 0 R] 0]",
            b"[[2] 999 0 R]",
        ] {
            assert!(
                DashPattern::from_ext_g_state(Array::from_bytes(bytes).unwrap()).is_none(),
                "{bytes:?}"
            );
        }
    }
}
