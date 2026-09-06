// Licensed to the Apache Software Foundation (ASF) under one or more
// contributor license agreements. See NOTICE.md for attribution.
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Adapted from Apache PDFBox 3.0.8 CloudyBorder.java. This Rust version emits
// bounded Kurbo paths, handles only rectangles/ellipses, and rejects geometry
// that cannot meet its subdivision tolerance or the host's resource budget.

use super::{Error, PathBuilder, ellipse};
use kurbo::{CubicBez, ParamCurve, PathEl, Point, Rect};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

const A34: f64 = 34.0 * PI / 180.0;
const A30: f64 = PI / 6.0;
const A12: f64 = PI / 15.0;

pub(super) fn draw(
    out: &mut PathBuilder<'_>,
    rect: Rect,
    is_ellipse: bool,
    intensity: f64,
    width: f64,
) -> Result<(), Error> {
    let radius = if is_ellipse { 4.75 } else { 4.0 } * intensity + 0.5 * width;
    if is_ellipse {
        cloudy_ellipse(out, rect, radius, width)?;
    } else {
        rectangle(out, rect, radius)?;
    }
    if !out.elements.is_empty() {
        out.push(PathEl::ClosePath)?;
    }
    Ok(())
}

fn count(out: &PathBuilder<'_>, value: f64) -> Result<usize, Error> {
    // Every curl emits at least three cubics. Check before float-to-integer
    // conversion, allocation, or a potentially unbounded repetition loop.
    if !value.is_finite() || value.max(0.0) > (out.budget.verbs / 3) as f64 {
        return Err(Error::PathLimit);
    }
    Ok(value.max(0.0) as usize)
}

fn rectangle(out: &mut PathBuilder<'_>, rect: Rect, radius: f64) -> Result<(), Error> {
    let radius = radius.max(0.5);
    let origin = Point::new(rect.x0, rect.y0);
    let mut vertices = [origin; 5];
    let len = if rect.width() < 1.0 {
        vertices[1] = Point::new(rect.x0, rect.y1);
        3
    } else if rect.height() < 1.0 {
        vertices[1] = Point::new(rect.x1, rect.y0);
        3
    } else {
        vertices[1] = Point::new(rect.x1, rect.y0);
        vertices[2] = Point::new(rect.x1, rect.y1);
        vertices[3] = Point::new(rect.x0, rect.y1);
        5
    };
    let mut polygon = [origin; 5];
    let mut kept = 0;
    for (i, p) in vertices[..len].iter().enumerate() {
        if i == 0
            || (p.x - vertices[i - 1].x).abs() >= 0.5
            || (p.y - vertices[i - 1].y).abs() >= 0.5
        {
            polygon[kept] = *p;
            kept += 1;
        }
    }
    let polygon = &polygon[..kept];
    if polygon.len() < 2 {
        return Ok(());
    }
    let k = A34.cos();
    let advance = 2.0 * k * radius;
    let previous = polygon[polygon.len() - 2];
    let (n, initial_alpha, _) = polygon_params(out, radius, previous.distance(polygon[0]))?;
    let mut alpha_prev = if n == 0 { initial_alpha } else { A34 };
    let mut angle_prev = (polygon[0].y - previous.y).atan2(polygon[0].x - previous.x);
    for edge in polygon.windows(2) {
        out.checkpoint()?;
        let delta = edge[1] - edge[0];
        let length = delta.hypot();
        if length == 0.0 {
            alpha_prev = A34;
            continue;
        }
        let (n, alpha, dx) = polygon_params(out, radius, length)?;
        let angle = delta.y.atan2(delta.x);
        corner(out, angle_prev, angle, radius, edge[0], alpha, alpha_prev)?;
        let tangent = delta / length;
        let mut center = edge[0] + tangent * (advance + 2.0 * dx);
        if n >= 1 {
            let a = angle + PI;
            segment(out, a + alpha, a + alpha - A30, center, radius)?;
            segment(out, a + alpha - A30, a + FRAC_PI_2, center, radius)?;
            segment(out, a + FRAC_PI_2, a + PI - A34, center, radius)?;
            center += tangent * advance;
        }
        for _ in 1..n {
            let a = angle + PI;
            segment(out, a + A34, a + A12, center, radius)?;
            segment(out, a + A12, a + FRAC_PI_2, center, radius)?;
            segment(out, a + FRAC_PI_2, a + PI - A34, center, radius)?;
            center += tangent * advance;
        }
        angle_prev = angle;
        alpha_prev = if n == 0 { alpha } else { A34 };
    }
    Ok(())
}

