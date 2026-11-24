//! The scene model, its R-tree and the snap queries.

use std::time::Duration;

use pl_geom::{closest_point_on_segment, segment_intersection, Point, Rect, SegmentIntersection};
use rstar::{RTree, RTreeObject, AABB};

/// Where a segment came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// A straight line in the content stream (including a closing edge).
    Line,
    /// One edge of a rectangle (`re`, or a path hayro recognised as one).
    RectEdge,
    /// One piece of a flattened Bezier curve.
    CurvePiece,
}

/// One straight piece of page content, in page space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    /// Start point.
    pub a: Point,
    /// End point.
    pub b: Point,
    /// Where the segment came from.
    pub kind: SegmentKind,
    /// Whether `a` is a real vertex of the drawing. False for the interior
    /// joints of a flattened curve, which are not endpoint snap targets.
    pub a_vertex: bool,
    /// Whether `b` is a real vertex of the drawing.
    pub b_vertex: bool,
    /// Index of the paint operation that produced the segment, in drawing
    /// order. Segments of one path share it.
    pub path: u32,
}

impl Segment {
    /// A plain line segment whose two ends are both vertices.
    pub fn line(a: Point, b: Point) -> Segment {
        Segment {
            a,
            b,
            kind: SegmentKind::Line,
            a_vertex: true,
            b_vertex: true,
            path: 0,
        }
    }

    /// Length in user units.
    pub fn length(&self) -> f64 {
        self.a.distance(self.b)
    }

    /// The midpoint.
    pub fn midpoint(&self) -> Point {
        self.a.midpoint(self.b)
    }

    pub(crate) fn is_usable(&self) -> bool {
        self.a.x.is_finite()
            && self.a.y.is_finite()
            && self.b.x.is_finite()
            && self.b.y.is_finite()
            && self.a != self.b
    }
}

/// A rectangle drawn in the content. It may be rotated by the CTM, so it is
/// stored as its four corners in drawing order; its edges are also in
/// [`Scene::segments`] as four consecutive [`SegmentKind::RectEdge`] items.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneRect {
    /// The four corners in page space.
    pub corners: [Point; 4],
    /// Index of the first of its four edges in [`Scene::segments`], or
    /// `None` when the rectangle was degenerate and its edges were dropped.
    pub first_segment: Option<u32>,
}

/// The kind of a snap, in priority order: the first kind that has a
/// candidate inside the radius wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SnapKind {
    /// A vertex: the end of a line, a rectangle corner, the end of a curve.
    Endpoint,
    /// The crossing of two segments.
    Intersection,
    /// The middle of a line or rectangle edge.
    Midpoint,
    /// The closest point on a segment.
    Nearest,
}

impl SnapKind {
    /// All kinds, highest priority first.
    pub const PRIORITY: [SnapKind; 4] = [
        SnapKind::Endpoint,
        SnapKind::Intersection,
        SnapKind::Midpoint,
        SnapKind::Nearest,
    ];
}

/// Which snap kinds a query may return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapModes {
    /// Allow [`SnapKind::Endpoint`].
    pub endpoint: bool,
    /// Allow [`SnapKind::Intersection`].
    pub intersection: bool,
    /// Allow [`SnapKind::Midpoint`].
    pub midpoint: bool,
    /// Allow [`SnapKind::Nearest`].
    pub nearest: bool,
}

impl SnapModes {
    /// Every kind.
    pub const ALL: SnapModes = SnapModes {
        endpoint: true,
        intersection: true,
        midpoint: true,
        nearest: true,
    };
    /// No kind; queries return nothing.
    pub const NONE: SnapModes = SnapModes {
        endpoint: false,
        intersection: false,
        midpoint: false,
        nearest: false,
    };

    /// Only the given kind.
    pub fn only(kind: SnapKind) -> SnapModes {
        let mut m = SnapModes::NONE;
        match kind {
            SnapKind::Endpoint => m.endpoint = true,
            SnapKind::Intersection => m.intersection = true,
            SnapKind::Midpoint => m.midpoint = true,
            SnapKind::Nearest => m.nearest = true,
        }
        m
    }

    /// Whether the kind is allowed.
    pub fn allows(&self, kind: SnapKind) -> bool {
        match kind {
            SnapKind::Endpoint => self.endpoint,
            SnapKind::Intersection => self.intersection,
            SnapKind::Midpoint => self.midpoint,
            SnapKind::Nearest => self.nearest,
        }
    }
}

impl Default for SnapModes {
    fn default() -> Self {
        SnapModes::ALL
    }
}

