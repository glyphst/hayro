use super::{InterpolationHelpers, read_free_form_triangles};
use hayro_syntax::bit_reader::BitReader;
use kurbo::Point;

fn append_mesh_bits(bytes: &mut Vec<u8>, position: &mut usize, value: u32, width: u8) {
    for shift in (0..width).rev() {
        if (*position).is_multiple_of(8) {
            bytes.push(0);
        }
        let last = bytes.last_mut().unwrap();
        *last |= (((value >> shift) & 1) as u8) << (7 - *position % 8);
        *position += 1;
    }
}

fn check_shading_type_integer(kind: u8) {
    use super::{Shading, ShadingType};
    use crate::cache::Cache;
    use hayro_syntax::Pdf;
    use hayro_syntax::object::{Object, ObjectIdentifier};

    let warning: crate::interpret::WarningSinkFn =
        std::sync::Arc::new(|warning| panic!("unexpected shading warning: {warning:?}"));
    let mut declarations = vec![
        (kind.to_string(), true),
        (format!("+{kind}"), true),
        (format!("000{kind}"), true),
        (format!("{kind}.5"), false),
        (format!("{kind}.0"), false),
    ];
    declarations.extend(
        [
            "0", "8", "-1", "false", "null", "/Wrong", "(type)", "<< >>", "garbage", "nulljunk",
            "99 0 R", "256",
        ]
        .map(|value| (value.to_owned(), false)),
    );
    declarations.extend([
        (format!("[{kind}]"), false),
        ("9".repeat(400), false),
        (String::new(), false),
    ]);
    for (declaration, valid) in declarations {
        for indirect in [false, true] {
            let entry = if declaration.is_empty() {
                String::new()
            } else {
                format!(
                    "/ShadingType {}",
                    if indirect { "6 0 R" } else { &declaration }
                )
            };
            let stream = |entries: &str, data: &[u8]| {
                let mut bytes =
                    format!("<< {entries} /Length {} >>\nstream\n", data.len()).into_bytes();
                bytes.extend_from_slice(data);
                bytes.extend_from_slice(b"\nendstream");
                bytes
            };
            let shading = match kind {
                1 => format!("<< {entry} /ColorSpace /DeviceGray /Function 5 0 R >>").into_bytes(),
                2 | 3 => {
                    let coords = if kind == 2 {
                        "0 0 1 0"
                    } else {
                        ".5 .5 0 .5 .5 .5"
                    };
                    format!("<< {entry} /ColorSpace /DeviceGray /Coords [{coords}] /Function << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [1] /N 1 >> >>").into_bytes()
                }
                _ => {
                    let data = if kind == 4 {
                        vec![0, 0, 0, 255, 0, 255, 0, 255, 0, 255, 255, 255]
                    } else if kind == 5 {
                        vec![0, 0, 255, 255, 0, 255, 0, 255, 255, 255, 255, 255]
                    } else {
                        let mut data = vec![0];
                        for [x, y] in [
                            [0, 0],
                            [0, 85],
                            [0, 170],
                            [0, 255],
                            [85, 255],
                            [170, 255],
                            [255, 255],
                            [255, 170],
                            [255, 85],
                            [255, 0],
                            [170, 0],
                            [85, 0],
                            [85, 85],
                            [85, 170],
                            [170, 170],
                            [170, 85],
                        ]
                        .into_iter()
                        .take(if kind == 6 { 12 } else { 16 })
                        {
                            data.extend_from_slice(&[x, y]);
                        }
                        data.extend_from_slice(&[255; 4]);
                        data
                    };
                    let layout = if kind == 5 {
                        "/VerticesPerRow 2"
                    } else {
                        "/BitsPerFlag 8"
                    };
                    stream(
                        &format!(
                            "{entry} /ColorSpace /DeviceGray /BitsPerCoordinate 8 /BitsPerComponent 8 {layout} /Decode [0 1 0 1 0 1]"
                        ),
                        &data,
                    )
                }
            };
            // Independently authored objects and offsets keep indirect declarations
            // separate from the shading and the two-input calculator function.
            let objects = [
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>".to_vec(),
                shading,
                stream(
                    "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1]",
                    b"{ pop pop 1 }",
                ),
                if declaration.is_empty() {
                    b"null".to_vec()
                } else {
                    declaration.as_bytes().to_vec()
                },
            ];
            let mut bytes = b"%PDF-1.7\n".to_vec();
            let mut offsets = Vec::new();
            for (index, object) in objects.iter().enumerate() {
                offsets.push(bytes.len());
                bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
                bytes.extend_from_slice(object);
                bytes.extend_from_slice(b"\nendobj\n");
            }
            let xref = bytes.len();
            bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
            for (index, offset) in offsets.into_iter().enumerate() {
                assert!(bytes[offset..].starts_with(format!("{} 0 obj\n", index + 1).as_bytes()));
                bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            }
            bytes.extend_from_slice(
                format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                    .as_bytes(),
            );
            let pdf = Pdf::new(bytes).expect("literal shading declaration PDF");
            let object = pdf
                .xref()
                .get::<Object<'_>>(ObjectIdentifier::new(4, 0))
                .expect("shading object");
            let shading = match &object {
                Object::Dict(dict) => Shading::new(dict, None, &Cache::new(), &warning),
                Object::Stream(stream) => {
                    Shading::new(stream.dict(), Some(stream), &Cache::new(), &warning)
                }
                _ => panic!("literal shading object must be a dictionary or stream"),
            };
            if !valid {
                assert!(
                    shading.is_none(),
                    "Type {kind} malformed declaration {declaration:?}, indirect={indirect}"
                );
                continue;
            }
            let shading = shading.expect("legal integer shading kind");
            match (kind, shading.shading_type.as_ref()) {
                (1, ShadingType::FunctionBased { function, .. }) => {
                    assert_eq!(
                        function
                            .eval(&smallvec::smallvec![0.25, 0.75])
                            .unwrap()
                            .as_slice(),
                        &[1.0]
                    );
                }
                (
                    2 | 3,
                    ShadingType::RadialAxial {
                        axial, function, ..
                    },
                ) => {
                    assert_eq!(*axial, kind == 2);
                    assert_eq!(
                        function
                            .eval(&smallvec::smallvec![0.25])
                            .unwrap()
                            .as_slice(),
                        &[1.0]
                    );
                }
                (
                    4 | 5,
                    ShadingType::TriangleMesh {
                        shading_type,
                        triangles,
                        ..
                    },
                ) => {
                    assert_eq!(*shading_type, kind);
                    assert_eq!(triangles.len(), if kind == 4 { 1 } else { 2 });
                }
                (6, ShadingType::CoonsPatchMesh { patches, .. }) => assert_eq!(patches.len(), 1),
                (7, ShadingType::TensorProductPatchMesh { patches, .. }) => {
                    assert_eq!(patches.len(), 1)
                }
                _ => panic!("integer declaration must preserve literal shading kind {kind}"),
            }
        }
    }
}

