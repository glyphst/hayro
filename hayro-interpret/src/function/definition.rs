use super::{CalculatorFunction, Function};

/// Validated function data for retained execution and cache serialization.
///
/// Sample tables and stitching children are borrowed to let a caller check its
/// own resource budget before copying them. The function owns every referenced
/// value independently of the PDF parser. A consumer must preserve all clamps,
/// encoding, interpolation and exponent-parity fields, and validate its owned
/// representation again when decoding an untrusted cache.
#[derive(Debug)]
#[non_exhaustive]
pub enum FunctionDefinition<'a> {
    /// A PDF Type 0 sampled function.
    Sampled {
        /// Number of samples along each input dimension.
        sizes: &'a [usize],
        /// Exact unsigned samples. Outputs are adjacent; the first input
        /// dimension varies fastest among multidimensional table indices.
        samples: &'a [u32],
        /// Largest sample value for the declared `BitsPerSample`.
        sample_maximum: u32,
        /// Declared interpolation order, 1 or 3. Dimensions smaller than four
        /// samples ignore cubic order as required by PDF 7.10.2.
        order: u8,
        /// Inclusive input clamps.
        domain: &'a [(f32, f32)],
        /// Inclusive output clamps.
        range: &'a [(f32, f32)],
        /// Input-to-table-coordinate maps.
        encode: &'a [(f32, f32)],
        /// Sample-to-output maps, before the Range clamps.
        decode: &'a [(f32, f32)],
    },
    /// A PDF Type 2 exponential function.
    Exponential {
        /// Inclusive scalar input clamp.
        domain: [f32; 2],
        /// Optional inclusive output clamps.
        range: Option<&'a [(f32, f32)]>,
        /// Output constants at power zero.
        c0: &'a [f32],
        /// Output constants at power one.
        c1: &'a [f32],
        /// The exponent, before binary32 narrowing.
        exponent: f64,
        /// Exact odd-integer parity for negative inputs. Large integer tokens
        /// can lose parity when converted to the binary64 exponent above.
        odd_integer_exponent: bool,
    },
    /// A PDF Type 3 stitching function.
    Stitching {
        /// Inclusive scalar input clamp.
        domain: [f32; 2],
        /// Optional inclusive output clamps, applied after the child function.
        range: Option<&'a [(f32, f32)]>,
        /// Ordered children. Their own Domain and Range remain significant.
        functions: &'a [Function],
        /// Internal interval boundaries. A boundary belongs to the later
        /// child, including a singleton final interval at `Domain[1]`.
        bounds: &'a [f32],
        /// One input encoding pair for each child.
        encode: &'a [(f32, f32)],
    },
    /// An owned, bounded PDF Type 4 program with typed instructions and clamps.
    Calculator(CalculatorFunction),
}

#[cfg(test)]
mod tests {
    use super::FunctionDefinition;
    use crate::{CalculatorInstruction, Function, TransferFunction};
    use hayro_syntax::bit_reader::BitWriter;
    use hayro_syntax::object::{FromBytes, Object, Stream};
    use hayro_syntax::reader::{Reader, ReaderContext, ReaderExt};
    use smallvec::smallvec;

    fn stream_function(entries: &str, data: &[u8]) -> Function {
        let mut bytes = format!("<< {entries} /Length {} >> stream\n", data.len()).into_bytes();
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(b"\nendstream");
        let mut reader = Reader::new(&bytes);
        let stream = reader
            .read_with_context::<Stream<'_>>(&ReaderContext::dummy())
            .unwrap();
        Function::new(&Object::Stream(stream)).unwrap()
    }

