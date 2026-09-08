//! PDF functions.
//!
//! PDF has the concept of functions, representing objects that take a certain number of values
//! as input, do some processing on them and then return some output.

#[cfg(test)]
mod deferred_tests;
mod definition;
mod numeric;
pub use definition::FunctionDefinition;
mod transfer;
#[cfg(test)]
mod transfer_tests;
pub use transfer::{TransferFunction, selected_transfer_function};
mod type0;
mod type2;
#[cfg(test)]
mod type23_tests;
mod type3;
mod type4;

use crate::function::type0::Type0;
use crate::function::type2::Type2;
use crate::function::type3::Type3;
use crate::function::type4::Type4;
use hayro_syntax::object::Dict;
use hayro_syntax::object::dict::keys::{DOMAIN, FUNCTION_TYPE, RANGE};
use hayro_syntax::object::{Number, Object, dict_or_stream};
use smallvec::SmallVec;
use std::sync::Arc;

/// The input/output type of functions.
pub(crate) type Values = SmallVec<[f32; 4]>;
pub(crate) type StitchingBounds = SmallVec<[f32; 3]>;
type TupleVec = SmallVec<[(f32, f32); 4]>;

/// One instruction in a bounded, forward-only PDF Type 4 calculator program.
///
/// Conditional procedures are flattened into forward jumps so retained
/// renderers can validate and execute the same owned program on either the CPU
/// or GPU without retaining parser objects.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum CalculatorInstruction {
    /// Push a signed 32-bit integer without converting it to a real number.
    Integer(i32),
    /// Push a real number.
    Number(f32),
    /// Apply the PostScript `abs` operator.
    Abs,
    /// Apply the PostScript `add` operator.
    Add,
    /// Apply the PostScript `atan` operator.
    Atan,
    /// Apply the PostScript `ceiling` operator.
    Ceiling,
    /// Apply the PostScript `cos` operator.
    Cos,
    /// Apply the PostScript `cvi` operator.
    Cvi,
    /// Apply the PostScript `cvr` operator.
    Cvr,
    /// Apply the PostScript `div` operator.
    Div,
    /// Apply the PostScript `exp` operator.
    Exp,
    /// Apply the PostScript `floor` operator.
    Floor,
    /// Apply the PostScript `idiv` operator.
    Idiv,
    /// Apply the PostScript `ln` operator.
    Ln,
    /// Apply the PostScript `log` operator.
    Log,
    /// Apply the PostScript `mod` operator.
    Mod,
    /// Apply the PostScript `mul` operator.
    Mul,
    /// Apply the PostScript `neg` operator.
    Neg,
    /// Apply the PostScript `round` operator.
    Round,
    /// Apply the PostScript `sin` operator.
    Sin,
    /// Apply the PostScript `sqrt` operator.
    Sqrt,
    /// Apply the PostScript `sub` operator.
    Sub,
    /// Apply the PostScript `truncate` operator.
    Truncate,
    /// Apply the PostScript `and` operator.
    And,
    /// Apply the PostScript `bitshift` operator.
    Bitshift,
    /// Apply the PostScript `eq` operator.
    Eq,
    /// Push the boolean value `false`.
    False,
    /// Apply the PostScript `ge` operator.
    Ge,
    /// Apply the PostScript `gt` operator.
    Gt,
    /// Apply the PostScript `le` operator.
    Le,
    /// Apply the PostScript `lt` operator.
    Lt,
    /// Apply the PostScript `ne` operator.
    Ne,
    /// Apply the PostScript `not` operator.
    Not,
    /// Apply the PostScript `or` operator.
    Or,
    /// Push the boolean value `true`.
    True,
    /// Apply the PostScript `xor` operator.
    Xor,
    /// Pop a condition and continue at this absolute instruction index when
    /// it is false. The target always points forwards.
    JumpIfFalse(u32),
    /// Continue at this absolute instruction index. The target always points
    /// forwards.
    Jump(u32),
    /// Apply the PostScript `copy` operator.
    Copy,
    /// Apply the PostScript `dup` operator.
    Dup,
    /// Apply the PostScript `exch` operator.
    Exch,
    /// Apply the PostScript `index` operator.
    Index,
    /// Apply the PostScript `pop` operator.
    Pop,
    /// Apply the PostScript `roll` operator.
    Roll,
}

/// An owned, bounded PDF Type 4 calculator function.
#[derive(Clone, Debug, PartialEq)]
pub struct CalculatorFunction {
    /// Inclusive input clamps from the function dictionary's `/Domain`.
    pub input_domain: Vec<[f32; 2]>,
    /// Inclusive output clamps from `/Range`, when one is present.
    pub output_range: Option<Vec<[f32; 2]>>,
    /// Forward-only calculator bytecode.
    pub instructions: Vec<CalculatorInstruction>,
}

#[derive(Debug)]
enum FunctionType {
    Type0(Type0),
    Type2(Type2),
    Type3(Type3),
    Type4(Type4),
}

/// A PDF function.
#[derive(Debug, Clone)]
pub struct Function(Arc<FunctionType>);

impl Function {
    /// Create a new function.
    pub fn new(obj: &Object<'_>) -> Option<Self> {
        Self::parse(obj, 0, &mut 4096)
    }

