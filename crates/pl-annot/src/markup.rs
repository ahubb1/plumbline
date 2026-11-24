//! Text markups from a text selection: turning the boxes of the selected characters into the
//! `/QuadPoints` of a Highlight, Underline, StrikeOut or Squiggly, one quad per line.

use pl_draw::shapes::Quad;
use pl_geom::{Point, Rect, Vec2};

use crate::model::{AnnotKind, Annotation, Origin};

/// The quad of an axis-aligned character (or word) box whose text reads in the direction
/// `rotation` (0, 90, 180 or 270 degrees anticlockwise from +x in page space; other values are
/// taken as 0). The ring starts at the start of the baseline, as
/// [`Geometry::Quads`](crate::Geometry::Quads) wants it.
pub fn quad_for_box(rect: Rect, rotation: u16) -> Quad {
    let r = rect.abs();
    let (bl, br, tr, tl) = (
        Point::new(r.x0, r.y0),
        Point::new(r.x1, r.y0),
        Point::new(r.x1, r.y1),
        Point::new(r.x0, r.y1),
    );
    match rotation {
        90 => [br, tr, tl, bl],
        180 => [tr, tl, bl, br],
        270 => [tl, bl, br, tr],
        _ => [bl, br, tr, tl],
    }
}

/// The reading direction of a run of characters, from the positions of their origins (the
/// points on the baseline where each glyph starts): 0, 90, 180 or 270 degrees anticlockwise
/// from +x in page space. Text set normally on a page reads at 0 whatever the page's
/// `/Rotate`; 0 is also the answer when there are too few characters to tell.
pub fn reading_direction(origins: &[Point]) -> u16 {
    let mut sum = Vec2::ZERO;
    for pair in origins.windows(2) {
        let step = pair[1] - pair[0];
        // A jump back to the start of the next line is not the reading direction.
        if step.hypot() > 0.0 && step.x.is_finite() && step.y.is_finite() {
            sum += step / step.hypot();
        }
    }
    if sum.hypot() < 1e-9 {
        return 0;
    }
    if sum.x.abs() >= sum.y.abs() {
        if sum.x >= 0.0 {
            0
        } else {
            180
        }
    } else if sum.y > 0.0 {
        90
    } else {
        270
    }
}

struct Frame {
    origin: Point,
    along: Vec2,
    up: Vec2,
}

impl Frame {
    /// The frame of a quad: unit vectors along its baseline and up its height. `None` for a
    /// quad with no length or no height.
    fn of(q: &Quad) -> Option<Frame> {
        let along = q[1] - q[0];
        let up = q[3] - q[0];
        if along.hypot() <= 0.0 || up.hypot() <= 0.0 {
            return None;
        }
        let along = along / along.hypot();
        // Up, square to the baseline, on the side the quad's height is.
        let normal = Vec2::new(-along.y, along.x);
        let up = if normal.dot(up) >= 0.0 {
            normal
        } else {
            -normal
        };
        Some(Frame {
            origin: q[0],
            along,
            up,
        })
    }

    /// The extent of a quad in this frame: (start, end) along and (bottom, top) up.
    fn extent(&self, q: &Quad) -> (f64, f64, f64, f64) {
        let mut e = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in q {
            let d = *p - self.origin;
            let (a, u) = (d.dot(self.along), d.dot(self.up));
            e = (e.0.min(a), e.1.max(a), e.2.min(u), e.3.max(u));
        }
        e
    }

    fn quad(&self, e: (f64, f64, f64, f64)) -> Quad {
        let at = |a: f64, u: f64| self.origin + self.along * a + self.up * u;
        [at(e.0, e.2), at(e.1, e.2), at(e.1, e.3), at(e.0, e.3)]
    }
}

