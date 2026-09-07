use super::{MAX_COMPONENTS, MAX_CONTRIBUTIONS, Type0};
use hayro_syntax::bit_reader::BitWriter;
use hayro_syntax::object::Stream;
use hayro_syntax::reader::{Reader, ReaderContext, ReaderExt};
use smallvec::smallvec;

fn parse(entries: &str, data: &[u8]) -> Option<Type0> {
    let mut bytes = format!("<< {entries} /Length {} >> stream\n", data.len()).into_bytes();
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(b"\nendstream");
    let mut reader = Reader::new(&bytes);
    let stream = reader.read_with_context::<Stream<'_>>(&ReaderContext::dummy())?;
    Type0::new(&stream)
}

fn packed(bits: u8, values: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; (usize::from(bits) * values.len()).div_ceil(8)];
    let mut writer = BitWriter::new(&mut bytes, bits).unwrap();
    for &value in values {
        writer.write(value).unwrap();
    }
    bytes
}

#[test]
fn all_sample_widths_preserve_values_padding_and_interpolation() {
    for bits in [1, 2, 4, 8, 12, 16, 24, 32] {
        let max = ((1_u64 << bits) - 1) as u32;
        let values = [0, max / 3, max];
        let bytes = packed(bits, &values);
        let entries = format!("/Domain [0 2] /Range [0 1] /Size [3] /BitsPerSample {bits}");
        let function = parse(&entries, &bytes).unwrap();
        assert_eq!(function.table, values);
        for (input, expected) in [
            (0.0, 0.0),
            (1.0, f64::from(max / 3) / f64::from(max)),
            (2.0, 1.0),
            (1.5, (f64::from(max / 3) / f64::from(max) + 1.0) / 2.0),
        ] {
            assert_eq!(
                function.eval(smallvec![input]).unwrap()[0],
                expected as f32,
                "{bits}: {input}"
            );
        }
        assert!(parse(&entries, &bytes[..bytes.len() - 1]).is_none());
        let mut trailing = bytes.clone();
        trailing.extend([255; 8]);
        assert_eq!(parse(&entries, &trailing).unwrap().table, values);
    }
}

#[test]
fn cubic_values_and_boundaries_match_independent_hermite_controls() {
    let function = parse(
        "/Domain [0 3] /Range [0 1] /Size [4] /BitsPerSample 8 /Order 3",
        &[0, 255, 0, 0],
    )
    .unwrap();
    for (input, expected) in [
        (-1.0, 0.0),
        (0.25, 0.2265625),
        (0.5, 0.5625),
        (0.75, 0.8671875),
        (1.0, 1.0),
        (1.5, 0.5625),
        (2.5, 0.0),
        (4.0, 0.0),
    ] {
        assert_eq!(
            function.eval(smallvec![input]).unwrap()[0],
            expected,
            "{input}"
        );
    }
    let unclipped = parse(
        "/Domain [0 3] /Range [-1 2] /Decode [0 1] /Size [4] /BitsPerSample 8 /Order 3",
        &[0, 255, 0, 0],
    )
    .unwrap();
    assert_eq!(unclipped.eval(smallvec![2.5]).unwrap()[0], -0.0625);
    for (size, bytes) in [(1, &[255][..]), (2, &[0, 255][..]), (3, &[0, 255, 0][..])] {
        let function = parse(
            &format!(
                "/Domain [0 2] /Range [0 1] /Encode [0 2] /Size [{size}] /BitsPerSample 8 /Order 3"
            ),
            bytes,
        )
        .unwrap();
        assert_eq!(
            function.eval(smallvec![0.5]).unwrap()[0],
            if size == 1 { 1.0 } else { 0.5 }
        );
    }
}

#[test]
fn multidimensional_tables_keep_first_dimension_fastest_and_outputs_adjacent() {
    let function = parse(
        "/Domain [0 1 0 1] /Range [0 1 0 1] /Size [2 2] /BitsPerSample 8",
        &[0, 255, 64, 128, 192, 64, 255, 0],
    )
    .unwrap();
    assert_eq!(
        function.eval(smallvec![1.0, 0.0]).unwrap().as_slice(),
        &[64.0 / 255.0, 128.0 / 255.0]
    );
    assert_eq!(
        function.eval(smallvec![0.0, 1.0]).unwrap().as_slice(),
        &[192.0 / 255.0, 64.0 / 255.0]
    );
    let result = function.eval(smallvec![0.25, 0.75]).unwrap();
    assert_eq!(
        result.as_slice(),
        &[(159.8125_f64 / 255.0) as f32, (91.8125_f64 / 255.0) as f32]
    );
    for sizes in [vec![4, 4], vec![4, 3], vec![4, 4, 4], vec![4, 1, 4]] {
        let entries = format!(
            "/Domain [{}] /Range [0 1] /Size [{}] /BitsPerSample 8 /Order 3 /Encode [{}]",
            "0 1 ".repeat(sizes.len()),
            sizes
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            "0.5 0.5 ".repeat(sizes.len())
        );
        let mut bytes = vec![0; sizes.iter().product()];
        for (offset, value) in bytes.iter_mut().enumerate() {
            let mut key = offset;
            let mut nonzero = true;
            for size in &sizes {
                nonzero &= *size == 1 || key % size == 1;
                key /= size;
            }
            if nonzero {
                *value = 255;
            }
        }
        let function = parse(&entries, &bytes).unwrap();
        let expected = sizes
            .iter()
            .map(|size| match size {
                1 => 1.0_f32,
                3 => 0.5,
                _ => 0.5625,
            })
            .product::<f32>();
        assert_eq!(
            function.eval(smallvec![0.5;sizes.len()]).unwrap()[0],
            expected
        );
    }
}

