//! The one adapter between a [`DrawList`] and the egui [`Painter`].
//!
//! A markup's appearance is a `DrawList` in page space (pl-draw). The same list is written into
//! the file as the `/AP` stream and rastered by tiny-skia in the tests; this module is the
//! third consumer, drawing it on the canvas every frame: paths with their fills, strokes,
//! dashes and opacity, and groups with their transforms. Line endings and cloud borders arrive
//! as ordinary paths.
//!
//! What egui's painter cannot do exactly is left to the raster overlay ([`crate::overlay`]):
//! Multiply, text, images, and a translucent group of several shapes. [`needs_raster`] says
//! which lists those are. If such a list does reach this adapter (its page tile is not there
//! yet) it is drawn as well as the painter can: Multiply as plain translucency, each member of
//! a group with the group's opacity.

use egui::epaint::{Mesh, PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Shape};
use pl_doc::pl_draw::kurbo::{self, PathEl};
use pl_doc::pl_draw::{BlendMode, DrawList, DrawOp, LineCap, PathOp, Rgb};
use pl_doc::pl_geom::{Affine, Point};

/// How far a flattened curve may be from the true one, in screen pixels.
const FLATTEN_TOLERANCE: f64 = 0.15;
/// Outlines with more corners than this are filled as if convex (ear clipping is quadratic).
const MAX_EAR_POINTS: usize = 3000;

/// True for a list the painter cannot draw as it will look in the file.
pub fn needs_raster(list: &DrawList) -> bool {
    list.ops.iter().any(op_needs_raster)
}

fn op_needs_raster(op: &DrawOp) -> bool {
    match op {
        DrawOp::Path(p) => p.blend != BlendMode::Normal,
        DrawOp::Text(_) | DrawOp::Image(_) => true,
        DrawOp::Group(g) => {
            g.blend != BlendMode::Normal
                || (g.opacity < 1.0 && g.ops.len() > 1)
                || g.ops.iter().any(op_needs_raster)
        }
    }
}

/// Paints `list` (page space) through `to_view`, the page-to-view transform of the page it is
/// on; `origin` is where the view's top-left corner is on screen.
pub fn paint_list(painter: &Painter, origin: Pos2, to_view: Affine, list: &DrawList) {
    for op in &list.ops {
        paint_op(painter, origin, to_view, op, 1.0);
    }
}

fn paint_op(painter: &Painter, origin: Pos2, transform: Affine, op: &DrawOp, alpha: f64) {
    match op {
        DrawOp::Path(path) => paint_path(painter, origin, transform, path, alpha),
        DrawOp::Group(group) => {
            let inner = transform * group.transform;
            // A clip stays a rectangle on screen: the view only turns in quarter turns.
            let clipped;
            let painter = match group.clip {
                Some(clip) => {
                    let r = inner.transform_rect_bbox(clip);
                    let on_screen = egui::Rect::from_min_max(
                        Pos2::new(origin.x + r.x0 as f32, origin.y + r.y0 as f32),
                        Pos2::new(origin.x + r.x1 as f32, origin.y + r.y1 as f32),
                    );
                    clipped = painter.with_clip_rect(painter.clip_rect().intersect(on_screen));
                    &clipped
                }
                None => painter,
            };
            for child in &group.ops {
                paint_op(painter, origin, inner, child, alpha * group.opacity);
            }
        }
        // Left to the raster overlay; see the module documentation.
        DrawOp::Text(_) | DrawOp::Image(_) => {}
    }
}