    #[test]
    fn sampled_definition_preserves_exact_widths_maps_and_order_without_parser_storage() {
        for bits in [1, 2, 4, 8, 12, 16, 24, 32] {
            let maximum = ((1_u64 << bits) - 1) as u32;
            let expected_samples = [0, maximum / 3, maximum];
            let mut data = vec![0; (usize::from(bits) * 3).div_ceil(8)];
            let mut writer = BitWriter::new(&mut data, bits).unwrap();
            for sample in expected_samples {
                writer.write(sample).unwrap();
            }
            let function = stream_function(
                &format!(
                    "/FunctionType 0 /Domain [0 2] /Range [0 1] /Size [3] /BitsPerSample {bits} /Order 3 /Encode [0 2] /Decode [-0.25 1.25]"
                ),
                &data,
            );
            // All parser bytes, reader contexts and streams in the helper have
            // already gone away; the exported slices belong to the function.
            let FunctionDefinition::Sampled {
                sizes,
                samples,
                sample_maximum,
                order,
                domain,
                range,
                encode,
                decode,
            } = function.definition().unwrap()
            else {
                panic!("sampled")
            };
            assert_eq!(sizes, [3]);
            assert_eq!(samples, expected_samples);
            assert_eq!(sample_maximum, maximum);
            assert_eq!(order, 3);
            assert_eq!(domain, [(0.0, 2.0)]);
            assert_eq!(range, [(0.0, 1.0)]);
            assert_eq!(encode, [(0.0, 2.0)]);
            assert_eq!(decode, [(-0.25, 1.25)]);
            assert_eq!(
                function.eval(smallvec![1.0]).unwrap()[0],
                (-0.25 + 1.5 * f64::from(maximum / 3) / f64::from(maximum)).clamp(0.0, 1.0) as f32
            );
        }
    }

    #[test]
    fn exponential_definition_preserves_integer_parity_lost_by_binary64() {
        let object = Object::from_bytes(
            b"<< /FunctionType 2 /Domain [-1 -1] /C0 [0] /C1 [1] /N 9007199254740993 >>",
        )
        .unwrap();
        let function = Function::new(&object).unwrap();
        let FunctionDefinition::Exponential {
            domain,
            range,
            c0,
            c1,
            exponent,
            odd_integer_exponent,
        } = function.definition().unwrap()
        else {
            panic!("exponential")
        };
        assert_eq!(domain, [-1.0, -1.0]);
        assert_eq!(range, None);
        assert_eq!(c0, [0.0]);
        assert_eq!(c1, [1.0]);
        assert_eq!(exponent, 9_007_199_254_740_992.0);
        assert!(odd_integer_exponent);
        assert_eq!(function.eval(smallvec![-1.0]).unwrap()[0], -1.0);
    }

    #[test]
    fn stitching_definition_preserves_the_final_singleton_and_child_clamps() {
        let object = Object::from_bytes(b"<< /FunctionType 3 /Domain [0 1] /Range [0.1 0.9] /Functions [<< /FunctionType 2 /Domain [0 1] /C0 [0.2] /C1 [0.2] /N 1 >> << /FunctionType 2 /Domain [0 1] /C0 [0.8] /C1 [0.8] /N 1 >>] /Bounds [1] /Encode [1 0 0 1] >>").unwrap();
        let function = Function::new(&object).unwrap();
        let transfer = TransferFunction::new(function).unwrap();
        let FunctionDefinition::Stitching {
            domain,
            range,
            functions,
            bounds,
            encode,
        } = transfer.function().unwrap().definition().unwrap()
        else {
            panic!("stitching")
        };
        assert_eq!(domain, [0.0, 1.0]);
        assert_eq!(range, Some([(0.1, 0.9)].as_slice()));
        assert_eq!(bounds, [1.0]);
        assert_eq!(encode, [(1.0, 0.0), (0.0, 1.0)]);
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].eval(smallvec![0.0]).unwrap()[0], 0.2);
        assert_eq!(functions[1].eval(smallvec![1.0]).unwrap()[0], 0.8);
        assert_eq!(transfer.apply_f32(1.0_f32.next_down()), Some(0.2));
        assert_eq!(transfer.apply_f32(1.0), Some(0.8));
        assert!(TransferFunction::identity().function().is_none());
    }

    #[test]
    fn calculator_definition_retains_typed_integer_instructions_and_clamps() {
        let function = stream_function(
            "/FunctionType 4 /Domain [0 1] /Range [0 20000000]",
            b"{ pop 16777217 1 sub }",
        );
        let FunctionDefinition::Calculator(program) = function.definition().unwrap() else {
            panic!("calculator")
        };
        assert_eq!(program.input_domain, [[0.0, 1.0]]);
        assert_eq!(program.output_range, Some(vec![[0.0, 20_000_000.0]]));
        assert_eq!(
            program.instructions,
            [
                CalculatorInstruction::Pop,
                CalculatorInstruction::Integer(16_777_217),
                CalculatorInstruction::Integer(1),
                CalculatorInstruction::Sub
            ]
        );
        assert_eq!(function.eval(smallvec![0.5]).unwrap()[0], 16_777_216.0);
    }
}
