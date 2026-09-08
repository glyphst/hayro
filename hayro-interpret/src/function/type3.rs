use super::numeric::{affine, objects, pairs, values};
use crate::function::{Clamper, Function, StitchingBounds, TupleVec, Values};
use hayro_syntax::object::Dict;
use smallvec::smallvec;

/// A type 3 function (stitching function).
#[derive(Debug)]
pub(crate) struct Type3 {
    functions: Vec<Function>,
    bounds: Vec<f32>,
    encode: TupleVec,
    clamper: Clamper,
}

impl Type3 {
    pub(super) fn definition(&self) -> super::FunctionDefinition<'_> {
        let (low, high) = self.clamper.domain[0];
        super::FunctionDefinition::Stitching {
            domain: [low, high],
            range: self.clamper.range.as_deref(),
            functions: &self.functions,
            bounds: &self.bounds[1..self.bounds.len() - 1],
            encode: &self.encode,
        }
    }

    pub(crate) fn new(dict: &Dict<'_>, depth: usize, remaining: &mut usize) -> Option<Self> {
        let clamper = Clamper::new(dict)?;
        if clamper.domain.len() != 1 {
            return None;
        }
        let children = objects(dict, b"Functions", *remaining)?;
        if children.is_empty() {
            return None;
        }
        let functions = children
            .iter()
            .map(|object| Function::parse(object, depth + 1, remaining))
            .collect::<Option<Vec<_>>>()?;
        let (_, outputs) = functions.first()?.arity()?;
        if functions
            .iter()
            .any(|child| child.arity() != Some((1, outputs)))
            || clamper
                .range
                .as_ref()
                .is_some_and(|range| range.len() != outputs)
        {
            return None;
        }
        let (low, high) = clamper.domain[0];
        let internal = values(dict, b"Bounds", functions.len() - 1)?;
        let encode = pairs(dict, b"Encode", functions.len())?;
        if internal.len() + 1 != functions.len()
            || encode.len() != functions.len()
            || (functions.len() > 1 && low == high)
        {
            return None;
        }
        let mut previous = low;
        for &bound in &internal {
            if bound <= previous || bound > high {
                return None;
            }
            previous = bound;
        }
        let mut bounds = Vec::with_capacity(functions.len() + 1);
        bounds.push(low);
        bounds.extend(internal);
        bounds.push(high);
        Some(Self {
            functions,
            bounds,
            encode,
            clamper,
        })
    }

    pub(super) fn arity(&self) -> Option<(usize, usize)> {
        Some((1, self.functions.first()?.arity()?.1))
    }

    pub(crate) fn eval(&self, input: f32) -> Option<Values> {
        let (low, high) = self.clamper.domain[0];
        let input = input.clamp(low, high);
        // Interior intervals are closed on the left. The final interval is
        // closed at both ends, including the permitted singleton at Domain[1].
        let index = self.bounds[1..self.bounds.len() - 1].partition_point(|bound| *bound <= input);
        let (start, end) = self.encode[index];
        let encoded = if self.bounds[index] == self.bounds[index + 1] {
            start
        } else {
            affine(
                input,
                self.bounds[index],
                self.bounds[index + 1],
                start,
                end,
            ) as f32
        };
        let mut output = self.functions[index].eval(smallvec![encoded])?;
        self.clamper.clamp_output(&mut output);
        Some(output)
    }

    pub(crate) fn stitching_bounds(&self) -> StitchingBounds {
        let mut result = StitchingBounds::new();
        result.extend_from_slice(&self.bounds[1..self.bounds.len() - 1]);
        for (index, function) in self.functions.iter().enumerate() {
            let (start, end) = self.encode[index];
            if start == end {
                continue;
            }
            let (low, high) = (self.bounds[index], self.bounds[index + 1]);
            for child in function.stitching_bounds() {
                let bound = affine(child, start, end, low, high) as f32;
                if bound > low && bound < high {
                    result.push(bound);
                }
            }
        }
        result.sort_by(f32::total_cmp);
        result.dedup_by(|a, b| *a == *b);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::object::FromBytes;
    use hayro_syntax::object::Object;

    #[test]
    fn simple() {
        let data = b"<<
  /FunctionType 3
  /Domain [-7 7]
  /Functions [
    << /FunctionType 2
       /Domain [0 1]
       /C0 [0.5 0.5 0.5]
       /C1 [0.5 0.5 0.5]
       /N 1
    >>
    << /FunctionType 2
       /Domain [0 1]
       /C0 [0.7 0.7 0.7]
       /C1 [0.7 0.7 0.7]
       /N 1
    >>
  ]
  /Bounds [0]
  /Encode [0 1 0 1]
>>";

        let dict = Object::from_bytes(data).unwrap();
        let function = Function::new(&dict).unwrap();

        assert_eq!(
            function.eval(smallvec![-7.0]).unwrap().as_slice(),
            &[0.5, 0.5, 0.5]
        );
        assert_eq!(
            function.eval(smallvec![-3.0]).unwrap().as_slice(),
            &[0.5, 0.5, 0.5]
        );
        assert_eq!(
            function.eval(smallvec![-0.5]).unwrap().as_slice(),
            &[0.5, 0.5, 0.5]
        );
        assert_eq!(
            function.eval(smallvec![0.0]).unwrap().as_slice(),
            &[0.7, 0.7, 0.7]
        );
        assert_eq!(
            function.eval(smallvec![7.0]).unwrap().as_slice(),
            &[0.7, 0.7, 0.7]
        );
    }
}
