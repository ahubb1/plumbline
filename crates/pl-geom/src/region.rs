//! Regions: a polygon with holes cut out of it.
//!
//! A region is an outer ring and any number of hole rings, each a list of vertices without the
//! first one repeated. Inside and outside follow the even-odd rule over all the rings together,
//! which is also how such a region is painted. The holes are expected to lie inside the outer
//! ring and not to overlap each other; the area functions say what they return otherwise.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use kurbo::{Point, Rect};

use crate::measure::{
    point_in_polygon, point_segment_distance, polygon_area, polygon_centroid, polygon_perimeter,
    segment_intersection, SegmentIntersection,
};

/// The area of a polygon with holes (cutouts): the outer area less the area of every hole.
///
/// Exact when the holes are inside the outer ring and do not overlap each other. Holes that
/// overlap, or that reach outside, take away more than the even-odd painting shows; the result
/// is never below zero.
pub fn polygon_area_with_holes(outer: &[Point], holes: &[Vec<Point>]) -> f64 {
    let cut: f64 = holes.iter().map(|h| polygon_area(h)).sum();
    (polygon_area(outer) - cut).max(0.0)
}

/// The length of every edge of a region: the outer perimeter plus the perimeter of each hole.
pub fn region_edge_length(outer: &[Point], holes: &[Vec<Point>]) -> f64 {
    polygon_perimeter(outer) + holes.iter().map(|h| polygon_perimeter(h)).sum::<f64>()
}

/// Whether a point is inside a region: inside the outer ring and in none of the holes.
pub fn point_in_region(p: Point, outer: &[Point], holes: &[Vec<Point>]) -> bool {
    point_in_polygon(p, outer) && !holes.iter().any(|h| point_in_polygon(p, h))
}

fn ring_distance(p: Point, ring: &[Point]) -> f64 {
    let n = ring.len();
    match n {
        0 => f64::INFINITY,
        1 => p.distance(ring[0]),
        _ => (0..n)
            .map(|i| point_segment_distance(p, ring[i], ring[(i + 1) % n]))
            .fold(f64::INFINITY, f64::min),
    }
}

/// The distance from `p` to the nearest edge of a region: positive inside it, negative outside
/// (or inside a hole).
pub fn region_signed_distance(p: Point, outer: &[Point], holes: &[Vec<Point>]) -> f64 {
    let d = holes
        .iter()
        .map(|h| ring_distance(p, h))
        .fold(ring_distance(p, outer), f64::min);
    if point_in_region(p, outer, holes) {
        d
    } else {
        -d
    }
}

/// Where a point is relative to a ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Inside,
    On,
    Outside,
}

/// The distance under which two rings' points count as the same: a few units in the last place
/// of their larger coordinates.
fn ring_tolerance(a: &[Point], b: &[Point]) -> f64 {
    let extent = a
        .iter()
        .chain(b)
        .map(|p| p.x.abs().max(p.y.abs()))
        .fold(1.0, f64::max);
    1e-9 * extent
}

fn side_of(p: Point, ring: &[Point], tol: f64) -> Side {
    if ring_distance(p, ring) <= tol {
        Side::On
    } else if point_in_polygon(p, ring) {
        Side::Inside
    } else {
        Side::Outside
    }
}

/// Every edge of `ring` split where it meets an edge of `other`: the middle of each piece. A
/// piece's middle is never on a crossing, so it is wholly inside, on or outside `other`.
fn piece_middles(ring: &[Point], other: &[Point]) -> Vec<Point> {
    let n = ring.len();
    let m = other.len();
    let mut middles = Vec::new();
    for i in 0..n {
        let (a0, a1) = (ring[i], ring[(i + 1) % n]);
        let r = a1 - a0;
        let len2 = r.hypot2();
        if len2 == 0.0 {
            continue;
        }
        let mut ts = vec![0.0, 1.0];
        for j in 0..m {
            let (b0, b1) = (other[j], other[(j + 1) % m]);
            match segment_intersection(a0, a1, b0, b1) {
                SegmentIntersection::Point { t, .. } => ts.push(t),
                SegmentIntersection::Overlap(p, q) => {
                    ts.push((p - a0).dot(r) / len2);
                    ts.push((q - a0).dot(r) / len2);
                }
                SegmentIntersection::None => {}
            }
        }
        ts.sort_by(f64::total_cmp);
        for w in ts.windows(2) {
            if w[1] - w[0] > 1e-12 {
                middles.push(a0 + r * ((w[0] + w[1]) / 2.0));
            }
        }
    }
    middles
}

