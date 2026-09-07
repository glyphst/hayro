use crate::function::{CalculatorFunction, CalculatorInstruction, Clamper, Values};
use hayro_syntax::content;
use hayro_syntax::object::Number;
use hayro_syntax::object::Stream;
use hayro_syntax::reader::Reader;
use hayro_syntax::reader::{ReaderContext, ReaderExt};
use smallvec::SmallVec;
use std::array;
mod value;
use value::Argument;

#[cfg(test)]
mod numeric_tests;

const MAX_CALCULATOR_OPERATIONS: usize = 16_384;
const MAX_CALCULATOR_NESTING: usize = 128;

/// A type 4 function (postscript function).
#[derive(Debug)]
pub(crate) struct Type4 {
    program: Vec<PostScriptOp>,
    clamper: Clamper,
}

impl Type4 {
    /// Create a new type 4 function.
    pub(crate) fn new(stream: &Stream<'_>) -> Option<Self> {
        let dict = stream.dict().clone();
        let clamper = Clamper::new(&dict)?;

        Some(Self {
            clamper,
            program: parse_procedure(&stream.decoded().ok()?)?,
        })
    }

    /// Evaluate the function with the given input.
    pub(crate) fn eval(&self, mut input: Values) -> Option<Values> {
        if input.len() != self.clamper.domain.len() || input.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        self.clamper.clamp_input(&mut input);

        let mut arg_stack = InterpreterStack::new();

        for input in input {
            arg_stack.push(Argument::real(input)?)?;
        }

        eval_inner(&self.program, &mut arg_stack)?;

        let mut out: SmallVec<_> = arg_stack
            .items()
            .iter()
            .map(|i| i.as_f32())
            .collect::<Option<_>>()?;

        if self
            .clamper
            .range
            .as_ref()
            .is_some_and(|range| range.len() != out.len())
        {
            return None;
        }
        self.clamper.clamp_output(&mut out);

        Some(out)
    }

    pub(crate) fn calculator_function(&self) -> Option<CalculatorFunction> {
        let mut instructions = Vec::new();
        flatten_program(&self.program, &mut instructions)?;
        if instructions.len() > MAX_CALCULATOR_OPERATIONS.saturating_mul(2) {
            return None;
        }
        Some(CalculatorFunction {
            input_domain: self
                .clamper
                .domain
                .iter()
                .map(|(min, max)| [*min, *max])
                .collect(),
            output_range: self
                .clamper
                .range
                .as_ref()
                .map(|range| range.iter().map(|(min, max)| [*min, *max]).collect()),
            instructions,
        })
    }
}

struct ArgumentsStack<T: Default, const C: usize> {
    stack: [T; C],
    len: usize,
}

