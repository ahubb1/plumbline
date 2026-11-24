//! The tools that create a shape: Rectangle, Ellipse, Cloud Box, Line, Arrow, Polyline,
//! Polygon, Cloud, Pen, Highlighter and Note.

use pl_annot::{AnnotKind, Annotation, Geometry};
use pl_draw::DrawList;
use pl_geom::{Point, Rect, Rotation};

use crate::geom::{constrain_angle, constrain_square, simplify_points};
use crate::input::{Key, Pointer, ToolInput};
use crate::intent::{Cursor, Intent};
use crate::preset::ToolKind;
use crate::tool::{
    usable, Phase, ToolState, CLOSE_PX, DRAG_PX, HIT_PX, NOTE_SIZE, SIMPLIFY_PX, THIN_PX,
};
use crate::view::DocView;

pub(crate) fn distance(a: Point, b: Point) -> f64 {
    (a - b).hypot()
}

/// The annotation `state`'s tool makes from `geometry`, in the preset's style.
pub(crate) fn build(
    state: &ToolState,
    page: usize,
    geometry: Geometry,
    view: &dyn DocView,
) -> Annotation {
    let kind = state.tool().annot_kind().unwrap_or(AnnotKind::Square);
    let mut a = Annotation::new(kind, page, geometry, &view.origin());
    state.preset.apply_to(&mut a);
    a
}

pub(crate) fn create(a: Annotation, select: bool, intents: &mut Vec<Intent>) {
    let id = a.id.clone();
    intents.push(Intent::Create(Box::new(a)));
    if select {
        intents.push(Intent::Select(vec![id]));
    }
}

// ---------------------------------------------------------------------------------------
// Rectangle, Ellipse, Line, Arrow
// ---------------------------------------------------------------------------------------

fn is_box(tool: ToolKind) -> bool {
    matches!(
        tool,
        ToolKind::Rectangle | ToolKind::Ellipse | ToolKind::CloudBox
    )
}

fn drag_point(tool: ToolKind, anchor: Point, pointer: &Pointer, rotation: Rotation) -> Point {
    let p = pointer.effective();
    match (pointer.modifiers.shift, is_box(tool)) {
        (false, _) => p,
        (true, true) => constrain_square(anchor, p, rotation),
        (true, false) => constrain_angle(anchor, p, rotation),
    }
}

fn drag_geometry(tool: ToolKind, anchor: Point, current: Point, centred: bool) -> Geometry {
    if is_box(tool) {
        let from = if centred {
            anchor - (current - anchor)
        } else {
            anchor
        };
        Geometry::Rect(Rect::from_points(from, current))
    } else {
        Geometry::Line {
            a: anchor,
            b: current,
        }
    }
}

pub(crate) fn drag_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    let tool = state.tool();
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => {
            let anchor = p.effective();
            state.phase = Phase::Drag {
                page: p.page,
                press: p.point,
                anchor,
                current: anchor,
                centred: is_box(tool) && p.modifiers.ctrl,
                moved: false,
            };
        }
        ToolInput::PointerMove(p) | ToolInput::PointerUp(p) => {
            let up = matches!(input, ToolInput::PointerUp(_));
            let Phase::Drag {
                page,
                press,
                anchor,
                current,
                centred,
                moved,
            } = &mut state.phase
            else {
                if !up && usable(view, p) {
                    intents.push(Intent::SetCursor(Cursor::Crosshair));
                }
                return;
            };
            if p.page == *page && p.is_finite() {
                *current = drag_point(tool, *anchor, p, view.rotation(*page));
                *centred = is_box(tool) && p.modifiers.ctrl;
                *moved |= distance(*press, p.point) >= DRAG_PX * view.pixel_size();
            }
            if !up {
                return;
            }
            let (page, anchor, current, centred, moved) =
                (*page, *anchor, *current, *centred, *moved);
            state.phase = Phase::Idle;
            // A click draws nothing, however far apart snapping put its two ends: what counts
            // is how far the pointer itself went. Nor does a shape too small to be meant.
            if !moved || distance(anchor, current) < DRAG_PX * view.pixel_size() {
                return;
            }
            let geometry = drag_geometry(tool, anchor, current, centred);
            if let Geometry::Rect(r) = &geometry {
                if r.width() <= 0.0 || r.height() <= 0.0 {
                    return;
                }
            }
            create(build(state, page, geometry, view), true, intents);
        }
        ToolInput::Key {
            key: Key::Escape, ..
        } => state.phase = Phase::Idle,
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Polyline, Polygon, Cloud
// ---------------------------------------------------------------------------------------