#[test]
fn shading_type_integer_type1_constructor_rejects_real_declarations() {
    check_shading_type_integer(1);
}
#[test]
fn shading_type_integer_type2_constructor_rejects_real_declarations() {
    check_shading_type_integer(2);
}
#[test]
fn shading_type_integer_type3_constructor_rejects_real_declarations() {
    check_shading_type_integer(3);
}
#[test]
fn shading_type_integer_type4_constructor_rejects_real_declarations() {
    check_shading_type_integer(4);
}
#[test]
fn shading_type_integer_type5_constructor_rejects_real_declarations() {
    check_shading_type_integer(5);
}
#[test]
fn shading_type_integer_type6_constructor_rejects_real_declarations() {
    check_shading_type_integer(6);
}
#[test]
fn shading_type_integer_type7_constructor_rejects_real_declarations() {
    check_shading_type_integer(7);
}

#[test]
fn free_form_mesh_fields_follow_subbyte_flags_and_vertex_padding() {
    for flag_bits in [2, 4] {
        for coordinate_bits in [1, 2, 4, 8, 12, 16, 24, 32] {
            for component_bits in [1, 2, 4, 8, 12, 16] {
                let coordinate_max = u32::MAX >> (32 - coordinate_bits);
                let component_max = u32::MAX >> (32 - component_bits);
                let mut data = Vec::new();
                let mut position = 0;
                for (x, y, color) in [
                    (0, 0, 0),
                    (coordinate_max, 0, component_max),
                    (coordinate_max, coordinate_max, component_max),
                ] {
                    for (value, width) in [
                        (0, flag_bits),
                        (x, coordinate_bits),
                        (y, coordinate_bits),
                        (color, component_bits),
                    ] {
                        append_mesh_bits(&mut data, &mut position, value, width);
                    }
                    // Type 4 vertex padding carries no semantics, including ones.
                    let padding = (8 - position % 8) % 8;
                    append_mesh_bits(&mut data, &mut position, u32::MAX, padding as u8);
                }
                for function in [false, true] {
                    let triangles = read_free_form_triangles(
                        &data,
                        flag_bits,
                        coordinate_bits,
                        component_bits,
                        function,
                        &[-11.0, 51.0, -7.0, 83.0, 0.0, 1.0],
                    )
                    .expect("packed triangle");
                    assert_eq!(triangles.len(), 1);
                    let t = &triangles[0];
                    assert_eq!(t.p0.point, Point::new(-11.0, -7.0));
                    assert_eq!(t.p1.point, Point::new(51.0, -7.0));
                    assert_eq!(t.p2.point, Point::new(51.0, 83.0));
                    assert_eq!(t.p0.colors.as_slice(), &[0.0]);
                    assert_eq!(t.p1.colors.as_slice(), &[1.0]);
                    assert_eq!(t.p2.colors.as_slice(), &[1.0]);
                }
            }
        }
    }
}

