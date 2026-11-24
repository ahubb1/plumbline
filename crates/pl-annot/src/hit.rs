//! Hit-testing and edit handles, as plain data in page space. Tolerances are in page units:
//! the caller converts its pixel tolerance with the view's zoom.

use pl_draw::kurbo::{flatten, BezPath, PathEl, Shape};
use pl_geom::{point_in_polygon, point_segment_distance, Point, Rect, Vec2};

use crate::model::{AnnotKind, Annotation, Geometry};

/// A corner or edge midpoint of a rectangle, named in unrotated page space (y up, so "top" is
/// the larger y). On a rotated page the app maps these to what the user sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RectHandle {
    BottomLeft,
    Bottom,
    BottomRight,
    Right,
    TopRight,
    Top,
    TopLeft,
    Left,
}

impl RectHandle {
    pub const ALL: [RectHandle; 8] = [
        RectHandle::BottomLeft,
        RectHandle::Bottom,
        RectHandle::BottomRight,
        RectHandle::Right,
        RectHandle::TopRight,
        RectHandle::Top,
        RectHandle::TopLeft,
        RectHandle::Left,
    ];

    /// Which sides the handle drags: (x side, y side), each -1 (left/bottom), 0 (neither) or
    /// 1 (right/top).
    fn sides(self) -> (i8, i8) {
        match self {
            RectHandle::BottomLeft => (-1, -1),
            RectHandle::Bottom => (0, -1),
            RectHandle::BottomRight => (1, -1),
            RectHandle::Right => (1, 0),
            RectHandle::TopRight => (1, 1),
            RectHandle::Top => (0, 1),
            RectHandle::TopLeft => (-1, 1),
            RectHandle::Left => (-1, 0),
        }
    }

    /// Where the handle sits on `rect`.
    pub fn position(self, rect: Rect) -> Point {
        let (sx, sy) = self.sides();
        let pick = |side: i8, lo: f64, hi: f64| match side {
            -1 => lo,
            1 => hi,
            _ => (lo + hi) / 2.0,
        };
        Point::new(pick(sx, rect.x0, rect.x1), pick(sy, rect.y0, rect.y1))
    }

    /// `rect` with this handle dragged to `to`. Dragging past the opposite side flips the
    /// rectangle; the result is always normalised.
    pub fn drag(self, rect: Rect, to: Point) -> Rect {
        let (sx, sy) = self.sides();
        let mut r = rect;
        match sx {
            -1 => r.x0 = to.x,
            1 => r.x1 = to.x,
            _ => {}
        }
        match sy {
            -1 => r.y0 = to.y,
            1 => r.y1 = to.y,
            _ => {}
        }
        r.abs()
    }
}

/// Which part of an annotation a handle edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleKind {
    /// Resizes the rectangle of a Square, Circle, Link or Popup, or scales an Ink drawing.
    Rect(RectHandle),
    /// Moves one vertex of a PolyLine or Polygon; for a Line, 0 is the start and 1 the end.
    Vertex(usize),
}

/// An edit handle and where it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Handle {
    pub kind: HandleKind,
    pub at: Point,
}

fn distance_to_polyline(p: Point, points: &[Point]) -> f64 {
    match points {
        [] => f64::INFINITY,
        [only] => (p - *only).hypot(),
        _ => points
            .windows(2)
            .map(|w| point_segment_distance(p, w[0], w[1]))
            .fold(f64::INFINITY, f64::min),
    }
}

fn distance_to_path(p: Point, path: &BezPath) -> f64 {
    let mut best = f64::INFINITY;
    let mut start = Point::ZERO;
    let mut last = Point::ZERO;
    flatten(path.elements().iter().copied(), 0.05, |el| match el {
        PathEl::MoveTo(q) => {
            start = q;
            last = q;
        }
        PathEl::LineTo(q) => {
            best = best.min(point_segment_distance(p, last, q));
            last = q;
        }
        PathEl::ClosePath => {
            best = best.min(point_segment_distance(p, last, start));
            last = start;
        }
        _ => {}
    });
    best
}

