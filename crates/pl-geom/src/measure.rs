//! Plane measurements: areas, lengths, angles, distances and intersections.
//!
//! Everything here is unit-agnostic: feed page-space points and get page-space units (or their
//! squares) back. Polygons are given as their vertices without repeating the first one; a
//! repeated closing vertex is harmless.

use kurbo::{Point, Vec2};

/// Relative tolerance used to decide that two directions are parallel or a point is on a line.
const REL_EPS: f64 = 1e-12;

/// The signed area of a polygon by the shoelace formula: positive when the vertices run
/// counter-clockwise in a y-up space (page space), negative when clockwise. Fewer than three
/// vertices give 0. A self-intersecting polygon gives the algebraic sum of its lobes.
pub fn polygon_signed_area(vertices: &[Point]) -> f64 {
    if vertices.len() < 3 {
        return 0.0;
    }
    // Work relative to the first vertex so large coordinate offsets do not cost precision.
    let o = vertices[0];
    let mut twice = 0.0;
    for i in 1..vertices.len() - 1 {
        twice += (vertices[i] - o).cross(vertices[i + 1] - o);
    }
    twice / 2.0
}

/// The area of a polygon (absolute value of [`polygon_signed_area`]).
pub fn polygon_area(vertices: &[Point]) -> f64 {
    polygon_signed_area(vertices).abs()
}

/// The perimeter of a polygon, including the closing edge from the last vertex to the first.
pub fn polygon_perimeter(vertices: &[Point]) -> f64 {
    match vertices.len() {
        0 | 1 => 0.0,
        n => polyline_length(vertices) + vertices[n - 1].distance(vertices[0]),
    }
}

/// The length of an open polyline.
pub fn polyline_length(points: &[Point]) -> f64 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// The area centroid of a polygon. Falls back to the mean of the vertices when the area is zero
/// (a degenerate or collinear polygon); `None` only for an empty slice.
pub fn polygon_centroid(vertices: &[Point]) -> Option<Point> {
    let first = *vertices.first()?;
    let mut twice_area = 0.0;
    let mut acc = Vec2::ZERO;
    if vertices.len() >= 3 {
        for i in 1..vertices.len() - 1 {
            let a = vertices[i] - first;
            let b = vertices[i + 1] - first;
            let cross = a.cross(b);
            twice_area += cross;
            acc += (a + b) * cross;
        }
    }
    let scale: f64 = vertices
        .iter()
        .map(|v| (*v - first).hypot2())
        .fold(0.0, f64::max);
    if twice_area.abs() <= REL_EPS * scale {
        let n = vertices.len() as f64;
        let sum = vertices.iter().fold(Vec2::ZERO, |s, v| s + (*v - first));
        return Some(first + sum / n);
    }
    Some(first + acc / (3.0 * twice_area))
}