#[test]
fn patch_mesh_byte_fields_survive_successive_unaligned_records() {
    for flag_bits in [2, 4] {
        for count in [12, 16] {
            let mut data = Vec::new();
            let mut position = 0;
            for patch in 0..4 {
                append_mesh_bits(&mut data, &mut position, 0, flag_bits);
                for index in 0..count {
                    append_mesh_bits(&mut data, &mut position, 17 + patch * 31 + index, 8);
                    append_mesh_bits(&mut data, &mut position, 239 - patch * 29 - index, 8);
                }
                for color in [17, 63, 129, 241] {
                    append_mesh_bits(&mut data, &mut position, color, 8);
                }
            }
            let patches = super::read_patch_mesh(
                &data,
                flag_bits,
                8,
                8,
                false,
                &[0.0, 255.0, 0.0, 255.0, 0.0, 1.0],
                count as usize,
                |points, colors| (points, colors),
            )
            .expect("packed patches");
            assert_eq!(patches.len(), 4);
            for (patch, (points, colors)) in patches.iter().enumerate() {
                for (index, point) in points.iter().enumerate().take(count as usize) {
                    assert_eq!(
                        *point,
                        Point::new(
                            (17 + patch * 31 + index) as f64,
                            (239 - patch * 29 - index) as f64,
                        ),
                    );
                }
                for (color, sample) in colors.iter().zip([17, 63, 129, 241]) {
                    assert_eq!(color.as_slice(), &[sample as f32 / 255.0]);
                }
            }
        }
    }
}

#[test]
fn analytic_shading_coordinates_retain_source_origin_offsets() {
    use super::{Shading, ShadingType};
    use crate::cache::Cache;
    use hayro_syntax::object::{Dict, FromBytes};

    let warning: crate::interpret::WarningSinkFn =
        std::sync::Arc::new(|warning| panic!("unexpected shading warning: {warning:?}"));
    for origin in [0_i64, 1 << 24, 1 << 40] {
        for kind in [2, 3] {
            let expected = if kind == 2 {
                vec![origin + 17, -origin + 49, origin + 81, -origin + 49]
            } else {
                vec![origin + 47, -origin + 49, 8, origin + 47, -origin + 49, 24]
            };
            let coordinates = expected
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            let source = format!(
                "<< /ShadingType {kind} /ColorSpace /DeviceGray /Coords [{coordinates}] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >>"
            );
            let dictionary = Dict::from_bytes(source.as_bytes()).expect("shading dictionary");
            let shading = Shading::new(&dictionary, None, &Cache::new(), &warning)
                .expect("valid translated shading");
            let ShadingType::RadialAxial { coords, .. } = shading.shading_type.as_ref() else {
                panic!("distinct source coordinates must retain geometry: {source}");
            };
            for (actual, expected) in coords.iter().zip(expected) {
                assert_eq!(*actual, expected as f64, "type {kind}, origin {origin}");
            }
        }
    }
}

#[test]
fn free_form_meshes_keep_distinct_vertices_at_small_source_scales() {
    // Three byte-aligned vertices with an eight-bit gray component. Decode
    // endpoints make every coordinate exact at each power-of-two scale.
    let data = [0, 0, 0, 11, 0, 255, 0, 87, 0, 255, 255, 235];
    for exponent in [0, 8, 14, 32, 100] {
        let scale = 2_f32.powi(-exponent);
        for (sx, sy) in [(scale, 1.0), (1.0, scale), (scale, scale)] {
            let triangles = read_free_form_triangles(
                &data,
                8,
                8,
                8,
                false,
                &[0.0, f64::from(sx), 0.0, f64::from(sy), 0.0, 1.0],
            )
            .expect("valid small mesh");
            assert_eq!(triangles.len(), 1, "source scale {sx}, {sy}");
            let triangle = &triangles[0];
            assert_eq!(triangle.p0.point, Point::new(0.0, 0.0));
            assert_eq!(triangle.p1.point, Point::new(f64::from(sx), 0.0));
            assert_eq!(triangle.p2.point, Point::new(f64::from(sx), f64::from(sy)));
        }
    }
}

