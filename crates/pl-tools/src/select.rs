//! The Select tool: pick, box-select, move, resize (one markup or several together), edit
//! vertices and callout leaders, nudge and delete.

use pl_annot::{
    callout_route, AnnotId, AnnotKind, Annotation, Geometry, Handle, HandleKind, MeasureKind,
    RectHandle, Rgb,
};
use pl_draw::shapes::rect_path;
use pl_draw::{Dash, DrawList, Fill, ShapeStyle, Stroke};
use pl_geom::{Point, Rect, Rotation, Vec2};

use crate::arrange::{movable, nudge, Shape};
use crate::geom::{constrain_angle, constrain_axis, to_display};
use crate::input::{Key, Pointer, ToolInput};
use crate::intent::{AnnotChange, Cursor, Intent};
use crate::measure_edit::{self, MeasureHandle};
use crate::tool::{
    usable, Phase, ToolState, DRAG_PX, HANDLE_PX, HIT_PX, NUDGE, NUDGE_FINE, NUDGE_LARGE,
};
use crate::view::DocView;

/// The colour of the selection box and of outlines drawn for markups that have no drawing of
/// their own.
pub(crate) const GUIDE: Rgb = Rgb {
    r: 0.2,
    g: 0.45,
    b: 0.85,
};

/// True for annotations the pointer can pick: not a popup window, not hidden.
fn pickable(a: &Annotation) -> bool {
    a.kind != AnnotKind::Popup && !a.meta.flags.is_hidden()
}

/// The topmost pickable annotation under `p`.
fn hit(view: &dyn DocView, page: usize, p: Point) -> Option<&Annotation> {
    let tolerance = HIT_PX * view.pixel_size();
    view.annotations(page)
        .into_iter()
        .rev()
        .find(|a| pickable(a) && a.hit_test(p, tolerance))
}

/// The one selected annotation, when exactly one is selected and it may be reshaped.
fn sole(view: &dyn DocView) -> Option<&Annotation> {
    match view.selection() {
        [only] => view.annotation(only).filter(|a| movable(a)),
        _ => None,
    }
}

/// The selected annotations on `page` that a group resize acts on (not locked, not hidden)
/// and the box around them, when there are two or more.
fn group(view: &dyn DocView, page: usize) -> Option<(Vec<&Annotation>, Rect)> {
    let selection = view.selection();
    if selection.len() < 2 {
        return None;
    }
    let members: Vec<&Annotation> = selection
        .iter()
        .filter_map(|id| view.annotation(id))
        .filter(|a| a.page == page && movable(a) && pickable(a))
        .collect();
    if members.len() < 2 {
        return None;
    }
    let mut boxes = members.iter().map(|a| Shape::of(a).bounds());
    let first = boxes.next()?;
    let bounds = boxes.fold(first, |r, b| r.union(b));
    bounds.is_finite().then_some((members, bounds))
}

/// The box around the selected markups of `page` when two or more are selected there and may
/// be moved: the box whose eight handles [`selection_handles`] returns and a group resize
/// scales. It is around the defining points (a callout's leader included), not the painted
/// extent.
pub fn group_bounds(view: &dyn DocView, page: usize) -> Option<Rect> {
    group(view, page).map(|(_, bounds)| bounds)
}

/// The handles to show. With exactly one markup selected (and not locked): its own. With
/// several selected: the eight handles of the box around them ([`group_bounds`]), on the page
/// of the first selected markup. Draw them at a fixed screen size; [`handle_cursor`] gives
/// each one's cursor.
pub fn selection_handles(view: &dyn DocView) -> Vec<Handle> {
    if let Some(a) = sole(view) {
        return a.handles();
    }
    let page = view
        .selection()
        .iter()
        .find_map(|id| view.annotation(id))
        .map(|a| a.page);
    match page.and_then(|page| group(view, page)) {
        Some((_, bounds)) => RectHandle::ALL
            .iter()
            .map(|h| Handle {
                kind: HandleKind::Rect(*h),
                at: h.position(bounds),
            })
            .collect(),
        None => Vec::new(),
    }
}

