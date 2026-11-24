//! Builders for the shapes markups are made of. Everything is in page space (y-up).
//!
//! Path builders return a [`BezPath`]; the builders that need several ops (a line with
//! endings) return ops that are opaque and unblended, ready for
//! [`DrawList::push_composite`](crate::DrawList::push_composite).

use std::f64::consts::PI;

use kurbo::{Arc, BezPath, Point, Rect, Vec2};

use crate::list::{DrawOp, PathOp};
use crate::style::{BlendMode, Fill, LineCap, LineJoin, Rgb, Stroke};

/// Four corners of a quadrilateral in ring order (each corner adjacent to the next).
pub type Quad = [Point; 4];

/// A closed rectangle.
pub fn rect_path(rect: Rect) -> BezPath {
    let r = rect.abs();
    let mut p = BezPath::new();
    p.move_to((r.x0, r.y0));
    p.line_to((r.x1, r.y0));
    p.line_to((r.x1, r.y1));
    p.line_to((r.x0, r.y1));
    p.close_path();
    p
}

/// The rectangle a border of `width` is drawn along so that its outer edge lies on `rect`:
/// `rect` shrunk by half the width (never past its centre). This is how `/Rect` relates to the
/// path of a Square or Circle annotation.
pub fn inset_for_stroke(rect: Rect, width: f64) -> Rect {
    let r = rect.abs();
    let dx = (width / 2.0).clamp(0.0, r.width() / 2.0);
    let dy = (width / 2.0).clamp(0.0, r.height() / 2.0);
    Rect::new(r.x0 + dx, r.y0 + dy, r.x1 - dx, r.y1 - dy)
}

/// The ellipse inscribed in `rect`, as four cubic Béziers starting at its rightmost point and
/// running anticlockwise.
pub fn ellipse_path(rect: Rect) -> BezPath {
    // Distance of the control points for a quarter circle: 4/3 * (sqrt(2) - 1).
    const K: f64 = 0.552_284_749_830_793_4;
    let r = rect.abs();
    let (cx, cy) = (r.center().x, r.center().y);
    let (rx, ry) = (r.width() / 2.0, r.height() / 2.0);
    let (kx, ky) = (rx * K, ry * K);
    let mut p = BezPath::new();
    p.move_to((cx + rx, cy));
    p.curve_to((cx + rx, cy + ky), (cx + kx, cy + ry), (cx, cy + ry));
    p.curve_to((cx - kx, cy + ry), (cx - rx, cy + ky), (cx - rx, cy));
    p.curve_to((cx - rx, cy - ky), (cx - kx, cy - ry), (cx, cy - ry));
    p.curve_to((cx + kx, cy - ry), (cx + rx, cy - ky), (cx + rx, cy));
    p.close_path();
    p
}

/// An open path through the points. Fewer than two points give an empty path.
pub fn polyline_path(points: &[Point]) -> BezPath {
    let mut p = BezPath::new();
    if points.len() < 2 {
        return p;
    }
    p.move_to(points[0]);
    for q in &points[1..] {
        p.line_to(*q);
    }
    p
}

/// A closed path through the points. Fewer than two points give an empty path.
pub fn polygon_path(points: &[Point]) -> BezPath {
    let mut p = polyline_path(points);
    if !p.elements().is_empty() {
        p.close_path();
    }
    p
}

/// A smooth open curve through the points of a freehand stroke.
///
/// Consecutive duplicates are dropped and the rest are joined by a uniform Catmull-Rom spline,
/// written as cubic Béziers, so the curve passes through every remaining point. A single point
/// becomes a zero-length line, which a round cap draws as a dot.
pub fn ink_path(points: &[Point]) -> BezPath {
    let mut pts: Vec<Point> = Vec::with_capacity(points.len());
    for p in points {
        if pts.last() != Some(p) {
            pts.push(*p);
        }
    }
    let mut path = BezPath::new();
    match pts.len() {
        0 => {}
        1 => {
            path.move_to(pts[0]);
            path.line_to(pts[0]);
        }
        2 => {
            path.move_to(pts[0]);
            path.line_to(pts[1]);
        }
        n => {
            path.move_to(pts[0]);
            for i in 0..n - 1 {
                let p0 = pts[i.saturating_sub(1)];
                let p1 = pts[i];
                let p2 = pts[i + 1];
                let p3 = pts[(i + 2).min(n - 1)];
                let c1 = p1 + (p2 - p0) / 6.0;
                let c2 = p2 - (p3 - p1) / 6.0;
                path.curve_to(c1, c2, p2);
            }
        }
    }
    path
}

/// One path covering every quad (for a text highlight). Filled with the non-zero rule, quads
/// that overlap are not darkened twice; corners are wound consistently so that holds whatever
/// order each quad's ring was given in.
pub fn highlight_path(quads: &[Quad]) -> BezPath {
    let mut p = BezPath::new();
    for q in quads {
        let area = signed_area(q);
        p.move_to(q[0]);
        if area >= 0.0 {
            p.line_to(q[1]);
            p.line_to(q[2]);
            p.line_to(q[3]);
        } else {
            p.line_to(q[3]);
            p.line_to(q[2]);
            p.line_to(q[1]);
        }
        p.close_path();
    }
    p
}

/// A highlighter mark over the quads: a Multiply fill, so ink beneath stays as dark as it was.
pub fn highlight_op(quads: &[Quad], color: Rgb, opacity: f64) -> DrawOp {
    DrawOp::Path(
        PathOp::filled(highlight_path(quads), Fill::new(color))
            .with_opacity(opacity)
            .with_blend(BlendMode::Multiply),
    )
}