#[test]
fn mapping_uses_finite_wide_intermediates_reversal_and_final_range_clipping() {
    for (entries, input, expected) in [
        ("/Domain [-1 1] /Range [0 1] /Encode [1 0]", -0.5, 0.75),
        ("/Domain [-1 1] /Range [0 1] /Decode [1 0]", -0.5, 0.75),
        ("/Domain [0 1] /Range [0.25 0.75] /Decode [-2 2]", 0.0, 0.25),
        ("/Domain [0 1] /Range [0.25 0.75] /Decode [-2 2]", 1.0, 0.75),
        (
            "/Domain [-300000000000000000000000000000000000000 300000000000000000000000000000000000000] /Range [0 1]",
            0.0,
            0.5,
        ),
        (
            "/Domain [0 1] /Range [0 1] /Encode [-300000000000000000000000000000000000000 300000000000000000000000000000000000000]",
            0.5,
            0.0,
        ),
        (
            "/Domain [0 1] /Range [-1 1] /Decode [-300000000000000000000000000000000000000 300000000000000000000000000000000000000]",
            0.5,
            0.0,
        ),
    ] {
        let function = parse(&format!("{entries} /Size [2] /BitsPerSample 8"), &[0, 255]).unwrap();
        assert_eq!(
            function.eval(smallvec![input]).unwrap()[0],
            expected,
            "{entries}"
        );
        assert!(function.eval(smallvec![f32::NAN]).is_none());
        assert!(function.eval(smallvec![f32::INFINITY]).is_none());
        assert!(function.eval(smallvec![]).is_none());
    }
}

#[test]
fn malformed_shapes_types_and_resource_limits_fail_before_table_decoding() {
    let base = "/Domain [0 1] /Range [0 1] /Size [2] /BitsPerSample 8";
    for (old, new) in [
        ("/Domain [0 1]", "/Domain []"),
        ("/Domain [0 1]", "/Domain [0 1 0]"),
        ("/Domain [0 1]", "/Domain [0 0]"),
        ("/Domain [0 1]", "/Domain [1 0]"),
        ("/Domain [0 1]", "/Domain [0 /Bad]"),
        ("/Range [0 1]", "/Range []"),
        ("/Range [0 1]", "/Range [1 0]"),
        ("/Size [2]", "/Size [0]"),
        ("/Size [2]", "/Size [-1]"),
        ("/Size [2]", "/Size [2.0]"),
        ("/Size [2]", "/Size [2 /Bad]"),
        ("/BitsPerSample 8", "/BitsPerSample 8.0"),
        ("/BitsPerSample 8", "/BitsPerSample 3"),
        ("/Size [2]", "/Size [16777217]"),
    ] {
        assert!(parse(&base.replace(old, new), &[0, 255]).is_none(), "{new}");
    }
    for extra in [
        "/Order 2",
        "/Order 3.0",
        "/Encode []",
        "/Encode [0 1 0]",
        "/Encode /Bad",
        "/Decode [0 /Bad]",
        "/Decode [0 1 0 1]",
        "/Encode [0 9999999999999999999999999999999999999999999999]",
    ] {
        assert!(
            parse(&format!("{base} {extra}"), &[0, 255]).is_none(),
            "{extra}"
        );
    }
    for (dimensions, size, order) in [
        (MAX_COMPONENTS + 1, 1, 1),
        (17, 2, 1),
        (9, 4, 3),
        (4, usize::MAX, 1),
    ] {
        let entries = format!(
            "/Domain [{}] /Range [0 1] /Size [{}] /BitsPerSample 1 /Order {order}",
            "0 1 ".repeat(dimensions),
            format!("{size} ").repeat(dimensions)
        );
        assert!(
            parse(&entries, &[]).is_none(),
            "{dimensions}/{size}/{order}"
        );
    }
    // The inclusive work boundary remains admitted.
    let entries = format!(
        "/Domain [{}] /Range [0 1] /Size [{}] /BitsPerSample 1",
        "0 1 ".repeat(16),
        "2 ".repeat(16)
    );
    assert!(parse(&entries, &vec![0; MAX_CONTRIBUTIONS / 8]).is_some());
}