fn polygon_params(
    out: &PathBuilder<'_>,
    radius: f64,
    length: f64,
) -> Result<(usize, f64, f64), Error> {
    let advance = 2.0 * A34.cos() * radius;
    let n = count(out, ((length - advance) / advance).ceil())?;
    let dx = (length - (advance + n as f64 * advance)) * 0.5;
    let arg = (A34.cos() * radius + dx) / radius;
    let alpha = if (-1.0..=1.0).contains(&arg) {
        arg.acos()
    } else {
        0.0
    };
    Ok((n, alpha, dx))
}

fn corner(
    out: &mut PathBuilder<'_>,
    angle_prev: f64,
    angle: f64,
    radius: f64,
    center: Point,
    alpha: f64,
    alpha_prev: f64,
) -> Result<(), Error> {
    let a = angle_prev + PI + alpha_prev;
    let b = a - 22.0_f64.to_radians();
    if out.elements.is_empty() {
        out.push(PathEl::MoveTo(
            center + kurbo::Vec2::new(radius * a.cos(), radius * a.sin()),
        ))?;
    }
    segment(out, a, b, center, radius)?;
    let mut sweep = (angle - alpha - b).rem_euclid(TAU);
    let mut start = b;
    while sweep > FRAC_PI_2 {
        segment(out, start, start + FRAC_PI_2, center, radius)?;
        start += FRAC_PI_2;
        sweep -= FRAC_PI_2;
    }
    if sweep > 0.0 {
        segment(out, start, start + sweep, center, radius)?;
    }
    Ok(())
}

fn segment(
    out: &mut PathBuilder<'_>,
    start: f64,
    end: f64,
    center: Point,
    radius: f64,
) -> Result<(), Error> {
    let denom = ((end - start) * 0.5).sin();
    if denom == 0.0 {
        return Ok(());
    }
    let bcp = 1.333333333 * (1.0 - ((end - start) * 0.5).cos()) / denom;
    let a = center
        + kurbo::Vec2::new(
            radius * (start.cos() - bcp * start.sin()),
            radius * (start.sin() + bcp * start.cos()),
        );
    let b = center
        + kurbo::Vec2::new(
            radius * (end.cos() + bcp * end.sin()),
            radius * (end.sin() - bcp * end.cos()),
        );
    let c = center + kurbo::Vec2::new(radius * end.cos(), radius * end.sin());
    out.push(PathEl::CurveTo(a, b, c))
}