#[test]
fn free_form_connections_survive_exactly_coincident_vertices() {
    for scale in [1.0, 2_f32.powi(-20)] {
        for flag in [1, 2] {
            // The first triangle is exactly empty. Its decoded points and
            // colors still seed the next triangle through either reuse flag.
            let data = match flag {
                1 => [0, 0, 0, 10, 0, 0, 0, 20, 0, 255, 0, 30, 1, 0, 255, 40],
                2 => [0, 0, 0, 10, 0, 255, 0, 20, 0, 255, 0, 30, 2, 0, 255, 40],
                _ => unreachable!(),
            };
            let triangles = read_free_form_triangles(
                &data,
                8,
                8,
                8,
                false,
                &[0.0, f64::from(scale), 0.0, f64::from(scale), 0.0, 1.0],
            )
            .expect("connected small mesh");
            assert_eq!(triangles.len(), 1, "connection {flag}, scale {scale}");
            let triangle = &triangles[0];
            assert_eq!(triangle.p0.point, Point::new(0.0, 0.0));
            assert_eq!(triangle.p1.point, Point::new(f64::from(scale), 0.0));
            assert_eq!(triangle.p2.point, Point::new(0.0, f64::from(scale)));
            let first = if flag == 1 { 20_u8 } else { 10_u8 };
            assert_eq!(triangle.p0.colors.as_slice(), &[f32::from(first) / 255.0]);
            assert_eq!(triangle.p1.colors.as_slice(), &[30.0 / 255.0]);
            assert_eq!(triangle.p2.colors.as_slice(), &[40.0 / 255.0]);
        }
    }
}

#[test]
fn mesh_coordinate_decode_retains_all_32_encoded_bits() {
    // Each authored Decode interval spans exactly 2^32 - 1. Its expected
    // coordinate is therefore an integer translation of the encoded word.
    // Decimal literals keep the test input fixed when Decode storage widens.
    for (lo, hi, base) in [
        (0.0, 4_294_967_295.0, 0_u32),
        (-16_777_216.0, 4_278_190_079.0, 16_777_216),
        (-268_435_456.0, 4_026_531_839.0, 268_435_456),
        (-1_073_741_824.0, 3_221_225_471.0, 1_073_741_824),
        (-2_147_483_648.0, 2_147_483_647.0, 2_147_483_648),
    ] {
        for reverse in [false, true] {
            let (a, b) = if reverse { (hi, lo) } else { (lo, hi) };
            let helpers = InterpolationHelpers::new(32, 8, a, b, a, b);
            for coordinate in [0_u32, 1, 16, 32, 64, 80, 127, 255] {
                let encoded = base + coordinate;
                let encoded = if reverse { u32::MAX - encoded } else { encoded };
                let bytes = encoded.to_be_bytes().repeat(2);
                let point = helpers
                    .read_point(&mut BitReader::new(&bytes))
                    .expect("complete 32-bit point");
                assert_eq!(
                    point,
                    Point::new(f64::from(coordinate), f64::from(coordinate)),
                    "base {base}, reverse {reverse}, coordinate {coordinate}",
                );
            }
        }
    }
}

#[test]
fn mesh_coordinate_decode_preserves_large_origin_offsets() {
    // Eight-bit samples and a Decode span of 255 independently map to an
    // exact integer offset, including source origins above binary32 precision.
    for (lo, hi, origin) in [
        (0.0, 255.0, 0_u64),
        (16_777_216.0, 16_777_471.0, 16_777_216),
        (268_435_456.0, 268_435_711.0, 268_435_456),
        (1_099_511_627_776.0, 1_099_511_628_031.0, 1_099_511_627_776),
    ] {
        for reverse in [false, true] {
            let (a, b) = if reverse { (hi, lo) } else { (lo, hi) };
            let helpers = InterpolationHelpers::new(8, 8, a, b, a, b);
            for coordinate in [0_u32, 1, 16, 32, 64, 80, 127, 255] {
                let encoded = if reverse {
                    255 - coordinate
                } else {
                    coordinate
                };
                let bytes = [encoded as u8; 2];
                let point = helpers
                    .read_point(&mut BitReader::new(&bytes))
                    .expect("complete eight-bit point");
                let expected = (origin + u64::from(coordinate)) as f64;
                assert_eq!(
                    point,
                    Point::new(expected, expected),
                    "origin {origin}, reverse {reverse}, coordinate {coordinate}",
                );
            }
        }
    }
}

#[test]
fn mesh_coordinate_decode_retains_declared_endpoints() {
    for bits in [1, 2, 4, 8, 12, 16, 24, 32] {
        let maximum = u32::MAX >> (32 - bits);
        for (lo, hi) in [(-1_073_741_824.0, 16.0), (-0.7, 0.3)] {
            for (start, end) in [(lo, hi), (hi, lo)] {
                let helpers = InterpolationHelpers::new(bits, 8, start, end, start, end);
                assert_eq!(helpers.interpolate_coord(0, start, end), start);
                assert_eq!(
                    helpers.interpolate_coord(maximum, start, end),
                    end,
                    "{bits}-bit Decode [{start}, {end}] endpoint",
                );
            }
        }
    }
}