/// The extra handles of the selected measurement ([`measure_handles`](crate::measure_handles))
/// when exactly one markup is selected and it may be reshaped; otherwise none.
pub fn selection_measure_handles(view: &dyn DocView) -> Vec<MeasureHandle> {
    sole(view).map(crate::measure_handles).unwrap_or_default()
}

/// How far from a handle of the box `b` a press at `p` still grabs it; `None` for a box with
/// no extent on screen.
fn rect_reach(b: Rect, p: Point, px: f64) -> Option<f64> {
    let reach = HANDLE_PX * px;
    if b.width().max(b.height()) < px {
        return None;
    }
    let inside = p.x >= b.x0 && p.x <= b.x1 && p.y >= b.y0 && p.y <= b.y1;
    Some(if inside && b.width() > 0.0 && b.height() > 0.0 {
        reach.min(b.width().min(b.height()) / 3.0)
    } else {
        reach
    })
}

/// The handle of the group box `bounds` a press at `p` grabs, by the rule of [`grab_handle`].
fn grab_group(bounds: Rect, p: Point, px: f64) -> Option<RectHandle> {
    let (distance, handle) = RectHandle::ALL
        .iter()
        .map(|h| ((h.position(bounds) - p).hypot(), *h))
        .filter(|(d, _)| *d <= HANDLE_PX * px)
        .min_by(|a, b| a.0.total_cmp(&b.0))?;
    (distance <= rect_reach(bounds, p, px)?).then_some(handle)
}

/// The handle of `a` a press at `p` grabs, where one screen pixel is `px` page units.
///
/// A handle is grabbed within [`HANDLE_PX`] of it, except where that would leave no way to
/// pick the markup up by its body: on a markup that is small on screen (a short line, a small
/// rectangle at a low zoom) every point of it is that near a handle. So the reach of a handle
/// is also limited to a third of the distance to the nearest other grip: for a box (a
/// rectangle, or the bounds of pen strokes) a third of its shorter side when the press is
/// inside it, and for a vertex a third of the way to the nearest other vertex. The middle of
/// a small markup therefore always moves it, and from outside a box its handles keep their
/// full reach. A box with no extent at all on screen (a pen dot) has nothing to resize.
pub fn grab_handle(a: &Annotation, p: Point, px: f64) -> Option<Handle> {
    let reach = HANDLE_PX * px;
    let handles = a.handles();
    let (distance, nearest) = handles
        .iter()
        .map(|h| ((h.at - p).hypot(), h))
        .filter(|(d, _)| *d <= reach)
        .min_by(|a, b| a.0.total_cmp(&b.0))?;
    let limit = match nearest.kind {
        HandleKind::Rect(_) => rect_reach(a.geometry.bounds(), p, px)?,
        HandleKind::Vertex(_) => {
            let gap = handles
                .iter()
                .filter(|h| h.kind != nearest.kind)
                .map(|h| (h.at - nearest.at).hypot())
                .fold(f64::INFINITY, f64::min);
            reach.min(gap / 3.0)
        }
    };
    (distance <= limit).then_some(*nearest)
}

/// Which sides of a rectangle a handle drags: -1 (left, bottom), 0 or 1 (right, top) on each
/// page axis.
fn sides(handle: RectHandle) -> Vec2 {
    handle.position(Rect::new(-1.0, -1.0, 1.0, 1.0)).to_vec2()
}

/// The cursor for a handle on a page displayed at `rotation`: the direction the handle
/// resizes in *on screen*.
pub fn handle_cursor(handle: HandleKind, rotation: Rotation) -> Cursor {
    let HandleKind::Rect(h) = handle else {
        return Cursor::Crosshair;
    };
    let d = to_display(rotation, sides(h));
    if d.y == 0.0 {
        Cursor::ResizeHorizontal
    } else if d.x == 0.0 {
        Cursor::ResizeVertical
    } else if d.x * d.y > 0.0 {
        Cursor::ResizeNwSe
    } else {
        Cursor::ResizeNeSw
    }
}