/// The result of a snap query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapHit {
    /// The snapped point in page space. For an endpoint this is the stored
    /// vertex, bit for bit, not a projection of the cursor.
    pub point: Point,
    /// What was snapped to.
    pub kind: SnapKind,
    /// Distance from the cursor to `point`, in user units.
    pub distance: f64,
    /// Index into [`Scene::segments`] of the segment snapped to.
    pub segment: u32,
    /// The second segment of an intersection.
    pub other_segment: Option<u32>,
}

/// Figures recorded while a scene was built.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SceneStats {
    /// Paint operations seen (paths and rectangles).
    pub paint_ops: u32,
    /// Time spent parsing and interpreting the page.
    pub interpret_time: Duration,
    /// Time spent building the R-tree.
    pub index_time: Duration,
    /// Estimated heap bytes held by the scene.
    pub approx_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    a: [f64; 2],
    b: [f64; 2],
    index: u32,
}

impl RTreeObject for Node {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(self.a, self.b)
    }
}

/// Estimated heap bytes per stored segment: the segment itself, its R-tree
/// leaf and a share of the tree's inner nodes.
pub(crate) const BYTES_PER_SEGMENT: usize =
    std::mem::size_of::<Segment>() + std::mem::size_of::<Node>() * 3 / 2;

/// At most this many of the closest segments take part in one query.
const MAX_CANDIDATES: usize = 256;
/// At most this many of the closest segments are intersected pairwise.
const MAX_INTERSECTION_CANDIDATES: usize = 64;
/// Two candidate points closer than this are treated as the same point.
const SAME_POINT: f64 = 1e-9;

/// The indexed vector geometry of one page.
#[derive(Debug)]
pub struct Scene {
    segments: Vec<Segment>,
    rects: Vec<SceneRect>,
    tree: RTree<Node>,
    bounds: Option<Rect>,
    stats: SceneStats,
}

impl Scene {
    /// Builds a scene from segments already in page space. Zero-length and
    /// non-finite segments are dropped.
    pub fn from_segments(mut segments: Vec<Segment>) -> Scene {
        segments.retain(Segment::is_usable);
        Scene::from_parts(segments, Vec::new(), SceneStats::default())
    }