fn mesh_flag_records(records: &[(u32, u32, u32, u32)], flag_bits: u8, high_bits: u32) -> Vec<u8> {
    let mut data = Vec::new();
    let mut position = 0;
    for &(flag, x, y, color) in records {
        for (value, width) in [(flag | high_bits, flag_bits), (x, 8), (y, 8), (color, 8)] {
            append_mesh_bits(&mut data, &mut position, value, width);
        }
        let padding = (8 - position % 8) % 8;
        append_mesh_bits(&mut data, &mut position, u32::MAX, padding as u8);
    }
    data
}

#[test]
fn free_form_connections_ignore_high_flag_bits() {
    // Literal decoded triangles independently cover initial/reset flag 0 and
    // both connection flags. Every possible ignored prefix is exercised.
    let records = [
        (0, 16, 16, 17),
        (0, 80, 16, 63),
        (0, 80, 80, 129),
        (1, 16, 80, 241),
        (2, 16, 16, 17),
        (0, 96, 16, 37),
        (0, 144, 16, 127),
        (0, 144, 80, 223),
    ];
    let expected = [
        [(16, 16, 17), (80, 16, 63), (80, 80, 129)],
        [(80, 16, 63), (80, 80, 129), (16, 80, 241)],
        [(80, 16, 63), (16, 80, 241), (16, 16, 17)],
        [(96, 16, 37), (144, 16, 127), (144, 80, 223)],
    ];
    for flag_bits in [2, 4, 8] {
        for high_bits in (0..(1_u32 << flag_bits)).step_by(4) {
            let data = mesh_flag_records(&records, flag_bits, high_bits);
            for has_function in [false, true] {
                let triangles = read_free_form_triangles(
                    &data,
                    flag_bits,
                    8,
                    8,
                    has_function,
                    &[0.0, 255.0, 0.0, 255.0, 0.0, 1.0],
                )
                .expect("connected mesh with ignored flag bits");
                assert_eq!(
                    triangles.len(),
                    4,
                    "flag bits {flag_bits}, prefix {high_bits}"
                );
                for (triangle, vertices) in triangles.iter().zip(expected) {
                    for (actual, (x, y, color)) in [&triangle.p0, &triangle.p1, &triangle.p2]
                        .into_iter()
                        .zip(vertices)
                    {
                        assert_eq!(actual.point, Point::new(f64::from(x), f64::from(y)));
                        assert_eq!(actual.colors.as_slice(), &[color as f32 / 255.0]);
                    }
                }
            }
        }
    }
}

#[test]
fn free_form_high_flag_bits_keep_coincident_connection_state() {
    for flag_bits in [2, 4, 8] {
        for high_bits in (0..(1_u32 << flag_bits)).step_by(4) {
            for connection in [1, 2] {
                let second_x = if connection == 1 { 0 } else { 255 };
                let records = [
                    (0, 0, 0, 10),
                    (0, second_x, 0, 20),
                    (0, 255, 0, 30),
                    (connection, 0, 255, 40),
                ];
                let data = mesh_flag_records(&records, flag_bits, high_bits);
                for has_function in [false, true] {
                    let triangles = read_free_form_triangles(
                        &data,
                        flag_bits,
                        8,
                        8,
                        has_function,
                        &[0.0, 255.0, 0.0, 255.0, 0.0, 1.0],
                    )
                    .expect("connected mesh seeded by an empty triangle");
                    assert_eq!(triangles.len(), 1);
                    let triangle = &triangles[0];
                    assert_eq!(triangle.p0.point, Point::new(0.0, 0.0));
                    assert_eq!(triangle.p1.point, Point::new(255.0, 0.0));
                    assert_eq!(triangle.p2.point, Point::new(0.0, 255.0));
                    let first_color = if connection == 1 { 20.0 } else { 10.0 };
                    assert_eq!(triangle.p0.colors.as_slice(), &[first_color / 255.0]);
                    assert_eq!(triangle.p1.colors.as_slice(), &[30.0 / 255.0]);
                    assert_eq!(triangle.p2.colors.as_slice(), &[40.0 / 255.0]);
                }
            }
        }
    }
}

