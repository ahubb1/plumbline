//! Circles, arcs and slopes: the helpers behind radius, arc-length and pitched-length
//! measurements. Unit-agnostic, like [`crate::measure`].

use kurbo::Point;

/// The circle through three points: its centre and radius. `None` when the points are on one
/// line (or two of them coincide), or when a coordinate is not finite.
pub fn circle_from_three_points(a: Point, b: Point, c: Point) -> Option<(Point, f64)> {
    // Work relative to `a` so large coordinate offsets do not cost precision.
    let (u, v) = (b - a, c - a);
    let cross = u.cross(v);
    let scale = u.hypot2().max(v.hypot2());
    if !(cross.is_finite() && scale.is_finite()) || cross.abs() <= 1e-12 * scale {
        return None;
    }
    let (uu, vv) = (u.hypot2(), v.hypot2());
    let cx = (v.y * uu - u.y * vv) / (2.0 * cross);
    let cy = (u.x * vv - v.x * uu) / (2.0 * cross);
    let radius = cx.hypot(cy);
    if !radius.is_finite() {
        return None;
    }
    Some((Point::new(a.x + cx, a.y + cy), radius))
}

/// The radius of the circle through three points. See [`circle_from_three_points`].
pub fn radius_from_three_points(a: Point, b: Point, c: Point) -> Option<f64> {
    circle_from_three_points(a, b, c).map(|(_, r)| r)
}

/// The length of an arc of `radius` that turns through `sweep` radians (either sign).
pub fn arc_length(radius: f64, sweep: f64) -> f64 {
    (radius * sweep).abs()
}

/// An arc given by three points on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arc3 {
    pub centre: Point,
    pub radius: f64,
    /// The direction from the centre to the first point, in radians from the +x axis.
    pub start_angle: f64,
    /// How far the arc turns from the first point through the second to the third, in
    /// radians: positive counter-clockwise in a y-up space, in `(-2 pi, 2 pi)`.
    pub sweep: f64,
}

impl Arc3 {
    /// The length along the arc.
    pub fn length(&self) -> f64 {
        arc_length(self.radius, self.sweep)
    }

    /// The straight distance between the arc's ends.
    pub fn chord(&self) -> f64 {
        2.0 * self.radius * (self.sweep / 2.0).sin().abs()
    }
}

/// The arc that starts at `a`, passes through `b` and ends at `c`. `None` when the three
/// points are on one line.
pub fn arc_through_three_points(a: Point, b: Point, c: Point) -> Option<Arc3> {
    let (centre, radius) = circle_from_three_points(a, b, c)?;
    let angle = |p: Point| (p.y - centre.y).atan2(p.x - centre.x);
    let tau = std::f64::consts::TAU;
    let start_angle = angle(a);
    // Counter-clockwise turns from a to b and from a to c, each in [0, 2 pi).
    let to_b = (angle(b) - start_angle).rem_euclid(tau);
    let to_c = (angle(c) - start_angle).rem_euclid(tau);
    // Going counter-clockwise, b comes before c exactly when the arc is counter-clockwise.
    let sweep = if to_b <= to_c { to_c } else { to_c - tau };
    Some(Arc3 {
        centre,
        radius,
        start_angle,
        sweep,
    })
}

/// The length along a slope that covers `run` horizontally while rising `rise`: what a rafter
/// or a ramp measures when the plan shows only its run.
pub fn slope_length(run: f64, rise: f64) -> f64 {
    run.hypot(rise)
}

/// The length along a slope of the given pitch (rise over run: 0.5 for a 6-in-12 roof) that
/// covers `run` horizontally.
pub fn slope_length_from_pitch(run: f64, pitch: f64) -> f64 {
    run.abs() * 1.0f64.hypot(pitch)
}

/// The length along a slope at `angle` radians from the horizontal that covers `run`
/// horizontally. Infinite for a vertical slope.
pub fn slope_length_from_angle(run: f64, angle: f64) -> f64 {
    let cos = angle.cos().abs();
    if cos <= f64::EPSILON {
        f64::INFINITY
    } else {
        run.abs() / cos
    }
}