fn cloudy_ellipse(
    out: &mut PathBuilder<'_>,
    original: Rect,
    mut radius: f64,
    width: f64,
) -> Result<(), Error> {
    let w = original.width();
    let h = original.height();
    if w < 0.5 * radius && h < 0.5 * radius {
        return ellipse(out, original);
    }
    if (w < 5.0 && h > 20.0) || (w > 20.0 && h < 5.0) {
        return rectangle(out, original, radius);
    }
    let adjustment = A12.sin() * radius - 1.5;
    let center = original.center();
    let rx = if w > 2.0 * adjustment {
        w * 0.5 - adjustment
    } else {
        0.1
    };
    let ry = if h > 2.0 * adjustment {
        h * 0.5 - adjustment
    } else {
        0.1
    };
    let rect = Rect::new(center.x - rx, center.y - ry, center.x + rx, center.y + ry);
    let polygon = flatten_ellipse(out, rect)?;
    let total: f64 = polygon.windows(2).map(|p| p[0].distance(p[1])).sum();
    let k = A34.cos();
    let n = count(out, (total / (2.0 * k * radius)).ceil())?;
    if n < 2 {
        return ellipse(out, original);
    }
    let mut advance = total / n as f64;
    radius = advance / (2.0 * k);
    if radius < 0.5 {
        radius = 0.5;
        advance = 2.0 * k * radius;
    } else if radius < 3.0 {
        return ellipse(out, original);
    }
    // Pre-reserve the complete bounded centre array. Checkpoints and a hard
    // iteration count prevent a floating-point advance from stalling a loop.
    let mut centers = Vec::new();
    out.budget.reserve(&mut centers, n)?;
    let tolerance = width * 0.1;
    let mut remain = 0.0;
    for (i, edge) in polygon.windows(2).enumerate() {
        out.checkpoint()?;
        let length = edge[0].distance(edge[1]);
        if length == 0.0 {
            continue;
        }
        let mut todo = length + remain;
        if todo >= advance - tolerance || i == polygon.len() - 2 {
            let tangent = (edge[1] - edge[0]) / length;
            let mut distance = advance - remain;
            let mut iterations = 0;
            loop {
                out.checkpoint()?;
                if centers.len() < n {
                    centers.push(edge[0] + tangent * distance);
                }
                todo -= advance;
                distance += advance;
                iterations += 1;
                if todo < advance - tolerance {
                    break;
                }
                if iterations > n + 1 {
                    return Err(Error::InvalidGeometry);
                }
            }
            remain = todo.max(0.0);
        } else {
            remain += length;
        }
    }
    if centers.len() < 2 {
        return Err(Error::InvalidGeometry);
    }
    let last = centers[centers.len() - 1];
    let mut angle_prev = (centers[0].y - last.y).atan2(centers[0].x - last.x);
    let mut alpha_prev = ellipse_alpha(last, centers[0], radius);
    for i in 0..centers.len() {
        let a = centers[i];
        let b = centers[(i + 1) % centers.len()];
        let angle = (b.y - a.y).atan2(b.x - a.x);
        let alpha = ellipse_alpha(a, b, radius);
        corner(out, angle_prev, angle, radius, a, alpha, alpha_prev)?;
        angle_prev = angle;
        alpha_prev = alpha;
    }
    Ok(())
}

fn ellipse_alpha(a: Point, b: Point, radius: f64) -> f64 {
    let length = a.distance(b);
    if length == 0.0 {
        return A34;
    }
    let arg = length / (2.0 * radius);
    if (-1.0..=1.0).contains(&arg) {
        arg.acos()
    } else {
        0.0
    }
}

fn flatten_ellipse(out: &mut PathBuilder<'_>, rect: Rect) -> Result<Vec<Point>, Error> {
    // Match the four cubic arcs and the 0.5 point flatness of Java's ellipse
    // iterator. Only bounded temporary samples are retained, not a second
    // renderable path. De Casteljau subdivision uses Kurbo's existing cubic.
    let center = rect.center();
    let rx = rect.width() * 0.5;
    let ry = rect.height() * 0.5;
    let k = 0.5522847498307933;
    let mut points = Vec::new();
    out.budget.reserve(&mut points, 1)?;
    let mut start = Point::new(center.x + rx, center.y);
    points.push(start);
    for (a, b, c) in [
        ((rx, k * ry), (k * rx, ry), (0.0, ry)),
        ((-k * rx, ry), (-rx, k * ry), (-rx, 0.0)),
        ((-rx, -k * ry), (-k * rx, -ry), (0.0, -ry)),
        ((k * rx, -ry), (rx, -k * ry), (rx, 0.0)),
    ] {
        let end = center + kurbo::Vec2::from(c);
        flatten(
            out,
            CubicBez::new(
                start,
                center + kurbo::Vec2::from(a),
                center + kurbo::Vec2::from(b),
                end,
            ),
            0,
            &mut points,
        )?;
        start = end;
    }
    Ok(points)
}

fn flatten(
    out: &mut PathBuilder<'_>,
    curve: CubicBez,
    depth: u8,
    points: &mut Vec<Point>,
) -> Result<(), Error> {
    out.checkpoint()?;
    let delta = curve.p3 - curve.p0;
    let length2 = delta.hypot2();
    let distance2 = |p: Point| {
        let t = if length2 == 0.0 {
            0.0
        } else {
            ((p - curve.p0).dot(delta) / length2).clamp(0.0, 1.0)
        };
        p.distance_squared(curve.p0 + delta * t)
    };
    if distance2(curve.p1).max(distance2(curve.p2)) <= 0.25 {
        out.budget.reserve(points, 1)?;
        points.push(curve.p3);
        return Ok(());
    }
    if depth == 10 {
        return Err(Error::InvalidGeometry);
    }
    let (first, second) = curve.subdivide();
    flatten(out, first, depth + 1, points)?;
    flatten(out, second, depth + 1, points)
}
