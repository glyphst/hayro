use super::{InterpolationHelpers, read_free_form_triangles};
use hayro_syntax::bit_reader::BitReader;
use kurbo::Point;

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