/// The axis-aligned quad of a rectangle, in ring order starting at the bottom-left corner.
pub fn rect_quad(rect: Rect) -> Quad {
    let r = rect.abs();
    [
        Point::new(r.x0, r.y0),
        Point::new(r.x1, r.y0),
        Point::new(r.x1, r.y1),
        Point::new(r.x0, r.y1),
    ]
}

/// Twice-signed-area test: positive for an anticlockwise ring in y-up space.
fn signed_area(points: &[Point]) -> f64 {
    let n = points.len();
    let mut s = 0.0;
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        s += a.x * b.y - b.x * a.y;
    }
    s / 2.0
}

// ---------------------------------------------------------------------------------------------
// Cloud border
// ---------------------------------------------------------------------------------------------

/// Shape of a cloud border's scallops.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudStyle {
    /// Target distance along the outline from one scallop to the next, in page units. Each edge
    /// is divided into a whole number of equal scallops as near to this as it can be.
    pub scallop: f64,
    /// How far each scallop bulges out, as a fraction of its base: 0.5 is a half circle, less is
    /// flatter. Kept within `0.05..=1.0`.
    pub bulge: f64,
}

impl CloudStyle {
    /// A default look: scallops a little flatter than half circles.
    pub fn new(scallop: f64) -> CloudStyle {
        CloudStyle {
            scallop,
            bulge: 0.38,
        }
    }

    /// How far the scallops reach outside the polygon they follow, at most.
    pub fn reach(&self) -> f64 {
        self.scallop.abs() * self.bulge.clamp(0.05, 1.0) * 1.5
    }
}

/// Tolerance for turning arcs into Béziers, in page units.
const ARC_TOLERANCE: f64 = 0.01;

/// The most scallops one cloud outline is given. The largest page a PDF allows (14,400 units
/// a side) with the finest scallop this application uses (5 units) needs 11,520.
pub const MAX_CLOUD_SCALLOPS: usize = 20_000;

/// A cloud border: a closed path of circular arcs that follows `polygon`, each arc starting and
/// ending on the polygon's outline and bulging away from its inside.
///
/// Every edge is cut into equal scallops, so scallops always meet at the polygon's corners and
/// the outline closes exactly. The polygon may be wound either way. Fewer than three distinct
/// points give an empty path.
///
/// Scallops meet in cusps, which are sharpest at a polygon's inward corners. Stroke a cloud
/// with round joins: a miter join there sits right at the miter limit, where renderers differ
/// on whether to draw the spike.
pub fn cloud_path(polygon: &[Point], style: &CloudStyle) -> BezPath {
    let mut pts: Vec<Point> = Vec::with_capacity(polygon.len());
    for p in polygon {
        if pts.last().is_none_or(|q| (*q - *p).hypot() > 1e-9) {
            pts.push(*p);
        }
    }
    while pts.len() > 1 && (pts[0] - pts[pts.len() - 1]).hypot() <= 1e-9 {
        pts.pop();
    }
    let mut path = BezPath::new();
    if pts.len() < 3 {
        return path;
    }
    let scallop = if style.scallop.is_finite() && style.scallop > 0.0 {
        style.scallop
    } else {
        return polygon_path(&pts);
    };
    let bulge = if style.bulge.is_finite() {
        style.bulge.clamp(0.05, 1.0)
    } else {
        0.38
    };
    // In y-up space an anticlockwise ring has its inside on the left of each edge, so "out" is
    // to the right; a clockwise ring is the mirror image.
    let side = if signed_area(&pts) >= 0.0 { 1.0 } else { -1.0 };

    let n = pts.len();
    // An outline of absurd size (coordinates from a hostile or broken file) would ask for
    // scallops without end; it is drawn as the plain polygon instead.
    let scallops: f64 = (0..n)
        .map(|i| {
            ((pts[(i + 1) % n] - pts[i]).hypot() / scallop)
                .round()
                .max(1.0)
        })
        .sum();
    if scallops.is_nan() || scallops > MAX_CLOUD_SCALLOPS as f64 {
        return polygon_path(&pts);
    }

    path.move_to(pts[0]);
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let edge = b - a;
        let len = edge.hypot();
        let dir = edge / len;
        let out = Vec2::new(dir.y, -dir.x) * side;
        let count = (len / scallop).round().max(1.0) as usize;
        for k in 0..count {
            let start = a + edge * (k as f64 / count as f64);
            let end = if k + 1 == count {
                b
            } else {
                a + edge * ((k + 1) as f64 / count as f64)
            };
            append_bulge(&mut path, start, end, out, bulge);
        }
    }
    path.close_path();
    path
}

/// A cloud border around a rectangle.
pub fn cloud_rect_path(rect: Rect, style: &CloudStyle) -> BezPath {
    cloud_path(&rect_quad(rect), style)
}

/// Appends a circular arc from the current point `start` to `end` whose highest point is
/// `bulge * |end - start|` from the chord, on the side `out` points to.
fn append_bulge(path: &mut BezPath, start: Point, end: Point, out: Vec2, bulge: f64) {
    let chord = (end - start).hypot();
    if chord <= 1e-12 {
        return;
    }
    let sagitta = bulge * chord;
    let radius = (chord * chord / 4.0 + sagitta * sagitta) / (2.0 * sagitta);
    let mid = start.midpoint(end);
    let center = mid + out * (sagitta - radius);
    let start_angle = (start - center).atan2();
    // The angle the arc turns through: under a half turn when the bulge is under half the chord.
    let half = (chord / 2.0 / radius).clamp(-1.0, 1.0).asin();
    let magnitude = if sagitta <= radius {
        2.0 * half
    } else {
        2.0 * PI - 2.0 * half
    };
    // Passing through the top of the bulge means turning clockwise when `out` is to the left of
    // the chord direction, anticlockwise otherwise.
    let sign = -(end - start).cross(out).signum();
    let arc = Arc::new(center, (radius, radius), start_angle, sign * magnitude, 0.0);
    let mut last = None;
    for el in arc.append_iter(ARC_TOLERANCE) {
        if let Some(prev) = last.replace(el) {
            path.push(prev);
        }
    }
    // Land exactly on `end` so neighbouring scallops and the closing point share a vertex.
    match last {
        Some(kurbo::PathEl::CurveTo(c1, c2, _)) => path.curve_to(c1, c2, end),
        _ => path.line_to(end),
    }
}