impl Annotation {
    /// True when `p` is on the annotation, within `tolerance` page units.
    ///
    /// A filled shape is hit anywhere inside; an unfilled one only on its outline, so that a
    /// large empty rectangle does not swallow clicks meant for what is under it. Lines and ink
    /// are hit along the stroke. Text markups, notes, links, popups and annotations shown from
    /// their own appearance are hit anywhere in their area.
    pub fn hit_test(&self, p: Point, tolerance: f64) -> bool {
        if !(p.x.is_finite() && p.y.is_finite()) || !self.geometry.is_finite() {
            return false;
        }
        let tolerance = tolerance.max(0.0);
        let reach = tolerance
            + if self.style.stroke.is_some() {
                self.style.width.max(0.0) / 2.0
            } else {
                0.0
            };
        let in_box = |r: Rect| r.inflate(tolerance, tolerance).contains(p);
        if !self.is_modelled() {
            return in_box(self.geometry.bounds());
        }
        if let Some(m) = self.measurement() {
            if let Some(hit) = crate::measure_draw::hit(self, m, p, reach, tolerance) {
                return hit;
            }
        }
        match (&self.kind, &self.geometry) {
            (AnnotKind::Square | AnnotKind::Circle | AnnotKind::Polygon, _) => {
                let Some(path) = self.outline() else {
                    return false;
                };
                (self.style.fill.is_some() && path.contains(p))
                    || distance_to_path(p, &path) <= reach
            }
            (_, Geometry::Line { a, b }) => point_segment_distance(p, *a, *b) <= reach,
            (_, Geometry::Points(points)) => distance_to_polyline(p, points) <= reach,
            (_, Geometry::Ink(strokes)) => {
                strokes.iter().any(|s| distance_to_polyline(p, s) <= reach)
            }
            (_, Geometry::Quads(quads)) => quads.iter().any(|q| {
                point_in_polygon(p, q)
                    || (0..4).any(|i| point_segment_distance(p, q[i], q[(i + 1) % 4]) <= tolerance)
            }),
            (AnnotKind::FreeText, Geometry::Rect(rect)) => {
                let on_leader = self
                    .free_text
                    .as_ref()
                    .is_some_and(|t| distance_to_polyline(p, &t.callout) <= reach);
                in_box(*rect) || on_leader
            }
            (_, Geometry::Rect(rect)) => in_box(*rect),
        }
    }

    /// The handles a selected annotation shows. Empty for kinds that can only be moved: notes,
    /// text markups and everything unmodelled.
    pub fn handles(&self) -> Vec<Handle> {
        if !self.is_modelled() {
            return Vec::new();
        }
        let around = |rect: Rect| {
            RectHandle::ALL
                .iter()
                .map(|h| Handle {
                    kind: HandleKind::Rect(*h),
                    at: h.position(rect),
                })
                .collect()
        };
        let vertices = |points: &[Point]| {
            points
                .iter()
                .enumerate()
                .map(|(i, p)| Handle {
                    kind: HandleKind::Vertex(i),
                    at: *p,
                })
                .collect()
        };
        if self
            .measurement()
            .is_some_and(|m| m.kind == crate::measure::MeasureKind::Count)
        {
            // Each marker of a count is dragged on its own.
            return vertices(&self.count_points());
        }
        match (&self.kind, &self.geometry) {
            (AnnotKind::Text, _) | (_, Geometry::Quads(_)) => Vec::new(),
            (AnnotKind::FreeText, Geometry::Rect(rect)) => {
                // The box, then the leader's free points: the tip and the knee. Its last
                // point belongs to the box.
                let mut handles: Vec<Handle> = around(*rect);
                if let Some(t) = &self.free_text {
                    let free = t.callout.len().saturating_sub(1);
                    handles.extend(vertices(&t.callout[..free]));
                }
                handles
            }
            (_, Geometry::Rect(rect)) => around(*rect),
            (_, Geometry::Line { a, b }) => vertices(&[*a, *b]),
            (_, Geometry::Points(points)) => vertices(points),
            (_, Geometry::Ink(_)) => around(self.geometry.bounds()),
        }
    }

