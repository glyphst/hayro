//! Independently stated results from PDF calculator / PLRM 8.2 semantics.

use super::{Argument, InterpreterStack, Type4, eval_inner, parse_procedure};
use crate::function::Clamper;
use smallvec::smallvec;

fn execute(program: &str) -> Option<Vec<Argument>> {
    let procedure = parse_procedure(format!("{{ {program} }}").as_bytes())?;
    let mut stack = InterpreterStack::new();
    eval_inner(&procedure, &mut stack)?;
    Some(stack.items().to_vec())
}

#[test]
fn numbers_preserve_integer_real_and_boolean_objects() {
    use Argument::{Bool, Float, Integer};
    for (program, expected) in [
        ("16777217", Integer(16_777_217)),
        ("16777217 16777216 sub", Integer(1)),
        ("16777217 127 mul", Integer(2_130_706_559)),
        ("16777217 129 mul", Float(2_164_261_120.0)),
        ("2147483647 1 add", Float(2_147_483_648.0)),
        ("2147483647 -1 sub", Float(2_147_483_648.0)),
        ("-2147483648 neg", Float(2_147_483_648.0)),
        ("-2147483648 abs", Float(2_147_483_648.0)),
        ("2147483648", Float(2_147_483_648.0)),
        ("2147483647 1 mul", Integer(i32::MAX)),
        ("3 2 add", Integer(5)),
        ("3.0 2 add", Float(5.0)),
        ("3 2.0 add", Float(5.0)),
        ("3 2 div", Float(1.5)),
        ("-5 2 idiv", Integer(-2)),
        ("-5 3 mod", Integer(-2)),
        ("-2147483648 -1 mod", Integer(0)),
        ("-47.8 cvi", Integer(-47)),
        ("-2147483648.0 cvi", Integer(i32::MIN)),
        ("16777217 cvr", Float(16_777_216.0)),
        ("16777217 round", Integer(16_777_217)),
        ("3.5 round", Float(4.0)),
        ("-0.5 round", Float(0.0)),
        ("3.0 ceiling", Float(3.0)),
        ("3 floor", Integer(3)),
        ("-3.2 truncate", Float(-3.0)),
        ("true false and", Bool(false)),
        ("true false or", Bool(true)),
        ("true true xor", Bool(false)),
        ("true not", Bool(false)),
        ("0 not", Integer(-1)),
        ("16777217 1 and", Integer(1)),
        ("16777216 1 or", Integer(16_777_217)),
        ("16777217 1 xor", Integer(16_777_216)),
        ("1 1 eq not", Bool(false)),
        ("true 1 eq", Bool(false)),
        ("false 0 ne", Bool(true)),
        ("16777217 16777216 eq", Bool(false)),
        ("16777217 16777216.0 gt", Bool(true)),
        ("-16777217 -16777216.0 lt", Bool(true)),
        ("2147483647 2147483648.0 lt", Bool(true)),
        ("3 3.0 eq", Bool(true)),
        ("0 0 exp", Float(1.0)),
        ("-1 2147483647 exp", Float(-1.0)),
    ] {
        assert_eq!(execute(program), Some(vec![expected]), "{program}");
    }
}

#[test]
fn shifts_use_all_integer_bits_and_zero_fill_without_wrapping_the_shift() {
    for (program, expected) in [
        ("-1 -1 bitshift", i32::MAX),
        ("-1 -31 bitshift", 1),
        ("1 31 bitshift", i32::MIN),
        ("1 32 bitshift", 0),
        ("-1 -32 bitshift", 0),
        ("1 64 bitshift", 0),
        ("-1 -2147483648 bitshift", 0),
    ] {
        assert_eq!(
            execute(program),
            Some(vec![Argument::Integer(expected)]),
            "{program}"
        );
    }
}

#[test]
fn invalid_operands_and_arithmetic_abort_even_when_later_discarded() {
    for program in [
        "true 1 add",
        "1.0 1 and",
        "true 1 or",
        "false 0 xor",
        "1.0 not",
        "3.0 2 idiv",
        "5 3.0 mod",
        "1 1.0 bitshift",
        "1 { 0 } if",
        "0 { 0 } { 1 } ifelse",
        "true false lt",
        "2147483648.0 cvi",
        "-2147483904.0 cvi",
        "1 0 div pop 1",
        "1 0 idiv pop 1",
        "-2147483648 -1 idiv",
        "1 0 mod pop 1",
        "0 0 atan pop 1",
        "-1 sqrt pop 1",
        "0 ln pop 1",
        "0 log pop 1",
        "-1 0.5 exp pop 1",
        "0 -1 exp pop 1",
        "1 1.0 copy",
        "1 -1 copy",
        "1 0.0 index",
        "1 1.0 1 roll",
        "1 1 1.0 roll",
        "true cvi",
        "false cvr",
        "340282346638528859811704183484516925440 2 mul pop 1",
    ] {
        assert_eq!(execute(program), None, "{program}");
    }
    assert!(execute(&"0 ".repeat(65)).is_none());
    assert!(execute(&format!("{} 33 copy", "0 ".repeat(33))).is_none());
    assert_eq!(
        execute("1 2 3 3 -2147483648 roll"),
        Some(vec![
            Argument::Integer(3),
            Argument::Integer(1),
            Argument::Integer(2),
        ])
    );
}

#[test]
fn function_boundaries_require_finite_inputs_and_numeric_output_counts() {
    for program in ["{ pop true }", "{ pop 0 1 }", "{ pop }"] {
        let function = Type4 {
            program: parse_procedure(program.as_bytes()).unwrap(),
            clamper: Clamper {
                domain: smallvec![(0.0, 1.0)],
                range: Some(smallvec![(0.0, 1.0)]),
            },
        };
        assert!(function.eval(smallvec![0.5]).is_none(), "{program}");
    }
    let identity = Type4 {
        program: parse_procedure(b"{ }").unwrap(),
        clamper: Clamper {
            domain: smallvec![(0.0, 1.0)],
            range: Some(smallvec![(0.0, 1.0)]),
        },
    };
    for input in [
        smallvec![f32::NAN],
        smallvec![f32::INFINITY],
        smallvec![],
        smallvec![0.0, 1.0],
    ] {
        assert!(identity.eval(input).is_none());
    }
    assert_eq!(identity.eval(smallvec![2.0]), Some(smallvec![1.0]));
}