fn is_closed(tool: ToolKind) -> bool {
    matches!(tool, ToolKind::Polygon | ToolKind::Cloud)
}

fn min_points(tool: ToolKind) -> usize {
    if is_closed(tool) {
        3
    } else {
        2
    }
}

/// Where the next point of a path goes for this pointer, and whether that closes the shape
/// (a polygon or cloud with three points, the pointer on the first).
fn path_point(
    tool: ToolKind,
    points: &[Point],
    pointer: &Pointer,
    view: &dyn DocView,
    page: usize,
) -> (Point, bool) {
    let near = CLOSE_PX * view.pixel_size();
    if is_closed(tool) && points.len() >= 3 && distance(pointer.point, points[0]) <= near {
        return (points[0], true);
    }
    let p = pointer.effective();
    match points.last() {
        Some(last) if pointer.modifiers.shift => {
            (constrain_angle(*last, p, view.rotation(page)), false)
        }
        _ => (p, false),
    }
}

fn finish_path(state: &mut ToolState, view: &dyn DocView, intents: &mut Vec<Intent>) {
    let Phase::Path { page, points, .. } = std::mem::replace(&mut state.phase, Phase::Idle) else {
        return;
    };
    if points.len() >= min_points(state.tool()) {
        create(
            build(state, page, Geometry::Points(points), view),
            true,
            intents,
        );
    }
}