// ---------------------------------------------------------------------------------------------
// Line endings
// ---------------------------------------------------------------------------------------------

/// The line-ending styles of PDF's `/LE`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineEnding {
    #[default]
    None,
    OpenArrow,
    ClosedArrow,
    Butt,
    Circle,
    Square,
    Diamond,
    Slash,
}

impl LineEnding {
    pub const ALL: [LineEnding; 8] = [
        LineEnding::None,
        LineEnding::OpenArrow,
        LineEnding::ClosedArrow,
        LineEnding::Butt,
        LineEnding::Circle,
        LineEnding::Square,
        LineEnding::Diamond,
        LineEnding::Slash,
    ];

    /// The name used in a `/LE` array.
    pub fn pdf_name(self) -> &'static str {
        match self {
            LineEnding::None => "None",
            LineEnding::OpenArrow => "OpenArrow",
            LineEnding::ClosedArrow => "ClosedArrow",
            LineEnding::Butt => "Butt",
            LineEnding::Circle => "Circle",
            LineEnding::Square => "Square",
            LineEnding::Diamond => "Diamond",
            LineEnding::Slash => "Slash",
        }
    }

    /// Parses a `/LE` name. The reversed arrows of PDF 1.6 and unknown names give `None`.
    pub fn from_pdf_name(name: &str) -> Option<LineEnding> {
        LineEnding::ALL.into_iter().find(|e| e.pdf_name() == name)
    }

    /// True for the endings that enclose an area, which `/IC` fills.
    pub fn is_closed(self) -> bool {
        matches!(
            self,
            LineEnding::ClosedArrow | LineEnding::Circle | LineEnding::Square | LineEnding::Diamond
        )
    }
}

/// Half the angle between an arrowhead's two sides.
const ARROW_HALF_ANGLE: f64 = PI / 6.0;

/// Length of an arrowhead along the line for a given line width.
pub fn arrow_length(width: f64) -> f64 {
    3.5 * width.max(0.0) + 5.0
}

/// Diameter of a circle ending, side of a square, diagonal of a diamond and length of a butt or
/// slash, for a given line width.
pub fn ending_size(width: f64) -> f64 {
    3.0 * width.max(0.0) + 3.0
}

/// The outline of one line ending.
#[derive(Clone, Debug, PartialEq)]
pub struct EndingShape {
    /// The outline to stroke (never dashed) and, when [`LineEnding::is_closed`], to fill.
    pub path: BezPath,
    /// How far the line itself must stop short of its end point so that it meets the ending
    /// instead of running through it.
    pub inset: f64,
}

/// Builds the ending drawn at `tip`, the end point of a line of the given width. `dir` points
/// along the line towards the tip (it need not be unit length). `None`, or a zero `dir`, gives
/// an empty path.
///
/// Arrowheads are set back by one line width so that the point of the mitered stroke, not the
/// centre line of the stroke, lands on `tip`.
pub fn ending_shape(ending: LineEnding, tip: Point, dir: Vec2, width: f64) -> EndingShape {
    let mut path = BezPath::new();
    let len = dir.hypot();
    if ending == LineEnding::None || len <= 1e-12 || !len.is_finite() {
        return EndingShape { path, inset: 0.0 };
    }
    let d = dir / len;
    let n = Vec2::new(-d.y, d.x);
    let w = width.max(0.0);
    let size = ending_size(w);
    let half = size / 2.0;
    let inset = match ending {
        LineEnding::None => 0.0,
        LineEnding::OpenArrow | LineEnding::ClosedArrow => {
            // The mitered point is (w / 2) / sin(half angle) beyond the apex of the centre line.
            let setback = (w / 2.0) / ARROW_HALF_ANGLE.sin();
            let apex = tip - d * setback;
            let length = arrow_length(w);
            let spread = length * ARROW_HALF_ANGLE.tan();
            let base = apex - d * length;
            path.move_to(base + n * spread);
            path.line_to(apex);
            path.line_to(base - n * spread);
            if ending == LineEnding::ClosedArrow {
                path.close_path();
                setback + length
            } else {
                setback
            }
        }
        LineEnding::Butt => {
            path.move_to(tip + n * half);
            path.line_to(tip - n * half);
            0.0
        }
        LineEnding::Slash => {
            // Thirty degrees clockwise from the perpendicular, as the PDF reference describes.
            let (s, c) = (PI / 3.0).sin_cos();
            let u = d * c + n * s;
            path.move_to(tip + u * half);
            path.line_to(tip - u * half);
            0.0
        }
        LineEnding::Circle => {
            path = ellipse_path(Rect::new(
                tip.x - half,
                tip.y - half,
                tip.x + half,
                tip.y + half,
            ));
            half
        }
        LineEnding::Square => {
            path.move_to(tip - d * half - n * half);
            path.line_to(tip + d * half - n * half);
            path.line_to(tip + d * half + n * half);
            path.line_to(tip - d * half + n * half);
            path.close_path();
            half
        }
        LineEnding::Diamond => {
            path.move_to(tip - d * half);
            path.line_to(tip - n * half);
            path.line_to(tip + d * half);
            path.line_to(tip + n * half);
            path.close_path();
            half
        }
    };
    EndingShape { path, inset }
}