    /// `segments` must all be usable (the collector guarantees it), so that
    /// `SceneRect::first_segment` stays valid.
    pub(crate) fn from_parts(
        mut segments: Vec<Segment>,
        rects: Vec<SceneRect>,
        mut stats: SceneStats,
    ) -> Scene {
        segments.shrink_to_fit();
        let started = std::time::Instant::now();
        let mut bounds: Option<Rect> = None;
        let nodes: Vec<Node> = segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let r = Rect::from_points(s.a, s.b);
                bounds = Some(bounds.map_or(r, |b| b.union(r)));
                Node {
                    a: [s.a.x, s.a.y],
                    b: [s.b.x, s.b.y],
                    index: i as u32,
                }
            })
            .collect();
        let tree = RTree::bulk_load(nodes);
        stats.index_time = started.elapsed();
        stats.approx_bytes =
            segments.len() * BYTES_PER_SEGMENT + rects.len() * std::mem::size_of::<SceneRect>();
        Scene {
            segments,
            rects,
            tree,
            bounds,
            stats,
        }
    }

    /// All segments, in drawing order.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The rectangles drawn on the page.
    pub fn rects(&self) -> &[SceneRect] {
        &self.rects
    }

    /// Number of segments.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Whether the page has no vector geometry.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// The bounding box of all segments, if there are any.
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds
    }

    /// Build figures.
    pub fn stats(&self) -> SceneStats {
        self.stats
    }

    /// Indices of the segments whose bounding box touches `rect`, ascending.
    pub fn segments_in_rect(&self, rect: Rect) -> Vec<u32> {
        let rect = rect.abs();
        let mut out: Vec<u32> = self
            .tree
            .locate_in_envelope_intersecting(AABB::from_corners(
                [rect.x0, rect.y0],
                [rect.x1, rect.y1],
            ))
            .map(|n| n.index)
            .collect();
        out.sort_unstable();
        out
    }

    /// The segments within `radius` of `cursor`, closest first, capped at an
    /// internal limit. Each item is (segment index, closest point, distance).
    fn candidates(&self, cursor: Point, radius: f64) -> Vec<(u32, Point, f64)> {
        let envelope = AABB::from_corners(
            [cursor.x - radius, cursor.y - radius],
            [cursor.x + radius, cursor.y + radius],
        );
        let mut found: Vec<(u32, Point, f64)> = self
            .tree
            .locate_in_envelope_intersecting(envelope)
            .filter_map(|n| {
                let s = &self.segments[n.index as usize];
                let (p, _) = closest_point_on_segment(cursor, s.a, s.b);
                let d = p.distance(cursor);
                (d <= radius).then_some((n.index, p, d))
            })
            .collect();
        // Distance first, then index, so results do not depend on tree order.
        found.sort_unstable_by(|x, y| x.2.total_cmp(&y.2).then(x.0.cmp(&y.0)));
        found.truncate(MAX_CANDIDATES);
        found
    }

    /// The best candidate of each allowed kind within `radius` (user units)
    /// of `cursor` (page space), highest priority first.
    pub fn snap_candidates(&self, cursor: Point, radius: f64, modes: SnapModes) -> Vec<SnapHit> {
        if !(radius.is_finite() && radius >= 0.0 && cursor.x.is_finite() && cursor.y.is_finite()) {
            return Vec::new();
        }
        let near = self.candidates(cursor, radius);
        if near.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(4);

        if modes.endpoint {
            let mut best: Option<SnapHit> = None;
            for &(index, _, _) in &near {
                let s = &self.segments[index as usize];
                for (p, is_vertex) in [(s.a, s.a_vertex), (s.b, s.b_vertex)] {
                    let d = p.distance(cursor);
                    if is_vertex && d <= radius && best.is_none_or(|b| d < b.distance) {
                        best = Some(SnapHit {
                            point: p,
                            kind: SnapKind::Endpoint,
                            distance: d,
                            segment: index,
                            other_segment: None,
                        });
                    }
                }
            }
            out.extend(best);
        }

        if modes.intersection {
            let pool = &near[..near.len().min(MAX_INTERSECTION_CANDIDATES)];
            // (hit, number of coordinates known exactly)
            let mut best: Option<(SnapHit, u8)> = None;
            for (i, &(ia, _, _)) in pool.iter().enumerate() {
                for &(ib, _, _) in &pool[i + 1..] {
                    let sa = &self.segments[ia as usize];
                    let sb = &self.segments[ib as usize];
                    if is_flattening_joint(ia, sa, ib, sb) {
                        continue;
                    }
                    let Some((p, exact)) = exact_intersection(sa, sb) else {
                        continue;
                    };
                    let d = p.distance(cursor);
                    if d > radius {
                        continue;
                    }
                    let better = match &best {
                        None => true,
                        Some((b, b_exact)) => {
                            d < b.distance - SAME_POINT
                                || ((d - b.distance).abs() <= SAME_POINT && exact > *b_exact)
                        }
                    };
                    if better {
                        let hit = SnapHit {
                            point: p,
                            kind: SnapKind::Intersection,
                            distance: d,
                            segment: ia.min(ib),
                            other_segment: Some(ia.max(ib)),
                        };
                        best = Some((hit, exact));
                    }
                }
            }
            out.extend(best.map(|(hit, _)| hit));
        }

        if modes.midpoint {
            let mut best: Option<SnapHit> = None;
            for &(index, _, _) in &near {
                let s = &self.segments[index as usize];
                if s.kind == SegmentKind::CurvePiece {
                    continue;
                }
                let p = s.midpoint();
                let d = p.distance(cursor);
                if d <= radius && best.is_none_or(|b| d < b.distance) {
                    best = Some(SnapHit {
                        point: p,
                        kind: SnapKind::Midpoint,
                        distance: d,
                        segment: index,
                        other_segment: None,
                    });
                }
            }
            out.extend(best);
        }

        if modes.nearest {
            let (index, p, d) = near[0];
            out.push(SnapHit {
                point: p,
                kind: SnapKind::Nearest,
                distance: d,
                segment: index,
                other_segment: None,
            });
        }
        out
    }

    /// Snaps `cursor` (page space) to the content within `radius` (user
    /// units). Kinds are tried in [`SnapKind::PRIORITY`] order and the first
    /// kind with a candidate inside the radius wins, its closest candidate
    /// being returned. `None` means nothing is in range.
    pub fn snap(&self, cursor: Point, radius: f64, modes: SnapModes) -> Option<SnapHit> {
        self.snap_candidates(cursor, radius, modes)
            .into_iter()
            .next()
    }
}

/// Whether two segments are consecutive pieces of one flattened curve, which
/// meet at a joint that exists only because of the flattening.
fn is_flattening_joint(ia: u32, sa: &Segment, ib: u32, sb: &Segment) -> bool {
    let (first, second) = if ia < ib { (sa, sb) } else { (sb, sa) };
    ia.abs_diff(ib) == 1
        && first.kind == SegmentKind::CurvePiece
        && second.kind == SegmentKind::CurvePiece
        && first.path == second.path
        && !first.b_vertex
        && !second.a_vertex
        && first.b == second.a
}

