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