/// `rect` with `handle` dragged to `to`, always normalised.
///
/// With `keep_aspect` the result has the proportions of `rect`: a corner scales about the
/// opposite corner by whichever of the two offsets asks for more; an edge scales about the
/// opposite edge and grows the other dimension equally to both sides. A rectangle with no
/// width or no height has no proportions and is dragged freely.
pub fn resize_rect(rect: Rect, handle: RectHandle, to: Point, keep_aspect: bool) -> Rect {
    let (w, h) = (rect.width(), rect.height());
    if !keep_aspect || w <= 0.0 || h <= 0.0 {
        return handle.drag(rect, to);
    }
    let s = sides(handle);
    let opposite = Point::new(
        if s.x > 0.0 { rect.x0 } else { rect.x1 },
        if s.y > 0.0 { rect.y0 } else { rect.y1 },
    );
    let centre = rect.center();
    if s.x != 0.0 && s.y != 0.0 {
        let v = to - opposite;
        let k = (v.x.abs() / w).max(v.y.abs() / h);
        let dir = |v: f64, side: f64| if v == 0.0 { side } else { v.signum() };
        let corner = opposite + Vec2::new(dir(v.x, s.x) * k * w, dir(v.y, s.y) * k * h);
        Rect::from_points(opposite, corner)
    } else if s.x != 0.0 {
        let k = (to.x - opposite.x).abs() / w;
        Rect::new(
            opposite.x.min(to.x),
            centre.y - k * h / 2.0,
            opposite.x.max(to.x),
            centre.y + k * h / 2.0,
        )
    } else {
        let k = (to.y - opposite.y).abs() / h;
        Rect::new(
            centre.x - k * w / 2.0,
            opposite.y.min(to.y),
            centre.x + k * w / 2.0,
            opposite.y.max(to.y),
        )
    }
}

/// `p` carried from `old` to the same relative place in `new`. An axis on which `old` has no
/// extent is moved to the middle of `new` instead.
fn refit(p: Point, old: Rect, new: Rect) -> Point {
    let axis = |v: f64, o0: f64, o1: f64, n0: f64, n1: f64| {
        if o1 - o0 > 0.0 {
            n0 + (v - o0) * (n1 - n0) / (o1 - o0)
        } else {
            v + (n0 + n1 - o0 - o1) / 2.0
        }
    };
    Point::new(
        axis(p.x, old.x0, old.x1, new.x0, new.x1),
        axis(p.y, old.y0, old.y1, new.y0, new.y1),
    )
}