fn color(rgb: Rgb, alpha: f64) -> Color32 {
    let [r, g, b] = rgb.to_u8();
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// One flattened subpath in screen coordinates.
struct Outline {
    points: Vec<Pos2>,
    closed: bool,
}

fn flatten(path: &kurbo::BezPath, transform: Affine, origin: Pos2) -> Vec<Outline> {
    let mut out: Vec<Outline> = Vec::new();
    let to_pos = |p: Point| Pos2::new(origin.x + p.x as f32, origin.y + p.y as f32);
    kurbo::flatten(transform * path.clone(), FLATTEN_TOLERANCE, |el| match el {
        PathEl::MoveTo(p) => out.push(Outline {
            points: vec![to_pos(p)],
            closed: false,
        }),
        PathEl::LineTo(p) => {
            if let Some(last) = out.last_mut() {
                let p = to_pos(p);
                if last.points.last() != Some(&p) {
                    last.points.push(p);
                }
            }
        }
        PathEl::ClosePath => {
            if let Some(last) = out.last_mut() {
                last.closed = true;
                if last.points.len() > 1 && last.points.first() == last.points.last() {
                    last.points.pop();
                }
            }
        }
        // `flatten` emits lines only.
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => {}
    });
    out
}

fn paint_path(painter: &Painter, origin: Pos2, transform: Affine, op: &PathOp, alpha: f64) {
    let alpha = alpha * op.opacity;
    if alpha <= 0.0 {
        return;
    }
    let outlines = flatten(&op.path, transform, origin);
    if let Some(fill) = &op.fill {
        let fill = color(fill.color, alpha);
        for outline in &outlines {
            fill_outline(painter, &outline.points, fill);
        }
    }
    let Some(stroke) = &op.stroke else {
        return;
    };
    let scale = transform.determinant().abs().sqrt();
    let width = ((stroke.width * scale) as f32).max(0.6);
    let pen = egui::Stroke::new(width, color(stroke.color, alpha));
    let dash = stroke.effective_dash().map(|(array, phase)| {
        (
            array.iter().map(|d| d * scale).collect::<Vec<f64>>(),
            phase * scale,
        )
    });
    let round = stroke.cap == LineCap::Round && width >= 3.0 && alpha >= 1.0;
    for outline in outlines {
        match &dash {
            Some((array, phase)) => {
                for piece in dash_pieces(&outline, array, *phase) {
                    painter.add(Shape::line(piece, PathStroke::from(pen)));
                }
            }
            None => {
                if outline.points.len() == 1 {
                    // A dot: a zero-length stroke shows only through its cap.
                    if stroke.cap != LineCap::Butt {
                        painter.circle_filled(outline.points[0], width / 2.0, pen.color);
                    }
                    continue;
                }
                if round && !outline.closed {
                    for end in [outline.points[0], outline.points[outline.points.len() - 1]] {
                        painter.circle_filled(end, width / 2.0, pen.color);
                    }
                }
                painter.add(if outline.closed {
                    Shape::closed_line(outline.points, PathStroke::from(pen))
                } else {
                    Shape::line(outline.points, PathStroke::from(pen))
                });
            }
        }
    }
}

/// Cuts an outline into the "on" pieces of a dash pattern (lengths in pixels).
fn dash_pieces(outline: &Outline, array: &[f64], phase: f64) -> Vec<Vec<Pos2>> {
    let total: f64 = array.iter().sum();
    let mut pieces: Vec<Vec<Pos2>> = Vec::new();
    if array.is_empty() || total <= 0.0 || !total.is_finite() || outline.points.len() < 2 {
        return vec![outline.points.clone()];
    }
    // An odd array repeats itself with on and off swapped.
    let pattern: Vec<f64> = if array.len() % 2 == 1 {
        array.iter().chain(array.iter()).copied().collect()
    } else {
        array.to_vec()
    };
    let period: f64 = pattern.iter().sum();
    let mut index = 0;
    let mut left = pattern[0];
    let mut skip = phase.rem_euclid(period);
    while skip > 0.0 {
        if skip >= left {
            skip -= left;
            index = (index + 1) % pattern.len();
            left = pattern[index];
        } else {
            left -= skip;
            skip = 0.0;
        }
    }
    let mut points = outline.points.clone();
    if outline.closed {
        points.push(outline.points[0]);
    }
    let mut current: Vec<Pos2> = Vec::new();
    let on = |index: usize| index.is_multiple_of(2);
    if on(index) {
        current.push(points[0]);
    }
    // A guard against patterns far finer than a pixel on a long outline.
    let mut budget = 200_000usize;
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let length = (b - a).length() as f64;
        if length <= 0.0 {
            continue;
        }
        let mut done = 0.0;
        while length - done > left && budget > 0 {
            budget -= 1;
            done += left;
            let at = a + (b - a) * (done / length) as f32;
            if on(index) {
                current.push(at);
                pieces.push(std::mem::take(&mut current));
            } else {
                current.push(at);
            }
            index = (index + 1) % pattern.len();
            left = pattern[index].max(1e-6);
        }
        left -= length - done;
        if on(index) {
            current.push(b);
        }
    }
    if current.len() > 1 {
        pieces.push(current);
    }
    pieces.retain(|p| p.len() > 1);
    pieces
}