/// A straight line from `a` to `b` with an ending at each end. See [`polyline_ops`].
pub fn line_ops(
    a: Point,
    b: Point,
    start: LineEnding,
    end: LineEnding,
    stroke: &Stroke,
    interior: Option<Rgb>,
) -> Vec<DrawOp> {
    polyline_ops(&[a, b], start, end, stroke, interior)
}

/// An open polyline with a line ending at its first and last point (a Line or PolyLine
/// annotation with `/LE`).
///
/// The returned ops are opaque: first the line, stroked with `stroke` (dashes included) and
/// shortened where an ending needs it, then each ending, stroked solid in the same colour and
/// width and, for closed endings, filled with `interior` (`/IC`) when given. To make the whole
/// thing translucent pass the ops to
/// [`DrawList::push_composite`](crate::DrawList::push_composite), which keeps the overlaps from
/// showing.
pub fn polyline_ops(
    points: &[Point],
    start: LineEnding,
    end: LineEnding,
    stroke: &Stroke,
    interior: Option<Rgb>,
) -> Vec<DrawOp> {
    let mut pts: Vec<Point> = Vec::with_capacity(points.len());
    for p in points {
        if pts.last().is_none_or(|q| (*q - *p).hypot() > 1e-9) {
            pts.push(*p);
        }
    }
    if pts.len() < 2 {
        return Vec::new();
    }
    let n = pts.len();
    let first = pts[0];
    let last = pts[n - 1];
    let start_shape = ending_shape(start, first, first - pts[1], stroke.width);
    let end_shape = ending_shape(end, last, last - pts[n - 2], stroke.width);

    // Shorten the first and last segments, never past the middle of a segment they share.
    let first_len = (pts[1] - first).hypot();
    let last_len = (last - pts[n - 2]).hypot();
    let share = if n == 2 { 0.5 } else { 1.0 };
    let start_inset = start_shape.inset.min(first_len * share);
    let end_inset = end_shape.inset.min(last_len * share);
    let new_first = first + (pts[1] - first) / first_len * start_inset;
    let new_last = last + (pts[n - 2] - last) / last_len * end_inset;
    pts[0] = new_first;
    pts[n - 1] = new_last;

    let mut ops = vec![DrawOp::Path(PathOp::stroked(
        polyline_path(&pts),
        stroke.clone(),
    ))];
    let ending_stroke = Stroke {
        dash: None,
        cap: LineCap::Butt,
        join: LineJoin::Miter,
        miter_limit: Stroke::DEFAULT_MITER_LIMIT,
        ..stroke.clone()
    };
    for (kind, shape) in [(start, start_shape), (end, end_shape)] {
        if shape.path.elements().is_empty() {
            continue;
        }
        let mut op = PathOp::stroked(shape.path, ending_stroke.clone());
        if kind.is_closed() {
            if let Some(color) = interior {
                op.fill = Some(Fill::new(color));
            }
        }
        ops.push(DrawOp::Path(op));
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::{ops_bounds, stroke_bounds};
    use kurbo::{ParamCurve, ParamCurveArclen, PathSeg, Shape};

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn rect_path_is_the_four_corners() {
        let p = rect_path(Rect::new(30.0, 40.0, 10.0, 20.0));
        assert_eq!(p.bounding_box(), Rect::new(10.0, 20.0, 30.0, 40.0));
        assert!(approx(p.area().abs(), 400.0, 1e-9));
        assert_eq!(p.elements().len(), 5);
    }

    #[test]
    fn inset_for_stroke_never_inverts() {
        let r = Rect::new(0.0, 0.0, 100.0, 10.0);
        assert_eq!(inset_for_stroke(r, 4.0), Rect::new(2.0, 2.0, 98.0, 8.0));
        let tight = inset_for_stroke(r, 30.0);
        assert_eq!(tight, Rect::new(15.0, 5.0, 85.0, 5.0));
        // The stroked inset rectangle's outer edge is the original rectangle.
        let b = stroke_bounds(
            &rect_path(inset_for_stroke(r, 4.0)),
            &Stroke::new(4.0, Rgb::BLACK),
        );
        let b = b.unwrap();
        assert!(approx(b.x0, 0.0, 1e-6) && approx(b.y1, 10.0, 1e-6));
    }

    #[test]
    fn ellipse_path_stays_on_the_ellipse() {
        let r = Rect::new(10.0, 20.0, 110.0, 60.0);
        let p = ellipse_path(r);
        assert!(approx(p.bounding_box().x0, 10.0, 1e-9));
        assert!(approx(p.bounding_box().y1, 60.0, 1e-9));
        assert!(approx(
            p.area().abs(),
            PI * 50.0 * 20.0,
            50.0 * 20.0 * 0.002
        ));
        for seg in p.segments() {
            for i in 0..=10 {
                let q = seg.eval(i as f64 / 10.0);
                let v = ((q.x - 60.0) / 50.0).powi(2) + ((q.y - 40.0) / 20.0).powi(2);
                assert!(approx(v, 1.0, 0.001), "point {q:?} is off the ellipse: {v}");
            }
        }
    }

    #[test]
    fn polyline_and_polygon() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
        ];
        let open = polyline_path(&pts);
        assert_eq!(open.elements().len(), 3);
        assert!(approx(
            open.segments().map(|s| s.arclen(1e-9)).sum::<f64>(),
            20.0,
            1e-9
        ));
        let closed = polygon_path(&pts);
        assert_eq!(closed.elements().len(), 4);
        assert!(approx(closed.area().abs(), 50.0, 1e-9));
        assert!(polyline_path(&pts[..1]).elements().is_empty());
        assert!(polygon_path(&[]).elements().is_empty());
    }

    #[test]
    fn ink_passes_through_every_point_and_is_smooth() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 8.0),
            Point::new(10.0, 8.0), // duplicate sample
            Point::new(22.0, 3.0),
            Point::new(30.0, 12.0),
            Point::new(41.0, 0.0),
        ];
        let p = ink_path(&pts);
        let segs: Vec<PathSeg> = p.segments().collect();
        assert_eq!(segs.len(), 4, "one curve per distinct interval");
        let distinct = [pts[0], pts[1], pts[3], pts[4], pts[5]];
        for (i, s) in segs.iter().enumerate() {
            assert_eq!(s.start(), distinct[i]);
            assert_eq!(s.end(), distinct[i + 1]);
        }
        // Tangents agree across each interior joint (C1 continuity).
        for w in segs.windows(2) {
            let (PathSeg::Cubic(a), PathSeg::Cubic(b)) = (w[0], w[1]) else {
                panic!("expected cubics");
            };
            let out = a.p3 - a.p2;
            let inn = b.p1 - b.p0;
            assert!((out - inn).hypot() < 1e-9, "{out:?} vs {inn:?}");
        }
        // The curve is longer than the chords but not wildly so.
        let chords: f64 = distinct.windows(2).map(|w| (w[1] - w[0]).hypot()).sum();
        let len: f64 = segs.iter().map(|s| s.arclen(1e-6)).sum();
        assert!(len >= chords && len < chords * 1.2, "{len} vs {chords}");
    }

    #[test]
    fn ink_degenerate_inputs() {
        assert!(ink_path(&[]).elements().is_empty());
        let dot = ink_path(&[Point::new(3.0, 4.0)]);
        assert_eq!(dot.elements().len(), 2);
        assert_eq!(dot.bounding_box(), Rect::new(3.0, 4.0, 3.0, 4.0));
        let two = ink_path(&[Point::ZERO, Point::new(5.0, 0.0)]);
        assert!(matches!(two.segments().next(), Some(PathSeg::Line(_))));
        let same = ink_path(&[Point::ZERO, Point::ZERO, Point::ZERO]);
        assert_eq!(same.elements().len(), 2, "repeats collapse to a dot");
    }

    #[test]
    fn highlight_path_winds_every_quad_the_same_way() {
        let a = rect_quad(Rect::new(0.0, 0.0, 10.0, 4.0));
        let mut b = rect_quad(Rect::new(5.0, 0.0, 20.0, 4.0));
        b.reverse(); // given clockwise
        let p = highlight_path(&[a, b]);
        // Same winding: signed areas add instead of cancelling.
        assert!(approx(p.area(), 40.0 + 60.0, 1e-9), "{}", p.area());
        // The overlap has winding 2, never 0, so a non-zero fill covers it.
        assert_eq!(p.winding(Point::new(7.0, 2.0)).abs(), 2);
        assert_eq!(p.bounding_box(), Rect::new(0.0, 0.0, 20.0, 4.0));
        let op = highlight_op(&[a], Rgb::YELLOW, 0.8);
        assert!(matches!(
            op,
            DrawOp::Path(PathOp { blend: BlendMode::Multiply, opacity, fill: Some(_), stroke: None, .. })
                if opacity == 0.8
        ));
    }

    // -- cloud --------------------------------------------------------------------------------

    fn cloud_checks(polygon: &[Point], style: &CloudStyle) -> BezPath {
        let path = cloud_path(polygon, style);
        let segs: Vec<PathSeg> = path.segments().collect();
        assert!(!segs.is_empty());
        // Closed and continuous.
        for w in segs.windows(2) {
            assert!((w[0].end() - w[1].start()).hypot() < 1e-9);
        }
        assert!((segs[0].start() - segs[segs.len() - 1].end()).hypot() < 1e-9);
        // Every polygon vertex is a point of the outline.
        for v in polygon {
            assert!(
                segs.iter().any(|s| (s.start() - *v).hypot() < 1e-9),
                "vertex {v:?} is not on the cloud"
            );
        }
        path
    }

    #[test]
    fn cloud_scallops_bulge_outwards_for_either_winding() {
        let square = [
            Point::new(100.0, 100.0),
            Point::new(200.0, 100.0),
            Point::new(200.0, 180.0),
            Point::new(100.0, 180.0),
        ];
        let style = CloudStyle::new(20.0);
        let reversed: Vec<Point> = square.iter().rev().copied().collect();
        for polygon in [&square[..], &reversed[..]] {
            let path = cloud_checks(polygon, &style);
            let inside = Rect::new(100.0, 100.0, 200.0, 180.0);
            // No point of the outline is inside the polygon (beyond rounding).
            for seg in path.segments() {
                for i in 0..=8 {
                    let q = seg.eval(i as f64 / 8.0);
                    assert!(
                        !inside.inflate(-1e-6, -1e-6).contains(q),
                        "{q:?} dips inside the polygon"
                    );
                }
            }
            // The outline reaches out by the bulge: 5 scallops of 20 on the bottom edge.
            let b = path.bounding_box();
            let rise = 20.0 * style.bulge;
            assert!(approx(b.y0, 100.0 - rise, 0.05), "{b:?}");
            assert!(approx(b.y1, 180.0 + rise, 0.05), "{b:?}");
            assert!(approx(b.x0, 100.0 - rise, 0.05), "{b:?}");
            assert!(b.x0 >= 100.0 - style.reach() && b.y1 <= 180.0 + style.reach());
            // The cloud encloses more than the polygon.
            assert!(path.area().abs() > 100.0 * 80.0);
        }
    }

    #[test]
    fn cloud_divides_each_edge_into_whole_equal_scallops() {
        let style = CloudStyle::new(20.0);
        // Edges of 100 and 80 give 5 and 4 scallops; one of 90 gives 4 or 5 of equal size.
        let path = cloud_checks(
            &[
                Point::new(0.0, 0.0),
                Point::new(90.0, 0.0),
                Point::new(90.0, 50.0),
                Point::new(0.0, 50.0),
            ],
            &style,
        );
        // Scallop joints are the outline points that lie on the polygon's edges.
        let mut on_bottom: Vec<f64> = path
            .segments()
            .map(|s| s.start())
            .filter(|p| p.y.abs() < 1e-9)
            .map(|p| p.x)
            .collect();
        on_bottom.sort_by(|a, b| a.partial_cmp(b).unwrap());
        on_bottom.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        let steps: Vec<f64> = on_bottom.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(steps.len() == 4 || steps.len() == 5, "{steps:?}");
        for s in &steps {
            assert!(approx(*s, 90.0 / steps.len() as f64, 1e-9), "{steps:?}");
        }
    }

    #[test]
    fn cloud_arcs_are_circular() {
        // One scallop per edge of a small triangle: each must be a circular arc through its
        // two vertices with the requested rise.
        let style = CloudStyle {
            scallop: 1000.0,
            bulge: 0.5,
        };
        let tri = [
            Point::new(0.0, 0.0),
            Point::new(40.0, 0.0),
            Point::new(20.0, 30.0),
        ];
        let path = cloud_checks(&tri, &style);
        // Bottom edge, half-circle bulge: centre (20, 0), radius 20, below the edge.
        let mut seen = 0;
        for seg in path.segments() {
            for i in 0..=10 {
                let q = seg.eval(i as f64 / 10.0);
                if q.y < -1e-6 {
                    seen += 1;
                    assert!(
                        approx((q - Point::new(20.0, 0.0)).hypot(), 20.0, 0.02),
                        "{q:?}"
                    );
                }
            }
        }
        assert!(seen > 5);
        assert!(approx(path.bounding_box().y0, -20.0, 0.02));
    }

    #[test]
    fn cloud_bulge_over_a_half_circle_and_flat() {
        let sq = rect_quad(Rect::new(0.0, 0.0, 30.0, 30.0));
        let fat = cloud_checks(
            &sq,
            &CloudStyle {
                scallop: 30.0,
                bulge: 0.8,
            },
        );
        assert!(
            approx(fat.bounding_box().y0, -24.0, 0.05),
            "{:?}",
            fat.bounding_box()
        );
        // More than a half circle is wider than its base: the bottom scallop overhangs the
        // corner it starts from.
        assert!(fat.segments().any(|s| {
            let q = s.eval(0.5);
            q.y < -1.0 && q.x < -1.0
        }));
        let flat = cloud_checks(
            &sq,
            &CloudStyle {
                scallop: 30.0,
                bulge: 0.1,
            },
        );
        assert!(approx(flat.bounding_box().y0, -3.0, 0.02));
    }

    #[test]
    fn a_cloud_of_absurd_size_is_the_plain_polygon() {
        // One scallop per ten units of a 4e7-unit outline would be millions of arcs (and at
        // 1e12 an allocation no machine has); the outline is drawn plain instead.
        let style = CloudStyle::new(10.0);
        for side in [1e7, 1e12, 1e300] {
            let start = std::time::Instant::now();
            let path = cloud_rect_path(Rect::new(0.0, 0.0, side, side), &style);
            assert_eq!(path.elements().len(), 5, "{side}: four corners and a close");
            assert!(start.elapsed() < std::time::Duration::from_secs(1));
        }
        // The largest page there can be still gets its cloud.
        let page = cloud_rect_path(
            Rect::new(0.0, 0.0, 14_400.0, 14_400.0),
            &CloudStyle::new(5.0),
        );
        assert!(page.elements().len() > 11_520);
    }

    #[test]
    fn cloud_degenerate_inputs() {
        let style = CloudStyle::new(10.0);
        assert!(cloud_path(&[], &style).elements().is_empty());
        assert!(cloud_path(&[Point::ZERO, Point::new(5.0, 0.0)], &style)
            .elements()
            .is_empty());
        // Repeated and closing points are ignored.
        let sq = rect_quad(Rect::new(0.0, 0.0, 20.0, 20.0));
        let mut noisy = vec![sq[0], sq[0], sq[1], sq[2], sq[2], sq[3], sq[0]];
        let a = cloud_path(&sq, &style);
        let b = cloud_path(&noisy, &style);
        assert_eq!(a, b);
        noisy.truncate(3);
        assert!(cloud_path(&noisy, &style).elements().is_empty());
        // A zero scallop falls back to the plain polygon rather than dividing by zero.
        let plain = cloud_path(
            &sq,
            &CloudStyle {
                scallop: 0.0,
                bulge: 0.4,
            },
        );
        assert_eq!(plain, polygon_path(&sq));
        let rect = cloud_rect_path(Rect::new(0.0, 0.0, 20.0, 20.0), &style);
        assert_eq!(rect, a);
    }

    // -- line endings -------------------------------------------------------------------------

    #[test]
    fn le_names_round_trip() {
        for e in LineEnding::ALL {
            assert_eq!(LineEnding::from_pdf_name(e.pdf_name()), Some(e));
        }
        assert_eq!(LineEnding::from_pdf_name("ROpenArrow"), None);
        assert_eq!(LineEnding::from_pdf_name("none"), None);
        assert_eq!(LineEnding::default(), LineEnding::None);
    }

    #[test]
    fn no_ending_and_zero_direction_draw_nothing() {
        let s = ending_shape(LineEnding::None, Point::ZERO, Vec2::new(1.0, 0.0), 2.0);
        assert!(s.path.elements().is_empty() && s.inset == 0.0);
        let s = ending_shape(LineEnding::Circle, Point::ZERO, Vec2::ZERO, 2.0);
        assert!(s.path.elements().is_empty() && s.inset == 0.0);
    }

    #[test]
    fn arrowheads_put_the_stroked_point_exactly_on_the_tip() {
        let tip = Point::new(100.0, 50.0);
        for ending in [LineEnding::OpenArrow, LineEnding::ClosedArrow] {
            for w in [0.5, 1.0, 3.0, 8.0] {
                let shape = ending_shape(ending, tip, Vec2::new(5.0, 0.0), w);
                let b = stroke_bounds(&shape.path, &Stroke::new(w, Rgb::BLACK)).unwrap();
                assert!(approx(b.x1, tip.x, 0.01), "{ending:?} w={w}: {b:?}");
                // Symmetric about the line.
                assert!(approx(b.y0 + b.y1, 2.0 * tip.y, 0.01));
                // The head is as long as arrow_length says, measured on the centre line.
                let pb = shape.path.bounding_box();
                assert!(approx(pb.width(), arrow_length(w), 1e-9));
            }
        }
        let open = ending_shape(LineEnding::OpenArrow, tip, Vec2::new(1.0, 0.0), 2.0);
        assert!(approx(open.inset, 2.0, 1e-9), "the line runs to the apex");
        let closed = ending_shape(LineEnding::ClosedArrow, tip, Vec2::new(1.0, 0.0), 2.0);
        assert!(
            approx(closed.inset, 2.0 + arrow_length(2.0), 1e-9),
            "the line stops at the base"
        );
        assert_eq!(
            closed.path.elements().last(),
            Some(&kurbo::PathEl::ClosePath)
        );
        assert_ne!(open.path.elements().last(), Some(&kurbo::PathEl::ClosePath));
    }

    #[test]
    fn arrowheads_follow_the_line_direction() {
        let tip = Point::new(0.0, 0.0);
        // Pointing straight up: the head lies below the tip.
        let s = ending_shape(LineEnding::ClosedArrow, tip, Vec2::new(0.0, 3.0), 1.0);
        let b = s.path.bounding_box();
        assert!(b.y1 < 0.0 && approx(b.x0 + b.x1, 0.0, 1e-9), "{b:?}");
        // The full angle at the apex is 60 degrees: spread = length * tan(30).
        assert!(approx(
            b.width(),
            2.0 * arrow_length(1.0) * (PI / 6.0).tan(),
            1e-9
        ));
    }

    #[test]
    fn centred_endings_are_centred_on_the_tip() {
        let tip = Point::new(40.0, 40.0);
        let dir = Vec2::new(3.0, 4.0);
        let w = 2.0;
        let size = ending_size(w);
        for ending in [LineEnding::Circle, LineEnding::Square, LineEnding::Diamond] {
            let s = ending_shape(ending, tip, dir, w);
            let b = s.path.bounding_box();
            assert!((b.center() - tip).hypot() < 1e-9, "{ending:?}");
            assert!(approx(s.inset, size / 2.0, 1e-9));
            assert!(ending.is_closed());
        }
        let circle = ending_shape(LineEnding::Circle, tip, dir, w);
        assert!(approx(
            circle.path.area().abs(),
            PI * (size / 2.0).powi(2),
            0.05
        ));
        let square = ending_shape(LineEnding::Square, tip, dir, w);
        assert!(approx(square.path.area().abs(), size * size, 1e-9));
        // The square is turned with the line: its box is wider than its side.
        assert!(square.path.bounding_box().width() > size + 0.1);
        let diamond = ending_shape(LineEnding::Diamond, tip, dir, w);
        assert!(approx(diamond.path.area().abs(), size * size / 2.0, 1e-9));
        // One corner of the diamond is on the line, half a size beyond the tip.
        let ahead = tip + dir / dir.hypot() * (size / 2.0);
        assert!(diamond
            .path
            .segments()
            .any(|s| (s.start() - ahead).hypot() < 1e-9));
    }

    #[test]
    fn butt_is_perpendicular_and_slash_leans_thirty_degrees() {
        let tip = Point::new(10.0, 10.0);
        let dir = Vec2::new(1.0, 0.0);
        let size = ending_size(1.0);
        let butt = ending_shape(LineEnding::Butt, tip, dir, 1.0);
        assert_eq!(butt.inset, 0.0);
        let b = butt.path.bounding_box();
        assert!(approx(b.width(), 0.0, 1e-9) && approx(b.height(), size, 1e-9));
        assert!((b.center() - tip).hypot() < 1e-9);

        let slash = ending_shape(LineEnding::Slash, tip, dir, 1.0);
        let seg = slash.path.segments().next().unwrap();
        let v = seg.end() - seg.start();
        assert!(approx(v.hypot(), size, 1e-9));
        // 60 degrees from the line, i.e. 30 from the perpendicular.
        let angle = (v.cross(dir).abs()).atan2(v.dot(dir).abs()).to_degrees();
        assert!(approx(angle, 60.0, 1e-6), "{angle}");
        assert!((seg.eval(0.5) - tip).hypot() < 1e-9);
        assert!(!LineEnding::Slash.is_closed() && !LineEnding::Butt.is_closed());
    }

    #[test]
    fn line_ops_shorten_the_line_to_meet_closed_endings() {
        let a = Point::new(10.0, 10.0);
        let b = Point::new(110.0, 10.0);
        let stroke = Stroke::new(2.0, Rgb::RED).with_dash(crate::Dash::new([6.0, 3.0], 0.0));
        let ops = line_ops(
            a,
            b,
            LineEnding::Circle,
            LineEnding::ClosedArrow,
            &stroke,
            Some(Rgb::BLUE),
        );
        assert_eq!(ops.len(), 3);
        let DrawOp::Path(line) = &ops[0] else {
            panic!()
        };
        let lb = line.path.bounding_box();
        assert!(approx(lb.x0, 10.0 + ending_size(2.0) / 2.0, 1e-9));
        assert!(approx(lb.x1, 110.0 - 2.0 - arrow_length(2.0), 1e-9));
        assert!(
            line.stroke.as_ref().unwrap().dash.is_some(),
            "the line keeps its dashes"
        );
        for op in &ops[1..] {
            let DrawOp::Path(p) = op else { panic!() };
            assert_eq!(p.fill, Some(Fill::new(Rgb::BLUE)));
            let s = p.stroke.as_ref().unwrap();
            assert!(s.dash.is_none(), "endings are never dashed");
            assert_eq!((s.width, s.color), (2.0, Rgb::RED));
        }
        // Bounds include both endings: the circle behind the start, the arrow tip at the end.
        let bounds = ops_bounds(&ops).unwrap();
        assert!(
            approx(bounds.x0, 10.0 - ending_size(2.0) / 2.0 - 1.0, 0.06),
            "{bounds:?}"
        );
        assert!(approx(bounds.x1, 110.0, 0.01), "{bounds:?}");
        assert!(bounds.height() >= ending_size(2.0));
    }

    #[test]
    fn open_endings_take_no_fill_and_without_ic_nothing_is_filled() {
        let a = Point::new(0.0, 0.0);
        let b = Point::new(100.0, 0.0);
        let stroke = Stroke::new(1.0, Rgb::BLACK);
        let ops = line_ops(
            a,
            b,
            LineEnding::OpenArrow,
            LineEnding::Slash,
            &stroke,
            Some(Rgb::BLUE),
        );
        for op in &ops {
            let DrawOp::Path(p) = op else { panic!() };
            assert!(p.fill.is_none());
        }
        let ops = line_ops(a, b, LineEnding::Diamond, LineEnding::Square, &stroke, None);
        assert_eq!(ops.len(), 3);
        for op in &ops {
            let DrawOp::Path(p) = op else { panic!() };
            assert!(p.fill.is_none());
        }
        let plain = line_ops(a, b, LineEnding::None, LineEnding::None, &stroke, None);
        assert_eq!(plain.len(), 1);
        assert_eq!(ops_bounds(&plain), Some(Rect::new(0.0, -0.5, 100.0, 0.5)));
    }

    #[test]
    fn every_ending_grows_the_bounds_past_the_bare_line() {
        let a = Point::new(50.0, 50.0);
        let b = Point::new(150.0, 90.0);
        let stroke = Stroke::new(2.0, Rgb::BLACK);
        let bare = ops_bounds(&line_ops(
            a,
            b,
            LineEnding::None,
            LineEnding::None,
            &stroke,
            None,
        ))
        .unwrap();
        for e in LineEnding::ALL.into_iter().skip(1) {
            let with = ops_bounds(&line_ops(a, b, e, e, &stroke, None)).unwrap();
            assert!(
                with.area() > bare.area() + 1.0,
                "{e:?}: {with:?} vs {bare:?}"
            );
            // And nothing is left out: every op's own bounds are inside the total.
            for op in line_ops(a, b, e, e, &stroke, None) {
                assert_eq!(with.union(op.bounds().unwrap()), with);
            }
        }
    }

    #[test]
    fn polyline_endings_use_the_end_segments() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(0.0, 50.0),
            Point::new(80.0, 50.0),
        ];
        let stroke = Stroke::new(1.0, Rgb::BLACK);
        let ops = polyline_ops(
            &pts,
            LineEnding::ClosedArrow,
            LineEnding::ClosedArrow,
            &stroke,
            None,
        );
        assert_eq!(ops.len(), 3);
        let DrawOp::Path(start) = &ops[1] else {
            panic!()
        };
        let DrawOp::Path(end) = &ops[2] else { panic!() };
        // The start arrow points down (towards the first point), the end arrow right.
        let sb = stroke_bounds(&start.path, &stroke).unwrap();
        assert!(
            approx(sb.y0, 0.0, 0.01) && approx(sb.x0 + sb.x1, 0.0, 0.01),
            "{sb:?}"
        );
        let eb = stroke_bounds(&end.path, &stroke).unwrap();
        assert!(
            approx(eb.x1, 80.0, 0.01) && approx(eb.y0 + eb.y1, 100.0, 0.01),
            "{eb:?}"
        );
        // The middle vertex is untouched.
        let DrawOp::Path(line) = &ops[0] else {
            panic!()
        };
        assert!(line.path.segments().any(|s| s.end() == pts[1]));
    }

    #[test]
    fn short_lines_are_not_turned_inside_out() {
        // Shorter than the two insets together: each end gives up at most half.
        let a = Point::new(0.0, 0.0);
        let b = Point::new(4.0, 0.0);
        let stroke = Stroke::new(2.0, Rgb::BLACK);
        let ops = line_ops(
            a,
            b,
            LineEnding::ClosedArrow,
            LineEnding::ClosedArrow,
            &stroke,
            None,
        );
        let DrawOp::Path(line) = &ops[0] else {
            panic!()
        };
        let seg = line.path.segments().next().unwrap();
        assert!(approx(seg.start().x, 2.0, 1e-9) && approx(seg.end().x, 2.0, 1e-9));
        // Degenerate input: nothing to draw.
        assert!(line_ops(a, a, LineEnding::Circle, LineEnding::Circle, &stroke, None).is_empty());
        assert!(polyline_ops(&[a], LineEnding::Circle, LineEnding::None, &stroke, None).is_empty());
    }
}
