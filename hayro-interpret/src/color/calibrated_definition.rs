//! Validated parameters of the fixed calibrated-color output policy.
use super::{ColorSpace, ColorSpaceType};

/// The real-valued conversion used by a direct `CalGray` or `CalRGB` space.
///
/// Clamp each input to [0, 1], then raise it to the corresponding `gamma`.
/// `CalGray` repeats its single input for all three axes. Multiply the decoded
/// vector by the row-major `matrix` and add `offset`; these already include
/// the declared XYZ matrix, Bradford adaptation and the fixed black-point
/// policy. Clamp each linear result to [0, 1], then apply the sRGB encoding
/// function (12.92*x through 0.0031308; 1.055*x^(1/2.4)-0.055 above it).
///
/// These are the stored binary64 conversion coefficients, not a reparsed or
/// independently rounded color-space dictionary. Paint conversion narrows the
/// encoded channels to binary32; native image conversion keeps binary64 channels.
/// Byte image output quantizes only after conversion. The rendering-intent and
/// fixed output-color policies are unchanged.
#[derive(Debug, Clone, Copy)]
pub struct CalibratedRgbDefinition {
    /// One for `CalGray`, three for `CalRGB`.
    pub input_components: u8,
    /// Validated positive binary32 PDF gamma values.
    pub gamma: [f32; 3],
    /// The row-major transform from gamma-decoded inputs to linear sRGB.
    pub matrix: [[f64; 3]; 3],
    /// The linear-sRGB offset from the declared black-point policy.
    pub offset: [f64; 3],
}

impl ColorSpace {
    /// Inspect a direct calibrated space's validated conversion parameters.
    ///
    /// Other spaces, including tint and palette wrappers, return None. A caller
    /// composing an ordinary tint may inspect its [`Self::tint_transform`]
    /// alternate after preserving the tint function and its component bounds.
    pub fn calibrated_rgb_definition(&self) -> Option<CalibratedRgbDefinition> {
        match self.0.as_ref() {
            ColorSpaceType::CalGray(gray) => Some(gray.definition()),
            ColorSpaceType::CalRgb(rgb) => Some(rgb.definition(3)),
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

    fn evaluate(definition: CalibratedRgbDefinition, input: &[f64]) -> [f64; 3] {
        let decoded: [f64; 3] = core::array::from_fn(|axis| {
            let index = if definition.input_components == 1 {
                0
            } else {
                axis
            };
            input[index]
                .clamp(0.0, 1.0)
                .powf(f64::from(definition.gamma[axis]))
        });
        core::array::from_fn(|channel| {
            let value = (definition.matrix[channel]
                .iter()
                .zip(decoded)
                .map(|(a, b)| a * b)
                .sum::<f64>()
                + definition.offset[channel])
                .clamp(0.0, 1.0);
            if value <= 0.0031308 {
                12.92 * value
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            }
        })
    }

    #[test]
    fn calibrated_definition_matches_real_and_paint_conversion() {
        for pdf in [
            "[/CalGray<</WhitePoint[.95047 1 1.08883]/Gamma .5>>]",
            "[/CalGray<</WhitePoint[.96422 1 .82521]/BlackPoint[.02 .03 .01]/Gamma 2>>]",
            "[/CalRGB<</WhitePoint[1 1 1]/Gamma[.5 2 3]/Matrix[1 .2 -.1 -.2 1 .1 .3 -.1 1]>>]",
            "[/CalRGB<</WhitePoint[.96422 1 .82521]/BlackPoint[.096422 .1 .082521]/Matrix[.96422 0 0 0 1 0 0 0 .82521]>>]",
        ] {
            let color = space(pdf);
            let definition = color.calibrated_rgb_definition().unwrap();
            assert_eq!(
                usize::from(definition.input_components),
                color.component_count()
            );
            for i in 0..1025 {
                let input = [
                    -0.125 + f64::from(i) / 819.2,
                    f64::from(i % 37) / 36.0,
                    1.125 - f64::from(i) / 819.2,
                ];
                let input = &input[..usize::from(definition.input_components)];
                let expected = color.image_rgb_f64(input).unwrap();
                let actual = evaluate(definition, input);
                assert!(
                    actual
                        .into_iter()
                        .zip(expected)
                        .all(|(a, b)| (a - b).abs() < 1.0e-12),
                    "{pdf}: {input:?}"
                );
                let input: Vec<_> = input.iter().map(|v| *v as f32).collect();
                let expected = evaluate(
                    definition,
                    &input.iter().map(|v| f64::from(*v)).collect::<Vec<_>>(),
                );
                let paint = color.to_rgba(&input, 0.375).components();
                for channel in 0..3 {
                    assert!(
                        (f64::from(paint[channel]) - expected[channel]).abs()
                            <= 2.0 * f64::from(f32::EPSILON)
                    );
                }
                assert_eq!(paint[3], 0.375);
            }
        }
    }

    #[test]
    fn calibrated_paint_keeps_fractional_channels_until_output() {
        for pdf in [
            "[/CalGray<</WhitePoint[.95047 1 1.08883]/Gamma 2>>]",
            "[/CalRGB<</WhitePoint[.95047 1 1.08883]/Gamma[1 2 3]/Matrix[.4124564 .2126729 .0193339 .3575761 .7151522 .1191920 .1804375 .0721750 .9503041]>>]",
        ] {
            let color = space(pdf);
            let input = vec![0.314_159_27; color.component_count()];
            let rgba = color.to_rgba(&input, 0.375);
            let real = rgba.components();
            let byte = rgba.to_rgba8();
            assert!(
                real[..3]
                    .iter()
                    .zip(&byte[..3])
                    .any(|(a, b)| (f64::from(*a) - f64::from(*b) / 255.0).abs() > 1.0e-5)
            );
            assert_eq!(real[3], 0.375);
        }
    }

    #[test]
    fn calibrated_definition_keeps_wrappers_and_invalid_dictionaries_explicit() {
        for pdf in [
            "/DeviceGray",
            "/DeviceRGB",
            "/DeviceCMYK",
            "[/Lab<</WhitePoint[1 1 1]>>]",
            "[/Indexed[/CalGray<</WhitePoint[1 1 1]>>]1<00ff>]",
            "[/Separation/Spot[/CalGray<</WhitePoint[1 1 1]>>]<</FunctionType 2/Domain[0 1]/C0[0]/C1[1]/N 1>>]",
        ] {
            assert!(space(pdf).calibrated_rgb_definition().is_none(), "{pdf}");
        }
        for pdf in [
            "[/CalGray<</WhitePoint[1 1 1]/Gamma 0>>]",
            "[/CalRGB<</WhitePoint[1 1 1]/Gamma[1 -1 1]>>]",
            "[/CalRGB<</WhitePoint[1 1 1]/Matrix[1 0 0]>>]",
        ] {
            assert!(
                ColorSpace::from_pdf_object(Object::from_bytes(pdf.as_bytes()).unwrap()).is_none()
            );
        }
    }
}