/// The shape of `a` with one of its handles dragged to `to`; `None` when the annotation has
/// no such handle or the result would be empty.
///
/// Shift keeps the proportions of a box and holds a vertex to 45 degree steps. A stamp or a
/// picture is the other way round: it keeps its proportions unless Shift is held. Resizing a
/// callout's box, or dragging its tip, routes the leader again; dragging its knee moves the
/// knee and re-attaches the leader to the side of the box that faces it.
fn reshaped(
    a: &Annotation,
    handle: HandleKind,
    to: Point,
    shift: bool,
    rotation: Rotation,
) -> Option<Shape> {
    if !a.is_modelled() || a.kind == AnnotKind::Text {
        return None;
    }
    let leader = Shape::of(a).leader;
    let geometry = match (handle, &a.geometry) {
        (HandleKind::Vertex(i), Geometry::Rect(rect)) => {
            if a.kind != AnnotKind::FreeText {
                return None;
            }
            let shape = if i == 1 && leader.len() == 3 {
                let knee = if shift {
                    constrain_angle(leader[0], to, rotation)
                } else {
                    to
                };
                let end = callout_route(*rect, knee).last().copied()?;
                Shape {
                    geometry: a.geometry.clone(),
                    leader: vec![leader[0], knee, end],
                    cutouts: Vec::new(),
                }
            } else {
                let mut b = a.detached();
                if !b.move_handle(handle, to) {
                    return None;
                }
                Shape::of(&b)
            };
            return shape.is_finite().then_some(shape);
        }
        (HandleKind::Rect(h), Geometry::Rect(rect)) => {
            let keep_aspect = shift != (a.kind == AnnotKind::Stamp);
            let r = resize_rect(*rect, h, to, keep_aspect);
            if r.width() <= 0.0 || r.height() <= 0.0 {
                return None;
            }
            if leader.is_empty() {
                Geometry::Rect(r)
            } else {
                let mut b = a.detached();
                b.geometry = Geometry::Rect(r);
                b.reroute_callout();
                let shape = Shape::of(&b);
                return shape.is_finite().then_some(shape);
            }
        }
        (HandleKind::Rect(h), Geometry::Ink(_)) => {
            let old = a.geometry.bounds();
            let new = resize_rect(old, h, to, shift);
            a.geometry.map_points(|p| refit(p, old, new))
        }
        (HandleKind::Vertex(i), Geometry::Line { a: p0, b: p1 }) => match i {
            0 => Geometry::Line {
                a: if shift {
                    constrain_angle(*p1, to, rotation)
                } else {
                    to
                },
                b: *p1,
            },
            1 => Geometry::Line {
                a: *p0,
                b: if shift {
                    constrain_angle(*p0, to, rotation)
                } else {
                    to
                },
            },
            _ => return None,
        },
        (HandleKind::Vertex(i), Geometry::Ink(strokes)) => {
            // One marker of a count.
            let count = a
                .measurement()
                .is_some_and(|m| m.kind == MeasureKind::Count);
            if !count || i >= strokes.len() {
                return None;
            }
            let mut strokes = strokes.clone();
            strokes[i] = vec![to, to];
            Geometry::Ink(strokes)
        }
        (HandleKind::Vertex(i), Geometry::Points(points)) => {
            if i >= points.len() {
                return None;
            }
            let neighbour = if i == 0 {
                points.get(1)
            } else {
                points.get(i - 1)
            };
            let target = match neighbour {
                Some(n) if shift => constrain_angle(*n, to, rotation),
                _ => to,
            };
            let mut points = points.clone();
            points[i] = target;
            Geometry::Points(points)
        }
        _ => return None,
    };
    let shape = Shape {
        geometry,
        leader,
        cutouts: Shape::of(a).cutouts,
    };
    shape.is_finite().then_some(shape)
}

/// The shape of `a` when the box around a group is resized from `from` to `to`. A markup
/// that can be reshaped is scaled with the box (a text box keeps its font size; a callout's
/// leader is scaled too). A note, or a markup shown from its own appearance, keeps its size
/// and moves so that its centre keeps its place in the box.
fn regrouped(a: &Annotation, from: Rect, to: Rect) -> Shape {
    let shape = Shape::of(a);
    if a.is_modelled() && a.kind != AnnotKind::Text {
        shape.map(|p| refit(p, from, to))
    } else {
        let centre = shape.bounds().center();
        shape.moved(refit(centre, from, to) - centre)
    }
}

fn select(view: &dyn DocView, ids: Vec<AnnotId>, intents: &mut Vec<Intent>) {
    if view.selection() != ids.as_slice() {
        intents.push(Intent::Select(ids));
    }
}

/// The cursor for the pointer resting at `p`.
fn hover(view: &dyn DocView, p: &Pointer) -> Cursor {
    let px = view.pixel_size();
    if let Some(a) = sole(view).filter(|a| a.page == p.page) {
        if let Some(h) = grab_handle(a, p.point, px) {
            return handle_cursor(h.kind, view.rotation(p.page));
        }
        if measure_edit::grab_grip(a, p.point, px).is_some() {
            return Cursor::Move;
        }
    } else if let Some((_, bounds)) = group(view, p.page) {
        if let Some(h) = grab_group(bounds, p.point, px) {
            return handle_cursor(HandleKind::Rect(h), view.rotation(p.page));
        }
    }
    match hit(view, p.page, p.point) {
        Some(_) => Cursor::Move,
        None => Cursor::Default,
    }
}

/// True when the selection box is dragged from the screen's left to its right (it then takes
/// what it encloses; dragged the other way it takes what it touches).
fn encloses(anchor: Point, current: Point, rotation: Rotation) -> bool {
    to_display(rotation, current - anchor).x >= 0.0
}

