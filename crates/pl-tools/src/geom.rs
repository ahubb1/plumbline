//! Direction helpers: page vectors to and from the displayed orientation, the Shift
//! constraints, and point thinning for freehand strokes.

use pl_geom::{point_segment_distance, Point, Rotation, Vec2};

/// A page-space vector as it appears on screen: x to the right, y **down**, for a page shown
/// with `rotation`. This is the linear part of `PageGeometry::page_to_display`.
pub fn to_display(rotation: Rotation, v: Vec2) -> Vec2 {
    match rotation {
        Rotation::R0 => Vec2::new(v.x, -v.y),
        Rotation::R90 => Vec2::new(v.y, v.x),
        Rotation::R180 => Vec2::new(-v.x, v.y),
        Rotation::R270 => Vec2::new(-v.y, -v.x),
    }
}

/// The inverse of [`to_display`]: a screen vector (y down) as a page-space vector.
pub fn to_page(rotation: Rotation, d: Vec2) -> Vec2 {
    match rotation {
        Rotation::R0 => Vec2::new(d.x, -d.y),
        Rotation::R90 => Vec2::new(d.y, d.x),
        Rotation::R180 => Vec2::new(-d.x, d.y),
        Rotation::R270 => Vec2::new(-d.y, -d.x),
    }
}

/// tan(22.5 degrees): the boundary between "along an axis" and "along a diagonal".
const TAN_22_5: f64 = 0.414_213_562_373_095_03;

pub(crate) fn sign(v: f64) -> f64 {
    if v < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// `p` moved onto the nearest of the eight directions (the screen's horizontal, vertical and
/// diagonals) from `anchor`, by perpendicular projection.
///
/// On an axis the other coordinate is exactly the anchor's. On a diagonal both offsets have
/// exactly the same size.
pub fn constrain_angle(anchor: Point, p: Point, rotation: Rotation) -> Point {
    let d = to_display(rotation, p - anchor);
    let (ax, ay) = (d.x.abs(), d.y.abs());
    let c = if ay <= ax * TAN_22_5 {
        Vec2::new(d.x, 0.0)
    } else if ax <= ay * TAN_22_5 {
        Vec2::new(0.0, d.y)
    } else {
        let m = (ax + ay) / 2.0;
        Vec2::new(sign(d.x) * m, sign(d.y) * m)
    };
    anchor + to_page(rotation, c)
}

/// `p` moved so that the box from `anchor` to it is square, with the side of the longer of the
/// two offsets. A zero offset grows to the screen's right and downwards.
pub fn constrain_square(anchor: Point, p: Point, rotation: Rotation) -> Point {
    let d = to_display(rotation, p - anchor);
    let side = d.x.abs().max(d.y.abs());
    anchor + to_page(rotation, Vec2::new(sign(d.x) * side, sign(d.y) * side))
}

/// A movement kept to whichever screen axis it is mostly along; a tie goes to the horizontal.
pub fn constrain_axis(delta: Vec2, rotation: Rotation) -> Vec2 {
    let d = to_display(rotation, delta);
    let c = if d.x.abs() >= d.y.abs() {
        Vec2::new(d.x, 0.0)
    } else {
        Vec2::new(0.0, d.y)
    };
    to_page(rotation, c)
}

/// Drops points closer than `min_distance` to the last point kept. The first point is always
/// kept, and so is the last (it replaces nothing: it is added even when close).
pub fn thin_points(points: &[Point], min_distance: f64) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        match out.last() {
            None => out.push(*p),
            Some(last) => {
                let far = (*p - *last).hypot() >= min_distance;
                if far || (i + 1 == points.len() && *p != *last) {
                    out.push(*p);
                }
            }
        }
    }
    out
}