/// A point well inside a ring (none for a ring with no area).
fn inner_point(ring: &[Point], tol: f64) -> Option<Point> {
    polygon_label_point(ring, &[], 0.0).and_then(|(p, d)| (d > tol).then_some(p))
}

/// True when `ring` lies within `outer`: no part of it is outside, though it may touch the
/// edge. Both are simple polygons (vertices without the first repeated). Unlike a test of its
/// corners alone, this finds an edge that crosses out through a notch of an L-shaped outline.
pub fn ring_within(ring: &[Point], outer: &[Point]) -> bool {
    let tol = ring_tolerance(ring, outer);
    ring.iter()
        .chain(piece_middles(ring, outer).iter())
        .all(|p| side_of(*p, outer, tol) != Side::Outside)
}

/// True when two rings overlap: they share some area, more than a common edge or corner. Holds
/// for rings that cross, for one inside the other, and for the same ring twice. Both are simple
/// polygons (vertices without the first repeated).
pub fn rings_overlap(a: &[Point], b: &[Point]) -> bool {
    let tol = ring_tolerance(a, b);
    let enters = |x: &[Point], y: &[Point]| {
        piece_middles(x, y)
            .iter()
            .any(|p| side_of(*p, y, tol) == Side::Inside)
    };
    if enters(a, b) || enters(b, a) {
        return true;
    }
    // Neither edge goes into the other ring: they are apart, touch, or are the same ring.
    let within = |x: &[Point], y: &[Point]| {
        inner_point(x, tol).is_some_and(|p| side_of(p, y, tol) == Side::Inside)
    };
    within(a, b) || within(b, a)
}

/// What is wrong with the holes of a region, if anything (see [`region_problem`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionProblem {
    /// The hole at this index has fewer than three points, or a coordinate that is not finite.
    BadHole(usize),
    /// The hole at this index reaches outside the outer ring.
    HoleOutside(usize),
    /// The holes at these indices overlap.
    HolesOverlap(usize, usize),
}

/// Checks that every hole of a region is a ring of at least three finite points within the
/// outer ring, and that no two holes overlap: what [`polygon_area_with_holes`] needs to be the
/// area the even-odd painting shows.
pub fn region_problem(outer: &[Point], holes: &[Vec<Point>]) -> Option<RegionProblem> {
    for (i, h) in holes.iter().enumerate() {
        if h.len() < 3 || h.iter().any(|p| !(p.x.is_finite() && p.y.is_finite())) {
            return Some(RegionProblem::BadHole(i));
        }
        if !ring_within(h, outer) {
            return Some(RegionProblem::HoleOutside(i));
        }
    }
    for i in 0..holes.len() {
        for j in i + 1..holes.len() {
            if rings_overlap(&holes[i], &holes[j]) {
                return Some(RegionProblem::HolesOverlap(i, j));
            }
        }
    }
    None
}

/// A square cell of the search for the visual centre.
struct Cell {
    centre: Point,
    half: f64,
    /// Distance from the centre to the region's edge (negative outside).
    distance: f64,
    /// The largest distance any point of the cell can have.
    potential: f64,
}

impl Cell {
    fn new(centre: Point, half: f64, outer: &[Point], holes: &[Vec<Point>]) -> Cell {
        let distance = region_signed_distance(centre, outer, holes);
        Cell {
            centre,
            half,
            distance,
            potential: distance + half * std::f64::consts::SQRT_2,
        }
    }
}

impl PartialEq for Cell {
    fn eq(&self, other: &Cell) -> bool {
        self.potential.total_cmp(&other.potential) == Ordering::Equal
    }
}

impl Eq for Cell {}