fn boxed(view: &dyn DocView, page: usize, anchor: Point, current: Point) -> Vec<AnnotId> {
    let r = Rect::from_points(anchor, current);
    let enclose = encloses(anchor, current, view.rotation(page));
    view.annotations(page)
        .into_iter()
        .filter(|a| pickable(a))
        .filter(|a| {
            let b = a.geometry.bounds();
            if enclose {
                b.x0 >= r.x0 && b.x1 <= r.x1 && b.y0 >= r.y0 && b.y1 <= r.y1
            } else {
                b.x0 <= r.x1 && b.x1 >= r.x0 && b.y0 <= r.y1 && b.y1 >= r.y0
            }
        })
        .map(|a| a.id.clone())
        .collect()
}

fn pointer_down(state: &mut ToolState, p: &Pointer, view: &dyn DocView, out: &mut Vec<Intent>) {
    let px = view.pixel_size();
    // A handle of the selected markup comes first: it may sit outside the markup itself.
    if let Some(a) = sole(view).filter(|a| a.page == p.page) {
        if let Some(h) = grab_handle(a, p.point, px) {
            state.phase = Phase::Handle {
                page: p.page,
                id: a.id.clone(),
                handle: h.kind,
                start: p.point,
                shape: Shape::of(a),
                moved: false,
            };
            return;
        }
        // Then the dimension line, the value and the cutout corners of a measurement.
        if let (Some(grip), Some(m)) = (measure_edit::grab_grip(a, p.point, px), a.measurement()) {
            state.phase = Phase::Grip {
                page: p.page,
                id: a.id.clone(),
                grip,
                start: p.point,
                measure: Box::new(m.clone()),
                moved: false,
            };
            return;
        }
    } else if let Some((members, bounds)) = group(view, p.page) {
        // So do the handles of the box around several.
        if let Some(handle) = grab_group(bounds, p.point, px) {
            state.phase = Phase::Group {
                page: p.page,
                ids: members.iter().map(|a| a.id.clone()).collect(),
                handle,
                start: p.point,
                from: bounds,
                to: bounds,
                moved: false,
            };
            return;
        }
    }
    let Some(a) = hit(view, p.page, p.point) else {
        state.phase = Phase::Marquee {
            page: p.page,
            anchor: p.point,
            current: p.point,
            additive: p.modifiers.shift,
        };
        return;
    };
    let selection = view.selection();
    let selected = selection.contains(&a.id);
    if p.modifiers.shift {
        // Shift-click adds to the selection or takes away from it.
        let mut ids = selection.to_vec();
        if selected {
            ids.retain(|id| *id != a.id);
        } else {
            ids.push(a.id.clone());
        }
        state.phase = Phase::Idle;
        select(view, ids, out);
        return;
    }
    let ids = if selected {
        selection.to_vec()
    } else {
        vec![a.id.clone()]
    };
    select(view, ids.clone(), out);
    state.phase = Phase::Move {
        page: p.page,
        press: p.point,
        start: p.effective(),
        delta: Vec2::ZERO,
        collapse: selected && ids.len() > 1,
        ids,
        clicked: a.id.clone(),
        moved: false,
        copy: false,
    };
}

/// Updates the gesture for a pointer position (a move, or the position of the release).
fn track(state: &mut ToolState, p: &Pointer, view: &dyn DocView) {
    let threshold = DRAG_PX * view.pixel_size();
    match &mut state.phase {
        Phase::Move {
            page,
            press,
            start,
            delta,
            moved,
            copy,
            ..
        } if p.page == *page => {
            let d = p.effective() - *start;
            // A click or a drag is decided by how far the pointer itself went. Snapped
            // positions can jump by the whole snap radius between two events a pixel apart.
            *moved |= (p.point - *press).hypot() >= threshold;
            *delta = if !*moved {
                Vec2::ZERO
            } else if p.modifiers.shift {
                constrain_axis(d, view.rotation(*page))
            } else {
                d
            };
            *copy = p.modifiers.ctrl;
        }
        Phase::Marquee { page, current, .. } if p.page == *page => *current = p.point,
        Phase::Handle {
            page,
            id,
            handle,
            start,
            shape,
            moved,
        } if p.page == *page => {
            *moved |= (p.point - *start).hypot() >= threshold;
            if *moved {
                if let Some(a) = view.annotation(id) {
                    let rotation = view.rotation(*page);
                    if let Some(s) =
                        reshaped(a, *handle, p.effective(), p.modifiers.shift, rotation)
                    {
                        *shape = s;
                    }
                }
            }
        }
        Phase::Grip {
            page,
            id,
            grip,
            start,
            measure,
            moved,
        } if p.page == *page => {
            *moved |= (p.point - *start).hypot() >= threshold;
            if *moved {
                if let Some(a) = view.annotation(id) {
                    let rotation = view.rotation(*page);
                    if let Some(m) = measure_edit::regrip(
                        a,
                        *grip,
                        *start,
                        p.point,
                        p.effective(),
                        p.modifiers.shift,
                        rotation,
                    ) {
                        **measure = m;
                    }
                }
            }
        }
        Phase::Group {
            page,
            handle,
            start,
            from,
            to,
            moved,
            ..
        } if p.page == *page => {
            *moved |= (p.point - *start).hypot() >= threshold;
            if *moved {
                let r = resize_rect(*from, *handle, p.effective(), p.modifiers.shift);
                if r.is_finite() && r.width() > 0.0 && r.height() > 0.0 {
                    *to = r;
                }
            }
        }
        _ => {}
    }
}