/// Ramer-Douglas-Peucker: the fewest of `points`, in order, such that no dropped point is
/// further than `tolerance` from the simplified line. The end points are always kept.
pub fn simplify_points(points: &[Point], tolerance: f64) -> Vec<Point> {
    if points.len() < 3 || tolerance.is_nan() || tolerance <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        let mut worst = 0.0;
        let mut at = first;
        for i in first + 1..last {
            let d = point_segment_distance(points[i], points[first], points[last]);
            if d > worst {
                worst = d;
                at = i;
            }
        }
        if worst > tolerance {
            keep[at] = true;
            stack.push((first, at));
            stack.push((at, last));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_geom::{PageGeometry, Rect};

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn the_vector_maps_are_the_linear_part_of_the_page_transform() {
        let crop = Rect::new(150.0, 260.0, 650.0, 900.0);
        for r in Rotation::ALL {
            let g = PageGeometry::new(Rect::new(100.0, 200.0, 712.0, 992.0), Some(crop), r, 2.0);
            let (a, b) = (pt(200.0, 300.0), pt(217.0, 345.5));
            let want = g.page_to_display() * b - g.page_to_display() * a;
            assert_eq!(to_display(r, b - a), want, "{r:?}");
            let back = g.display_to_page() * pt(31.0, 12.0) - g.display_to_page() * pt(1.0, 2.0);
            assert_eq!(to_page(r, Vec2::new(30.0, 10.0)), back, "{r:?}");
            assert_eq!(
                to_page(r, to_display(r, Vec2::new(3.0, -7.0))),
                Vec2::new(3.0, -7.0)
            );
        }
    }

    #[test]
    fn angles_snap_to_the_eight_directions() {
        let o = pt(100.0, 100.0);
        // Nearly horizontal: y is exactly the anchor's.
        assert_eq!(
            constrain_angle(o, pt(180.0, 110.0), Rotation::R0),
            pt(180.0, 100.0)
        );
        assert_eq!(
            constrain_angle(o, pt(20.0, 95.0), Rotation::R0),
            pt(20.0, 100.0)
        );
        // Nearly vertical.
        assert_eq!(
            constrain_angle(o, pt(104.0, 30.0), Rotation::R0),
            pt(100.0, 30.0)
        );
        // Diagonal: (60, 40) projects to (50, 50).
        assert_eq!(
            constrain_angle(o, pt(160.0, 140.0), Rotation::R0),
            pt(150.0, 150.0)
        );
        assert_eq!(
            constrain_angle(o, pt(40.0, 140.0), Rotation::R0),
            pt(50.0, 150.0)
        );
        assert_eq!(
            constrain_angle(o, pt(160.0, 60.0), Rotation::R0),
            pt(150.0, 50.0)
        );
        assert_eq!(constrain_angle(o, o, Rotation::R0), o);
        // A quarter turn maps the eight directions onto themselves.
        for r in Rotation::ALL {
            assert_eq!(
                constrain_angle(o, pt(180.0, 110.0), r),
                pt(180.0, 100.0),
                "{r:?}"
            );
            assert_eq!(
                constrain_angle(o, pt(160.0, 140.0), r),
                pt(150.0, 150.0),
                "{r:?}"
            );
        }
    }

    #[test]
    fn squares_take_the_longer_side_and_ties_follow_the_screen() {
        let o = pt(10.0, 10.0);
        assert_eq!(
            constrain_square(o, pt(50.0, 30.0), Rotation::R0),
            pt(50.0, 50.0)
        );
        assert_eq!(
            constrain_square(o, pt(-20.0, 30.0), Rotation::R0),
            pt(-20.0, 40.0)
        );
        assert_eq!(
            constrain_square(o, pt(20.0, -40.0), Rotation::R0),
            pt(60.0, -40.0)
        );
        // A purely horizontal screen drag grows downwards on screen, whatever the rotation.
        // R0: screen right is page +x, screen down is page -y.
        assert_eq!(
            constrain_square(o, pt(30.0, 10.0), Rotation::R0),
            pt(30.0, -10.0)
        );
        // R90: screen right is page +y, screen down is page +x.
        assert_eq!(
            constrain_square(o, pt(10.0, 30.0), Rotation::R90),
            pt(30.0, 30.0)
        );
        // R180: screen right is page -x, screen down is page +y.
        assert_eq!(
            constrain_square(o, pt(-10.0, 10.0), Rotation::R180),
            pt(-10.0, 30.0)
        );
        // R270: screen right is page -y, screen down is page -x.
        assert_eq!(
            constrain_square(o, pt(10.0, -10.0), Rotation::R270),
            pt(-10.0, -10.0)
        );
    }

    #[test]
    fn a_move_keeps_its_main_screen_axis() {
        assert_eq!(
            constrain_axis(Vec2::new(9.0, 4.0), Rotation::R0),
            Vec2::new(9.0, 0.0)
        );
        assert_eq!(
            constrain_axis(Vec2::new(3.0, -4.0), Rotation::R0),
            Vec2::new(0.0, -4.0)
        );
        // A tie goes to the screen's horizontal: page x at 0 and 180, page y at 90 and 270.
        let tie = Vec2::new(5.0, 5.0);
        assert_eq!(constrain_axis(tie, Rotation::R0), Vec2::new(5.0, 0.0));
        assert_eq!(constrain_axis(tie, Rotation::R180), Vec2::new(5.0, 0.0));
        assert_eq!(constrain_axis(tie, Rotation::R90), Vec2::new(0.0, 5.0));
        assert_eq!(constrain_axis(tie, Rotation::R270), Vec2::new(0.0, 5.0));
    }

    #[test]
    fn thinning_keeps_the_ends_and_spaced_points() {
        let pts: Vec<Point> = (0..=10).map(|i| pt(i as f64, 0.0)).collect();
        assert_eq!(
            thin_points(&pts, 3.0),
            [
                pt(0.0, 0.0),
                pt(3.0, 0.0),
                pt(6.0, 0.0),
                pt(9.0, 0.0),
                pt(10.0, 0.0)
            ]
        );
        assert_eq!(thin_points(&[], 3.0), []);
        assert_eq!(
            thin_points(&[pt(1.0, 1.0), pt(1.0, 1.0)], 3.0),
            [pt(1.0, 1.0)]
        );
        assert_eq!(thin_points(&pts, 0.0), pts);
    }

    #[test]
    fn simplifying_drops_points_on_the_line_and_keeps_corners() {
        let pts = [
            pt(0.0, 0.0),
            pt(10.0, 0.1),
            pt(20.0, -0.1),
            pt(30.0, 0.0),
            pt(30.0, 10.0),
            pt(30.1, 20.0),
        ];
        assert_eq!(
            simplify_points(&pts, 0.5),
            [pt(0.0, 0.0), pt(30.0, 0.0), pt(30.1, 20.0)]
        );
        assert_eq!(simplify_points(&pts, 0.0), pts);
        assert_eq!(simplify_points(&pts[..2], 5.0), pts[..2]);
        // A closed loop (first == last) keeps its far side.
        let ring = [pt(0.0, 0.0), pt(10.0, 0.0), pt(10.0, 10.0), pt(0.0, 0.0)];
        assert_eq!(simplify_points(&ring, 1.0).len(), 4);
    }
}