    fn parse(obj: &Object<'_>, depth: usize, remaining: &mut usize) -> Option<Self> {
        if depth >= 32 || *remaining == 0 {
            return None;
        }
        *remaining -= 1;
        let (dict, stream) = dict_or_stream(obj)?;

        let function_type = match dict.get::<Number>(FUNCTION_TYPE)?.as_i64_exact()? {
            0 => FunctionType::Type0(Type0::new(stream?)?),
            2 => FunctionType::Type2(Type2::new(dict)?),
            3 => FunctionType::Type3(Type3::new(dict, depth, remaining)?),
            4 => FunctionType::Type4(Type4::new(stream?)?),
            _ => return None,
        };

        Some(Self(Arc::new(function_type)))
    }

    /// Evaluate the function with the given input.
    pub fn eval(&self, input: Values) -> Option<Values> {
        if input.iter().any(|value| !value.is_finite())
            || self.arity().is_some_and(|(count, _)| count != input.len())
        {
            return None;
        }
        match self.0.as_ref() {
            FunctionType::Type0(t0) => t0.eval(input),
            FunctionType::Type2(t2) => t2.eval(*input.first()?),
            FunctionType::Type3(t3) => t3.eval(*input.first()?),
            FunctionType::Type4(t4) => Some(t4.eval(input)?),
        }
    }

    /// Return the declared input and output counts. A legacy calculator with
    /// no Range has no known output count and returns `None`.
    pub fn arity(&self) -> Option<(usize, usize)> {
        match self.0.as_ref() {
            FunctionType::Type0(function) => Some(function.arity()),
            FunctionType::Type2(function) => Some(function.arity()),
            FunctionType::Type3(function) => function.arity(),
            FunctionType::Type4(function) => function.arity(),
        }
    }

    pub(crate) fn stitching_bounds(&self) -> StitchingBounds {
        match self.0.as_ref() {
            FunctionType::Type3(t3) => t3.stitching_bounds(),
            _ => SmallVec::new(),
        }
    }

    /// Return every nested Type 3 stitching boundary in input-domain order.
    ///
    /// Retained frontends use this read-only query during conservative raw
    /// preflight. A renderer that cannot prove the curved boundary of a
    /// discontinuous patch Function can reject it before interpretation while
    /// still accepting continuous one-input functions.
    pub fn stitching_boundaries(&self) -> Vec<f32> {
        self.stitching_bounds().into_vec()
    }

    /// Return an owned calculator program when this is a PDF Type 4 function.
    ///
    /// The parser bounds program size and nesting before this representation is
    /// made available. Other function types return `None`.
    pub fn calculator_function(&self) -> Option<CalculatorFunction> {
        match self.0.as_ref() {
            FunctionType::Type4(t4) => t4.calculator_function(),
            _ => None,
        }
    }

    /// Inspect the complete validated data needed for owned retained execution.
    ///
    /// Unlike a byte lookup table, this preserves real-input discontinuities
    /// and the original sample precision. Returns `None` for legacy calculator
    /// functions that cannot expose a bounded typed program.
    pub fn definition(&self) -> Option<FunctionDefinition<'_>> {
        Some(match self.0.as_ref() {
            FunctionType::Type0(function) => function.definition(),
            FunctionType::Type2(function) => function.definition(),
            FunctionType::Type3(function) => function.definition(),
            FunctionType::Type4(function) => {
                FunctionDefinition::Calculator(function.calculator_function()?)
            }
        })
    }
}

#[derive(Debug, Clone)]
struct Clamper {
    domain: TupleVec,
    range: Option<TupleVec>,
}

impl Clamper {
    fn new(dict: &Dict<'_>) -> Option<Self> {
        let domain = numeric::pairs(dict, DOMAIN, numeric::MAX_COMPONENTS)?;
        let range = if numeric::present(dict, RANGE) {
            Some(numeric::pairs(dict, RANGE, numeric::MAX_COMPONENTS)?)
        } else {
            None
        };
        if domain
            .iter()
            .chain(range.iter().flatten())
            .any(|&(low, high)| low > high)
        {
            return None;
        }
        Some(Self { domain, range })
    }

    fn clamp_input(&self, input: &mut [f32]) {
        if input.len() != self.domain.len() {
            warn!("the domain of the function didn't match the input arguments");
        }

        for ((min, max), val) in self.domain.iter().zip(input.iter_mut()) {
            *val = val.min(*max).max(*min);
        }
    }

    fn clamp_output(&self, output: &mut [f32]) {
        if let Some(range) = &self.range {
            if range.len() != output.len() {
                warn!("the range of the function didn't match the output arguments");
            }

            for ((min, max), val) in range.iter().zip(output.iter_mut()) {
                *val = val.min(*max).max(*min);
            }
        }
    }
}

/// Linearly interpolate the value `x`, assuming that it lies within the range `x_min` and `x_max`,
/// to the range `y_min` and `y_max`.
pub(crate) fn interpolate(x: f32, x_min: f32, x_max: f32, y_min: f32, y_max: f32) -> f32 {
    y_min + (x - x_min) * (y_max - y_min) / (x_max - x_min)
}
