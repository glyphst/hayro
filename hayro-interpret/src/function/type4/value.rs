//! Numeric objects for the PDF calculator's 32-bit integer/real implementation.

use super::PostScriptOp;
use hayro_syntax::object::Number;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Argument {
    Integer(i32),
    Float(f32),
    Bool(bool),
}

impl Default for Argument {
    fn default() -> Self {
        Self::Integer(0)
    }
}

impl Argument {
    pub(super) fn from_number(number: Number) -> Option<Self> {
        if let Some(value) = number.as_i64_exact().and_then(|v| i32::try_from(v).ok()) {
            Some(Self::Integer(value))
        } else {
            Self::real(number.as_f32())
        }
    }

    pub(super) fn real(value: f32) -> Option<Self> {
        value.is_finite().then_some(Self::Float(value))
    }

    pub(super) fn as_f32(self) -> Option<f32> {
        match self {
            Self::Integer(value) => Some(value as f32),
            Self::Float(value) => Some(value),
            Self::Bool(_) => None,
        }
    }

    fn as_f64(self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(f64::from(value)),
            Self::Float(value) => Some(f64::from(value)),
            Self::Bool(_) => None,
        }
    }

    pub(super) fn integer(self) -> Option<i32> {
        match self {
            Self::Integer(value) => Some(value),
            _ => None,
        }
    }

    pub(super) fn count(self) -> Option<usize> {
        usize::try_from(self.integer()?).ok()
    }

    pub(super) fn boolean(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            _ => None,
        }
    }

    fn wide_integer(value: i64) -> Self {
        i32::try_from(value).map_or_else(|_| Self::Float(value as f32), Self::Integer)
    }

    pub(super) fn unary(self, op: &PostScriptOp) -> Option<Self> {
        use PostScriptOp as Op;
        if matches!(op, Op::Not) {
            return match self {
                Self::Integer(value) => Some(Self::Integer(!value)),
                Self::Bool(value) => Some(Self::Bool(!value)),
                Self::Float(_) => None,
            };
        }
        if let Self::Integer(value) = self {
            match op {
                Op::Abs => return Some(Self::wide_integer(i64::from(value).abs())),
                Op::Neg => return Some(Self::wide_integer(-i64::from(value))),
                Op::Cvi | Op::Ceiling | Op::Floor | Op::Round | Op::Truncate => return Some(self),
                _ => {}
            }
        }
        let value = self.as_f32()?;
        let result = match op {
            Op::Abs => value.abs(),
            Op::Neg => -value,
            Op::Ceiling => value.ceil(),
            Op::Floor => value.floor(),
            Op::Round => {
                // PLRM 8.2: preserve the real type and round ties toward +infinity.
                let lower = value.floor();
                if value - lower >= 0.5 {
                    lower + 1.0
                } else {
                    lower
                }
            }
            Op::Truncate => value.trunc(),
            Op::Cvi => {
                if !(-2_147_483_648.0..2_147_483_648.0).contains(&value) {
                    return None;
                }
                return Some(Self::Integer(value as i32));
            }
            Op::Cvr => value,
            Op::Cos => value.to_radians().cos(),
            Op::Sin => value.to_radians().sin(),
            Op::Ln if value > 0.0 => value.ln(),
            Op::Log if value > 0.0 => value.log10(),
            Op::Sqrt if value >= 0.0 => value.sqrt(),
            _ => return None,
        };
        Self::real(result)
    }

    pub(super) fn binary(self, second: Self, op: &PostScriptOp) -> Option<Self> {
        use PostScriptOp as Op;
        match op {
            Op::And | Op::Or | Op::Xor => {
                return match (self, second) {
                    (Self::Integer(a), Self::Integer(b)) => Some(Self::Integer(match op {
                        Op::And => a & b,
                        Op::Or => a | b,
                        _ => a ^ b,
                    })),
                    (Self::Bool(a), Self::Bool(b)) => Some(Self::Bool(match op {
                        Op::And => a & b,
                        Op::Or => a | b,
                        _ => a ^ b,
                    })),
                    _ => None,
                };
            }
            Op::Eq | Op::Ne => {
                let equal = match (self, second) {
                    (Self::Bool(a), Self::Bool(b)) => a == b,
                    (Self::Bool(_), _) | (_, Self::Bool(_)) => false,
                    _ => self.as_f64()? == second.as_f64()?,
                };
                return Some(Self::Bool(equal != matches!(op, Op::Ne)));
            }
            Op::Ge | Op::Gt | Op::Le | Op::Lt => {
                let (a, b) = (self.as_f64()?, second.as_f64()?);
                return Some(Self::Bool(match op {
                    Op::Ge => a >= b,
                    Op::Gt => a > b,
                    Op::Le => a <= b,
                    _ => a < b,
                }));
            }
            Op::Idiv => {
                return Some(Self::Integer(
                    self.integer()?.checked_div(second.integer()?)?,
                ));
            }
            Op::Mod => {
                // MIN mod -1 is exactly zero, even though MIN / -1 overflows.
                let (a, b) = (i64::from(self.integer()?), i64::from(second.integer()?));
                return Some(Self::Integer(a.checked_rem(b)? as i32));
            }
            Op::Bitshift => {
                let (value, shift) = (self.integer()? as u32, second.integer()?);
                let result = if shift.unsigned_abs() >= 32 {
                    0
                } else if shift >= 0 {
                    value << shift as u32
                } else {
                    value >> shift.unsigned_abs()
                };
                return Some(Self::Integer(result as i32));
            }
            _ => {}
        }
        if let (Self::Integer(a), Self::Integer(b)) = (self, second) {
            let (a, b) = (i64::from(a), i64::from(b));
            match op {
                Op::Add => return Some(Self::wide_integer(a + b)),
                Op::Sub => return Some(Self::wide_integer(a - b)),
                Op::Mul => return Some(Self::wide_integer(a * b)),
                _ => {}
            }
        }
        let (a, b) = (self.as_f32()?, second.as_f32()?);
        let result = match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div if b != 0.0 => a / b,
            Op::Atan if a != 0.0 || b != 0.0 => a.atan2(b).to_degrees().rem_euclid(360.0),
            Op::Exp if a != 0.0 || b >= 0.0 => {
                if a < 0.0 {
                    let exponent = second.as_f64()?;
                    if exponent.fract() != 0.0 {
                        return None;
                    }
                    let magnitude = (-a).powf(b);
                    if exponent % 2.0 == 0.0 {
                        magnitude
                    } else {
                        -magnitude
                    }
                } else {
                    a.powf(b)
                }
            }
            _ => return None,
        };
        Self::real(result)
    }
}