impl<T: Default, const C: usize> ArgumentsStack<T, C> {
    fn new() -> Self {
        Self {
            stack: array::from_fn(|_| T::default()),
            len: 0,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn push(&mut self, n: T) -> Option<()> {
        if self.len == C {
            error!("overflowed post script argument stack");

            None
        } else {
            self.stack[self.len] = n;
            self.len += 1;

            Some(())
        }
    }

    #[inline]
    fn at(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            None
        } else {
            Some(&self.stack[index])
        }
    }

    #[inline]
    fn last(&self) -> Option<&T> {
        if self.is_empty() {
            None
        } else {
            Some(&self.stack[self.len - 1])
        }
    }

    #[inline]
    fn pop(&mut self) -> Option<T> {
        if self.is_empty() {
            error!("underflowed post script argument stack");

            None
        } else {
            self.len -= 1;
            Some(std::mem::take(&mut self.stack[self.len]))
        }
    }

    #[inline]
    fn items(&self) -> &[T] {
        &self.stack[..self.len]
    }

    #[inline]
    fn items_mut(&mut self) -> &mut [T] {
        &mut self.stack[..self.len]
    }
}

type InterpreterStack = ArgumentsStack<Argument, 64>;
type ParseStack = ArgumentsStack<Vec<PostScriptOp>, 2>;

fn eval_inner(procedure: &[PostScriptOp], stack: &mut InterpreterStack) -> Option<()> {
    use PostScriptOp as Op;
    for op in procedure {
        match op {
            Op::Number(number) => stack.push(Argument::from_number(*number)?)?,
            Op::True => stack.push(Argument::Bool(true))?,
            Op::False => stack.push(Argument::Bool(false))?,
            Op::If(body) => {
                if stack.pop()?.boolean()? {
                    eval_inner(body, stack)?;
                }
            }
            Op::IfElse(yes, no) => {
                let condition = stack.pop()?.boolean()?;
                eval_inner(if condition { yes } else { no }, stack)?;
            }
            Op::Copy => {
                let count = stack.pop()?.count()?;
                let start = stack.len().checked_sub(count)?;
                for index in start..stack.len() {
                    stack.push(*stack.at(index)?)?;
                }
            }
            Op::Dup => stack.push(*stack.last()?)?,
            Op::Exch => {
                let second = stack.pop()?;
                let first = stack.pop()?;
                stack.push(second)?;
                stack.push(first)?;
            }
            Op::Index => {
                let depth = stack.pop()?.count()?;
                let index = stack.len().checked_sub(depth.checked_add(1)?)?;
                stack.push(*stack.at(index)?)?;
            }
            Op::Pop => {
                stack.pop()?;
            }
            Op::Roll => {
                let shift = stack.pop()?.integer()?;
                let count = stack.pop()?.count()?;
                let start = stack.len().checked_sub(count)?;
                let values = &mut stack.items_mut()[start..];
                if !values.is_empty() {
                    if shift >= 0 {
                        values.rotate_right(shift as usize % values.len());
                    } else {
                        values.rotate_left(shift.unsigned_abs() as usize % values.len());
                    }
                }
            }
            Op::Abs
            | Op::Ceiling
            | Op::Cos
            | Op::Cvi
            | Op::Cvr
            | Op::Floor
            | Op::Ln
            | Op::Log
            | Op::Neg
            | Op::Not
            | Op::Round
            | Op::Sin
            | Op::Sqrt
            | Op::Truncate => {
                let value = stack.pop()?.unary(op)?;
                stack.push(value)?;
            }
            _ => {
                let second = stack.pop()?;
                let value = stack.pop()?.binary(second, op)?;
                stack.push(value)?;
            }
        }
    }
    Some(())
}

fn parse_procedure(data: &[u8]) -> Option<Vec<PostScriptOp>> {
    let mut r = Reader::new(data);
    let mut operation_count = 0;
    parse_procedure_inner(&mut r, 0, &mut operation_count)
}

fn parse_procedure_inner(
    r: &mut Reader<'_>,
    depth: usize,
    operation_count: &mut usize,
) -> Option<Vec<PostScriptOp>> {
    if depth > MAX_CALCULATOR_NESTING {
        return None;
    }
    let mut stack = ParseStack::new();

    let mut ops = vec![];
    r.skip_white_spaces_and_comments();
    r.forward_tag(b"{")?;

    loop {
        r.skip_white_spaces_and_comments();

        if r.peek_byte()? == b'}' {
            r.forward_tag(b"}")?;

            break;
        } else if r.peek_byte()? == b'{' {
            stack.push(parse_procedure_inner(r, depth + 1, operation_count)?)?;
        } else {
            let op = PostScriptOp::from_reader(r, &mut stack)?;
            *operation_count = operation_count.checked_add(1)?;
            if *operation_count > MAX_CALCULATOR_OPERATIONS {
                return None;
            }
            ops.push(op);
        }
    }

    Some(ops)
}

fn instruction_index(instructions: &[CalculatorInstruction]) -> Option<u32> {
    u32::try_from(instructions.len()).ok()
}

fn flatten_program(
    program: &[PostScriptOp],
    instructions: &mut Vec<CalculatorInstruction>,
) -> Option<()> {
    for op in program {
        let instruction = match op {
            PostScriptOp::Number(number) => match Argument::from_number(*number)? {
                Argument::Integer(value) => CalculatorInstruction::Integer(value),
                Argument::Float(value) => CalculatorInstruction::Number(value),
                Argument::Bool(_) => return None,
            },
            PostScriptOp::Abs => CalculatorInstruction::Abs,
            PostScriptOp::Add => CalculatorInstruction::Add,
            PostScriptOp::Atan => CalculatorInstruction::Atan,
            PostScriptOp::Ceiling => CalculatorInstruction::Ceiling,
            PostScriptOp::Cos => CalculatorInstruction::Cos,
            PostScriptOp::Cvi => CalculatorInstruction::Cvi,
            PostScriptOp::Cvr => CalculatorInstruction::Cvr,
            PostScriptOp::Div => CalculatorInstruction::Div,
            PostScriptOp::Exp => CalculatorInstruction::Exp,
            PostScriptOp::Floor => CalculatorInstruction::Floor,
            PostScriptOp::Idiv => CalculatorInstruction::Idiv,
            PostScriptOp::Ln => CalculatorInstruction::Ln,
            PostScriptOp::Log => CalculatorInstruction::Log,
            PostScriptOp::Mod => CalculatorInstruction::Mod,
            PostScriptOp::Mul => CalculatorInstruction::Mul,
            PostScriptOp::Neg => CalculatorInstruction::Neg,
            PostScriptOp::Round => CalculatorInstruction::Round,
            PostScriptOp::Sin => CalculatorInstruction::Sin,
            PostScriptOp::Sqrt => CalculatorInstruction::Sqrt,
            PostScriptOp::Sub => CalculatorInstruction::Sub,
            PostScriptOp::Truncate => CalculatorInstruction::Truncate,
            PostScriptOp::And => CalculatorInstruction::And,
            PostScriptOp::Bitshift => CalculatorInstruction::Bitshift,
            PostScriptOp::Eq => CalculatorInstruction::Eq,
            PostScriptOp::False => CalculatorInstruction::False,
            PostScriptOp::Ge => CalculatorInstruction::Ge,
            PostScriptOp::Gt => CalculatorInstruction::Gt,
            PostScriptOp::Le => CalculatorInstruction::Le,
            PostScriptOp::Lt => CalculatorInstruction::Lt,
            PostScriptOp::Ne => CalculatorInstruction::Ne,
            PostScriptOp::Not => CalculatorInstruction::Not,
            PostScriptOp::Or => CalculatorInstruction::Or,
            PostScriptOp::True => CalculatorInstruction::True,
            PostScriptOp::Xor => CalculatorInstruction::Xor,
            PostScriptOp::Copy => CalculatorInstruction::Copy,
            PostScriptOp::Dup => CalculatorInstruction::Dup,
            PostScriptOp::Exch => CalculatorInstruction::Exch,
            PostScriptOp::Index => CalculatorInstruction::Index,
            PostScriptOp::Pop => CalculatorInstruction::Pop,
            PostScriptOp::Roll => CalculatorInstruction::Roll,
            PostScriptOp::If(body) => {
                let branch = instructions.len();
                instructions.push(CalculatorInstruction::JumpIfFalse(0));
                flatten_program(body, instructions)?;
                let target = instruction_index(instructions)?;
                instructions[branch] = CalculatorInstruction::JumpIfFalse(target);
                continue;
            }
            PostScriptOp::IfElse(if_body, else_body) => {
                let branch = instructions.len();
                instructions.push(CalculatorInstruction::JumpIfFalse(0));
                flatten_program(if_body, instructions)?;
                let jump = instructions.len();
                instructions.push(CalculatorInstruction::Jump(0));
                let else_target = instruction_index(instructions)?;
                instructions[branch] = CalculatorInstruction::JumpIfFalse(else_target);
                flatten_program(else_body, instructions)?;
                let end_target = instruction_index(instructions)?;
                instructions[jump] = CalculatorInstruction::Jump(end_target);
                continue;
            }
        };
        instructions.push(instruction);
        if instructions.len() > MAX_CALCULATOR_OPERATIONS.saturating_mul(2) {
            return None;
        }
    }
    Some(())
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum PostScriptOp {
    Number(Number),
    Abs,
    Add,
    Atan,
    Ceiling,
    Cos,
    Cvi,
    Cvr,
    Div,
    Exp,
    Floor,
    Idiv,
    Ln,
    Log,
    Mod,
    Mul,
    Neg,
    Round,
    Sin,
    Sqrt,
    Sub,
    Truncate,
    And,
    Bitshift,
    Eq,
    False,
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    True,
    Xor,
    If(Vec<Self>),
    IfElse(Vec<Self>, Vec<Self>),
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
}

impl PostScriptOp {
    fn from_reader(r: &mut Reader<'_>, stack: &mut ParseStack) -> Option<Self> {
        let op = if let Some(n) = r.read::<Number>(&ReaderContext::dummy()) {
            // Calculator operands use PDF numeric syntax, without PostScript radix tokens.
            Self::Number(n)
        } else {
            let op = r.read::<content::Operator<'_>>(&ReaderContext::dummy())?;
            match op.as_ref() {
                b"abs" => Self::Abs,
                b"add" => Self::Add,
                b"atan" => Self::Atan,
                b"ceiling" => Self::Ceiling,
                b"cos" => Self::Cos,
                b"cvi" => Self::Cvi,
                b"cvr" => Self::Cvr,
                b"div" => Self::Div,
                b"exp" => Self::Exp,
                b"floor" => Self::Floor,
                b"idiv" => Self::Idiv,
                b"ln" => Self::Ln,
                b"log" => Self::Log,
                b"mod" => Self::Mod,
                b"mul" => Self::Mul,
                b"neg" => Self::Neg,
                b"round" => Self::Round,
                b"sin" => Self::Sin,
                b"sqrt" => Self::Sqrt,
                b"sub" => Self::Sub,
                b"truncate" => Self::Truncate,
                b"and" => Self::And,
                b"bitshift" => Self::Bitshift,
                b"eq" => Self::Eq,
                b"false" => Self::False,
                b"ge" => Self::Ge,
                b"gt" => Self::Gt,
                b"le" => Self::Le,
                b"lt" => Self::Lt,
                b"ne" => Self::Ne,
                b"not" => Self::Not,
                b"or" => Self::Or,
                b"true" => Self::True,
                b"xor" => Self::Xor,
                b"if" => Self::If(stack.pop()?),
                b"ifelse" => {
                    let s = stack.pop()?;
                    let f = stack.pop()?;
                    Self::IfElse(f, s)
                }
                b"copy" => Self::Copy,
                b"dup" => Self::Dup,
                b"exch" => Self::Exch,
                b"index" => Self::Index,
                b"pop" => Self::Pop,
                b"roll" => Self::Roll,
                _ => {
                    error!("encountered unknown postscript operator {op:?}");

                    return None;
                }
            }
        };

        Some(op)
    }
}

#[cfg(test)]
mod tests {
    use crate::function::type4::{
        MAX_CALCULATOR_NESTING, MAX_CALCULATOR_OPERATIONS, PostScriptOp, Type4, parse_procedure,
    };
    use crate::function::{CalculatorInstruction, Clamper, Function, FunctionType, TupleVec};
    use std::f32::consts::LN_10;
    use std::sync::Arc;

    use hayro_syntax::object::Number;
    use smallvec::smallvec;

    #[test]
    fn lex_1() {
        let program = b"{ copy dup 2.0 exch roll }";
        let parsed = parse_procedure(program).unwrap();

        assert_eq!(
            parsed,
            vec![
                PostScriptOp::Copy,
                PostScriptOp::Dup,
                PostScriptOp::Number(Number::from_f32(2.0)),
                PostScriptOp::Exch,
                PostScriptOp::Roll,
            ]
        );
    }

    #[test]
    fn lex_3() {
        let program = b" {  {dup exch} if {0} {1} ifelse }";
        let parsed = parse_procedure(program).unwrap();

        assert_eq!(
            parsed,
            vec![
                PostScriptOp::If(vec![PostScriptOp::Dup, PostScriptOp::Exch]),
                PostScriptOp::IfElse(
                    vec![PostScriptOp::Number(Number::from_i32(0))],
                    vec![PostScriptOp::Number(Number::from_i32(1))]
                )
            ]
        );
    }

    #[test]
    fn calculator_function_flattens_forward_branches() {
        let program = parse_procedure(b"{ dup 0 gt { 1 } { 2 } ifelse add }").unwrap();
        let type4 = Type4 {
            program,
            clamper: Clamper {
                domain: smallvec![(0.0, 1.0)],
                range: Some(smallvec![(0.0, 3.0)]),
            },
        };

        let calculator = type4.calculator_function().unwrap();
        assert_eq!(calculator.input_domain, vec![[0.0, 1.0]]);
        assert_eq!(calculator.output_range, Some(vec![[0.0, 3.0]]));
        assert_eq!(
            calculator.instructions,
            vec![
                CalculatorInstruction::Dup,
                CalculatorInstruction::Integer(0),
                CalculatorInstruction::Gt,
                CalculatorInstruction::JumpIfFalse(6),
                CalculatorInstruction::Integer(1),
                CalculatorInstruction::Jump(7),
                CalculatorInstruction::Integer(2),
                CalculatorInstruction::Add,
            ]
        );
    }

    #[test]
    fn calculator_parser_rejects_excessive_nesting() {
        let program = format!(
            "{{ {} true {} }}",
            "{".repeat(MAX_CALCULATOR_NESTING + 1),
            "} if".repeat(MAX_CALCULATOR_NESTING + 1)
        );
        assert!(parse_procedure(program.as_bytes()).is_none());
    }

    #[test]
    fn calculator_parser_rejects_excessive_operations() {
        let program = format!("{{ {} }}", "0 pop ".repeat(MAX_CALCULATOR_OPERATIONS));
        assert!(parse_procedure(program.as_bytes()).is_none());
    }

    fn op_impl(prog: &str, out: &[f32]) {
        let procedure = format!("{{{prog}}}");
        let procedure = parse_procedure(procedure.as_bytes()).unwrap();

        let type4 = Type4 {
            program: procedure,
            clamper: Clamper {
                domain: TupleVec::default(),
                range: None,
            },
        };

        let mut stack = super::InterpreterStack::new();
        super::eval_inner(&type4.program, &mut stack).unwrap();
        let res = stack
            .items()
            .iter()
            .map(|value| match value {
                super::Argument::Bool(value) => f32::from(u8::from(*value)),
                _ => value.as_f32().unwrap(),
            })
            .collect::<Vec<_>>();
        assert_eq!(res.as_slice(), out);
    }

    #[test]
    fn op_abs() {
        op_impl("4.5 abs", &[4.5]);
        op_impl("-3 abs", &[3.0]);
        op_impl("0 abs", &[0.0]);
    }

    #[test]
    fn op_atan() {
        op_impl("0 1 atan", &[0.0]);
        op_impl("1 0 atan", &[90.0]);
        op_impl("-100 0 atan", &[270.0]);
        op_impl("4 4 atan", &[45.0]);
    }

    #[test]
    fn op_ceiling() {
        op_impl("3.2 ceiling", &[4.0]);
        op_impl("-4.8 ceiling", &[-4.0]);
        op_impl("99 ceiling", &[99.0]);
    }

    #[test]
    fn op_cos() {
        op_impl("0 cos", &[1.0]);
        // Should be zero, but floating point impreciseness.
        op_impl("90 cos", &[-4.371139e-8]);
    }

    #[test]
    fn op_cvi() {
        op_impl("-47.8 cvi", &[-47.0]);
        op_impl("520.9 cvi", &[520.0]);
    }

    #[test]
    fn op_cvr() {
        op_impl("-47.8 cvr", &[-47.8]);
        op_impl("520 cvr", &[520.0]);
    }

    #[test]
    fn op_div() {
        op_impl("3 2 div", &[1.5]);
        op_impl("4 2 div", &[2.0]);
    }

    #[test]
    fn op_exp() {
        op_impl("9 0.5 exp", &[3.0]);
        op_impl("-9 -1 exp", &[-0.11111111]);
    }

    #[test]
    fn op_floor() {
        op_impl("3.2 floor", &[3.0]);
        op_impl("-4.8 floor", &[-5.0]);
        op_impl("99 floor", &[99.0]);
    }

    #[test]
    fn op_idiv() {
        op_impl("3 2 idiv", &[1.0]);
        op_impl("4 2 idiv", &[2.0]);
        op_impl("-5 2 idiv", &[-2.0]);
    }

    #[test]
    fn op_ln() {
        op_impl("10 ln", &[LN_10]);
        op_impl("100 ln", &[4.6051702]);
    }

    #[test]
    fn op_log() {
        op_impl("10 log", &[1.0]);
        op_impl("100 log", &[2.0]);
    }

    #[test]
    fn op_mod() {
        op_impl("5 3 mod", &[2.0]);
        op_impl("5 2 mod", &[1.0]);
        op_impl("-5 3 mod", &[-2.0]);
    }

    #[test]
    fn op_mul() {
        op_impl("5 3 mul", &[15.0]);
        op_impl("-2 6 mul", &[-12.0]);
    }

    #[test]
    fn op_neg() {
        op_impl("4.5 neg", &[-4.5]);
        op_impl("-3 neg", &[3.0]);
    }

    #[test]
    fn op_round() {
        op_impl("3.2 round", &[3.0]);
        op_impl("6.5 round", &[7.0]);
        op_impl("-4.8 round", &[-5.0]);
        op_impl("-6.5 round", &[-6.0]);
        op_impl("99 round", &[99.0]);
        op_impl("99.0 round", &[99.0]);
    }

    #[test]
    fn op_round_halfway_neighbors() {
        // The oracle is the stated nearest integer, with ties toward +infinity
        // (PostScript Language Reference, third edition, section 8.2, round).
        for (half, lower, upper) in [
            (-6.5_f32, -7.0, -6.0),
            (-1.5, -2.0, -1.0),
            (-0.5, -1.0, 0.0),
            (0.5, 0.0, 1.0),
            (1.5, 1.0, 2.0),
            (6.5, 6.0, 7.0),
        ] {
            for (input, expected) in [
                (half.next_down(), lower),
                (half, upper),
                (half.next_up(), upper),
            ] {
                op_impl(&format!("{input} round"), &[expected]);
            }
        }
        // Integral values must not move when the spacing reaches one or more.
        for value in [
            -16_777_216.0_f32,
            -8_388_609.0,
            0.0,
            8_388_609.0,
            16_777_216.0,
        ] {
            op_impl(&format!("{value} round"), &[value]);
        }
    }

    #[test]
    fn op_sin() {
        op_impl("0.0 sin", &[0.0]);
        op_impl("90.0 sin", &[1.0]);
    }

    #[test]
    fn op_sqrt() {
        op_impl("100 sqrt", &[10.0]);
    }

    #[test]
    fn op_sub() {
        op_impl("3 4 sub", &[-1.0]);
        op_impl("6 0 sub", &[6.0]);
    }

    #[test]
    fn op_truncate() {
        op_impl("3.2 truncate", &[3.0]);
        op_impl("-4.8 truncate", &[-4.0]);
        op_impl("99 truncate", &[99.0]);
    }

    #[test]
    fn op_and() {
        op_impl("true true and", &[1.0]);
        op_impl("true false and", &[0.0]);
        op_impl("false true and", &[0.0]);
        op_impl("false false and", &[0.0]);
        op_impl("99 1 and", &[1.0]);
        op_impl("52 7 and", &[4.0]);
    }

    #[test]
    fn op_bitshift() {
        op_impl("7 3 bitshift", &[56.0]);
        op_impl("142 -3 bitshift", &[17.0]);
    }

    #[test]
    fn op_eq() {
        op_impl("4.0 4 eq", &[1.0]);
        op_impl("-2.0 -3 eq", &[0.0]);
    }

    #[test]
    fn op_false() {
        op_impl("false", &[0.0]);
    }

    #[test]
    fn op_ge() {
        op_impl("4.2 4 ge", &[1.0]);
        op_impl("4.2 4.2 ge", &[1.0]);
        op_impl("4.2 6 ge", &[0.0]);
    }

    #[test]
    fn op_gt() {
        op_impl("4.2 4 gt", &[1.0]);
        op_impl("4.2 4.2 gt", &[0.0]);
        op_impl("4.2 6 gt", &[0.0]);
    }

    #[test]
    fn op_le() {
        op_impl("4.2 4 le", &[0.0]);
        op_impl("4.2 4.2 le", &[1.0]);
        op_impl("4.2 6 le", &[1.0]);
    }

    #[test]
    fn op_lt() {
        op_impl("4.2 4 lt", &[0.0]);
        op_impl("4.2 4.2 lt", &[0.0]);
        op_impl("4.2 6 lt", &[1.0]);
    }

    #[test]
    fn op_ne() {
        op_impl("3.0 3 ne", &[0.0]);
        op_impl("3.0 3.0 ne", &[0.0]);
        op_impl("3.0 3.1 ne", &[1.0]);
    }

    #[test]
    fn op_not() {
        op_impl("true not", &[0.0]);
        op_impl("false not", &[1.0]);
        op_impl("52 not", &[-53.0]);
    }

    #[test]
    fn op_or() {
        op_impl("true true or", &[1.0]);
        op_impl("true false or", &[1.0]);
        op_impl("false true or", &[1.0]);
        op_impl("false false or", &[0.0]);
        op_impl("17 5 or", &[21.0]);
    }

    #[test]
    fn op_xor() {
        op_impl("true true xor", &[0.0]);
        op_impl("true false xor", &[1.0]);
        op_impl("false true xor", &[1.0]);
        op_impl("false false xor", &[0.0]);
        op_impl("7 3 xor", &[4.0]);
        op_impl("12 3 xor", &[15.0]);
    }

    #[test]
    fn op_if() {
        op_impl("true { 1.0 } if", &[1.0]);
        op_impl("false { 1.0 } if", &[]);
    }

    #[test]
    fn op_ifelse() {
        op_impl("true { 1.0 } { 2.0 } ifelse", &[1.0]);
        op_impl("false { 1.0 } { 2.0 } ifelse", &[2.0]);
    }

    #[test]
    fn op_copy() {
        op_impl("1 2 3 2 copy", &[1.0, 2.0, 3.0, 2.0, 3.0]);
        op_impl("1 2 3 0 copy", &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn op_dup() {
        op_impl("1 2 3 dup", &[1.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn op_exch() {
        op_impl("1 2 3 exch", &[1.0, 3.0, 2.0]);
    }

    #[test]
    fn op_index() {
        op_impl("1 2 3 4 0 index", &[1.0, 2.0, 3.0, 4.0, 4.0]);
        op_impl("1 2 3 4 3 index", &[1.0, 2.0, 3.0, 4.0, 1.0]);
    }

    #[test]
    fn op_pop() {
        op_impl("1 2 3 pop", &[1.0, 2.0]);
    }

    #[test]
    fn op_roll() {
        op_impl("1 2 3 3 -1 roll", &[2.0, 3.0, 1.0]);
        op_impl("1 2 3 3 1 roll", &[3.0, 1.0, 2.0]);
        op_impl("1 2 3 3 0 roll", &[1.0, 2.0, 3.0]);
        op_impl("1 2 3 3 5 roll", &[2.0, 3.0, 1.0]);
        op_impl("0 2 roll", &[]);
        op_impl(
            "1 2 3 4 5 6 7 5 2 roll",
            &[1.0, 2.0, 6.0, 7.0, 3.0, 4.0, 5.0],
        );
    }

    #[test]
    fn domain() {
        let procedure = parse_procedure(b"{  }").unwrap();

        let type4 = Function(Arc::new(FunctionType::Type4(Type4 {
            program: procedure,
            clamper: Clamper {
                domain: smallvec![(-5.0, 5.0), (-5.0, 5.0), (-5.0, 5.0)],
                range: None,
            },
        })));

        let input = smallvec![-10.0, -2.0, 6.0];
        let res = type4.eval(input).unwrap();

        assert_eq!(res.as_slice(), &[-5.0, -2.0, 5.0]);
    }
}
