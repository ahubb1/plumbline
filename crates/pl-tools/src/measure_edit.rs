//! Editing a selected measurement with the Select tool, beyond the vertex handles every
//! shape has: the dimension line's offset, the caption's place, the corners of cutouts, and
//! inserting or deleting a vertex with a double-click.

use pl_annot::{Annotation, Caption, Geometry, MeasureKind, Measurement};
use pl_geom::{closest_point_on_segment, Point, Rotation, Vec2};

use crate::arrange::movable;
use crate::intent::{AnnotChange, Intent};
use crate::snap::constrain_step;
use crate::tool::{HANDLE_PX, HIT_PX};

/// A part of a measurement that the Select tool can drag besides its vertices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeasureGrip {
    /// The dimension line of a Length, Radius or Diameter: dragging it sets how far it stands
    /// off the measured points (`/LL`).
    Leader,
    /// The value of a Length, Radius or Diameter: dragging it moves it along and away from
    /// the line (`/CO`).
    Caption,
    /// One corner of a cutout of an Area or Volume.
    Cutout { ring: usize, index: usize },
}

/// A [`MeasureGrip`] and where it is drawn, in page space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasureHandle {
    pub grip: MeasureGrip,
    pub at: Point,
}

/// The extra handles of a measurement: the middle of the dimension line and the middle of the
/// value of a Length, Radius or Diameter, and every corner of every cutout. Empty for anything
/// that is not a measurement this crate draws. Show them beside
/// [`selection_handles`](crate::selection_handles) when the measurement is the only thing
/// selected.
pub fn measure_handles(a: &Annotation) -> Vec<MeasureHandle> {
    let Some(m) = a.measurement() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if m.kind.is_line() {
        if let Some((p, q)) = a.dimension_line() {
            out.push(MeasureHandle {
                grip: MeasureGrip::Leader,
                at: p.midpoint(q),
            });
        }
        if let Some(c) = a.caption() {
            out.push(MeasureHandle {
                grip: MeasureGrip::Caption,
                at: c.centre(),
            });
        }
    }
    for (r, ring) in m.cutouts.iter().enumerate() {
        for (i, p) in ring.iter().enumerate() {
            out.push(MeasureHandle {
                grip: MeasureGrip::Cutout { ring: r, index: i },
                at: *p,
            });
        }
    }
    out
}

/// True when `p` is on the caption's text, within `slack`.
fn on_caption(c: &Caption, p: Point, slack: f64) -> bool {
    let up = Vec2::new(-c.direction.y, c.direction.x);
    let v = p - c.origin;
    let (s, t) = (v.dot(c.direction), v.dot(up));
    s >= -slack && s <= c.width + slack && t >= -0.25 * c.size - slack && t <= c.size + slack
}

/// The grip of `a` a press at `p` takes, where one screen pixel is `px` page units: the
/// nearest [`measure_handles`] within [`HANDLE_PX`], else the caption when the press is on its
/// text.
pub(crate) fn grab_grip(a: &Annotation, p: Point, px: f64) -> Option<MeasureGrip> {
    if !a.is_modelled() || !movable(a) {
        return None;
    }
    let nearest = measure_handles(a)
        .into_iter()
        .map(|h| ((h.at - p).hypot(), h.grip))
        .filter(|(d, _)| *d <= HANDLE_PX * px)
        .min_by(|x, y| x.0.total_cmp(&y.0));
    if let Some((_, grip)) = nearest {
        return Some(grip);
    }
    let line = a.measurement().is_some_and(|m| m.kind.is_line());
    match a.caption() {
        Some(c) if line && on_caption(&c, p, HIT_PX * px) => Some(MeasureGrip::Caption),
        _ => None,
    }
}

/// The unit direction of a line measurement and its left normal.
fn axes(a: &Annotation) -> Option<(Point, Vec2, Vec2)> {
    let Geometry::Line { a: p, b: q } = a.geometry else {
        return None;
    };
    let d = q - p;
    let len = d.hypot();
    if !(len.is_finite() && len > 1e-9) {
        return None;
    }
    let along = d / len;
    Some((p, along, Vec2::new(-along.y, along.x)))
}