fn pointer_up(state: &mut ToolState, view: &dyn DocView, out: &mut Vec<Intent>) {
    match std::mem::replace(&mut state.phase, Phase::Idle) {
        Phase::Move {
            delta,
            ids,
            clicked,
            collapse,
            moved,
            copy,
            ..
        } => {
            if !moved {
                if collapse {
                    select(view, vec![clicked], out);
                }
                return;
            }
            if delta == Vec2::ZERO {
                return;
            }
            let targets = ids.iter().filter_map(|id| view.annotation(id));
            if copy {
                let origin = view.origin();
                let mut copies = Vec::new();
                for a in targets.filter(|a| a.kind != AnnotKind::Popup) {
                    let mut c = a.duplicate(&origin);
                    c.translate(delta);
                    copies.push(c.id.clone());
                    out.push(Intent::Create(Box::new(c)));
                }
                if !copies.is_empty() {
                    out.push(Intent::Select(copies));
                }
            } else {
                for a in targets.filter(|a| movable(a)) {
                    out.extend(Shape::of(a).moved(delta).intents(a));
                }
            }
        }
        Phase::Marquee {
            page,
            anchor,
            current,
            additive,
        } => {
            let dragged = (current - anchor).hypot() >= DRAG_PX * view.pixel_size();
            let mut ids = if additive {
                view.selection().to_vec()
            } else {
                Vec::new()
            };
            if dragged {
                for id in boxed(view, page, anchor, current) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            select(view, ids, out);
        }
        Phase::Handle {
            id,
            shape,
            moved: true,
            ..
        } => {
            if let Some(a) = view.annotation(&id) {
                out.extend(shape.intents(a));
            }
        }
        Phase::Grip {
            id,
            measure,
            moved: true,
            ..
        } => {
            if let Some(a) = view.annotation(&id) {
                if a.measurement() != Some(&*measure) {
                    out.push(Intent::Modify {
                        id,
                        change: AnnotChange::Measure(measure),
                    });
                }
            }
        }
        Phase::Group {
            ids,
            from,
            to,
            moved: true,
            ..
        } if from != to => {
            for a in ids.iter().filter_map(|id| view.annotation(id)) {
                if movable(a) {
                    out.extend(regrouped(a, from, to).intents(a));
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    out: &mut Vec<Intent>,
) {
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => pointer_down(state, p, view, out),
        ToolInput::PointerMove(p) if p.is_finite() => {
            if state.phase == Phase::Idle {
                if usable(view, p) {
                    out.push(Intent::SetCursor(hover(view, p)));
                }
            } else {
                track(state, p, view);
            }
        }
        ToolInput::PointerUp(p) => {
            if p.is_finite() {
                track(state, p, view);
            }
            pointer_up(state, view, out);
        }
        ToolInput::DoubleClick(p) if usable(view, p) => {
            state.phase = Phase::Idle;
            if let Some(a) = hit(view, p.page, p.point) {
                select(view, vec![a.id.clone()], out);
                if a.measurement().is_some() {
                    // A measurement's text is its value: a double-click edits its outline.
                    out.extend(measure_edit::double_click(a, p.point, view.pixel_size()));
                } else {
                    out.push(Intent::RequestTextEdit(a.id.clone()));
                }
            }
        }
        ToolInput::Key { key, modifiers } => {
            if *key == Key::Escape {
                if state.phase != Phase::Idle {
                    state.phase = Phase::Idle;
                } else {
                    select(view, Vec::new(), out);
                }
                return;
            }
            if state.phase != Phase::Idle {
                return;
            }
            let selection = view.selection();
            let distance = if modifiers.shift {
                NUDGE_LARGE
            } else if modifiers.ctrl {
                NUDGE_FINE
            } else {
                NUDGE
            };
            match key {
                Key::Delete | Key::Backspace => {
                    let ids: Vec<AnnotId> = selection
                        .iter()
                        .filter(|id| view.annotation(id).is_some_and(movable))
                        .cloned()
                        .collect();
                    if !ids.is_empty() {
                        out.push(Intent::Delete(ids));
                    }
                }
                Key::Enter => {
                    if let [only] = selection {
                        if view.annotation(only).is_some() {
                            out.push(Intent::RequestTextEdit(only.clone()));
                        }
                    }
                }
                Key::Left => out.extend(nudge(view, selection, Vec2::new(-distance, 0.0))),
                Key::Right => out.extend(nudge(view, selection, Vec2::new(distance, 0.0))),
                Key::Up => out.extend(nudge(view, selection, Vec2::new(0.0, -distance))),
                Key::Down => out.extend(nudge(view, selection, Vec2::new(0.0, distance))),
                Key::Escape => {}
            }
        }
        _ => {}
    }
}

/// `a` drawn in another shape; an annotation with no drawing of its own (a stamp, a text box
/// from another program) is shown as a dashed outline.
fn ghost(a: &Annotation, shape: &Shape, px: f64, list: &mut DrawList) {
    let moved = shape.on(a);
    let drawn = moved.draw_list();
    if drawn.is_empty() {
        let stroke = Stroke::new(px, GUIDE).with_dash(Dash::even(4.0 * px));
        list.push_shape(
            rect_path(moved.geometry.bounds()),
            &ShapeStyle::stroked(stroke),
        );
    } else {
        list.extend(drawn.ops);
    }
}

pub(crate) fn preview(state: &ToolState, view: &dyn DocView) -> DrawList {
    let px = view.pixel_size();
    let mut list = DrawList::new();
    match &state.phase {
        Phase::Move {
            delta,
            ids,
            moved: true,
            copy,
            ..
        } => {
            for a in ids.iter().filter_map(|id| view.annotation(id)) {
                if *copy || movable(a) {
                    ghost(a, &Shape::of(a).moved(*delta), px, &mut list);
                }
            }
        }
        Phase::Handle {
            id,
            shape,
            moved: true,
            ..
        } => {
            if let Some(a) = view.annotation(id) {
                ghost(a, shape, px, &mut list);
            }
        }
        Phase::Group {
            ids,
            from,
            to,
            moved: true,
            ..
        } => {
            for a in ids.iter().filter_map(|id| view.annotation(id)) {
                if movable(a) {
                    ghost(a, &regrouped(a, *from, *to), px, &mut list);
                }
            }
        }
        Phase::Grip {
            id,
            measure,
            moved: true,
            ..
        } => {
            if let Some(a) = view.annotation(id) {
                let mut b = a.detached();
                b.measure = Some((**measure).clone());
                b.sync_measurement();
                list.extend(b.draw_list().ops);
            }
        }
        Phase::Marquee {
            page,
            anchor,
            current,
            ..
        } if anchor != current => {
            let mut stroke = Stroke::new(px, GUIDE);
            if !encloses(*anchor, *current, view.rotation(*page)) {
                stroke = stroke.with_dash(Dash::even(4.0 * px));
            }
            let path = rect_path(Rect::from_points(*anchor, *current));
            list.push_shape(
                path.clone(),
                &ShapeStyle::filled(Fill::new(GUIDE)).with_opacity(0.12),
            );
            list.push_shape(path, &ShapeStyle::stroked(stroke));
        }
        _ => {}
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn free_resize_is_the_plain_handle_drag() {
        let r = Rect::new(10.0, 20.0, 110.0, 70.0);
        for h in RectHandle::ALL {
            assert_eq!(
                resize_rect(r, h, pt(150.0, 5.0), false),
                h.drag(r, pt(150.0, 5.0))
            );
        }
    }

    #[test]
    fn a_corner_keeps_proportions_about_the_opposite_corner() {
        // 100 x 50, so 2:1.
        let r = Rect::new(10.0, 20.0, 110.0, 70.0);
        // The x offset asks for 1.5 times, the y offset for 1.2 times: 1.5 wins.
        let got = resize_rect(r, RectHandle::TopRight, pt(160.0, 80.0), true);
        assert_eq!(got, Rect::new(10.0, 20.0, 160.0, 95.0));
        // The y offset wins: twice the height.
        let got = resize_rect(r, RectHandle::TopRight, pt(120.0, 120.0), true);
        assert_eq!(got, Rect::new(10.0, 20.0, 210.0, 120.0));
        // Bottom-left shrinks towards the top-right corner.
        let got = resize_rect(r, RectHandle::BottomLeft, pt(60.0, 60.0), true);
        assert_eq!(got, Rect::new(60.0, 45.0, 110.0, 70.0));
        // Dragged through the opposite corner it flips and stays 2:1.
        let got = resize_rect(r, RectHandle::TopRight, pt(-40.0, 10.0), true);
        assert_eq!(got, Rect::new(-40.0, -5.0, 10.0, 20.0));
    }

    #[test]
    fn an_edge_keeps_proportions_about_the_centre_line() {
        let r = Rect::new(10.0, 20.0, 110.0, 70.0);
        let got = resize_rect(r, RectHandle::Right, pt(210.0, 999.0), true);
        assert_eq!(got, Rect::new(10.0, -5.0, 210.0, 95.0));
        let got = resize_rect(r, RectHandle::Bottom, pt(999.0, 45.0), true);
        assert_eq!(got, Rect::new(35.0, 45.0, 85.0, 70.0));
        // No proportions to keep.
        let flat = Rect::new(0.0, 5.0, 10.0, 5.0);
        assert_eq!(
            resize_rect(flat, RectHandle::Top, pt(3.0, 9.0), true),
            Rect::new(0.0, 5.0, 10.0, 9.0)
        );
    }

    #[test]
    fn cursors_follow_the_screen() {
        let c = |h, r| handle_cursor(HandleKind::Rect(h), r);
        // Unrotated: the page's top-right corner is the screen's top-right.
        assert_eq!(c(RectHandle::TopRight, Rotation::R0), Cursor::ResizeNeSw);
        assert_eq!(c(RectHandle::TopLeft, Rotation::R0), Cursor::ResizeNwSe);
        assert_eq!(c(RectHandle::Right, Rotation::R0), Cursor::ResizeHorizontal);
        assert_eq!(c(RectHandle::Top, Rotation::R0), Cursor::ResizeVertical);
        // A quarter turn: the page's right edge is the screen's bottom edge, and the page's
        // top-right corner is the screen's bottom-right.
        assert_eq!(c(RectHandle::Right, Rotation::R90), Cursor::ResizeVertical);
        assert_eq!(c(RectHandle::Top, Rotation::R90), Cursor::ResizeHorizontal);
        assert_eq!(c(RectHandle::TopRight, Rotation::R90), Cursor::ResizeNwSe);
        assert_eq!(c(RectHandle::TopLeft, Rotation::R90), Cursor::ResizeNeSw);
        // Half a turn puts every diagonal back on itself.
        assert_eq!(c(RectHandle::TopRight, Rotation::R180), Cursor::ResizeNeSw);
        assert_eq!(
            c(RectHandle::Left, Rotation::R180),
            Cursor::ResizeHorizontal
        );
        assert_eq!(
            c(RectHandle::BottomLeft, Rotation::R270),
            Cursor::ResizeNwSe
        );
        assert_eq!(
            handle_cursor(HandleKind::Vertex(3), Rotation::R90),
            Cursor::Crosshair
        );
    }
}