#[test]
fn lattice_mesh_fields_and_row_geometry_follow_byte_padded_records() {
    for coordinate_bits in [1, 2, 4, 8, 12, 16, 24, 32] {
        for component_bits in [1, 2, 4, 8, 12, 16] {
            for components in [1, 3, 4] {
                for function in [false, true] {
                    let samples = if function { 1 } else { components };
                    let coordinate_max = u32::MAX >> (32 - coordinate_bits);
                    let component_max = u32::MAX >> (32 - component_bits);
                    let mut data = Vec::new();
                    let mut position = 0;
                    for (index, (x, y)) in [
                        (0, 0),
                        (coordinate_max, 0),
                        (0, coordinate_max),
                        (coordinate_max, coordinate_max),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        append_mesh_bits(&mut data, &mut position, x, coordinate_bits);
                        append_mesh_bits(&mut data, &mut position, y, coordinate_bits);
                        for sample in 0..samples {
                            append_mesh_bits(
                                &mut data,
                                &mut position,
                                if (index + sample).is_multiple_of(2) {
                                    0
                                } else {
                                    component_max
                                },
                                component_bits,
                            );
                        }
                        let padding = (8 - position % 8) % 8;
                        append_mesh_bits(&mut data, &mut position, u32::MAX, padding as u8);
                    }
                    let mut decode = vec![-11.0, 51.0, -7.0, 83.0];
                    for _ in 0..samples {
                        decode.extend([0.0, 1.0]);
                    }
                    let triangles = super::read_lattice_triangles(
                        &data,
                        coordinate_bits,
                        component_bits,
                        function,
                        2,
                        &decode,
                    )
                    .expect("complete packed lattice");
                    assert_eq!(triangles.len(), 2);
                    let points = [
                        Point::new(-11.0, -7.0),
                        Point::new(51.0, -7.0),
                        Point::new(-11.0, 83.0),
                        Point::new(51.0, 83.0),
                    ];
                    // PDF 1.7 page 321 defines these two cell triplets. Vertex
                    // order may rotate or reverse without changing a triangle.
                    for (triangle, indices) in triangles.iter().zip([[0, 1, 2], [1, 2, 3]]) {
                        let vertices = [&triangle.p0, &triangle.p1, &triangle.p2];
                        for index in indices {
                            let vertex = vertices
                                .iter()
                                .find(|vertex| vertex.point == points[index])
                                .expect("specified cell vertex");
                            let colors: Vec<_> = (0..samples)
                                .map(|sample| ((index + sample) % 2) as f32)
                                .collect();
                            assert_eq!(vertex.colors.as_slice(), colors.as_slice());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn lattice_mesh_row_width_guards_precede_reading() {
    let decode = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    for columns in [0, 1] {
        assert!(super::read_lattice_triangles(&[], 8, 8, false, columns, &decode).is_none());
        assert!(
            super::read_lattice_triangles(&[0, 0, 0, 255, 0, 255], 8, 8, false, columns, &decode)
                .is_none()
        );
    }
    for [coord_bits, comp_bits] in [[0, 0], [0, 8], [8, 0], [3, 8], [8, 3], [255, 255]] {
        assert!(
            super::read_lattice_triangles(&[], coord_bits, comp_bits, false, 2, &decode).is_none()
        );
    }
    for data in [&[][..], &[0, 0, 0, 255, 0, 255][..]] {
        assert!(
            super::read_lattice_triangles(data, 8, 8, false, 2, &decode)
                .expect("empty or single complete row stays legal")
                .is_empty()
        );
    }
}

#[test]
fn lattice_mesh_constructor_requires_actual_integer_layout_entries() {
    use super::{Shading, ShadingType};
    use crate::cache::Cache;
    use hayro_syntax::object::{FromBytes, Stream};

    let warning: crate::interpret::WarningSinkFn =
        std::sync::Arc::new(|warning| panic!("unexpected shading warning: {warning:?}"));
    let data = [0, 0, 0, 255, 0, 255, 0, 255, 0, 255, 255, 255];
    for layout in [
        ["2", "8", "8"],
        ["+2", "+8", "+8"],
        ["0002", "008", "008"],
        ["2.5", "8", "8"],
        ["2.0", "8", "8"],
        ["2", "8.5", "8"],
        ["2", "8.0", "8"],
        ["2", "8", "8.5"],
        ["2", "8", "8.0"],
        ["2", "0", "0"],
        ["2", "3", "8"],
        ["2", "8", "3"],
        ["0", "8", "8"],
        ["1", "8", "8"],
    ] {
        let [columns, coord_bits, comp_bits] = layout;
        let valid = matches!(columns, "2" | "+2" | "0002");
        let valid = valid
            && matches!(coord_bits, "8" | "+8" | "008")
            && matches!(comp_bits, "8" | "+8" | "008");
        let mut bytes = format!(
            "<< /Length {} /ShadingType 5 /ColorSpace /DeviceGray /BitsPerCoordinate {coord_bits} /BitsPerComponent {comp_bits} /VerticesPerRow {columns} /Decode [0 1 0 1 0 1] >>\nstream\n",
            data.len(),
        )
        .into_bytes();
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(b"\nendstream");
        let stream = Stream::from_bytes(&bytes).expect("independently authored lattice stream");
        let shading = Shading::new(stream.dict(), Some(&stream), &Cache::new(), &warning);
        if valid {
            let shading = shading.expect("valid integer declarations");
            let ShadingType::TriangleMesh { triangles, .. } = shading.shading_type.as_ref() else {
                panic!("expected lattice triangles");
            };
            assert_eq!(triangles.len(), 2);
        } else {
            assert!(shading.is_none(), "malformed layout {layout:?}");
        }
    }
}

#[test]
fn free_form_mesh_constructor_requires_actual_integer_layout_entries() {
    use super::{Shading, ShadingType};
    use crate::cache::Cache;
    use hayro_syntax::object::{FromBytes, Stream};

    let warning: crate::interpret::WarningSinkFn =
        std::sync::Arc::new(|warning| panic!("unexpected shading warning: {warning:?}"));
    // Three independently packed Gray vertices with nonzero ignored padding.
    let data = [0, 0, 0, 63, 63, 192, 63, 255, 63, 255, 255, 255];
    for layout in [
        ["2", "8", "8"],
        ["+2", "+8", "+8"],
        ["0002", "008", "008"],
        ["2", "8.5", "8"],
        ["2", "8.0", "8"],
        ["2", "8", "8.5"],
        ["2", "8", "8.0"],
        ["2.5", "8", "8"],
        ["2.0", "8", "8"],
        ["1", "8", "8"],
        ["2", "3", "8"],
        ["2", "8", "3"],
        ["0", "0", "0"],
    ] {
        let [flag_bits, coord_bits, comp_bits] = layout;
        let valid = matches!(flag_bits, "2" | "+2" | "0002")
            && matches!(coord_bits, "8" | "+8" | "008")
            && matches!(comp_bits, "8" | "+8" | "008");
        let mut bytes = format!(
            "<< /Length {} /ShadingType 4 /ColorSpace /DeviceGray /BitsPerCoordinate {coord_bits} /BitsPerComponent {comp_bits} /BitsPerFlag {flag_bits} /Decode [0 1 0 1 0 1] >>\nstream\n",
            data.len(),
        ).into_bytes();
        bytes.extend_from_slice(&data);
        bytes.extend_from_slice(b"\nendstream");
        let stream = Stream::from_bytes(&bytes).expect("independently authored triangle stream");
        let shading = Shading::new(stream.dict(), Some(&stream), &Cache::new(), &warning);
        if valid {
            let shading = shading.expect("valid integer declarations");
            let ShadingType::TriangleMesh { triangles, .. } = shading.shading_type.as_ref() else {
                panic!("expected free-form triangle");
            };
            assert_eq!(triangles.len(), 1);
            let triangle = &triangles[0];
            for (vertex, point, color) in [
                (&triangle.p0, Point::new(0.0, 0.0), 0.0_f32),
                (&triangle.p1, Point::new(1.0, 0.0), 1.0_f32),
                (&triangle.p2, Point::new(1.0, 1.0), 1.0_f32),
            ] {
                assert_eq!(vertex.point, point);
                assert_eq!(vertex.colors.as_slice(), &[color]);
            }
        } else {
            assert!(shading.is_none(), "malformed layout {layout:?}");
        }
    }
}

#[test]
fn free_form_mesh_layout_guards_precede_interpolation_and_reading() {
    let decode = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    // Even empty data must not turn illegal metadata into a valid empty mesh.
    // The old reader already rejects zero-sized fields; no non-progress claim.
    for widths in [
        [1, 8, 8],
        [0, 8, 8],
        [3, 8, 8],
        [2, 0, 8],
        [2, 3, 8],
        [2, 8, 0],
        [2, 8, 3],
        [0, 0, 0],
    ] {
        let [flag_bits, coord_bits, comp_bits] = widths;
        assert!(
            read_free_form_triangles(&[], flag_bits, coord_bits, comp_bits, false, &decode)
                .is_none(),
            "illegal layout {widths:?}"
        );
    }
    for flag_bits in [2, 4, 8] {
        for coord_bits in [1, 2, 4, 8, 12, 16, 24, 32] {
            for comp_bits in [1, 2, 4, 8, 12, 16] {
                assert!(
                    read_free_form_triangles(&[], flag_bits, coord_bits, comp_bits, false, &decode)
                        .expect("legal empty stream")
                        .is_empty()
                );
            }
        }
    }
}

fn check_patch_constructor_layout(kind: u8, data: &[u8]) {
    use super::{Shading, ShadingType};
    use crate::cache::Cache;
    use hayro_syntax::object::{FromBytes, Stream};

    let warning: crate::interpret::WarningSinkFn =
        std::sync::Arc::new(|warning| panic!("unexpected shading warning: {warning:?}"));
    let expected = [
        Point::new(0.0, 0.0),
        Point::new(0.0, 85.0),
        Point::new(0.0, 170.0),
        Point::new(0.0, 255.0),
        Point::new(85.0, 255.0),
        Point::new(170.0, 255.0),
        Point::new(255.0, 255.0),
        Point::new(255.0, 170.0),
        Point::new(255.0, 85.0),
        Point::new(255.0, 0.0),
        Point::new(170.0, 0.0),
        Point::new(85.0, 0.0),
        Point::new(85.0, 85.0),
        Point::new(85.0, 170.0),
        Point::new(170.0, 170.0),
        Point::new(170.0, 85.0),
    ];
    for layout in [
        ["2", "8", "8"],
        ["+2", "+8", "+8"],
        ["0002", "008", "008"],
        ["2", "8.5", "8"],
        ["2", "8.0", "8"],
        ["2", "8", "8.5"],
        ["2", "8", "8.0"],
        ["2.5", "8", "8"],
        ["2.0", "8", "8"],
        ["1", "8", "8"],
        ["2", "3", "8"],
        ["2", "8", "3"],
        ["0", "0", "0"],
    ] {
        let [flag_bits, coord_bits, comp_bits] = layout;
        let valid = matches!(flag_bits, "2" | "+2" | "0002")
            && matches!(coord_bits, "8" | "+8" | "008")
            && matches!(comp_bits, "8" | "+8" | "008");
        let mut bytes = format!(
            "<< /Length {} /ShadingType {kind} /ColorSpace /DeviceGray /BitsPerCoordinate {coord_bits} /BitsPerComponent {comp_bits} /BitsPerFlag {flag_bits} /Decode [0 255 0 255 0 1] >>\nstream\n",
            data.len(),
        ).into_bytes();
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(b"\nendstream");
        let stream = Stream::from_bytes(&bytes).expect("independently authored patch stream");
        let shading = Shading::new(stream.dict(), Some(&stream), &Cache::new(), &warning);
        if valid {
            let shading = shading.expect("valid integer patch declarations");
            let (points, colors) = match shading.shading_type.as_ref() {
                ShadingType::CoonsPatchMesh { patches, .. } if kind == 6 => {
                    assert_eq!(patches.len(), 1);
                    (&patches[0].control_points[..], &patches[0].colors)
                }
                ShadingType::TensorProductPatchMesh { patches, .. } if kind == 7 => {
                    assert_eq!(patches.len(), 1);
                    (&patches[0].control_points[..], &patches[0].colors)
                }
                _ => panic!("expected Type {kind} patch"),
            };
            assert_eq!(points, &expected[..if kind == 6 { 12 } else { 16 }]);
            for (color, sample) in colors.iter().zip([0.0_f32, 1.0, 1.0, 0.0]) {
                assert_eq!(color.as_slice(), &[sample]);
            }
        } else {
            assert!(shading.is_none(), "Type {kind} malformed layout {layout:?}");
        }
    }
}

#[test]
fn coons_patch_constructor_requires_actual_integer_layout_entries() {
    // Literal two-bit flag, twelve eight-bit point pairs and four Gray samples.
    // The six final padding bits are ones and carry no semantics.
    let data = [
        0, 0, 0, 21, 64, 42, 128, 63, 213, 127, 234, 191, 255, 255, 255, 234, 191, 213, 127, 192,
        42, 128, 21, 64, 0, 63, 255, 192, 63,
    ];
    check_patch_constructor_layout(6, &data);
}

#[test]
fn tensor_patch_constructor_requires_actual_integer_layout_entries() {
    // A separate literal record includes the four interior point pairs.
    let data = [
        0, 0, 0, 21, 64, 42, 128, 63, 213, 127, 234, 191, 255, 255, 255, 234, 191, 213, 127, 192,
        42, 128, 21, 64, 21, 85, 85, 106, 170, 170, 170, 149, 64, 63, 255, 192, 63,
    ];
    check_patch_constructor_layout(7, &data);
}

#[test]
fn patch_mesh_layout_guards_precede_interpolation_and_reading() {
    let decode = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    for count in [12, 16] {
        for function in [false, true] {
            for [flag_bits, coord_bits, comp_bits] in [
                [1, 8, 8],
                [0, 8, 8],
                [3, 8, 8],
                [2, 0, 8],
                [2, 3, 8],
                [2, 8, 0],
                [2, 8, 3],
                [0, 0, 0],
            ] {
                assert!(
                    super::read_patch_mesh(
                        &[],
                        flag_bits,
                        coord_bits,
                        comp_bits,
                        function,
                        &decode,
                        count,
                        |points, colors| (points, colors),
                    )
                    .is_none(),
                    "illegal patch layout {count}/{function}/{flag_bits}/{coord_bits}/{comp_bits}"
                );
            }
            for flag_bits in [2, 4, 8] {
                for coord_bits in [1, 2, 4, 8, 12, 16, 24, 32] {
                    for comp_bits in [1, 2, 4, 8, 12, 16] {
                        assert!(
                            super::read_patch_mesh(
                                &[],
                                flag_bits,
                                coord_bits,
                                comp_bits,
                                function,
                                &decode,
                                count,
                                |points, colors| (points, colors),
                            )
                            .expect("legal empty patch stream")
                            .is_empty()
                        );
                    }
                }
            }
        }
    }
}