/// The measurement of `a` with `grip` dragged from `start` to the pointer, which is at `raw`
/// (and at `snapped` with the snap point taken). The dimension line goes through the snapped
/// point; the caption follows the pointer itself; a cutout corner goes to the snapped point,
/// kept to 45 degree steps from the corner before it with Shift.
pub(crate) fn regrip(
    a: &Annotation,
    grip: MeasureGrip,
    start: Point,
    raw: Point,
    snapped: Point,
    shift: bool,
    rotation: Rotation,
) -> Option<Measurement> {
    let mut m = a.measurement()?.clone();
    match grip {
        MeasureGrip::Leader => {
            let (p, _, normal) = axes(a)?;
            let off = (snapped - p).dot(normal);
            let gap = m.leader_offset.max(0.0);
            m.leader = if off.abs() <= gap {
                0.0
            } else {
                off - off.signum() * gap
            };
        }
        MeasureGrip::Caption => {
            let (_, along, normal) = axes(a)?;
            let delta = raw - start;
            m.caption_offset += Vec2::new(delta.dot(along), delta.dot(normal));
        }
        MeasureGrip::Cutout { ring, index } => {
            let points = m.cutouts.get_mut(ring)?;
            let n = points.len();
            if index >= n {
                return None;
            }
            let previous = points[(index + n - 1) % n];
            points[index] = if shift {
                constrain_step(previous, snapped, rotation, 45.0)
            } else {
                snapped
            };
            // A hole dragged out of the outline or into another hole would take off area the
            // measurement does not cover: the drag stops short of that.
            if let Geometry::Points(outline) = &a.geometry {
                if pl_geom::region_problem(outline, &m.cutouts).is_some() {
                    return None;
                }
            }
        }
    }
    let finite = m.leader.is_finite()
        && m.caption_offset.x.is_finite()
        && m.caption_offset.y.is_finite()
        && m.cutouts
            .iter()
            .flatten()
            .all(|p| p.x.is_finite() && p.y.is_finite());
    finite.then_some(m)
}

/// A double-click at `p` on the measurement `a`: on a vertex of a Polylength, Perimeter, Area
/// or Volume (or of one of its cutouts) it deletes that vertex, unless the outline would be
/// left with too few (two for a polylength, three otherwise; a cutout left with two goes
/// altogether); on one of its segments it inserts a vertex at the nearest point of the
/// segment. Returns the intents; none for anything else.
pub(crate) fn double_click(a: &Annotation, p: Point, px: f64) -> Vec<Intent> {
    let Some(m) = a.measurement() else {
        return Vec::new();
    };
    if !movable(a) || !a.is_modelled() {
        return Vec::new();
    }
    let (closed, min) = match m.kind {
        MeasureKind::Polylength => (false, 2),
        MeasureKind::Perimeter | MeasureKind::Area | MeasureKind::Volume => (true, 3),
        _ => return Vec::new(),
    };
    let Geometry::Points(outer) = &a.geometry else {
        return Vec::new();
    };
    // Ring 0 is the outline, ring k the cutout k - 1.
    let rings: Vec<(&[Point], bool)> = std::iter::once((outer.as_slice(), closed))
        .chain(m.cutouts.iter().map(|c| (c.as_slice(), true)))
        .collect();
    let geometry = |points: Vec<Point>| Intent::Modify {
        id: a.id.clone(),
        change: AnnotChange::Geometry(Geometry::Points(points)),
    };
    let cutouts = |cutouts: Vec<Vec<Point>>| {
        let mut m = m.clone();
        m.cutouts = cutouts;
        Intent::Modify {
            id: a.id.clone(),
            change: AnnotChange::Measure(Box::new(m)),
        }
    };

    let vertex = rings
        .iter()
        .enumerate()
        .flat_map(|(r, (ring, _))| ring.iter().enumerate().map(move |(i, v)| (r, i, *v)))
        .map(|(r, i, v)| ((v - p).hypot(), r, i))
        .filter(|(d, _, _)| *d <= HANDLE_PX * px)
        .min_by(|x, y| x.0.total_cmp(&y.0));
    if let Some((_, r, i)) = vertex {
        if r == 0 {
            if outer.len() <= min {
                return Vec::new();
            }
            let mut points = outer.clone();
            points.remove(i);
            return vec![geometry(points)];
        }
        let mut all = m.cutouts.clone();
        if all[r - 1].len() <= 3 {
            all.remove(r - 1);
        } else {
            all[r - 1].remove(i);
        }
        return vec![cutouts(all)];
    }

    let reach = HIT_PX * px
        + if a.style.stroke.is_some() {
            a.style.width.max(0.0) / 2.0
        } else {
            0.0
        };
    let segment = rings
        .iter()
        .enumerate()
        .flat_map(|(r, (ring, closed))| {
            let n = ring.len();
            let count = if *closed { n } else { n.saturating_sub(1) };
            (0..count).map(move |i| (r, i, ring[i], ring[(i + 1) % n]))
        })
        .map(|(r, i, s, e)| {
            let (q, _) = closest_point_on_segment(p, s, e);
            ((q - p).hypot(), r, i, q)
        })
        .filter(|(d, ..)| *d <= reach)
        .min_by(|x, y| x.0.total_cmp(&y.0));
    let Some((_, r, i, q)) = segment else {
        return Vec::new();
    };
    if r == 0 {
        let mut points = outer.clone();
        points.insert(i + 1, q);
        vec![geometry(points)]
    } else {
        let mut all = m.cutouts.clone();
        all[r - 1].insert(i + 1, q);
        vec![cutouts(all)]
    }
}
