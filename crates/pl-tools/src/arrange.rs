//! Arrange operations as pure functions: nudge, align, distribute and stacking order.
//!
//! Positions are compared by the box around an annotation's defining points
//! (`Geometry::bounds`), not by what it paints, so a thick border or an arrowhead does not
//! shift the result. "Left", "top" and so on are as the user sees them: every function takes
//! the rotation the page is displayed at.

use pl_annot::{AnnotId, Annotation, Geometry};
use pl_geom::{Point, Rect, Rotation, Vec2};

use crate::geom::{to_display, to_page};
use crate::intent::{AnnotChange, Intent};
use crate::view::DocView;

/// Which edges or centres to line up, as seen on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Right,
    Top,
    Bottom,
    /// Centres on one vertical line (halfway between the leftmost and rightmost edge).
    Centre,
    /// Centres on one horizontal line (halfway between the topmost and bottommost edge).
    Middle,
}

/// A screen axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// The points that place an annotation: its geometry and, for a callout, its leader (which
/// the geometry, being the text box alone, does not hold).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shape {
    pub geometry: Geometry,
    /// `/CL` of a callout, tip first; empty for anything else.
    pub leader: Vec<Point>,
    /// The cutout rings of an area or volume measurement, which move and scale with its
    /// outline; empty for anything else.
    pub cutouts: Vec<Vec<Point>>,
}

impl Shape {
    pub fn of(a: &Annotation) -> Shape {
        Shape {
            geometry: a.geometry.clone(),
            leader: a
                .free_text
                .as_ref()
                .map(|t| t.callout.clone())
                .unwrap_or_default(),
            cutouts: a
                .measure
                .as_ref()
                .map(|m| m.cutouts.clone())
                .unwrap_or_default(),
        }
    }

    /// Every point mapped (a rectangle stays a rectangle, as in `Geometry::map_points`).
    pub fn map(&self, f: impl Fn(Point) -> Point) -> Shape {
        Shape {
            geometry: self.geometry.map_points(&f),
            leader: self.leader.iter().map(|p| f(*p)).collect(),
            cutouts: self
                .cutouts
                .iter()
                .map(|ring| ring.iter().map(|p| f(*p)).collect())
                .collect(),
        }
    }

    pub fn moved(&self, by: Vec2) -> Shape {
        self.map(|p| p + by)
    }

    /// The box around the defining points, leader included.
    pub fn bounds(&self) -> Rect {
        self.leader
            .iter()
            .fold(self.geometry.bounds(), |r, p| r.union_pt(*p))
    }

    pub fn is_finite(&self) -> bool {
        self.geometry.is_finite()
            && self
                .leader
                .iter()
                .all(|p| p.x.is_finite() && p.y.is_finite())
            && self
                .cutouts
                .iter()
                .flatten()
                .all(|p| p.x.is_finite() && p.y.is_finite())
    }

    /// `a`, detached from its file, in this shape.
    pub fn on(&self, a: &Annotation) -> Annotation {
        let mut b = a.detached();
        b.geometry = self.geometry.clone();
        if let Some(t) = b.free_text.as_mut() {
            t.callout = self.leader.clone();
        }
        if let Some(m) = b.measure.as_mut() {
            m.cutouts = self.cutouts.clone();
        }
        b.sync_measurement();
        b
    }

    /// The intents that give `a` this shape: none when it has it already, a geometry change,
    /// and for a callout whose leader differs a text change as well.
    pub fn intents(&self, a: &Annotation) -> Vec<Intent> {
        let mut out = Vec::new();
        if !self.is_finite() {
            return out;
        }
        if self.geometry != a.geometry {
            out.push(Intent::Modify {
                id: a.id.clone(),
                change: AnnotChange::Geometry(self.geometry.clone()),
            });
        }
        if let Some(t) = a.free_text.as_ref().filter(|t| t.callout != self.leader) {
            let mut text = t.clone();
            text.callout = self.leader.clone();
            out.push(Intent::Modify {
                id: a.id.clone(),
                change: AnnotChange::FreeText(text),
            });
        }
        if let Some(m) = a.measure.as_ref().filter(|m| m.cutouts != self.cutouts) {
            let mut m = m.clone();
            m.cutouts = self.cutouts.clone();
            out.push(Intent::Modify {
                id: a.id.clone(),
                change: AnnotChange::Measure(Box::new(m)),
            });
        }
        out
    }
}

pub(crate) fn movable(a: &Annotation) -> bool {
    !a.meta.flags.is_locked()
}

/// The intents that move `a` by `by`: one for most markups, two for a callout with a leader.
fn move_intent(a: &Annotation, by: Vec2) -> Vec<Intent> {
    if !movable(a) || by == Vec2::ZERO || !(by.x.is_finite() && by.y.is_finite()) {
        return Vec::new();
    }
    Shape::of(a).moved(by).intents(a)
}

/// The annotation's box in screen directions (x right, y down), up to a translation that is
/// the same for every annotation on the page.
fn display_box(a: &Annotation, rotation: Rotation) -> Rect {
    let b = a.geometry.bounds();
    let p = to_display(rotation, Vec2::new(b.x0, b.y0));
    let q = to_display(rotation, Vec2::new(b.x1, b.y1));
    Rect::from_points(Point::new(p.x, p.y), Point::new(q.x, q.y))
}

/// Moves each annotation by `screen` (x right, y **down**, in page units), turned into its own
/// page's directions. Locked annotations stay.
pub fn nudge(view: &dyn DocView, ids: &[AnnotId], screen: Vec2) -> Vec<Intent> {
    ids.iter()
        .filter_map(|id| view.annotation(id))
        .flat_map(|a| move_intent(a, to_page(view.rotation(a.page), screen)))
        .collect()
}

