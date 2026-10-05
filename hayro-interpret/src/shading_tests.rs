use super::read_free_form_triangles;
use kurbo::Point;

#[test]
fn free_form_meshes_keep_distinct_vertices_at_small_source_scales() {
    // Three byte-aligned vertices with an eight-bit gray component. Decode
    // endpoints make every coordinate exact at each power-of-two scale.
    let data = [0, 0, 0, 11, 0, 255, 0, 87, 0, 255, 255, 235];
    for exponent in [0, 8, 14, 32, 100] {
        let scale = 2_f32.powi(-exponent);
        for (sx, sy) in [(scale, 1.0), (1.0, scale), (scale, scale)] {
            let triangles =
                read_free_form_triangles(&data, 8, 8, 8, false, &[0.0, sx, 0.0, sy, 0.0, 1.0])
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
                &[0.0, scale, 0.0, scale, 0.0, 1.0],
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