    /// The handle under `p`, within `tolerance` page units; the nearest one if several are.
    pub fn handle_at(&self, p: Point, tolerance: f64) -> Option<Handle> {
        self.handles()
            .into_iter()
            .map(|h| ((h.at - p).hypot(), h))
            .filter(|(d, _)| *d <= tolerance.max(0.0))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, h)| h)
    }

    /// Drags a handle to `to`. Returns false, changing nothing, when the annotation has no
    /// such handle.
    pub fn move_handle(&mut self, handle: HandleKind, to: Point) -> bool {
        if !self.is_modelled() || !(to.x.is_finite() && to.y.is_finite()) {
            return false;
        }
        if self.kind == AnnotKind::Text {
            return false;
        }
        if let (HandleKind::Vertex(i), AnnotKind::FreeText) = (handle, &self.kind) {
            let Some(t) = self.free_text.as_mut() else {
                return false;
            };
            if i + 1 >= t.callout.len() {
                return false;
            }
            t.callout[i] = to;
            if i == 0 {
                // The tip moved: the leader may now leave another side of the box.
                self.reroute_callout();
            }
            return true;
        }
        let count = self
            .measurement()
            .is_some_and(|m| m.kind == crate::measure::MeasureKind::Count);
        if let (true, HandleKind::Vertex(i), Geometry::Ink(strokes)) =
            (count, handle, &mut self.geometry)
        {
            return match strokes.get_mut(i) {
                Some(stroke) => {
                    *stroke = vec![to, to];
                    true
                }
                None => false,
            };
        }
        match (handle, &mut self.geometry) {
            (HandleKind::Rect(h), Geometry::Rect(rect)) => {
                *rect = h.drag(*rect, to);
                self.reroute_callout();
                true
            }
            (HandleKind::Rect(h), Geometry::Ink(_)) => {
                let old = self.geometry.bounds();
                let new = h.drag(old, to);
                let scale = |v: f64, o0: f64, o1: f64, n0: f64, n1: f64| {
                    if o1 - o0 > 0.0 {
                        n0 + (v - o0) * (n1 - n0) / (o1 - o0)
                    } else {
                        v + (n0 + n1 - o0 - o1) / 2.0
                    }
                };
                self.geometry = self.geometry.map_points(|p| {
                    Point::new(
                        scale(p.x, old.x0, old.x1, new.x0, new.x1),
                        scale(p.y, old.y0, old.y1, new.y0, new.y1),
                    )
                });
                true
            }
            (HandleKind::Vertex(i), Geometry::Line { a, b }) => match i {
                0 => {
                    *a = to;
                    true
                }
                1 => {
                    *b = to;
                    true
                }
                _ => false,
            },
            (HandleKind::Vertex(i), Geometry::Points(points)) => match points.get_mut(i) {
                Some(p) => {
                    *p = to;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Moves the whole annotation.
    pub fn translate(&mut self, by: Vec2) {
        if by.x.is_finite() && by.y.is_finite() {
            self.geometry = self.geometry.map_points(|p| p + by);
            if let Some(t) = self.free_text.as_mut() {
                for p in &mut t.callout {
                    *p += by;
                }
            }
            if let Some(m) = self.measure.as_mut() {
                for p in m.cutouts.iter_mut().flatten() {
                    *p += by;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Origin;
    use crate::objects::PdfDate;
    use pl_draw::Rgb;

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn an_empty_rectangle_is_hit_on_its_border_only() {
        let mut a = Annotation::square(0, Rect::new(100.0, 100.0, 200.0, 160.0), &origin());
        a.style.width = 4.0;
        assert!(a.hit_test(pt(100.5, 130.0), 1.0));
        assert!(a.hit_test(pt(150.0, 158.0), 1.0));
        assert!(!a.hit_test(pt(150.0, 130.0), 1.0), "the middle is empty");
        assert!(!a.hit_test(pt(96.0, 130.0), 1.0));
        assert!(
            a.hit_test(pt(97.5, 130.0), 3.0),
            "tolerance widens the border"
        );
        a.style.fill = Some(Rgb::BLUE);
        assert!(
            a.hit_test(pt(150.0, 130.0), 1.0),
            "a filled one is hit inside"
        );
        assert!(!a.hit_test(pt(f64::NAN, 130.0), 1.0));
    }

    #[test]
    fn an_ellipse_is_hit_on_its_curve_not_its_box() {
        let mut a = Annotation::circle(0, Rect::new(0.0, 0.0, 200.0, 100.0), &origin());
        a.style.width = 2.0;
        assert!(a.hit_test(pt(100.0, 99.0), 1.0));
        assert!(a.hit_test(pt(1.0, 50.0), 1.0));
        assert!(
            !a.hit_test(pt(3.0, 3.0), 1.0),
            "the box corner is outside the curve"
        );
        a.style.fill = Some(Rgb::RED);
        assert!(a.hit_test(pt(100.0, 50.0), 0.0));
        assert!(!a.hit_test(pt(3.0, 3.0), 1.0));
    }

    #[test]
    fn lines_polylines_and_ink_are_hit_along_the_stroke() {
        let mut line = Annotation::line(0, pt(0.0, 0.0), pt(100.0, 100.0), &origin());
        line.style.width = 2.0;
        assert!(line.hit_test(pt(50.0, 51.0), 1.0));
        assert!(!line.hit_test(pt(50.0, 60.0), 1.0));
        assert!(!line.hit_test(pt(110.0, 110.0), 1.0), "past the end");

        let poly = Annotation::polyline(
            0,
            vec![pt(0.0, 0.0), pt(50.0, 0.0), pt(50.0, 50.0)],
            &origin(),
        );
        assert!(poly.hit_test(pt(50.0, 25.0), 1.0));
        assert!(!poly.hit_test(pt(25.0, 25.0), 1.0));

        let ink = Annotation::ink(
            0,
            vec![vec![pt(0.0, 0.0), pt(10.0, 10.0)], vec![pt(40.0, 40.0)]],
            &origin(),
        );
        assert!(ink.hit_test(pt(5.0, 5.0), 1.0));
        assert!(ink.hit_test(pt(40.5, 40.0), 1.0), "a dot");
        assert!(!ink.hit_test(pt(25.0, 25.0), 1.0));
    }

    #[test]
    fn a_polygon_is_hit_inside_only_when_filled() {
        let tri = vec![pt(0.0, 0.0), pt(100.0, 0.0), pt(50.0, 80.0)];
        let mut a = Annotation::polygon(0, tri, &origin());
        assert!(a.hit_test(pt(50.0, 0.5), 1.0));
        assert!(!a.hit_test(pt(50.0, 30.0), 1.0));
        a.style.fill = Some(Rgb::GREEN);
        assert!(a.hit_test(pt(50.0, 30.0), 1.0));
        assert!(!a.hit_test(pt(5.0, 60.0), 1.0));
        // A cloud border is hit on its scallops, which bulge outside the polygon.
        a.style.fill = None;
        a.style.cloud = Some(2.0);
        assert!(!a.hit_test(pt(50.0, 30.0), 1.0));
        let b = a.bounds();
        assert!(b.y0 < -1.0, "scallops reach below the base: {b:?}");
    }

    #[test]
    fn text_markups_and_boxes_are_hit_in_their_area() {
        let quad = [
            pt(10.0, 10.0),
            pt(60.0, 10.0),
            pt(60.0, 22.0),
            pt(10.0, 22.0),
        ];
        let h = Annotation::text_markup(AnnotKind::Highlight, 0, vec![quad], &origin());
        assert!(h.hit_test(pt(30.0, 15.0), 0.0));
        assert!(!h.hit_test(pt(30.0, 30.0), 1.0));
        assert!(h.handles().is_empty());

        let note = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "n", &origin());
        assert!(note.hit_test(pt(9.0, 9.0), 0.0));
        assert!(note.hit_test(pt(19.0, 9.0), 2.0));
        assert!(!note.hit_test(pt(25.0, 9.0), 2.0));
        assert!(note.handles().is_empty());
    }

    #[test]
    fn rectangle_handles_resize_and_flip() {
        let mut a = Annotation::square(0, Rect::new(10.0, 20.0, 110.0, 80.0), &origin());
        let handles = a.handles();
        assert_eq!(handles.len(), 8);
        assert_eq!(handles[0].at, pt(10.0, 20.0));
        assert_eq!(
            a.handle_at(pt(111.0, 81.0), 3.0).unwrap().kind,
            HandleKind::Rect(RectHandle::TopRight)
        );
        assert_eq!(
            a.handle_at(pt(60.0, 79.0), 3.0).unwrap().kind,
            HandleKind::Rect(RectHandle::Top)
        );
        assert_eq!(a.handle_at(pt(60.0, 50.0), 3.0), None);

        assert!(a.move_handle(HandleKind::Rect(RectHandle::TopRight), pt(150.0, 100.0)));
        assert_eq!(
            a.geometry,
            Geometry::Rect(Rect::new(10.0, 20.0, 150.0, 100.0))
        );
        assert!(a.move_handle(HandleKind::Rect(RectHandle::Left), pt(40.0, -999.0)));
        assert_eq!(
            a.geometry,
            Geometry::Rect(Rect::new(40.0, 20.0, 150.0, 100.0))
        );
        // Dragged past the opposite side.
        assert!(a.move_handle(HandleKind::Rect(RectHandle::Bottom), pt(0.0, 130.0)));
        assert_eq!(
            a.geometry,
            Geometry::Rect(Rect::new(40.0, 100.0, 150.0, 130.0))
        );
        assert!(!a.move_handle(HandleKind::Vertex(0), pt(0.0, 0.0)));
    }

    #[test]
    fn vertex_handles_move_single_points() {
        let mut line = Annotation::line(0, pt(0.0, 0.0), pt(10.0, 0.0), &origin());
        assert_eq!(line.handles().len(), 2);
        assert!(line.move_handle(HandleKind::Vertex(1), pt(10.0, 10.0)));
        assert_eq!(
            line.geometry,
            Geometry::Line {
                a: pt(0.0, 0.0),
                b: pt(10.0, 10.0)
            }
        );
        assert!(!line.move_handle(HandleKind::Vertex(2), pt(0.0, 0.0)));

        let mut poly = Annotation::polygon(
            0,
            vec![pt(0.0, 0.0), pt(10.0, 0.0), pt(5.0, 5.0)],
            &origin(),
        );
        assert_eq!(
            poly.handle_at(pt(5.2, 5.1), 1.0).unwrap().kind,
            HandleKind::Vertex(2)
        );
        assert!(poly.move_handle(HandleKind::Vertex(2), pt(5.0, 9.0)));
        assert_eq!(
            poly.geometry,
            Geometry::Points(vec![pt(0.0, 0.0), pt(10.0, 0.0), pt(5.0, 9.0)])
        );
        assert!(!poly.move_handle(HandleKind::Vertex(3), pt(0.0, 0.0)));
        assert!(!poly.move_handle(HandleKind::Vertex(0), pt(f64::INFINITY, 0.0)));
    }

    #[test]
    fn ink_scales_with_its_box_and_everything_translates() {
        let mut ink = Annotation::ink(
            0,
            vec![vec![pt(0.0, 0.0), pt(10.0, 20.0)], vec![pt(5.0, 10.0)]],
            &origin(),
        );
        assert_eq!(ink.handles().len(), 8);
        assert!(ink.move_handle(HandleKind::Rect(RectHandle::TopRight), pt(20.0, 40.0)));
        assert_eq!(
            ink.geometry,
            Geometry::Ink(vec![
                vec![pt(0.0, 0.0), pt(20.0, 40.0)],
                vec![pt(10.0, 20.0)]
            ])
        );
        ink.translate(Vec2::new(1.0, -1.0));
        assert_eq!(ink.geometry.bounds(), Rect::new(1.0, -1.0, 21.0, 39.0));
        ink.translate(Vec2::new(f64::NAN, 0.0));
        assert_eq!(ink.geometry.bounds(), Rect::new(1.0, -1.0, 21.0, 39.0));
    }
}
