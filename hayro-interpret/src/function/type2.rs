use super::numeric::{MAX_COMPONENTS, affine, present, values};
use crate::function::{Clamper, Values};
use hayro_syntax::object::{Dict, Number};
use smallvec::smallvec;

/// A type 2 function (exponential function).
#[derive(Debug)]
pub(crate) struct Type2 {
    c0: Values,
    c1: Values,
    clamper: Clamper,
    n: f64,
    odd: bool,
}

impl Type2 {
    pub(crate) fn new(dict: &Dict<'_>) -> Option<Self> {
        let c0 = if present(dict, b"C0") {
            values(dict, b"C0", MAX_COMPONENTS)?
        } else {
            smallvec![0.0]
        };
        let c1 = if present(dict, b"C1") {
            values(dict, b"C1", MAX_COMPONENTS)?
        } else {
            smallvec![1.0]
        };
        let clamper = Clamper::new(dict)?;
        if c0.is_empty()
            || c0.len() != c1.len()
            || clamper.domain.len() != 1
            || clamper
                .range
                .as_ref()
                .is_some_and(|range| range.len() != c0.len())
        {
            return None;
        }
        let number = dict.get::<Number>(b"N")?;
        let n = number.as_f64();
        let integer = number.as_i64_exact().or_else(|| {
            (n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0)
                .then_some(n as i64)
        });
        let (low, high) = clamper.domain[0];
        // PDF 1.7 7.10.3 constrains the entire Domain, even for constant C arrays.
        if !n.is_finite()
            || (low < 0.0 && integer.is_none())
            || (n < 0.0 && low <= 0.0 && high >= 0.0)
        {
            return None;
        }
        let function = Self {
            c0,
            c1,
            clamper,
            n,
            odd: integer.is_some_and(|n| n & 1 != 0),
        };
        if !function.numerically_bounded() {
            return None;
        }
        // Powers are monotone on either side of zero. Their endpoints and zero
        // bound every output, including unclipped outputs without a Range.
        function.eval(low)?;
        function.eval(high)?;
        if low <= 0.0 && high >= 0.0 {
            function.eval(0.0)?;
        }
        Some(function)
    }

    pub(super) fn arity(&self) -> (usize, usize) {
        (1, self.c0.len())
    }

    fn log_power(&self, x: f32) -> f64 {
        let x = f64::from(x).abs();
        let logarithm = if (0.5..=1.5).contains(&x) {
            (x - 1.0).ln_1p()
        } else {
            x.ln()
        };
        self.n * logarithm
    }

    fn numerically_bounded(&self) -> bool {
        if self.n == 0.0 || self.n == 1.0 {
            return true;
        }
        let (low, high) = self.clamper.domain[0];
        let tiny = f32::from_bits(1);
        for (start, end) in [(low, high.min(-tiny)), (low.max(tiny), high)] {
            if start > end {
                continue;
            }
            if start == end && start.abs() == 1.0 {
                continue;
            }
            let a = self.log_power(start);
            let b = self.log_power(end);
            let near_one = a.abs().max(b.abs()) <= 0.5;
            let magnitude = if near_one {
                a.exp_m1().abs().max(b.exp_m1().abs())
            } else {
                a.max(b).exp()
            };
            for (&c0, &c1) in self.c0.iter().zip(&self.c1) {
                if c0 == c1 {
                    continue;
                }
                let (c0, c1) = (f64::from(c0), f64::from(c1));
                let anchor = if near_one {
                    if start < 0.0 && self.odd {
                        2.0 * c0 - c1
                    } else {
                        c1
                    }
                } else {
                    c0
                };
                // Conservative admission budget before binary32 output rounding.
                // The log/exp argument factor is capped only in the underflow
                // tail, where exp(t) cannot be amplified by binary32 C values.
                // Ordinary coefficients and expm1's small residual remain native;
                // unproved large amplification requires the caller's fallback.
                let error = 128.0
                    * f64::EPSILON
                    * (1.0 + a.abs().max(b.abs()).min(1024.0))
                    * (anchor.abs() + (c1 - c0).abs() * magnitude).max(1.0);
                if !error.is_finite() || error > 1.0e-6 {
                    return false;
                }
            }
        }
        true
    }