fn cross(o: Pos2, a: Pos2, b: Pos2) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn is_convex(points: &[Pos2]) -> bool {
    let n = points.len();
    let mut sign = 0.0_f32;
    for i in 0..n {
        let c = cross(points[i], points[(i + 1) % n], points[(i + 2) % n]);
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

fn fill_outline(painter: &Painter, points: &[Pos2], fill: Color32) {
    if points.len() < 3 {
        return;
    }
    if points.len() > MAX_EAR_POINTS || is_convex(points) {
        painter.add(Shape::Path(PathShape::convex_polygon(
            points.to_vec(),
            fill,
            PathStroke::NONE,
        )));
        return;
    }
    let mut mesh = Mesh::default();
    for p in points {
        mesh.colored_vertex(*p, fill);
    }
    for [a, b, c] in ear_clip(points) {
        mesh.add_triangle(a, b, c);
    }
    painter.add(Shape::mesh(mesh));
}

fn in_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

/// Triangulates a simple polygon by ear clipping. Returns vertex index triples.
pub(crate) fn ear_clip(points: &[Pos2]) -> Vec<[u32; 3]> {
    let n = points.len();
    let mut triangles = Vec::with_capacity(n.saturating_sub(2));
    if n < 3 {
        return triangles;
    }
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    let winding = if area >= 0.0 { 1.0 } else { -1.0 };
    let mut ring: Vec<usize> = (0..n).collect();
    let mut guard = n * n + 8;
    while ring.len() > 3 && guard > 0 {
        guard -= 1;
        let m = ring.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (ring[(i + m - 1) % m], ring[i], ring[(i + 1) % m]);
            let (a, b, c) = (points[ia], points[ib], points[ic]);
            if cross(a, b, c) * winding <= 0.0 {
                continue;
            }
            let blocked = ring
                .iter()
                .filter(|&&k| k != ia && k != ib && k != ic)
                .any(|&k| in_triangle(points[k], a, b, c));
            if blocked {
                continue;
            }
            triangles.push([ia as u32, ib as u32, ic as u32]);
            ring.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            // Degenerate or self-crossing: give up on exactness and fan what is left.
            break;
        }
    }
    for i in 1..ring.len().saturating_sub(1) {
        triangles.push([ring[0] as u32, ring[i] as u32, ring[i + 1] as u32]);
    }
    triangles
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_doc::pl_draw::shapes::{highlight_op, polygon_path, rect_quad};
    use pl_doc::pl_draw::{Fill, ShapeStyle, Stroke};
    use pl_doc::pl_geom::Rect;

    fn area(points: &[Pos2], triangles: &[[u32; 3]]) -> f32 {
        triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| points[i as usize]);
                cross(a, b, c).abs() / 2.0
            })
            .sum()
    }

    #[test]
    fn ear_clipping_covers_a_concave_outline_exactly() {
        // An L shape: area 3.
        let l = [
            Pos2::new(0.0, 0.0),
            Pos2::new(2.0, 0.0),
            Pos2::new(2.0, 1.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(1.0, 2.0),
            Pos2::new(0.0, 2.0),
        ];
        assert!(!is_convex(&l));
        for points in [l.to_vec(), l.iter().rev().copied().collect::<Vec<_>>()] {
            let triangles = ear_clip(&points);
            assert_eq!(triangles.len(), 4);
            assert!((area(&points, &triangles) - 3.0).abs() < 1e-5);
        }
        let square = [
            Pos2::new(0.0, 0.0),
            Pos2::new(1.0, 0.0),
            Pos2::new(1.0, 1.0),
            Pos2::new(0.0, 1.0),
        ];
        assert!(is_convex(&square));
    }

    #[test]
    fn dashes_cut_a_line_into_the_on_pieces() {
        let outline = Outline {
            points: vec![Pos2::new(0.0, 0.0), Pos2::new(10.0, 0.0)],
            closed: false,
        };
        let pieces = dash_pieces(&outline, &[3.0, 2.0], 0.0);
        let spans: Vec<(f32, f32)> = pieces.iter().map(|p| (p[0].x, p[p.len() - 1].x)).collect();
        assert_eq!(spans, vec![(0.0, 3.0), (5.0, 8.0)]);
        // A phase starts part-way into the pattern.
        let pieces = dash_pieces(&outline, &[3.0, 2.0], 4.0);
        let spans: Vec<(f32, f32)> = pieces.iter().map(|p| (p[0].x, p[p.len() - 1].x)).collect();
        assert_eq!(spans, vec![(1.0, 4.0), (6.0, 9.0)]);
        // A closed outline dashes its closing edge too.
        let square = Outline {
            points: vec![
                Pos2::new(0.0, 0.0),
                Pos2::new(4.0, 0.0),
                Pos2::new(4.0, 4.0),
                Pos2::new(0.0, 4.0),
            ],
            closed: true,
        };
        let length: f32 = dash_pieces(&square, &[2.0, 2.0], 0.0)
            .iter()
            .map(|p| p.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f32>())
            .sum();
        assert!((length - 8.0).abs() < 1e-4);
    }

    #[test]
    fn multiply_and_translucent_groups_go_to_the_raster() {
        let rect = Rect::new(10.0, 10.0, 60.0, 40.0);
        let mut plain = DrawList::new();
        plain.push_shape(
            polygon_path(&[
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(5.0, 8.0),
            ]),
            &ShapeStyle::stroked(Stroke::new(1.0, Rgb::RED))
                .with_fill(Fill::new(Rgb::BLUE))
                .with_opacity(0.5),
        );
        let mut highlight = DrawList::new();
        highlight.push(highlight_op(&[rect_quad(rect)], Rgb::YELLOW, 0.6));
        assert!(needs_raster(&highlight));
        // Whatever shape `push_shape` gave the translucent fill-and-stroke, the answer must
        // agree with whether it is one isolated group of several shapes.
        let grouped = plain
            .ops
            .iter()
            .any(|op| matches!(op, DrawOp::Group(g) if g.opacity < 1.0 && g.ops.len() > 1));
        assert_eq!(needs_raster(&plain), grouped);
        let mut opaque = DrawList::new();
        opaque.push_shape(
            polygon_path(&[
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(5.0, 8.0),
            ]),
            &ShapeStyle::stroked(Stroke::new(1.0, Rgb::RED)),
        );
        assert!(!needs_raster(&opaque));
    }
}
