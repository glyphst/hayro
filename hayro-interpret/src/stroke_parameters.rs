//! Shared PDF graphics-state scalar conversion for operators and `ExtGState`.
use crate::{Context, InterpreterWarning, StrokeProps};
use hayro_syntax::object::Number;
use kurbo::{Cap, Join};

fn bounded_number(number: Number, minimum: f64) -> Option<f32> {
    let value = number.as_f64();
    if !value.is_finite() {
        return None;
    }
    // PDF 32000-1:2008 8.4.1: numeric state values are forced into range.
    // Do this before narrowing, so a large negative width still becomes zero.
    let value = value.max(minimum);
    let narrowed = value as f32;
    // A positive width must not become the distinct device-hairline value.
    (narrowed.is_finite() && (value == 0.0 || narrowed > 0.0)).then_some(narrowed)
}

impl StrokeProps {
    /// Read a finite PDF line width, clamping negative values to zero.
    /// Returns `None` if narrowing would overflow or turn a positive width into
    /// the distinct zero-width device hairline.
    pub fn read_line_width(number: Number) -> Option<f32> {
        bounded_number(number, 0.0)
    }

    /// Read a PDF miter limit, bounded below by the unit miter ratio.
    /// Nonfinite values and values too large for the retained scalar fail.
    pub fn read_miter_limit(number: Number) -> Option<f32> {
        bounded_number(number, 1.0)
    }

    /// Read an integer PDF line-cap code and clamp it to the range 0 through 2.
    /// Real objects are not integer codes, even when their value is integral.
    pub fn read_line_cap(number: Number) -> Option<Cap> {
        Some(match number.as_i64_exact()?.clamp(0, 2) {
            0 => Cap::Butt,
            1 => Cap::Round,
            _ => Cap::Square,
        })
    }

    /// Read an integer PDF line-join code and clamp it to the range 0 through 2.
    /// Real objects are not integer codes, even when their value is integral.
    pub fn read_line_join(number: Number) -> Option<Join> {
        Some(match number.as_i64_exact()?.clamp(0, 2) {
            0 => Join::Miter,
            1 => Join::Round,
            _ => Join::Bevel,
        })
    }
}

impl Context<'_> {
    pub(crate) fn set_stroke_parameter(&mut self, key: &[u8], number: Option<Number>) {
        let stroke = &mut self.get_mut().graphics_state.stroke_props;
        let applied = match key {
            b"LW" => number
                .and_then(StrokeProps::read_line_width)
                .map(|value| stroke.line_width = value),
            b"LC" => number
                .and_then(StrokeProps::read_line_cap)
                .map(|value| stroke.line_cap = value),
            b"LJ" => number
                .and_then(StrokeProps::read_line_join)
                .map(|value| stroke.line_join = value),
            b"ML" => number
                .and_then(StrokeProps::read_miter_limit)
                .map(|value| stroke.miter_limit = value),
            _ => unreachable!("known stroke parameter"),
        };
        if applied.is_none() {
            (self.settings.warning_sink)(InterpreterWarning::StrokeParameterFailure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::object::FromBytes;

    fn number(bytes: &[u8]) -> Number {
        Number::from_bytes(bytes).expect("authored number")
    }

    #[test]
    fn scalar_ranges_are_normalized_before_narrowing() {
        for source in [
            b"-8".as_slice(),
            b"-99999999999999999999999999999999999999999999999999",
        ] {
            assert_eq!(StrokeProps::read_line_width(number(source)), Some(0.0));
            assert_eq!(StrokeProps::read_miter_limit(number(source)), Some(1.0));
        }
        for source in [b"0".as_slice(), b"0.99999999999999999"] {
            assert_eq!(StrokeProps::read_miter_limit(number(source)), Some(1.0));
        }
        for source in [b"8".as_slice(), b"8.0"] {
            assert_eq!(StrokeProps::read_line_width(number(source)), Some(8.0));
            assert_eq!(StrokeProps::read_miter_limit(number(source)), Some(8.0));
        }
        let tiny = number(b"0.00000000000000000000000000000000000000000000000001");
        assert!(tiny.as_f64() > 0.0 && tiny.as_f32() == 0.0);
        assert_eq!(StrokeProps::read_line_width(tiny), None);
        let huge = number(b"99999999999999999999999999999999999999999999999999");
        assert_eq!(StrokeProps::read_line_width(huge), None);
        assert_eq!(StrokeProps::read_miter_limit(huge), None);
        for source in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            assert_eq!(StrokeProps::read_line_width(Number::from_f32(source)), None);
            assert_eq!(
                StrokeProps::read_miter_limit(Number::from_f32(source)),
                None
            );
        }
    }

    #[test]
    fn integer_styles_clamp_without_accepting_real_objects() {
        for (source, cap, join) in [
            (i32::MIN, Cap::Butt, Join::Miter),
            (0, Cap::Butt, Join::Miter),
            (1, Cap::Round, Join::Round),
            (2, Cap::Square, Join::Bevel),
            (i32::MAX, Cap::Square, Join::Bevel),
        ] {
            assert_eq!(
                StrokeProps::read_line_cap(Number::from_i32(source)),
                Some(cap)
            );
            assert_eq!(
                StrokeProps::read_line_join(Number::from_i32(source)),
                Some(join)
            );
        }
        for source in [b"1.0".as_slice(), b"1.5", b"2.", b"-.5"] {
            assert_eq!(StrokeProps::read_line_cap(number(source)), None);
            assert_eq!(StrokeProps::read_line_join(number(source)), None);
        }
    }
}
