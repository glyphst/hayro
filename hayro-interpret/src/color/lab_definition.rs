//! Validated parameters of the fixed Lab-to-RGB output policy.
use super::{ColorSpace, ColorSpaceType};

/// The real-valued conversion used by a direct `Lab` space.
///
/// Clamp L*, a* and b* to `ranges`. With m = (L* + 16)/116, form
/// [m + a*/500, m, m - b*/200]. Apply g(x) = x*x*x for x >= 6/29,
/// otherwise (108/841)*(x - 4/29), to each component. These intermediates
/// are not restricted to [0, 1]. Multiply by the row-major `matrix` and
/// add `offset`, then clamp linear RGB to [0, 1] and encode sRGB using
/// 12.92*x through 0.0031308 and 1.055*x^(1/2.4)-0.055 above it.
///
/// The stored binary64 coefficients include the declared white point,
/// Bradford adaptation and the fixed black-point mapping. This is an owned
/// description of the existing conversion, not a reparsed dictionary or a
/// change to rendering intent. Paint narrows the final channels to binary32;
/// byte image output quantizes after conversion.
#[derive(Debug, Clone, Copy)]
pub struct LabRgbDefinition {
    /// Validated source intervals: [0, 100] for L*, then declared a* and b*.
    pub ranges: [[f32; 2]; 3],
    /// The row-major transform from decoded LMN to linear sRGB.
    pub matrix: [[f64; 3]; 3],
    /// The linear-sRGB offset from the declared black-point policy.
    pub offset: [f64; 3],
}

impl ColorSpace {
    /// Inspect a direct Lab space's validated conversion parameters.
    ///
    /// Tint and palette wrappers return None. A caller composing an ordinary
    /// tint may inspect its [`Self::tint_transform`] alternate separately.
    pub fn lab_rgb_definition(&self) -> Option<LabRgbDefinition> {
        match self.0.as_ref() {
            ColorSpaceType::Lab(lab) => Some(lab.definition()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::object::{FromBytes, Object};

    fn space(pdf: &str) -> ColorSpace {
        ColorSpace::from_pdf_object(Object::from_bytes(pdf.as_bytes()).unwrap()).unwrap()
    }

    fn evaluate(definition: LabRgbDefinition, input: [f64; 3]) -> [f64; 3] {
        let input: [f64; 3] = core::array::from_fn(|axis| {
            input[axis].clamp(
                f64::from(definition.ranges[axis][0]),
                f64::from(definition.ranges[axis][1]),
            )
        });
        let m = (input[0] + 16.0) / 116.0;
        let decoded = [m + input[1] / 500.0, m, m - input[2] / 200.0].map(|v| {
            if v >= 6.0 / 29.0 {
                v * v * v
            } else {
                (108.0 / 841.0) * (v - 4.0 / 29.0)
            }
        });
        core::array::from_fn(|channel| {
            let linear = (definition.matrix[channel]
                .into_iter()
                .zip(decoded)
                .map(|(a, b)| a * b)
                .sum::<f64>()
                + definition.offset[channel])
                .clamp(0.0, 1.0);
            if linear <= 0.0031308 {
                12.92 * linear
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            }
        })
    }

    #[test]
    fn lab_definition_retains_declared_ranges_and_stored_conversion() {
        for pdf in [
            "[/Lab<</WhitePoint[.95047 1 1.08883]>>]",
            "[/Lab<</WhitePoint[.96422 1 .82521]/BlackPoint[.02 .03 .01]/Range[-40 60 -50 20]>>]",
            "[/Lab<</WhitePoint[1 1 1]/Range[0 0 20 20]>>]",
            "[/Lab<</WhitePoint[1 1 1]/Range[-300000000000000000000000000000000000000 300000000000000000000000000000000000000 -100 100]>>]",
        ] {
            let color = space(pdf);
            let definition = color.lab_rgb_definition().unwrap();
            assert_eq!(definition.ranges[0], [0.0, 100.0]);
            for i in 0..1025 {
                let input = [
                    -10.0 + 120.0 * f64::from(i) / 1024.0,
                    -150.0 + 300.0 * f64::from(i % 37) / 36.0,
                    150.0 - 300.0 * f64::from(i % 61) / 60.0,
                ];
                let expected = color.image_rgb_f64(&input).unwrap();
                let actual = evaluate(definition, input);
                assert!(
                    actual
                        .into_iter()
                        .zip(expected)
                        .all(|(a, b)| (a - b).abs() < 1.0e-12),
                    "{pdf}: {input:?}"
                );
                let input = input.map(|v| v as f32);
                let expected = evaluate(definition, input.map(f64::from));
                let paint = color.to_rgba(&input, 0.375).components();
                assert_eq!(paint[3], 0.375);
                assert!(
                    paint[..3]
                        .iter()
                        .zip(expected)
                        .all(|(a, b)| (f64::from(*a) - b).abs() <= 2.0 * f64::from(f32::EPSILON))
                );
            }
            for lightness in [0.0, 7.999999, 8.0, 8.000001, 100.0] {
                let input = [lightness, 0.0, 0.0];
                let actual = evaluate(definition, input);
                let expected = color.image_rgb_f64(&input).unwrap();
                assert!(
                    actual
                        .into_iter()
                        .zip(expected)
                        .all(|(a, b)| (a - b).abs() < 1.0e-12)
                );
            }
        }
    }

    #[test]
    fn lab_definition_preserves_wrapper_and_invalid_dictionary_boundaries() {
        for pdf in [
            "/DeviceRGB",
            "[/CalGray<</WhitePoint[1 1 1]>>]",
            "[/Indexed[/Lab<</WhitePoint[1 1 1]>>]0<000000>]",
            "[/Separation/Spot[/Lab<</WhitePoint[1 1 1]>>]<</FunctionType 2/Domain[0 1]/C0[0 0 0]/C1[100 0 0]/N 1>>]",
            "[/DeviceN[/Spot][/Lab<</WhitePoint[1 1 1]>>]<</FunctionType 2/Domain[0 1]/C0[0 0 0]/C1[100 0 0]/N 1>>]",
        ] {
            assert!(space(pdf).lab_rgb_definition().is_none(), "{pdf}");
        }
        for pdf in [
            "[/Lab<<>>]",
            "[/Lab<</WhitePoint[1 2 1]>>]",
            "[/Lab<</WhitePoint[1 1 1]/Range[1 -1 -20 20]>>]",
            "[/Lab<</WhitePoint[1 1 1]/Range[-1 1 -20 20 null]>>]",
        ] {
            assert!(
                ColorSpace::from_pdf_object(Object::from_bytes(pdf.as_bytes()).unwrap()).is_none(),
                "{pdf}"
            );
        }
    }
}