/// Whether a point lies inside a polygon (even-odd rule). Points exactly on an edge may fall on
/// either side.
pub fn point_in_polygon(p: Point, vertices: &[Point]) -> bool {
    let n = vertices.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (vertices[i], vertices[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// The unsigned angle at `vertex` between the rays to `a` and to `b`, in radians, in `[0, pi]`.
/// Returns 0 if either ray has zero length.
pub fn angle_at(vertex: Point, a: Point, b: Point) -> f64 {
    let u = a - vertex;
    let v = b - vertex;
    if u.hypot2() == 0.0 || v.hypot2() == 0.0 {
        return 0.0;
    }
    u.cross(v).abs().atan2(u.dot(v))
}

/// The signed angle at `vertex` from the ray to `a` round to the ray to `b`, in radians, in
/// `(-pi, pi]`. Positive is counter-clockwise in a y-up space (page space); in a y-down space
/// (view space) positive is clockwise. Returns 0 if either ray has zero length.
pub fn signed_angle_at(vertex: Point, a: Point, b: Point) -> f64 {
    let u = a - vertex;
    let v = b - vertex;
    if u.hypot2() == 0.0 || v.hypot2() == 0.0 {
        return 0.0;
    }
    u.cross(v).atan2(u.dot(v))
}

/// The point of segment `a`-`b` nearest to `p`, and its parameter `t` in `[0, 1]`
/// (`a + t * (b - a)`). A zero-length segment returns `a` and 0.
pub fn closest_point_on_segment(p: Point, a: Point, b: Point) -> (Point, f64) {
    let ab = b - a;
    let len2 = ab.hypot2();
    if len2 == 0.0 {
        return (a, 0.0);
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (a + ab * t, t)
}

/// The distance from `p` to the segment `a`-`b`.
pub fn point_segment_distance(p: Point, a: Point, b: Point) -> f64 {
    closest_point_on_segment(p, a, b).0.distance(p)
}

/// How two segments meet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SegmentIntersection {
    /// The segments do not touch.
    None,
    /// The segments meet at one point. `t` and `u` are the parameters along the first and second
    /// segment, each in `[0, 1]`.
    Point { point: Point, t: f64, u: f64 },
    /// The segments are collinear and share a stretch, given by its two ends in order along the
    /// first segment.
    Overlap(Point, Point),
}

impl SegmentIntersection {
    /// The single intersection point, if there is exactly one.
    pub fn point(&self) -> Option<Point> {
        match self {
            SegmentIntersection::Point { point, .. } => Some(*point),
            _ => None,
        }
    }
}

/// Intersects segment `a0`-`a1` with segment `b0`-`b1`. Endpoints count: segments that merely
/// touch intersect at that point.
pub fn segment_intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> SegmentIntersection {
    let r = a1 - a0;
    let s = b1 - b0;
    let w = b0 - a0;
    let r_len2 = r.hypot2();
    let s_len2 = s.hypot2();
    // Positional tolerance scaled to the segments.
    let scale = r_len2.max(s_len2).max(w.hypot2());
    let pos_eps2 = REL_EPS * REL_EPS * scale.max(f64::MIN_POSITIVE);

    // Zero-length segments are points.
    if r_len2 == 0.0 && s_len2 == 0.0 {
        return if w.hypot2() <= pos_eps2 {
            SegmentIntersection::Point {
                point: a0,
                t: 0.0,
                u: 0.0,
            }
        } else {
            SegmentIntersection::None
        };
    }
    if r_len2 == 0.0 {
        let (q, u) = closest_point_on_segment(a0, b0, b1);
        return if q.distance_squared(a0) <= pos_eps2.max(REL_EPS * REL_EPS * s_len2) {
            SegmentIntersection::Point {
                point: a0,
                t: 0.0,
                u,
            }
        } else {
            SegmentIntersection::None
        };
    }
    if s_len2 == 0.0 {
        let (q, t) = closest_point_on_segment(b0, a0, a1);
        return if q.distance_squared(b0) <= pos_eps2.max(REL_EPS * REL_EPS * r_len2) {
            SegmentIntersection::Point {
                point: b0,
                t,
                u: 0.0,
            }
        } else {
            SegmentIntersection::None
        };
    }

    let denom = r.cross(s);
    let par_eps = REL_EPS * (r_len2 * s_len2).sqrt();
    if denom.abs() <= par_eps {
        // Parallel. Collinear only if b0 lies on the line through a.
        if w.cross(r).abs() > REL_EPS * (r_len2 * scale).sqrt() {
            return SegmentIntersection::None;
        }
        // Project b onto a: parameters along r.
        let t0 = w.dot(r) / r_len2;
        let t1 = t0 + s.dot(r) / r_len2;
        let (lo, hi) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
        let lo = lo.max(0.0);
        let hi = hi.min(1.0);
        if lo > hi + REL_EPS {
            return SegmentIntersection::None;
        }
        if hi - lo <= REL_EPS {
            let t = lo.clamp(0.0, 1.0);
            let point = a0 + r * t;
            let u = ((point - b0).dot(s) / s_len2).clamp(0.0, 1.0);
            return SegmentIntersection::Point { point, t, u };
        }
        return SegmentIntersection::Overlap(a0 + r * lo, a0 + r * hi);
    }

    let t = w.cross(s) / denom;
    let u = w.cross(r) / denom;
    let in_range = |v: f64| (-REL_EPS..=1.0 + REL_EPS).contains(&v);
    if in_range(t) && in_range(u) {
        let t = t.clamp(0.0, 1.0);
        let u = u.clamp(0.0, 1.0);
        SegmentIntersection::Point {
            point: a0 + r * t,
            t,
            u,
        }
    } else {
        SegmentIntersection::None
    }
}

/// Intersects the infinite lines through `a0`-`a1` and `b0`-`b1`. `None` when the lines are
/// parallel (including coincident) or either pair of points coincides.
pub fn line_intersection(a0: Point, a1: Point, b0: Point, b1: Point) -> Option<Point> {
    let r = a1 - a0;
    let s = b1 - b0;
    let denom = r.cross(s);
    let lens = (r.hypot2() * s.hypot2()).sqrt();
    if lens == 0.0 || denom.abs() <= REL_EPS * lens {
        return None;
    }
    let t = (b0 - a0).cross(s) / denom;
    Some(a0 + r * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    fn near_pt(a: Point, b: Point) -> bool {
        a.distance(b) <= 1e-9 * a.to_vec2().hypot().max(1.0)
    }

    #[test]
    fn area_of_simple_shapes() {
        let square = [p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)];
        assert_eq!(polygon_signed_area(&square), 100.0);
        let mut cw = square;
        cw.reverse();
        assert_eq!(polygon_signed_area(&cw), -100.0);
        assert_eq!(polygon_area(&cw), 100.0);

        let tri = [p(0.0, 0.0), p(4.0, 0.0), p(0.0, 3.0)];
        assert_eq!(polygon_area(&tri), 6.0);
        assert_eq!(polygon_perimeter(&tri), 12.0);

        // L-shape (concave): 10x10 minus a 6x6 corner.
        let l = [
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 4.0),
            p(4.0, 4.0),
            p(4.0, 10.0),
            p(0.0, 10.0),
        ];
        assert_eq!(polygon_area(&l), 64.0);
        assert_eq!(polygon_perimeter(&l), 40.0);
    }

    #[test]
    fn area_degenerate_inputs() {
        assert_eq!(polygon_area(&[]), 0.0);
        assert_eq!(polygon_area(&[p(1.0, 1.0)]), 0.0);
        assert_eq!(polygon_area(&[p(1.0, 1.0), p(5.0, 5.0)]), 0.0);
        assert_eq!(polygon_area(&[p(0.0, 0.0), p(1.0, 1.0), p(2.0, 2.0)]), 0.0);
        assert_eq!(polygon_perimeter(&[]), 0.0);
        assert_eq!(polygon_perimeter(&[p(3.0, 3.0)]), 0.0);
        // Two points: out and back.
        assert_eq!(polygon_perimeter(&[p(0.0, 0.0), p(3.0, 4.0)]), 10.0);
        assert_eq!(polyline_length(&[]), 0.0);
        assert_eq!(polyline_length(&[p(1.0, 1.0)]), 0.0);
    }

    #[test]
    fn repeated_closing_vertex_changes_nothing() {
        let open = [p(0.0, 0.0), p(4.0, 0.0), p(4.0, 3.0)];
        let closed = [p(0.0, 0.0), p(4.0, 0.0), p(4.0, 3.0), p(0.0, 0.0)];
        assert_eq!(polygon_area(&open), polygon_area(&closed));
        assert_eq!(polygon_perimeter(&open), polygon_perimeter(&closed));
    }

    #[test]
    fn area_is_precise_far_from_origin() {
        // A unit square a long way from the origin: the naive shoelace sum loses digits here.
        let o = 1.0e9;
        let sq = [p(o, o), p(o + 1.0, o), p(o + 1.0, o + 1.0), p(o, o + 1.0)];
        assert_eq!(polygon_area(&sq), 1.0);
    }

    #[test]
    fn regular_polygon_approaches_circle() {
        let n = 720;
        let r = 25.0;
        let pts: Vec<Point> = (0..n)
            .map(|i| {
                let a = 2.0 * PI * i as f64 / n as f64;
                p(100.0 + r * a.cos(), -40.0 + r * a.sin())
            })
            .collect();
        let exact_area = 0.5 * n as f64 * r * r * (2.0 * PI / n as f64).sin();
        let exact_perim = 2.0 * n as f64 * r * (PI / n as f64).sin();
        assert!(near(polygon_area(&pts), exact_area));
        assert!(near(polygon_perimeter(&pts), exact_perim));
        assert!((polygon_area(&pts) - PI * r * r).abs() < 0.05);
        assert!(near_pt(polygon_centroid(&pts).unwrap(), p(100.0, -40.0)));
    }

    /// Property: area and perimeter are invariant under rotation and translation, and scale by
    /// k^2 and k.
    #[test]
    fn prop_area_perimeter_under_similarity() {
        let mut rng = Rng::new(31);
        for _ in 0..300 {
            let n = rng.int(3, 12) as usize;
            let pts: Vec<Point> = (0..n)
                .map(|_| p(rng.range(-100.0, 100.0), rng.range(-100.0, 100.0)))
                .collect();
            let k = rng.range(0.1, 10.0);
            let th = rng.range(0.0, 2.0 * PI);
            let t = kurbo::Affine::translate((rng.range(-1e4, 1e4), rng.range(-1e4, 1e4)))
                * kurbo::Affine::rotate(th)
                * kurbo::Affine::scale(k);
            let moved: Vec<Point> = pts.iter().map(|q| t * *q).collect();
            let a0 = polygon_signed_area(&pts);
            let a1 = polygon_signed_area(&moved);
            assert!((a1 - a0 * k * k).abs() <= 1e-7 * (a0.abs() * k * k).max(1.0));
            assert!(near(polygon_perimeter(&moved), polygon_perimeter(&pts) * k));
            assert!(near(polyline_length(&moved), polyline_length(&pts) * k));
        }
    }

    #[test]
    fn polyline_length_sums_segments() {
        let pts = [p(0.0, 0.0), p(3.0, 4.0), p(3.0, 10.0), p(-5.0, 10.0)];
        assert_eq!(polyline_length(&pts), 5.0 + 6.0 + 8.0);
    }

    #[test]
    fn centroid_cases() {
        let sq = [p(2.0, 2.0), p(6.0, 2.0), p(6.0, 6.0), p(2.0, 6.0)];
        assert!(near_pt(polygon_centroid(&sq).unwrap(), p(4.0, 4.0)));
        let tri = [p(0.0, 0.0), p(6.0, 0.0), p(0.0, 9.0)];
        assert!(near_pt(polygon_centroid(&tri).unwrap(), p(2.0, 3.0)));
        // Winding does not matter.
        let mut rev = tri;
        rev.reverse();
        assert!(near_pt(polygon_centroid(&rev).unwrap(), p(2.0, 3.0)));
        // Degenerate: mean of vertices.
        assert!(near_pt(
            polygon_centroid(&[p(0.0, 0.0), p(2.0, 2.0), p(4.0, 4.0)]).unwrap(),
            p(2.0, 2.0)
        ));
        assert!(near_pt(
            polygon_centroid(&[p(7.0, 8.0)]).unwrap(),
            p(7.0, 8.0)
        ));
        assert_eq!(polygon_centroid(&[]), None);
    }

    #[test]
    fn point_in_polygon_cases() {
        let l = [
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 4.0),
            p(4.0, 4.0),
            p(4.0, 10.0),
            p(0.0, 10.0),
        ];
        assert!(point_in_polygon(p(2.0, 2.0), &l));
        assert!(point_in_polygon(p(8.0, 2.0), &l));
        assert!(point_in_polygon(p(2.0, 8.0), &l));
        assert!(!point_in_polygon(p(8.0, 8.0), &l)); // in the notch
        assert!(!point_in_polygon(p(-1.0, 5.0), &l));
        assert!(!point_in_polygon(p(5.0, 11.0), &l));
        assert!(!point_in_polygon(p(1.0, 1.0), &l[..2]));
    }

    #[test]
    fn angles() {
        let v = p(1.0, 1.0);
        assert!(near(angle_at(v, p(2.0, 1.0), p(1.0, 5.0)), FRAC_PI_2));
        assert!(near(angle_at(v, p(2.0, 1.0), p(3.0, 3.0)), FRAC_PI_4));
        assert!(near(angle_at(v, p(2.0, 1.0), p(-3.0, 1.0)), PI));
        assert!(near(angle_at(v, p(2.0, 1.0), p(9.0, 1.0)), 0.0));
        // Unsigned: order does not matter.
        assert!(near(angle_at(v, p(1.0, 5.0), p(2.0, 1.0)), FRAC_PI_2));
        // Signed: counter-clockwise positive in y-up space.
        assert!(near(
            signed_angle_at(v, p(2.0, 1.0), p(1.0, 5.0)),
            FRAC_PI_2
        ));
        assert!(near(
            signed_angle_at(v, p(1.0, 5.0), p(2.0, 1.0)),
            -FRAC_PI_2
        ));
        assert!(near(signed_angle_at(v, p(2.0, 1.0), p(-3.0, 1.0)), PI));
        // Degenerate rays.
        assert_eq!(angle_at(v, v, p(3.0, 3.0)), 0.0);
        assert_eq!(signed_angle_at(v, p(3.0, 3.0), v), 0.0);
    }

    #[test]
    fn prop_angle_matches_construction() {
        let mut rng = Rng::new(32);
        for _ in 0..500 {
            let v = p(rng.range(-50.0, 50.0), rng.range(-50.0, 50.0));
            let base = rng.range(0.0, 2.0 * PI);
            let want = rng.range(-PI + 1e-6, PI - 1e-6);
            let a = v + Vec2::from_angle(base) * rng.range(0.5, 20.0);
            let b = v + Vec2::from_angle(base + want) * rng.range(0.5, 20.0);
            assert!((signed_angle_at(v, a, b) - want).abs() < 1e-9);
            assert!((angle_at(v, a, b) - want.abs()).abs() < 1e-9);
        }
    }

    #[test]
    fn point_segment_distance_cases() {
        let a = p(0.0, 0.0);
        let b = p(10.0, 0.0);
        // Perpendicular foot inside the segment.
        assert_eq!(point_segment_distance(p(4.0, 3.0), a, b), 3.0);
        assert_eq!(
            closest_point_on_segment(p(4.0, 3.0), a, b),
            (p(4.0, 0.0), 0.4)
        );
        // Beyond either end: distance to the endpoint.
        assert_eq!(point_segment_distance(p(-3.0, 4.0), a, b), 5.0);
        assert_eq!(point_segment_distance(p(13.0, -4.0), a, b), 5.0);
        assert_eq!(closest_point_on_segment(p(13.0, -4.0), a, b), (b, 1.0));
        // On the segment.
        assert_eq!(point_segment_distance(p(7.0, 0.0), a, b), 0.0);
        // Zero-length segment.
        assert_eq!(point_segment_distance(p(3.0, 4.0), a, a), 5.0);
        assert_eq!(closest_point_on_segment(p(3.0, 4.0), a, a), (a, 0.0));
    }

    /// Property: the reported distance is never beaten by any sampled point of the segment.
    #[test]
    fn prop_point_segment_distance_is_minimal() {
        let mut rng = Rng::new(33);
        for _ in 0..300 {
            let a = p(rng.range(-50.0, 50.0), rng.range(-50.0, 50.0));
            let b = p(rng.range(-50.0, 50.0), rng.range(-50.0, 50.0));
            let q = p(rng.range(-80.0, 80.0), rng.range(-80.0, 80.0));
            let d = point_segment_distance(q, a, b);
            for i in 0..=100 {
                let s = a.lerp(b, i as f64 / 100.0);
                assert!(d <= s.distance(q) + 1e-9);
            }
        }
    }

    #[test]
    fn segments_crossing() {
        let hit = segment_intersection(p(0.0, 0.0), p(10.0, 10.0), p(0.0, 10.0), p(10.0, 0.0));
        match hit {
            SegmentIntersection::Point { point, t, u } => {
                assert!(near_pt(point, p(5.0, 5.0)));
                assert!(near(t, 0.5) && near(u, 0.5));
            }
            other => panic!("expected a point, got {other:?}"),
        }
        assert_eq!(hit.point(), Some(p(5.0, 5.0)));
    }

    #[test]
    fn segments_missing() {
        // Lines cross, segments do not reach.
        assert_eq!(
            segment_intersection(p(0.0, 0.0), p(1.0, 1.0), p(0.0, 10.0), p(10.0, 0.0)),
            SegmentIntersection::None
        );
        // Parallel, apart.
        assert_eq!(
            segment_intersection(p(0.0, 0.0), p(10.0, 0.0), p(0.0, 1.0), p(10.0, 1.0)),
            SegmentIntersection::None
        );
        // Collinear, apart.
        assert_eq!(
            segment_intersection(p(0.0, 0.0), p(1.0, 0.0), p(2.0, 0.0), p(3.0, 0.0)),
            SegmentIntersection::None
        );
        assert_eq!(SegmentIntersection::None.point(), None);
    }

    #[test]
    fn segments_touching_at_endpoints() {
        // T-junction: the end of one lies on the other.
        let hit = segment_intersection(p(0.0, 0.0), p(10.0, 0.0), p(4.0, 0.0), p(4.0, 7.0));
        assert_eq!(
            hit,
            SegmentIntersection::Point {
                point: p(4.0, 0.0),
                t: 0.4,
                u: 0.0
            }
        );
        // Shared endpoint, at an angle.
        let hit = segment_intersection(p(0.0, 0.0), p(5.0, 5.0), p(5.0, 5.0), p(9.0, 0.0));
        assert_eq!(
            hit,
            SegmentIntersection::Point {
                point: p(5.0, 5.0),
                t: 1.0,
                u: 0.0
            }
        );
        // Collinear, meeting end to end.
        let hit = segment_intersection(p(0.0, 0.0), p(2.0, 0.0), p(2.0, 0.0), p(5.0, 0.0));
        assert_eq!(
            hit,
            SegmentIntersection::Point {
                point: p(2.0, 0.0),
                t: 1.0,
                u: 0.0
            }
        );
    }

    #[test]
    fn segments_overlapping() {
        let hit = segment_intersection(p(0.0, 0.0), p(10.0, 0.0), p(4.0, 0.0), p(15.0, 0.0));
        assert_eq!(hit, SegmentIntersection::Overlap(p(4.0, 0.0), p(10.0, 0.0)));
        assert_eq!(hit.point(), None);
        // Opposite direction, second inside first.
        let hit = segment_intersection(p(0.0, 0.0), p(10.0, 10.0), p(8.0, 8.0), p(2.0, 2.0));
        match hit {
            SegmentIntersection::Overlap(a, b) => {
                assert!(near_pt(a, p(2.0, 2.0)) && near_pt(b, p(8.0, 8.0)));
            }
            other => panic!("expected an overlap, got {other:?}"),
        }
    }

    #[test]
    fn segments_degenerate() {
        let a = p(3.0, 3.0);
        assert_eq!(
            segment_intersection(a, a, a, a),
            SegmentIntersection::Point {
                point: a,
                t: 0.0,
                u: 0.0
            }
        );
        assert_eq!(
            segment_intersection(a, a, p(4.0, 4.0), p(4.0, 4.0)),
            SegmentIntersection::None
        );
        // A point on a segment, either way round.
        assert_eq!(
            segment_intersection(a, a, p(0.0, 0.0), p(6.0, 6.0)),
            SegmentIntersection::Point {
                point: a,
                t: 0.0,
                u: 0.5
            }
        );
        assert_eq!(
            segment_intersection(p(0.0, 0.0), p(6.0, 6.0), a, a),
            SegmentIntersection::Point {
                point: a,
                t: 0.5,
                u: 0.0
            }
        );
        assert_eq!(
            segment_intersection(a, a, p(0.0, 0.0), p(6.0, 0.0)),
            SegmentIntersection::None
        );
    }

    /// Property: build two segments through a known point; the solver finds it. Then shorten one
    /// so that it stops short; the solver reports no intersection.
    #[test]
    fn prop_segment_intersection_recovers_planted_point() {
        let mut rng = Rng::new(34);
        for _ in 0..500 {
            let x = p(rng.range(-100.0, 100.0), rng.range(-100.0, 100.0));
            let th1 = rng.range(0.0, PI);
            let th2 = th1 + rng.range(0.2, PI - 0.2); // never near-parallel
            let d1 = Vec2::from_angle(th1);
            let d2 = Vec2::from_angle(th2);
            let (la, lb) = (rng.range(1.0, 50.0), rng.range(1.0, 50.0));
            let (lc, ld) = (rng.range(1.0, 50.0), rng.range(1.0, 50.0));
            let (a0, a1) = (x - d1 * la, x + d1 * lb);
            let (b0, b1) = (x - d2 * lc, x + d2 * ld);
            match segment_intersection(a0, a1, b0, b1) {
                SegmentIntersection::Point { point, t, u } => {
                    assert!(point.distance(x) < 1e-8);
                    assert!((t - la / (la + lb)).abs() < 1e-9);
                    assert!((u - lc / (lc + ld)).abs() < 1e-9);
                }
                other => panic!("expected a point, got {other:?}"),
            }
            assert!(line_intersection(a0, a1, b0, b1).unwrap().distance(x) < 1e-8);

            // Stop the second segment short of the crossing.
            let short = x - d2 * (lc * 0.01);
            assert_eq!(
                segment_intersection(a0, a1, b0, short),
                SegmentIntersection::None
            );
            // The infinite lines still cross there.
            assert!(line_intersection(a0, a1, b0, short).unwrap().distance(x) < 1e-6);
        }
    }

    #[test]
    fn line_intersection_cases() {
        assert_eq!(
            line_intersection(p(0.0, 0.0), p(1.0, 0.0), p(5.0, -3.0), p(5.0, -2.0)),
            Some(p(5.0, 0.0))
        );
        assert_eq!(
            line_intersection(p(0.0, 0.0), p(1.0, 1.0), p(0.0, 1.0), p(1.0, 2.0)),
            None
        );
        assert_eq!(
            line_intersection(p(0.0, 0.0), p(1.0, 1.0), p(2.0, 2.0), p(3.0, 3.0)),
            None
        );
        assert_eq!(
            line_intersection(p(0.0, 0.0), p(0.0, 0.0), p(2.0, 2.0), p(3.0, 3.0)),
            None
        );
    }
}