    pub(crate) fn eval(&self, input: f32) -> Option<Values> {
        if !input.is_finite() {
            return None;
        }
        let (low, high) = self.clamper.domain[0];
        let x = input.clamp(low, high);
        self.c0
            .iter()
            .zip(&self.c1)
            .enumerate()
            .map(|(index, (&c0, &c1))| {
                let output = if c0 == c1 {
                    f64::from(c0)
                } else if self.n == 0.0 || x == 1.0 {
                    f64::from(c1)
                } else if self.n == 1.0 {
                    affine(x, 0.0, 1.0, c0, c1)
                } else if x == 0.0 {
                    f64::from(c0)
                } else {
                    let (c0, c1) = (f64::from(c0), f64::from(c1));
                    let t = self.log_power(x);
                    let negative = x < 0.0 && self.odd;
                    if t.abs() <= 0.5 {
                        // c0 + exp(t)*(c1-c0) loses a tiny residual when exp(t)
                        // rounds to one. Keep the anchor at the exact endpoint.
                        if negative {
                            (-(c1 - c0)).mul_add(t.exp_m1(), 2.0 * c0 - c1)
                        } else {
                            (c1 - c0).mul_add(t.exp_m1(), c1)
                        }
                    } else {
                        let factor = if negative { -t.exp() } else { t.exp() };
                        (c1 - c0).mul_add(factor, c0)
                    }
                };
                if !output.is_finite() {
                    return None;
                }
                let output = if let Some(range) = &self.clamper.range {
                    output.clamp(f64::from(range[index].0), f64::from(range[index].1))
                } else {
                    output
                } as f32;
                output.is_finite().then_some(output)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::function::Function;

    use hayro_syntax::object::Object;
    use hayro_syntax::object::{Dict, FromBytes};
    use smallvec::smallvec;

    #[test]
    fn simple() {
        let func = Function::new(&Object::Dict(
            Dict::from_bytes(
                b"<<
              /FunctionType 2
              /Domain [ 0  1 ]
              /C0 [ 0 20  ]
              /C1 [ 30 -50 ]
              /N 1
            >>",
            )
            .unwrap(),
        ))
        .unwrap();

        assert_eq!(func.eval(smallvec![0.0]).unwrap().as_ref(), &[0.0, 20.0]);
        assert_eq!(func.eval(smallvec![0.5]).unwrap().as_ref(), &[15.0, -15.0]);
        assert_eq!(func.eval(smallvec![1.0]).unwrap().as_ref(), &[30.0, -50.0]);
    }

    #[test]
    fn with_exponent() {
        let func = Function::new(&Object::Dict(
            Dict::from_bytes(
                b"<<
              /FunctionType 2
              /Domain [ 0  1 ]
              /C0 [ 0  ]
              /C1 [ 30 ]
              /N 2
            >>",
            )
            .unwrap(),
        ))
        .unwrap();

        assert_eq!(func.eval(smallvec![0.5]), Some(smallvec![7.5]));
    }

    #[test]
    fn clamp_domain() {
        let func = Function::new(&Object::Dict(
            Dict::from_bytes(
                b"<<
              /FunctionType 2
              /Domain [ 0.2  0.8 ]
              /C0 [ 0  ]
              /C1 [ 30 ]
              /N 2
            >>",
            )
            .unwrap(),
        ))
        .unwrap();

        assert_eq!(
            func.eval(smallvec![0.0]).as_ref(),
            func.eval(smallvec![0.2]).as_ref(),
        );
        assert_eq!(
            func.eval(smallvec![-10.]).as_ref(),
            func.eval(smallvec![0.2]).as_ref(),
        );
        assert_eq!(
            func.eval(smallvec![0.8]).as_ref(),
            func.eval(smallvec![0.8]).as_ref(),
        );
        assert_eq!(
            func.eval(smallvec![1.2]).as_ref(),
            func.eval(smallvec![1.0]).as_ref(),
        );
    }

    #[test]
    fn clamp_range() {
        let func = Function::new(&Object::Dict(
            Dict::from_bytes(
                b"<<
              /FunctionType 2
              /Domain [ 0.0  1.0 ]
              /Range [10.0 20.0]
              /C0 [ 0  ]
              /C1 [ 30 ]
              /N 1
            >>",
            )
            .unwrap(),
        ))
        .unwrap();

        assert_eq!(func.eval(smallvec![0.0]), Some(smallvec![10.0]));
        assert_eq!(func.eval(smallvec![0.5]), Some(smallvec![15.0]));
        assert_eq!(func.eval(smallvec![1.0]), Some(smallvec![20.0]));
    }
}