pub(crate) fn path_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    let tool = state.tool();
    let slop = CLOSE_PX * view.pixel_size();
    match input {
        ToolInput::PointerDown(p) | ToolInput::DoubleClick(p) if usable(view, p) => {
            let double = matches!(input, ToolInput::DoubleClick(_));
            match &mut state.phase {
                Phase::Path {
                    page,
                    points,
                    cursor,
                } => {
                    if p.page != *page {
                        return;
                    }
                    let (at, closes) = path_point(tool, points, p, view, *page);
                    // A click on the last point is the first half of a double-click, or the
                    // repeat of it: it adds nothing.
                    // So does a click on the first point of a polygon that cannot close yet.
                    let repeat = points.last().is_some_and(|l| distance(*l, at) <= slop)
                        || points.last().is_some_and(|l| distance(*l, p.point) <= slop)
                        || (is_closed(tool)
                            && points
                                .first()
                                .is_some_and(|f| distance(*f, p.point) <= slop));
                    if !closes && !repeat {
                        points.push(at);
                    }
                    *cursor = None;
                    if closes || double {
                        finish_path(state, view, intents);
                    }
                }
                _ if !double => {
                    state.phase = Phase::Path {
                        page: p.page,
                        points: vec![p.effective()],
                        cursor: None,
                    };
                }
                _ => {}
            }
        }
        ToolInput::PointerMove(p) if usable(view, p) => match &mut state.phase {
            Phase::Path {
                page,
                points,
                cursor,
            } => {
                if p.page == *page {
                    *cursor = Some(path_point(tool, points, p, view, *page).0);
                }
            }
            _ => intents.push(Intent::SetCursor(Cursor::Crosshair)),
        },
        ToolInput::Key { key, .. } => match key {
            Key::Escape => state.phase = Phase::Idle,
            Key::Enter => finish_path(state, view, intents),
            Key::Backspace => {
                if let Phase::Path { points, .. } = &mut state.phase {
                    points.pop();
                    if points.is_empty() {
                        state.phase = Phase::Idle;
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Pen, Highlighter
// ---------------------------------------------------------------------------------------

/// The points a freehand gesture gives, before simplifying.
fn stroke_points(points: &[Point], end: Point, straight: bool, rotation: Rotation) -> Vec<Point> {
    let Some(first) = points.first() else {
        return Vec::new();
    };
    if straight {
        let to = constrain_angle(*first, end, rotation);
        return if to == *first {
            vec![*first]
        } else {
            vec![*first, to]
        };
    }
    let mut out = points.to_vec();
    if out.last() != Some(&end) {
        out.push(end);
    }
    out
}

pub(crate) fn stroke_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    let px = view.pixel_size();
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => {
            state.phase = Phase::Stroke {
                page: p.page,
                points: vec![p.point],
                end: p.point,
                straight: p.modifiers.shift,
            };
        }
        ToolInput::PointerMove(p) | ToolInput::PointerUp(p) => {
            let up = matches!(input, ToolInput::PointerUp(_));
            let Phase::Stroke {
                page,
                points,
                end,
                straight,
            } = &mut state.phase
            else {
                if !up && usable(view, p) {
                    intents.push(Intent::SetCursor(Cursor::Crosshair));
                }
                return;
            };
            if p.page == *page && p.is_finite() {
                *end = p.point;
                *straight = p.modifiers.shift;
                if points
                    .last()
                    .is_none_or(|l| distance(*l, p.point) >= THIN_PX * px)
                {
                    points.push(p.point);
                }
            }
            if !up {
                return;
            }
            let page = *page;
            let raw = stroke_points(points, *end, *straight, view.rotation(page));
            state.phase = Phase::Idle;
            let Some(first) = raw.first().copied() else {
                return;
            };
            let reach = raw.iter().map(|q| distance(first, *q)).fold(0.0, f64::max);
            let stroke = if reach < DRAG_PX * px {
                // A tap: the pen leaves a dot, the highlighter nothing.
                if state.tool() == ToolKind::Highlighter {
                    return;
                }
                vec![first]
            } else {
                simplify_points(&raw, SIMPLIFY_PX * px)
            };
            create(
                build(state, page, Geometry::Ink(vec![stroke]), view),
                false,
                intents,
            );
        }
        ToolInput::Key {
            key: Key::Escape, ..
        } => state.phase = Phase::Idle,
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Note
// ---------------------------------------------------------------------------------------

pub(crate) fn note_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => {
            // The second click of a double-click lands on the note the first one made.
            let on_new_note = match view.selection() {
                [only] => view.annotation(only).is_some_and(|a| {
                    a.kind == AnnotKind::Text
                        && a.page == p.page
                        && a.hit_test(p.point, HIT_PX * view.pixel_size())
                }),
                _ => false,
            };
            if on_new_note {
                return;
            }
            let unit = view.page(p.page).map_or(1.0, |g| g.user_unit());
            let half = NOTE_SIZE / unit / 2.0;
            let c = p.effective();
            let rect = Rect::new(c.x - half, c.y - half, c.x + half, c.y + half);
            let note = build(state, p.page, Geometry::Rect(rect), view);
            let id = note.id.clone();
            create(note, true, intents);
            intents.push(Intent::RequestTextEdit(id));
        }
        ToolInput::PointerMove(p) if usable(view, p) => {
            intents.push(Intent::SetCursor(Cursor::Crosshair));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------------------

pub(crate) fn preview(state: &ToolState, view: &dyn DocView) -> DrawList {
    let tool = state.tool();
    match &state.phase {
        Phase::Drag {
            page,
            anchor,
            current,
            centred,
            moved: true,
            ..
        } if anchor != current => build(
            state,
            *page,
            drag_geometry(tool, *anchor, *current, *centred),
            view,
        )
        .draw_list(),
        Phase::Path {
            page,
            points,
            cursor,
        } => {
            let mut all = points.clone();
            if let Some(c) = cursor {
                if all.last() != Some(c) && all.first() != Some(c) {
                    all.push(*c);
                }
            }
            if all.len() < 2 {
                return DrawList::new();
            }
            let mut a = build(state, *page, Geometry::Points(all.clone()), view);
            if all.len() < 3 {
                // Not yet a polygon: show the first edge as a plain line.
                a.kind = AnnotKind::PolyLine;
                a.style.cloud = None;
            }
            a.draw_list()
        }
        Phase::Stroke {
            page,
            points,
            end,
            straight,
        } => {
            let pts = stroke_points(points, *end, *straight, view.rotation(*page));
            build(state, *page, Geometry::Ink(vec![pts]), view).draw_list()
        }
        _ => DrawList::new(),
    }
}