/// The factor a plan area is multiplied by to give the area of a surface of the given pitch
/// (rise over run): a 6-in-12 roof has 1.118 times its plan area.
pub fn slope_factor(pitch: f64) -> f64 {
    1.0f64.hypot(pitch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI, SQRT_2};

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn radius_from_three_points_on_known_circles() {
        // The unit circle, any three points, any order.
        let (c, r) = circle_from_three_points(p(1.0, 0.0), p(0.0, 1.0), p(-1.0, 0.0)).unwrap();
        assert!(close(c.x, 0.0) && close(c.y, 0.0) && close(r, 1.0));
        // Centre (120, 80), radius 50, far from the origin.
        let centre = p(120.0, 80.0);
        let on = |t: f64| p(centre.x + 50.0 * t.cos(), centre.y + 50.0 * t.sin());
        for (t0, t1, t2) in [(0.1, 1.3, 4.0), (5.9, 0.2, 0.5), (3.0, 3.1, 3.2)] {
            let (c, r) = circle_from_three_points(on(t0), on(t1), on(t2)).unwrap();
            assert!(
                (c.x - 120.0).abs() < 1e-6 && (c.y - 80.0).abs() < 1e-6,
                "{c:?}"
            );
            assert!((r - 50.0).abs() < 1e-6, "{r}");
            assert!(
                (radius_from_three_points(on(t2), on(t0), on(t1)).unwrap() - 50.0).abs() < 1e-6
            );
        }
        // A right triangle: the hypotenuse is a diameter.
        let r = radius_from_three_points(p(0.0, 0.0), p(6.0, 0.0), p(0.0, 8.0)).unwrap();
        assert!(close(r, 5.0));
        // Collinear or repeated points have no circle.
        assert_eq!(
            circle_from_three_points(p(0.0, 0.0), p(1.0, 1.0), p(2.0, 2.0)),
            None
        );
        assert_eq!(
            circle_from_three_points(p(0.0, 0.0), p(0.0, 0.0), p(2.0, 2.0)),
            None
        );
        assert_eq!(
            circle_from_three_points(p(0.0, 0.0), p(f64::NAN, 0.0), p(2.0, 2.0)),
            None
        );
    }

    #[test]
    fn arc_lengths() {
        assert!(close(arc_length(10.0, PI), 10.0 * PI));
        assert!(close(arc_length(10.0, -FRAC_PI_2), 5.0 * PI));
        assert_eq!(arc_length(0.0, 3.0), 0.0);

        // A quarter of the unit circle, counter-clockwise, through its midpoint.
        let mid = p(SQRT_2 / 2.0, SQRT_2 / 2.0);
        let arc = arc_through_three_points(p(1.0, 0.0), mid, p(0.0, 1.0)).unwrap();
        assert!(close(arc.sweep, FRAC_PI_2) && close(arc.length(), FRAC_PI_2));
        assert!(close(arc.chord(), SQRT_2) && close(arc.start_angle, 0.0));
        // The same ends the other way round the circle: three quarters, clockwise.
        let arc = arc_through_three_points(p(1.0, 0.0), p(-1.0, 0.0), p(0.0, 1.0)).unwrap();
        assert!(close(arc.sweep, -1.5 * PI) && close(arc.length(), 1.5 * PI));
        assert!(close(arc.chord(), SQRT_2));
        // Reversing the direction keeps the length and flips the sweep.
        let back = arc_through_three_points(p(0.0, 1.0), mid, p(1.0, 0.0)).unwrap();
        assert!(close(back.sweep, -FRAC_PI_2) && close(back.length(), FRAC_PI_2));
        // A half circle of radius 25 about (100, 100).
        let arc =
            arc_through_three_points(p(125.0, 100.0), p(100.0, 125.0), p(75.0, 100.0)).unwrap();
        assert!(close(arc.radius, 25.0) && close(arc.length(), 25.0 * PI));
        assert!(close(arc.chord(), 50.0));
        assert_eq!(
            arc_through_three_points(p(0.0, 0.0), p(1.0, 0.0), p(2.0, 0.0)),
            None
        );
    }

    #[test]
    fn slope_adjusted_lengths() {
        assert!(close(slope_length(4.0, 3.0), 5.0));
        assert!(close(slope_length(12.0, 0.0), 12.0));
        // A 6-in-12 roof over a 12 ft run.
        assert!(close(slope_length_from_pitch(12.0, 0.5), 180f64.sqrt()));
        assert!(close(slope_length_from_pitch(4.0, 0.75), 5.0));
        assert!(close(slope_length_from_pitch(4.0, -0.75), 5.0));
        assert!(close(slope_length_from_angle(10.0, PI / 3.0), 20.0));
        assert!(close(slope_length_from_angle(10.0, 0.0), 10.0));
        assert_eq!(slope_length_from_angle(10.0, FRAC_PI_2), f64::INFINITY);
        assert!(close(slope_factor(0.5), 1.25f64.sqrt()));
        assert!(close(slope_factor(0.0), 1.0));
    }
}