impl PartialOrd for Cell {
    fn partial_cmp(&self, other: &Cell) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cell {
    fn cmp(&self, other: &Cell) -> Ordering {
        self.potential.total_cmp(&other.potential)
    }
}

/// The visual centre of a region and how far it is from the nearest edge: the pole of
/// inaccessibility, the centre of the largest circle that fits inside the outer ring without
/// touching a hole. This is where a caption has the most room, and unlike the centroid it is
/// always inside the region (an L-shaped room, a ring).
///
/// The search stops when no point can be more than `precision` further from the edges than the
/// one found (`precision` is in the units of the vertices; values at or below zero mean one
/// hundredth of the region's larger side). A region with no area (fewer than three vertices,
/// or collinear ones) gives its centroid with a distance of zero; `None` only when `outer` is
/// empty or holds a coordinate that is not finite.
pub fn polygon_label_point(
    outer: &[Point],
    holes: &[Vec<Point>],
    precision: f64,
) -> Option<(Point, f64)> {
    let first = *outer.first()?;
    if outer
        .iter()
        .chain(holes.iter().flatten())
        .any(|p| !(p.x.is_finite() && p.y.is_finite()))
    {
        return None;
    }
    let bounds = outer
        .iter()
        .fold(Rect::from_points(first, first), |r, p| r.union_pt(*p));
    let fallback = polygon_centroid(outer).unwrap_or(first);
    let cell_size = bounds.width().min(bounds.height());
    if outer.len() < 3 || cell_size <= 0.0 || polygon_area(outer) <= 0.0 {
        return Some((fallback, 0.0));
    }
    let precision = if precision > 0.0 {
        precision
    } else {
        bounds.width().max(bounds.height()) / 100.0
    };

    let mut queue: BinaryHeap<Cell> = BinaryHeap::new();
    let half = cell_size / 2.0;
    let mut x = bounds.x0;
    while x < bounds.x1 {
        let mut y = bounds.y0;
        while y < bounds.y1 {
            queue.push(Cell::new(
                Point::new(x + half, y + half),
                half,
                outer,
                holes,
            ));
            y += cell_size;
        }
        x += cell_size;
    }

    // The centroid and the middle of the box are good first guesses for ordinary shapes.
    let mut best = Cell::new(fallback, 0.0, outer, holes);
    let middle = Cell::new(bounds.center(), 0.0, outer, holes);
    if middle.distance > best.distance {
        best = middle;
    }

    // A cap on the work for hostile input; ordinary shapes finish in a few hundred cells.
    let mut budget = 50_000usize;
    while let Some(cell) = queue.pop() {
        if cell.distance > best.distance {
            best = Cell::new(cell.centre, 0.0, outer, holes);
        }
        if cell.potential - best.distance <= precision || budget == 0 {
            continue;
        }
        budget -= 1;
        let h = cell.half / 2.0;
        for (dx, dy) in [(-h, -h), (h, -h), (-h, h), (h, h)] {
            queue.push(Cell::new(
                Point::new(cell.centre.x + dx, cell.centre.y + dy),
                h,
                outer,
                holes,
            ));
        }
    }
    if best.distance <= 0.0 {
        // Nothing inside was found (a sliver thinner than the precision).
        return Some((fallback, 0.0));
    }
    Some((best.centre, best.distance))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(v: &[(f64, f64)]) -> Vec<Point> {
        v.iter().map(|(x, y)| Point::new(*x, *y)).collect()
    }

    fn square(x: f64, y: f64, side: f64) -> Vec<Point> {
        pts(&[(x, y), (x + side, y), (x + side, y + side), (x, y + side)])
    }

    #[test]
    fn area_with_and_without_cutouts() {
        let outer = square(0.0, 0.0, 100.0);
        assert_eq!(polygon_area_with_holes(&outer, &[]), 10_000.0);
        let holes = vec![square(10.0, 10.0, 20.0), square(50.0, 50.0, 30.0)];
        assert_eq!(
            polygon_area_with_holes(&outer, &holes),
            10_000.0 - 400.0 - 900.0
        );
        // Winding does not matter.
        let mut reversed = holes.clone();
        reversed[0].reverse();
        let mut outer_cw = outer.clone();
        outer_cw.reverse();
        assert_eq!(polygon_area_with_holes(&outer_cw, &reversed), 8_700.0);
        // A triangular hole in an L-shaped room.
        let l = pts(&[
            (0.0, 0.0),
            (60.0, 0.0),
            (60.0, 20.0),
            (20.0, 20.0),
            (20.0, 50.0),
            (0.0, 50.0),
        ]);
        let tri = vec![pts(&[(5.0, 5.0), (15.0, 5.0), (5.0, 15.0)])];
        assert_eq!(polygon_area_with_holes(&l, &tri), 1800.0 - 50.0);
        // Never negative, and a degenerate outline has no area.
        assert_eq!(
            polygon_area_with_holes(&square(0.0, 0.0, 1.0), &[square(0.0, 0.0, 5.0)]),
            0.0
        );
        assert_eq!(polygon_area_with_holes(&outer[..2], &holes), 0.0);
        assert_eq!(region_edge_length(&outer, &holes), 400.0 + 80.0 + 120.0);
    }

    #[test]
    fn inside_a_region_is_outside_its_holes() {
        let outer = square(0.0, 0.0, 100.0);
        let holes = vec![square(40.0, 40.0, 20.0)];
        assert!(point_in_region(Point::new(10.0, 10.0), &outer, &holes));
        assert!(!point_in_region(Point::new(50.0, 50.0), &outer, &holes));
        assert!(!point_in_region(Point::new(150.0, 50.0), &outer, &holes));
        assert_eq!(
            region_signed_distance(Point::new(10.0, 50.0), &outer, &holes),
            10.0
        );
        assert_eq!(
            region_signed_distance(Point::new(35.0, 50.0), &outer, &holes),
            5.0
        );
        assert_eq!(
            region_signed_distance(Point::new(50.0, 50.0), &outer, &holes),
            -10.0
        );
        assert_eq!(
            region_signed_distance(Point::new(-7.0, 50.0), &outer, &holes),
            -7.0
        );
    }

    #[test]
    fn the_label_point_of_simple_shapes_is_their_middle() {
        let (p, d) = polygon_label_point(&square(10.0, 20.0, 100.0), &[], 0.01).unwrap();
        assert!(
            (p.x - 60.0).abs() < 0.02 && (p.y - 70.0).abs() < 0.02,
            "{p:?}"
        );
        assert!((d - 50.0).abs() < 0.02);
        // A wide rectangle: anywhere on its centre line is as good; the distance is the proof.
        let wide = pts(&[(0.0, 0.0), (300.0, 0.0), (300.0, 40.0), (0.0, 40.0)]);
        let (p, d) = polygon_label_point(&wide, &[], 0.01).unwrap();
        assert!(
            (p.y - 20.0).abs() < 0.02 && (d - 20.0).abs() < 0.02,
            "{p:?} {d}"
        );
        // A 3-4-5 triangle: the incentre, with inradius 1.
        let tri = pts(&[(0.0, 0.0), (4.0, 0.0), (0.0, 3.0)]);
        let (p, d) = polygon_label_point(&tri, &[], 0.001).unwrap();
        assert!(
            (p.x - 1.0).abs() < 0.01 && (p.y - 1.0).abs() < 0.01,
            "{p:?}"
        );
        assert!((d - 1.0).abs() < 0.002);
    }

    #[test]
    fn the_label_point_stays_inside_where_the_centroid_does_not() {
        // An L: the centroid (21.67, 16.67) is 1.67 from the inner corner's edges; the
        // visual centre sits in the wide arm.
        let l = pts(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 20.0),
            (20.0, 20.0),
            (20.0, 100.0),
            (0.0, 100.0),
        ]);
        let (p, d) = polygon_label_point(&l, &[], 0.05).unwrap();
        assert!(point_in_polygon(p, &l));
        // The largest circle touches both outer edges at the corner and the inner corner
        // point: r = 20 (2 - sqrt 2).
        let want = 20.0 * (2.0 - std::f64::consts::SQRT_2);
        assert!((d - want).abs() < 0.06, "{d} vs {want}");