/// Merges the quads of consecutive characters (or words) into one quad per line.
///
/// Two neighbours are on the same line when they read in the same direction and their heights
/// overlap by more than half of the smaller one. The merged quad spans them from the first
/// baseline start to the furthest end and from the lowest bottom to the highest top, so
/// characters of different sizes on one line give one straight band. Quads with no area are
/// dropped. The order of lines is the order of the input.
pub fn merge_quads_by_line(quads: &[Quad]) -> Vec<Quad> {
    let mut out: Vec<Quad> = Vec::new();
    let mut current: Option<(Frame, (f64, f64, f64, f64))> = None;
    for q in quads {
        if !q.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
            continue;
        }
        let Some(frame) = Frame::of(q) else {
            continue;
        };
        if let Some((line, extent)) = current.as_mut() {
            let same_direction = line.along.dot(frame.along) > 0.99;
            let e = line.extent(q);
            let overlap = extent.3.min(e.3) - extent.2.max(e.2);
            let smaller = (extent.3 - extent.2).min(e.3 - e.2);
            if same_direction && smaller > 0.0 && overlap > 0.5 * smaller {
                *extent = (
                    extent.0.min(e.0),
                    extent.1.max(e.1),
                    extent.2.min(e.2),
                    extent.3.max(e.3),
                );
                continue;
            }
            out.push(line.quad(*extent));
        }
        let extent = frame.extent(q);
        current = Some((frame, extent));
    }
    if let Some((line, extent)) = current {
        out.push(line.quad(extent));
    }
    out
}

impl Annotation {
    /// A Highlight, Underline, StrikeOut or Squiggly over a text selection given as the boxes
    /// of its characters in reading order (axis-aligned, page space) and the direction the
    /// text reads in (see [`reading_direction`]). The boxes are merged into one quad per line.
    /// Any other `kind` is treated as Highlight. `None` when no box has any area.
    pub fn text_markup_from_boxes(
        kind: AnnotKind,
        page: usize,
        boxes: &[Rect],
        rotation: u16,
        origin: &Origin,
    ) -> Option<Annotation> {
        let quads: Vec<Quad> = boxes
            .iter()
            .filter(|r| r.width().abs() > 0.0 && r.height().abs() > 0.0)
            .map(|r| quad_for_box(*r, rotation))
            .collect();
        Annotation::text_markup_from_quads(kind, page, &quads, origin)
    }