/// The crossing point of two segments, tidied so that it is exact where the
/// input allows: a crossing that lands on a segment end returns that end, and
/// a coordinate fixed by a horizontal or vertical segment is copied from it.
/// The second value counts the coordinates known exactly (0 to 2).
fn exact_intersection(s: &Segment, t: &Segment) -> Option<(Point, u8)> {
    let SegmentIntersection::Point { point, .. } = segment_intersection(s.a, s.b, t.a, t.b) else {
        return None;
    };
    for end in [s.a, s.b, t.a, t.b] {
        if end.distance(point) <= SAME_POINT {
            return Some((end, 2));
        }
    }
    let mut p = point;
    let (mut x_exact, mut y_exact) = (false, false);
    for seg in [s, t] {
        if seg.a.x == seg.b.x {
            p.x = seg.a.x;
            x_exact = true;
        }
        if seg.a.y == seg.b.y {
            p.y = seg.a.y;
            y_exact = true;
        }
    }
    Some((p, x_exact as u8 + y_exact as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn cross() -> Scene {
        Scene::from_segments(vec![
            Segment::line(pt(0.0, 0.0), pt(100.0, 0.0)),
            Segment::line(pt(50.0, -50.0), pt(50.0, 50.0)),
            Segment::line(pt(0.0, 10.0), pt(30.0, 40.0)),
        ])
    }

    #[test]
    fn empty_scene_snaps_to_nothing() {
        let scene = Scene::from_segments(Vec::new());
        assert!(scene.is_empty());
        assert_eq!(scene.bounds(), None);
        assert_eq!(scene.snap(pt(0.0, 0.0), 10.0, SnapModes::ALL), None);
    }

    #[test]
    fn degenerate_segments_are_dropped() {
        let scene = Scene::from_segments(vec![
            Segment::line(pt(1.0, 1.0), pt(1.0, 1.0)),
            Segment::line(pt(f64::NAN, 0.0), pt(1.0, 1.0)),
            Segment::line(pt(0.0, 0.0), pt(f64::INFINITY, 1.0)),
            Segment::line(pt(0.0, 0.0), pt(2.0, 0.0)),
        ]);
        assert_eq!(scene.len(), 1);
        assert_eq!(scene.bounds(), Some(Rect::new(0.0, 0.0, 2.0, 0.0)));
    }

    #[test]
    fn priority_order_is_endpoint_intersection_midpoint_nearest() {
        let scene = cross();
        // Near the crossing at (50, 0), which is also the midpoint of both lines.
        let all = scene.snap_candidates(pt(50.4, 0.3), 2.0, SnapModes::ALL);
        let kinds: Vec<_> = all.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            [
                SnapKind::Intersection,
                SnapKind::Midpoint,
                SnapKind::Nearest
            ]
        );
        assert_eq!(all[0].point, pt(50.0, 0.0));
        assert_eq!((all[0].segment, all[0].other_segment), (0, Some(1)));
        assert_eq!(all[1].point, pt(50.0, 0.0));
        // The nearest point is the foot of the perpendicular, not the crossing.
        assert_eq!(all[2].point, pt(50.4, 0.0));

        // An endpoint further away than a midpoint still wins inside the radius.
        let hit = scene.snap(pt(14.0, 23.0), 30.0, SnapModes::ALL).unwrap();
        assert_eq!(hit.kind, SnapKind::Endpoint);
        assert_eq!(hit.point, pt(0.0, 10.0));
    }

    #[test]
    fn modes_filter_kinds() {
        let scene = cross();
        let cursor = pt(50.4, 0.3);
        assert_eq!(scene.snap(cursor, 2.0, SnapModes::NONE), None);
        let hit = scene
            .snap(cursor, 2.0, SnapModes::only(SnapKind::Nearest))
            .unwrap();
        assert_eq!(hit.kind, SnapKind::Nearest);
        let hit = scene
            .snap(cursor, 2.0, SnapModes::only(SnapKind::Midpoint))
            .unwrap();
        assert_eq!((hit.kind, hit.point), (SnapKind::Midpoint, pt(50.0, 0.0)));
        assert!(SnapModes::default().allows(SnapKind::Endpoint));
        assert!(!SnapModes::only(SnapKind::Nearest).allows(SnapKind::Endpoint));
    }

    #[test]
    fn radius_is_respected() {
        let scene = cross();
        assert_eq!(scene.snap(pt(50.0, 80.0), 29.9, SnapModes::ALL), None);
        let hit = scene.snap(pt(50.0, 80.0), 30.0, SnapModes::ALL).unwrap();
        assert_eq!(
            (hit.kind, hit.point, hit.distance),
            (SnapKind::Endpoint, pt(50.0, 50.0), 30.0)
        );
        // The bounding box of the diagonal contains the cursor, the line is far.
        assert_eq!(scene.snap(pt(28.0, 14.0), 3.0, SnapModes::ALL), None);
        // Bad input gives no hit instead of a panic.
        assert_eq!(scene.snap(pt(f64::NAN, 0.0), 3.0, SnapModes::ALL), None);
        assert_eq!(scene.snap(pt(0.0, 0.0), -1.0, SnapModes::ALL), None);
        assert_eq!(
            scene.snap(pt(0.0, 0.0), f64::INFINITY, SnapModes::ALL),
            None
        );
    }

    #[test]
    fn curve_joints_are_not_endpoints_or_midpoints() {
        let piece = |a: Point, b: Point, av: bool, bv: bool| Segment {
            a,
            b,
            kind: SegmentKind::CurvePiece,
            a_vertex: av,
            b_vertex: bv,
            path: 0,
        };
        let scene = Scene::from_segments(vec![
            piece(pt(0.0, 0.0), pt(10.0, 1.0), true, false),
            piece(pt(10.0, 1.0), pt(20.0, 0.0), false, true),
        ]);
        // A flattening joint is not an endpoint, a midpoint or an intersection.
        let hit = scene.snap(pt(10.0, 1.2), 1.0, SnapModes::ALL).unwrap();
        assert_eq!(hit.kind, SnapKind::Nearest);
        assert_eq!(
            scene
                .snap(pt(19.5, 0.0), 1.0, SnapModes::ALL)
                .unwrap()
                .point,
            pt(20.0, 0.0)
        );
    }

    #[test]
    fn oblique_intersection_is_close_and_t_junction_is_exact() {
        let scene = Scene::from_segments(vec![
            Segment::line(pt(0.0, 0.0), pt(30.0, 10.0)),
            Segment::line(pt(0.0, 10.0), pt(30.0, 0.0)),
            // A T-junction: the vertical ends on the horizontal's interior.
            Segment::line(pt(100.0, 0.0), pt(200.0, 0.0)),
            Segment::line(pt(125.5, 0.0), pt(125.5, 40.0)),
        ]);
        let only = SnapModes::only(SnapKind::Intersection);
        let hit = scene.snap(pt(15.2, 5.1), 1.0, only).unwrap();
        assert!(hit.point.distance(pt(15.0, 5.0)) < 1e-12);
        let hit = scene.snap(pt(125.0, 0.4), 1.0, only).unwrap();
        assert_eq!(hit.point, pt(125.5, 0.0));
    }

    #[test]
    fn segments_in_rect_uses_the_index() {
        let scene = cross();
        assert_eq!(
            scene.segments_in_rect(Rect::new(40.0, -1.0, 60.0, 1.0)),
            [0, 1]
        );
        assert_eq!(
            scene.segments_in_rect(Rect::new(60.0, 1.0, 40.0, -1.0)),
            [0, 1]
        );
        assert_eq!(scene.segments_in_rect(Rect::new(0.0, 20.0, 5.0, 30.0)), [2]);
        assert!(scene
            .segments_in_rect(Rect::new(200.0, 200.0, 300.0, 300.0))
            .is_empty());
    }

    #[test]
    fn dense_grid_queries_stay_correct() {
        // 100 x 100 crossing lines: every integer lattice point is a crossing.
        let mut segs = Vec::new();
        for i in 0..100 {
            let v = i as f64;
            segs.push(Segment::line(pt(v, 0.0), pt(v, 99.0)));
            segs.push(Segment::line(pt(0.0, v), pt(99.0, v)));
        }
        let scene = Scene::from_segments(segs);
        let hit = scene.snap(pt(42.2, 17.9), 0.4, SnapModes::ALL).unwrap();
        assert_eq!(
            (hit.kind, hit.point),
            (SnapKind::Intersection, pt(42.0, 18.0))
        );
        // A radius covering the whole grid still answers with the closest crossing.
        let hit = scene
            .snap(
                pt(42.2, 17.9),
                500.0,
                SnapModes::only(SnapKind::Intersection),
            )
            .unwrap();
        assert_eq!(hit.point, pt(42.0, 18.0));
    }
}