/// Lines `items` up. The target is the extreme edge among all of them (locked ones included,
/// though those do not move). Fewer than two items, or items already in line, give nothing.
pub fn align(items: &[&Annotation], how: Align, rotation: Rotation) -> Vec<Intent> {
    if items.len() < 2 {
        return Vec::new();
    }
    let boxes: Vec<Rect> = items.iter().map(|a| display_box(a, rotation)).collect();
    let min = |f: fn(&Rect) -> f64| boxes.iter().map(f).fold(f64::INFINITY, f64::min);
    let max = |f: fn(&Rect) -> f64| boxes.iter().map(f).fold(f64::NEG_INFINITY, f64::max);
    let target = match how {
        Align::Left => min(|b| b.x0),
        Align::Right => max(|b| b.x1),
        Align::Top => min(|b| b.y0),
        Align::Bottom => max(|b| b.y1),
        Align::Centre => (min(|b| b.x0) + max(|b| b.x1)) / 2.0,
        Align::Middle => (min(|b| b.y0) + max(|b| b.y1)) / 2.0,
    };
    items
        .iter()
        .zip(&boxes)
        .flat_map(|(a, b)| {
            let d = match how {
                Align::Left => Vec2::new(target - b.x0, 0.0),
                Align::Right => Vec2::new(target - b.x1, 0.0),
                Align::Top => Vec2::new(0.0, target - b.y0),
                Align::Bottom => Vec2::new(0.0, target - b.y1),
                Align::Centre => Vec2::new(target - (b.x0 + b.x1) / 2.0, 0.0),
                Align::Middle => Vec2::new(0.0, target - (b.y0 + b.y1) / 2.0),
            };
            move_intent(a, to_page(rotation, d))
        })
        .collect()
}

/// Spaces `items` evenly along a screen axis: the two outermost stay, and the ones between
/// are moved so that the gaps between neighbouring boxes are all equal. Fewer than three
/// items give nothing.
pub fn distribute(items: &[&Annotation], axis: Axis, rotation: Rotation) -> Vec<Intent> {
    if items.len() < 3 {
        return Vec::new();
    }
    let span = |a: &Annotation| {
        let b = display_box(a, rotation);
        match axis {
            Axis::Horizontal => (b.x0, b.x1),
            Axis::Vertical => (b.y0, b.y1),
        }
    };
    let mut sorted: Vec<(&Annotation, f64, f64)> = items
        .iter()
        .map(|a| {
            let (lo, hi) = span(a);
            (*a, lo, hi)
        })
        .collect();
    sorted.sort_by(|a, b| (a.1 + a.2).total_cmp(&(b.1 + b.2)));
    let first = sorted[0];
    let last = sorted[sorted.len() - 1];
    let inner: f64 = sorted[1..sorted.len() - 1].iter().map(|s| s.2 - s.1).sum();
    let gap = (last.1 - first.2 - inner) / (sorted.len() - 1) as f64;
    let mut cursor = first.2 + gap;
    let mut out = Vec::new();
    for (a, lo, hi) in &sorted[1..sorted.len() - 1] {
        let d = cursor - lo;
        let screen = match axis {
            Axis::Horizontal => Vec2::new(d, 0.0),
            Axis::Vertical => Vec2::new(0.0, d),
        };
        out.extend(move_intent(a, to_page(rotation, screen)));
        cursor += hi - lo + gap;
    }
    out
}

/// `order` (one page, bottom to top) with the selected annotations moved to the top, keeping
/// their order among themselves.
pub fn bring_to_front(order: &[AnnotId], selected: &[AnnotId]) -> Vec<AnnotId> {
    let (top, rest): (Vec<AnnotId>, Vec<AnnotId>) =
        order.iter().cloned().partition(|id| selected.contains(id));
    rest.into_iter().chain(top).collect()
}

/// `order` with the selected annotations moved to the bottom, keeping their order among
/// themselves.
pub fn send_to_back(order: &[AnnotId], selected: &[AnnotId]) -> Vec<AnnotId> {
    let (bottom, rest): (Vec<AnnotId>, Vec<AnnotId>) =
        order.iter().cloned().partition(|id| selected.contains(id));
    bottom.into_iter().chain(rest).collect()
}

/// `order` with each selected annotation moved one place up, past the unselected annotation
/// above it. A selected annotation already at the top, or under other selected ones that are,
/// stays.
pub fn bring_forward(order: &[AnnotId], selected: &[AnnotId]) -> Vec<AnnotId> {
    let mut out = order.to_vec();
    for i in (0..out.len().saturating_sub(1)).rev() {
        if selected.contains(&out[i]) && !selected.contains(&out[i + 1]) {
            out.swap(i, i + 1);
        }
    }
    out
}

/// `order` with each selected annotation moved one place down.
pub fn send_backward(order: &[AnnotId], selected: &[AnnotId]) -> Vec<AnnotId> {
    let mut out = order.to_vec();
    for i in 1..out.len() {
        if selected.contains(&out[i]) && !selected.contains(&out[i - 1]) {
            out.swap(i, i - 1);
        }
    }
    out
}

/// The intent that restacks `page` with `arrange` (one of the four order functions) applied
/// to the selection; `None` when that changes nothing.
pub fn reorder_intent(
    view: &dyn DocView,
    page: usize,
    arrange: fn(&[AnnotId], &[AnnotId]) -> Vec<AnnotId>,
) -> Option<Intent> {
    let before: Vec<AnnotId> = view
        .annotations(page)
        .iter()
        .map(|a| a.id.clone())
        .collect();
    let order = arrange(&before, view.selection());
    (order != before).then_some(Intent::Reorder { page, order })
}
