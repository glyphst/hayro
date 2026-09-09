use super::*;

fn sample(bitmap: &Bitmap, x: i64, y: i64) -> u16 {
    if x < 0 || y < 0 || x >= i64::from(bitmap.width) || y >= i64::from(bitmap.height) {
        0
    } else {
        u16::from(bitmap.get_pixel(x as u32, y as u32))
    }
}

// Scalar sampling of T.88 Figures 12/13, independently of word buffers and
// shift masks. Context bit order follows the figures in reading order, with
// the adaptive pixel retaining its assigned bit when its coordinates move.
#[test]
fn shifted_contexts_and_typical_prediction_match_scalar_sampling() {
    let default = [AdaptiveTemplatePixel { x: -1, y: -1 }; 2];
    let custom = [
        AdaptiveTemplatePixel { x: -2, y: 0 },
        AdaptiveTemplatePixel { x: 2, y: 1 },
    ];
    let extreme_at = [
        AdaptiveTemplatePixel { x: -128, y: -128 },
        AdaptiveTemplatePixel { x: 127, y: 127 },
    ];
    let mut reference = Bitmap::new(97, 9).unwrap();
    for y in 0..reference.height {
        for x in 0..reference.width {
            reference.set_pixel(x, y, u8::from((x * 7 + y * 3 + x * y) % 5 != 0));
        }
    }
    for (template, at) in [
        (RefinementTemplate::Template0, default),
        (RefinementTemplate::Template0, custom),
        (RefinementTemplate::Template0, extreme_at),
        (RefinementTemplate::Template1, default),
    ] {
        for dx in [
            i32::MIN,
            -97,
            -64,
            -33,
            -32,
            -31,
            -1,
            0,
            1,
            31,
            32,
            33,
            64,
            97,
            i32::MAX,
        ] {
            for dy in [i32::MIN, -10, -1, 0, 1, 10, i32::MAX] {
                let mut region = Bitmap::new(67, 7).unwrap();
                let mut gatherer = RefinementContextGatherer::new(template, &at, dx, dy);
                for y in 0..region.height {
                    gatherer.start_row(&region, &reference, y);
                    for x in 0..region.width {
                        gatherer.maybe_reload_buffers(&region, &reference, x);
                        let actual = match template {
                            RefinementTemplate::Template0 if gatherer.use_default_at => {
                                gatherer.gather_template0_default(&region, &reference, x)
                            }
                            RefinementTemplate::Template0 => {
                                gatherer.gather_template0_custom(&region, &reference, x)
                            }
                            RefinementTemplate::Template1 => {
                                gatherer.gather_template1(&region, &reference, x)
                            }
                        };
                        let (xi, yi) = (i64::from(x), i64::from(y));
                        let (rx, ry) = (xi - i64::from(dx), yi - i64::from(dy));
                        let mut expected = 0;
                        let (ax, ay) = if template == RefinementTemplate::Template0 {
                            (i64::from(at[0].x), i64::from(at[0].y))
                        } else {
                            (-1, -1)
                        };
                        for (ox, oy) in [(ax, ay), (0, -1), (1, -1), (-1, 0)] {
                            expected = (expected << 1) | sample(&region, xi + ox, yi + oy);
                        }
                        let t0 = [
                            (i64::from(at[1].x), i64::from(at[1].y)),
                            (0, -1),
                            (1, -1),
                            (-1, 0),
                            (0, 0),
                            (1, 0),
                            (-1, 1),
                            (0, 1),
                            (1, 1),
                        ];
                        let t1 = [(0, -1), (-1, 0), (0, 0), (1, 0), (0, 1), (1, 1)];
                        let points: &[(i64, i64)] = if template == RefinementTemplate::Template0 {
                            &t0
                        } else {
                            &t1
                        };
                        for &(ox, oy) in points {
                            expected = (expected << 1) | sample(&reference, rx + ox, ry + oy);
                        }
                        assert_eq!(
                            actual, expected,
                            "{template:?} {at:?} offset {dx},{dy} pixel {x},{y}"
                        );
                        let center = sample(&reference, rx, ry);
                        let uniform = (-1..=1).all(|oy| {
                            (-1..=1).all(|ox| sample(&reference, rx + ox, ry + oy) == center)
                        });
                        assert_eq!(gatherer.tpgr_all_same(x), uniform);
                        assert_eq!(u16::from(gatherer.ref_center_pixel(x)), center);
                        let value = u8::from((x * 3 + y * 5 + x * y) % 7 < 3);
                        region.set_pixel(x, y, value);
                        gatherer.update_current_row(x, value);
                    }
                }
            }
        }
    }
}