    /// As [`Annotation::text_markup_from_boxes`], from quads that already start at the start
    /// of their baseline (slanted text, or the quads of a search hit).
    pub fn text_markup_from_quads(
        kind: AnnotKind,
        page: usize,
        quads: &[Quad],
        origin: &Origin,
    ) -> Option<Annotation> {
        let merged = merge_quads_by_line(quads);
        if merged.is_empty() {
            return None;
        }
        Some(Annotation::text_markup(kind, page, merged, origin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Geometry;
    use crate::objects::PdfDate;

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn boxes(y: f64, h: f64, xs: &[(f64, f64)]) -> Vec<Rect> {
        xs.iter()
            .map(|(a, b)| Rect::new(*a, y, *b, y + h))
            .collect()
    }

    #[test]
    fn characters_on_a_line_merge_and_a_new_line_starts_a_new_quad() {
        let mut all = boxes(100.0, 12.0, &[(10.0, 16.0), (16.0, 22.5), (22.5, 30.0)]);
        // A taller character on the same line, then the next line.
        all.push(Rect::new(30.0, 99.0, 38.0, 114.0));
        all.extend(boxes(84.0, 12.0, &[(10.0, 15.0), (15.0, 40.0)]));
        let a = Annotation::text_markup_from_boxes(AnnotKind::Highlight, 0, &all, 0, &origin())
            .unwrap();
        let Geometry::Quads(quads) = &a.geometry else {
            panic!()
        };
        assert_eq!(
            quads[..],
            [
                quad_for_box(Rect::new(10.0, 99.0, 38.0, 114.0), 0),
                quad_for_box(Rect::new(10.0, 84.0, 40.0, 96.0), 0),
            ]
        );
        assert_eq!(
            quads[0][0],
            Point::new(10.0, 99.0),
            "starts at the baseline start"
        );
        assert_eq!(a.kind, AnnotKind::Highlight);
        // Boxes without area are skipped; nothing at all gives no annotation.
        let none = Annotation::text_markup_from_boxes(
            AnnotKind::Underline,
            0,
            &[Rect::new(5.0, 5.0, 5.0, 9.0), Rect::ZERO],
            0,
            &origin(),
        );
        assert!(none.is_none());
        let other = Annotation::text_markup_from_boxes(AnnotKind::Square, 0, &all, 0, &origin());
        assert_eq!(other.unwrap().kind, AnnotKind::Highlight);
    }

    #[test]
    fn rotated_text_keeps_its_baseline_at_the_start_of_each_quad() {
        // Text reading up the page: characters stacked in +y, lines stepping in -x... the
        // glyph boxes are tall in x (the line height) and short in y (the advance).
        let column = |x: f64, ys: &[(f64, f64)]| -> Vec<Rect> {
            ys.iter()
                .map(|(a, b)| Rect::new(x, *a, x + 12.0, *b))
                .collect()
        };
        let mut all = column(200.0, &[(50.0, 56.0), (56.0, 63.0), (63.0, 70.0)]);
        all.extend(column(216.0, &[(50.0, 58.0), (58.0, 61.0)]));
        let a = Annotation::text_markup_from_boxes(AnnotKind::Underline, 0, &all, 90, &origin())
            .unwrap();
        let Geometry::Quads(quads) = &a.geometry else {
            panic!()
        };
        assert_eq!(quads.len(), 2);
        // Baseline start is the bottom-right corner in page space; the baseline runs up.
        assert_eq!(quads[0][0], Point::new(212.0, 50.0));
        assert_eq!(quads[0][1], Point::new(212.0, 70.0));
        assert_eq!(quads[0][3], Point::new(200.0, 50.0));
        assert_eq!(quads[1][1], Point::new(228.0, 61.0));
        for rotation in [0u16, 90, 180, 270] {
            let q = quad_for_box(Rect::new(0.0, 0.0, 4.0, 2.0), rotation);
            let along = q[1] - q[0];
            let want = match rotation {
                0 => Vec2::new(4.0, 0.0),
                90 => Vec2::new(0.0, 2.0),
                180 => Vec2::new(-4.0, 0.0),
                _ => Vec2::new(0.0, -2.0),
            };
            assert_eq!(along, want);
            // Anticlockwise ring: the height is to the left of the baseline.
            assert!(along.cross(q[3] - q[0]) > 0.0);
        }
    }

    #[test]
    fn reading_direction_follows_the_origins() {
        let run = |f: fn(f64) -> Point| -> Vec<Point> { (0..6).map(|i| f(i as f64)).collect() };
        assert_eq!(
            reading_direction(&run(|i| Point::new(10.0 + 6.0 * i, 50.0))),
            0
        );
        assert_eq!(
            reading_direction(&run(|i| Point::new(10.0, 50.0 + 6.0 * i))),
            90
        );
        assert_eq!(
            reading_direction(&run(|i| Point::new(90.0 - 6.0 * i, 50.0))),
            180
        );
        assert_eq!(
            reading_direction(&run(|i| Point::new(10.0, 90.0 - 6.0 * i))),
            270
        );
        // Two lines: the jump back does not outweigh the steps along the lines.
        let mut two = run(|i| Point::new(10.0 + 6.0 * i, 50.0));
        two.extend(run(|i| Point::new(10.0 + 6.0 * i, 36.0)));
        assert_eq!(reading_direction(&two), 0);
        assert_eq!(reading_direction(&[]), 0);
        assert_eq!(reading_direction(&[Point::new(1.0, 1.0)]), 0);
    }

    #[test]
    fn slanted_quads_merge_along_their_own_baseline() {
        // Two words on one slanted line (slope 1/4), then one on the next line.
        let word = |x: f64, y: f64, len: f64| -> Quad {
            let along = Vec2::new(4.0, 1.0) / 17f64.sqrt();
            let up = Vec2::new(-1.0, 4.0) / 17f64.sqrt();
            let o = Point::new(x, y);
            [
                o,
                o + along * len,
                o + along * len + up * 10.0,
                o + up * 10.0,
            ]
        };
        let along = Vec2::new(4.0, 1.0) / 17f64.sqrt();
        let first = word(0.0, 0.0, 30.0);
        let start2 = Point::ZERO + along * 34.0;
        let second = word(start2.x, start2.y, 20.0);
        let below = word(4.0, -16.0, 25.0);
        let merged = merge_quads_by_line(&[first, second, below]);
        assert_eq!(merged.len(), 2);
        let want_end = Point::ZERO + along * 54.0;
        assert!((merged[0][0] - Point::ZERO).hypot() < 1e-9);
        assert!((merged[0][1] - want_end).hypot() < 1e-9);
        assert!((merged[0][3] - first[3]).hypot() < 1e-9);
        assert!((merged[1][0] - below[0]).hypot() < 1e-9);
        // Degenerate and non-finite quads are dropped.
        let flat = [
            Point::ZERO,
            Point::new(5.0, 0.0),
            Point::new(5.0, 0.0),
            Point::ZERO,
        ];
        let nan = [Point::new(f64::NAN, 0.0); 4];
        assert!(merge_quads_by_line(&[flat, nan]).is_empty());
    }
}