        // A U whose centroid is in the gap between the arms.
        let u = pts(&[
            (0.0, 0.0),
            (90.0, 0.0),
            (90.0, 90.0),
            (60.0, 90.0),
            (60.0, 30.0),
            (30.0, 30.0),
            (30.0, 90.0),
            (0.0, 90.0),
        ]);
        let centroid = polygon_centroid(&u).unwrap();
        assert!(!point_in_polygon(Point::new(45.0, 60.0), &u));
        let (p, d) = polygon_label_point(&u, &[], 0.05).unwrap();
        assert!(point_in_polygon(p, &u), "{p:?} (centroid {centroid:?})");
        // Where an arm meets the base: touching two outer edges and the inner corner point.
        let want = 30.0 * (2.0 - std::f64::consts::SQRT_2);
        assert!((d - want).abs() < 0.06, "{d} vs {want}");

        // A square with a square hole in the middle: the centroid is in the hole.
        let outer = square(0.0, 0.0, 100.0);
        let holes = vec![square(30.0, 30.0, 40.0)];
        let (p, d) = polygon_label_point(&outer, &holes, 0.05).unwrap();
        assert!(point_in_region(p, &outer, &holes), "{p:?}");
        // The best spots are the four corners: a circle touching two outer edges and the
        // hole's corner. r satisfies (30 - r) sqrt 2 = r.
        let want = 30.0 * std::f64::consts::SQRT_2 / (1.0 + std::f64::consts::SQRT_2);
        assert!((d - want).abs() < 0.06, "{d} vs {want}");
    }

    #[test]
    fn degenerate_regions_fall_back_to_the_centroid() {
        assert_eq!(polygon_label_point(&[], &[], 1.0), None);
        let one = pts(&[(3.0, 4.0)]);
        assert_eq!(
            polygon_label_point(&one, &[], 1.0),
            Some((Point::new(3.0, 4.0), 0.0))
        );
        let line = pts(&[(0.0, 0.0), (10.0, 0.0), (20.0, 0.0)]);
        assert_eq!(
            polygon_label_point(&line, &[], 1.0),
            Some((Point::new(10.0, 0.0), 0.0))
        );
        let nan = pts(&[(0.0, 0.0), (f64::NAN, 1.0), (1.0, 1.0)]);
        assert_eq!(polygon_label_point(&nan, &[], 1.0), None);
        // A default precision is chosen when none is given.
        let (p, _) = polygon_label_point(&square(0.0, 0.0, 10.0), &[], 0.0).unwrap();
        assert!((p.x - 5.0).abs() < 0.2 && (p.y - 5.0).abs() < 0.2);
    }

    fn l_shape() -> Vec<Point> {
        pts(&[
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 4.0),
            (4.0, 4.0),
            (4.0, 10.0),
            (0.0, 10.0),
        ])
    }

    #[test]
    fn a_hole_is_within_only_when_none_of_its_edges_leaves_the_outline() {
        let outer = square(0.0, 0.0, 10.0);
        assert!(ring_within(&square(1.0, 1.0, 5.0), &outer));
        // Touching the edge, or being the whole outline, is within.
        assert!(ring_within(&square(0.0, 0.0, 5.0), &outer));
        assert!(ring_within(&outer, &outer));
        assert!(!ring_within(&square(8.0, 8.0, 5.0), &outer));
        // Every corner of this triangle is inside the L, but its long edge crosses the notch.
        let triangle = pts(&[(1.0, 1.0), (9.0, 1.0), (1.0, 9.0)]);
        assert!(triangle.iter().all(|p| point_in_polygon(*p, &l_shape())));
        assert!(!ring_within(&triangle, &l_shape()));
        // One that stays in the L's arms is fine, even along the notch's edges.
        let inside = pts(&[(1.0, 1.0), (9.0, 1.0), (4.0, 4.0), (1.0, 9.0)]);
        assert!(ring_within(&inside, &l_shape()));
    }

    #[test]
    fn overlapping_duplicate_and_containing_rings_overlap() {
        let a = square(1.0, 1.0, 5.0);
        // Crossing edges.
        assert!(rings_overlap(&a, &square(4.0, 4.0, 5.0)));
        // The same ring twice, either way round.
        assert!(rings_overlap(&a, &a));
        let mut reversed = a.clone();
        reversed.reverse();
        assert!(rings_overlap(&a, &reversed));
        // One inside the other, apart or sharing edges.
        assert!(rings_overlap(&square(0.0, 0.0, 9.0), &a));
        assert!(rings_overlap(&a, &square(0.0, 0.0, 9.0)));
        assert!(rings_overlap(&square(1.0, 1.0, 2.0), &a));
        // Overlapping along shared edges with no crossing: [0,2]x[0,2] and [1,3]x[0,2].
        let left = pts(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let right = pts(&[(1.0, 0.0), (3.0, 0.0), (3.0, 2.0), (1.0, 2.0)]);
        assert!(rings_overlap(&left, &right));
        // Apart, sharing an edge, or a corner: no overlap.
        assert!(!rings_overlap(&a, &square(7.0, 7.0, 1.0)));
        assert!(!rings_overlap(&a, &square(6.0, 1.0, 5.0)));
        assert!(!rings_overlap(&a, &square(6.0, 6.0, 1.0)));
    }

    #[test]
    fn region_problems_name_the_hole() {
        let outer = square(0.0, 0.0, 10.0);
        assert_eq!(region_problem(&outer, &[square(1.0, 1.0, 2.0)]), None);
        assert_eq!(
            region_problem(&outer, &[square(1.0, 1.0, 5.0), square(4.0, 4.0, 5.0)]),
            Some(RegionProblem::HolesOverlap(0, 1))
        );
        assert_eq!(
            region_problem(&outer, &[square(1.0, 1.0, 2.0), square(8.0, 8.0, 5.0)]),
            Some(RegionProblem::HoleOutside(1))
        );
        assert_eq!(
            region_problem(&outer, &[pts(&[(1.0, 1.0), (2.0, 2.0)])]),
            Some(RegionProblem::BadHole(0))
        );
        // Where the holes are valid, the area is what even-odd painting covers.
        let holes = vec![square(1.0, 1.0, 3.0), square(4.0, 1.0, 3.0)];
        assert_eq!(region_problem(&outer, &holes), None);
        assert_eq!(polygon_area_with_holes(&outer, &holes), 100.0 - 18.0);
    }
}
