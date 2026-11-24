//! The text tools: Text Box, Typewriter, Callout and Cloud Callout.
//!
//! Each ends its pointer gesture by asking for the text ([`Intent::RequestTextEdit`]) and then
//! waits in [`Phase::TextEntry`] with the markup drafted but not created. The application's
//! editor answers through [`step_text`](crate::step_text).

use pl_annot::{callout_route, Annotation, DrawError, Geometry, LineEnding, Style};
use pl_draw::DrawList;
use pl_geom::{Point, Rect, Rotation, Vec2};

use crate::draw::{create, distance};
use crate::geom::{constrain_angle, to_display, to_page};
use crate::input::{Key, Pointer, TextInput, ToolInput};
use crate::intent::{Cursor, Intent};
use crate::preset::ToolKind;
use crate::tool::{
    usable, BoxDrag, Fit, Phase, ToolState, CALLOUT_BOX_SIZE, CLOSE_PX, DRAG_PX, TEXT_BOX_SIZE,
};
use crate::view::DocView;

/// A box fitted to its text is this much larger than the text needs, before rounding up to a
/// quarter of a unit, so that rounding never clips the last line or wraps the last word.
const FIT_SLACK: f64 = 0.01;
/// The width a typewriter line is laid out in to find how wide its text is.
const UNWRAPPED: f64 = 1.0e6;

// ---------------------------------------------------------------------------------------
// Press, drag, release: shared with the stamp and text markup tools
// ---------------------------------------------------------------------------------------

/// What a press-and-release came to.
pub(crate) enum Released {
    /// The pointer did not travel: a click at the press (or its snap point).
    Click { page: usize, at: Point },
    Dragged {
        page: usize,
        anchor: Point,
        current: Point,
        /// Shift was held at the end.
        shift: bool,
    },
}

/// Starts a [`Phase::Drag`] at the pointer.
pub(crate) fn press(state: &mut ToolState, p: &Pointer, snap: bool) {
    let anchor = if snap { p.effective() } else { p.point };
    state.phase = Phase::Drag {
        page: p.page,
        press: p.point,
        anchor,
        current: anchor,
        centred: p.modifiers.shift,
        moved: false,
    };
}

/// Follows a [`Phase::Drag`] for a pointer move or release. On release the phase ends and the
/// result says whether the gesture was a click or a drag; otherwise, and in any other phase,
/// `None`.
pub(crate) fn track_drag(
    state: &mut ToolState,
    p: &Pointer,
    up: bool,
    snap: bool,
    view: &dyn DocView,
) -> Option<Released> {
    let px = view.pixel_size();
    let Phase::Drag {
        page,
        press,
        anchor,
        current,
        centred,
        moved,
    } = &mut state.phase
    else {
        return None;
    };
    if p.page == *page && p.is_finite() {
        *current = if snap { p.effective() } else { p.point };
        *centred = p.modifiers.shift;
        *moved |= distance(*press, p.point) >= DRAG_PX * px;
    }
    if !up {
        return None;
    }
    let released = if *moved && distance(*anchor, *current) >= DRAG_PX * px {
        Released::Dragged {
            page: *page,
            anchor: *anchor,
            current: *current,
            shift: *centred,
        }
    } else {
        Released::Click {
            page: *page,
            at: *anchor,
        }
    };
    state.phase = Phase::Idle;
    Some(released)
}

// ---------------------------------------------------------------------------------------
// Boxes as the user sees them
// ---------------------------------------------------------------------------------------

/// The page's `/UserUnit`: sizes given in points are divided by it.
pub(crate) fn unit(view: &dyn DocView, page: usize) -> f64 {
    view.page(page)
        .map(|g| g.user_unit())
        .filter(|u| u.is_finite() && *u > 0.0)
        .unwrap_or(1.0)
}

fn scaled(size: (f64, f64), unit: f64) -> (f64, f64) {
    (size.0 / unit, size.1 / unit)
}

/// The box of `size` (width and height on screen) whose top-left corner on screen is `at`.
pub(crate) fn box_at(at: Point, size: (f64, f64), rotation: Rotation) -> Rect {
    Rect::from_points(at, at + to_page(rotation, Vec2::new(size.0, size.1)))
}

/// The box of `size` (width and height on screen) centred on `centre`.
pub(crate) fn box_around(centre: Point, size: (f64, f64), rotation: Rotation) -> Rect {
    let half = to_page(rotation, Vec2::new(size.0 / 2.0, size.1 / 2.0));
    Rect::from_points(centre - half, centre + half)
}

/// `rect` given a new width and height on screen, its top-left corner on screen staying put.
fn resized(rect: Rect, rotation: Rotation, size: impl FnOnce(f64, f64) -> (f64, f64)) -> Rect {
    let a = to_display(rotation, Vec2::new(rect.x0, rect.y0));
    let b = to_display(rotation, Vec2::new(rect.x1, rect.y1));
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (w, h) = size((a.x - b.x).abs(), (a.y - b.y).abs());
    let p = to_page(rotation, Vec2::new(x0, y0));
    let q = to_page(rotation, Vec2::new(x0 + w, y0 + h));
    Rect::from_points(p.to_point(), q.to_point())
}

/// The point of `rect`'s edge nearest `p`.
fn nearest_on_edge(rect: Rect, p: Point) -> Point {
    let q = Point::new(p.x.clamp(rect.x0, rect.x1), p.y.clamp(rect.y0, rect.y1));
    if q != p {
        return q;
    }
    // Inside: out through the nearest side.
    let sides = [
        (p.x - rect.x0, Point::new(rect.x0, p.y)),
        (rect.x1 - p.x, Point::new(rect.x1, p.y)),
        (p.y - rect.y0, Point::new(p.x, rect.y0)),
        (rect.y1 - p.y, Point::new(p.x, rect.y1)),
    ];
    sides
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(p, |(_, at)| at)
}

// ---------------------------------------------------------------------------------------
// The markups
// ---------------------------------------------------------------------------------------

/// A callout's leader from `tip` to `rect`: through `knee` when one was placed outside the
/// box (the leader then meets the side of the box that faces the knee), routed by pl-annot
/// otherwise.
fn leader(rect: Rect, tip: Point, knee: Option<Point>) -> Vec<Point> {
    let inside = |k: Point| k.x >= rect.x0 && k.x <= rect.x1 && k.y >= rect.y0 && k.y <= rect.y1;
    match knee {
        Some(k) if !inside(k) => match callout_route(rect, k).last() {
            Some(end) => vec![tip, k, *end],
            None => callout_route(rect, tip),
        },
        _ => callout_route(rect, tip),
    }
}

/// Gives a text markup the preset's style and text settings. Sizes in the preset are in
/// points; on a `/UserUnit` page the font size and the padding are divided by the unit, so the
/// text is the same size on paper as anywhere else.
fn restyle(state: &ToolState, a: &mut Annotation, unit: f64) {
    state.preset.apply_to(a);
    // A cloud border belongs to the cloud of a Cloud Callout, never to the text box.
    a.style.cloud = None;
    if let Some(t) = a.free_text.as_mut() {
        t.size /= unit;
        t.padding /= unit;
    }
}

/// The text markup the tool makes in `rect`, with no text yet: a typewriter line, a callout
/// with `leader`, or a text box. It is turned to read upright on the page as displayed.
fn draft(
    state: &ToolState,
    page: usize,
    rect: Rect,
    leader: Option<Vec<Point>>,
    view: &dyn DocView,
) -> Annotation {
    let origin = view.origin();
    let mut a = match (state.tool(), leader.as_deref()) {
        (ToolKind::Typewriter, _) => Annotation::typewriter(page, rect, "", &origin),
        (_, Some([tip, ..])) => Annotation::callout(page, rect, *tip, "", &origin),
        _ => Annotation::free_text(page, rect, "", &origin),
    };
    restyle(state, &mut a, unit(view, page));
    if let (Some(points), Some(t)) = (leader, a.free_text.as_mut()) {
        t.callout = points;
    }
    a.set_rotation(view.rotation(page).degrees());
    a
}

fn cloud_style(state: &ToolState, a: &mut Annotation) {
    state.preset.apply_to(a);
    a.style.line_start = LineEnding::None;
    a.style.line_end = LineEnding::None;
}

/// The cloud of a Cloud Callout.
fn cloud_markup(state: &ToolState, page: usize, rect: Rect, view: &dyn DocView) -> Annotation {
    let mut a = Annotation::square(page, rect, &view.origin());
    cloud_style(state, &mut a);
    a
}

/// Starts waiting for the text of `draft`.
fn begin(
    state: &mut ToolState,
    page: usize,
    draft: Annotation,
    companion: Option<Annotation>,
    fit: Fit,
    intents: &mut Vec<Intent>,
) {
    intents.push(Intent::RequestTextEdit(draft.id.clone()));
    state.phase = Phase::TextEntry {
        page,
        draft: Box::new(draft),
        companion: companion.map(Box::new),
        fit,
    };
}

/// While the editor is open the tool takes no pointer input; Esc abandons the markup.
fn waiting(state: &mut ToolState, input: &ToolInput) {
    if let ToolInput::Key {
        key: Key::Escape, ..
    } = input
    {
        state.phase = Phase::Idle;
    }
}

// ---------------------------------------------------------------------------------------
// Text Box, Typewriter
// ---------------------------------------------------------------------------------------

/// The box a click places: a text box of [`TEXT_BOX_SIZE`], or one typewriter line of that
/// width.
fn default_size(state: &ToolState, unit: f64) -> (f64, f64) {
    if state.tool() != ToolKind::Typewriter {
        return scaled(TEXT_BOX_SIZE, unit);
    }
    let t = &state.preset.text;
    let border = if state.preset.style.stroke.is_some() {
        state.preset.style.width.max(0.0)
    } else {
        0.0
    };
    let line = t.size * t.line_spacing.max(1.0) + 2.0 * t.padding.max(0.0);
    (TEXT_BOX_SIZE.0 / unit, line / unit + 2.0 * border)
}

pub(crate) fn box_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    if matches!(state.phase, Phase::TextEntry { .. }) {
        waiting(state, input);
        return;
    }
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => press(state, p, true),
        ToolInput::PointerMove(p) | ToolInput::PointerUp(p) => {
            let up = matches!(input, ToolInput::PointerUp(_));
            if !matches!(state.phase, Phase::Drag { .. }) {
                if !up && usable(view, p) {
                    intents.push(Intent::SetCursor(Cursor::Crosshair));
                }
                return;
            }
            let Some(released) = track_drag(state, p, up, true, view) else {
                return;
            };
            let typewriter = state.tool() == ToolKind::Typewriter;
            let (page, at, drawn) = match released {
                Released::Click { page, at } => (page, at, None),
                Released::Dragged {
                    page,
                    anchor,
                    current,
                    ..
                } => {
                    let r = Rect::from_points(anchor, current);
                    // A drag along one screen axis has no box in it: it places like a click.
                    let drawn = (r.width() > 0.0 && r.height() > 0.0).then_some(r);
                    (page, anchor, drawn)
                }
            };
            let (rect, fit) = match drawn {
                Some(r) if typewriter => (r, Fit::Height),
                Some(r) => (r, Fit::Exact),
                None => {
                    let size = default_size(state, unit(view, page));
                    let fit = if typewriter {
                        Fit::Both
                    } else {
                        Fit::GrowHeight
                    };
                    (box_at(at, size, view.rotation(page)), fit)
                }
            };
            let text = draft(state, page, rect, None, view);
            begin(state, page, text, None, fit, intents);
        }
        ToolInput::Key {
            key: Key::Escape, ..
        } => state.phase = Phase::Idle,
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Callout, Cloud Callout
// ---------------------------------------------------------------------------------------

/// Ends a [`Phase::Leader`] with the text box at `rect` and starts waiting for the text.
fn finish(state: &mut ToolState, rect: Rect, view: &dyn DocView, intents: &mut Vec<Intent>) {
    let Phase::Leader {
        page,
        points,
        cloud,
        ..
    } = std::mem::replace(&mut state.phase, Phase::Idle)
    else {
        return;
    };
    let (tip, knee) = match (points.first(), cloud) {
        (Some(tip), _) => (*tip, points.get(1).copied()),
        // A Cloud Callout's leader starts on the cloud, at the point nearest the text.
        (None, Some(c)) => (nearest_on_edge(c, rect.center()), None),
        (None, None) => return,
    };
    let text = draft(state, page, rect, Some(leader(rect, tip, knee)), view);
    let companion = cloud.map(|c| cloud_markup(state, page, c, view));
    begin(state, page, text, companion, Fit::Exact, intents);
}

pub(crate) fn callout_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    if matches!(state.phase, Phase::TextEntry { .. }) {
        waiting(state, input);
        return;
    }
    let tool = state.tool();
    let px = view.pixel_size();
    let slop = CLOSE_PX * px;
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => match &mut state.phase {
            Phase::Leader { page, drag, .. } => {
                if p.page == *page {
                    let at = p.effective();
                    *drag = Some(BoxDrag {
                        press: p.point,
                        anchor: at,
                        current: at,
                        moved: false,
                    });
                }
            }
            // A callout starts with a click on what it points at.
            _ if tool == ToolKind::Callout => {
                state.phase = Phase::Leader {
                    page: p.page,
                    points: vec![p.effective()],
                    cloud: None,
                    hover: None,
                    drag: None,
                };
            }
            // A cloud callout starts with the cloud.
            _ => press(state, p, true),
        },
        ToolInput::PointerMove(p) | ToolInput::PointerUp(p) => {
            let up = matches!(input, ToolInput::PointerUp(_));
            // The box to finish with, when this event places it.
            let placed = match &mut state.phase {
                Phase::Drag { .. } => {
                    if let Some(Released::Dragged {
                        page,
                        anchor,
                        current,
                        ..
                    }) = track_drag(state, p, up, true, view)
                    {
                        let cloud = Rect::from_points(anchor, current);
                        if cloud.width() > 0.0 && cloud.height() > 0.0 {
                            state.phase = Phase::Leader {
                                page,
                                points: Vec::new(),
                                cloud: Some(cloud),
                                hover: None,
                                drag: None,
                            };
                        }
                    }
                    None
                }
                Phase::Leader {
                    page,
                    points,
                    hover,
                    drag,
                    ..
                } => {
                    if p.page == *page && p.is_finite() {
                        match drag {
                            Some(d) => {
                                d.current = p.effective();
                                d.moved |= distance(d.press, p.point) >= DRAG_PX * px;
                            }
                            None => *hover = Some(p.effective()),
                        }
                    }
                    if !up {
                        return;
                    }
                    let Some(d) = drag.take() else {
                        return;
                    };
                    let rect = Rect::from_points(d.anchor, d.current);
                    let dragged = d.moved
                        && distance(d.anchor, d.current) >= DRAG_PX * px
                        && rect.width() > 0.0
                        && rect.height() > 0.0;
                    if dragged {
                        Some(rect)
                    } else {
                        // A click.
                        let near = |q: Point| {
                            distance(d.press, q) <= slop || distance(d.anchor, q) <= slop
                        };
                        let rotation = view.rotation(*page);
                        match points.as_slice() {
                            // The first click after the tip is the knee; one on the tip
                            // itself is the second half of a double-click and adds nothing.
                            [tip] => {
                                if !near(*tip) {
                                    let knee = if p.modifiers.shift {
                                        constrain_angle(*tip, d.anchor, rotation)
                                    } else {
                                        d.anchor
                                    };
                                    points.push(knee);
                                    *hover = None;
                                }
                                return;
                            }
                            // A second click where the knee went is a double-click: the
                            // box goes there instead, and the leader is routed to it.
                            [_, knee] if near(*knee) => {
                                points.pop();
                            }
                            _ => {}
                        }
                        let size = scaled(CALLOUT_BOX_SIZE, unit(view, *page));
                        Some(box_around(d.anchor, size, rotation))
                    }
                }
                _ => {
                    if !up && usable(view, p) {
                        intents.push(Intent::SetCursor(Cursor::Crosshair));
                    }
                    None
                }
            };
            if let Some(rect) = placed {
                finish(state, rect, view, intents);
            }
        }
        ToolInput::DoubleClick(p) if usable(view, p) => {
            let Phase::Leader { page, points, .. } = &mut state.phase else {
                return;
            };
            if p.page != *page {
                return;
            }
            let at = p.effective();
            let near = |q: Point| distance(p.point, q) <= slop || distance(at, q) <= slop;
            match points.as_slice() {
                [tip] if near(*tip) => return,
                [_, knee] if near(*knee) => {
                    points.pop();
                }
                _ => {}
            }
            let size = scaled(CALLOUT_BOX_SIZE, unit(view, *page));
            let rect = box_around(at, size, view.rotation(*page));
            finish(state, rect, view, intents);
        }
        ToolInput::Key { key, .. } => match key {
            Key::Escape => state.phase = Phase::Idle,
            Key::Backspace => {
                if let Phase::Leader { points, drag, .. } = &mut state.phase {
                    *drag = None;
                    if points.len() == 2 {
                        points.pop();
                    } else {
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
// The text arrives
// ---------------------------------------------------------------------------------------

/// Fits the box of `a` to its text as `fit` says. Fails, changing nothing that matters, when
/// the text cannot be laid out (a character outside WinAnsi).
fn fitted(a: &mut Annotation, fit: Fit) -> Result<(), DrawError> {
    let mut layout = a.text_layout().ok_or(DrawError::InvalidNumber)??;
    let Geometry::Rect(rect) = a.geometry else {
        return Ok(());
    };
    if fit == Fit::Exact {
        return Ok(());
    }
    let rotation = Rotation::from_degrees(i32::from(a.rotation())).unwrap_or(Rotation::R0);
    let mut rect = rect;
    if fit == Fit::Both {
        // Laid out with all the room in the world, a line is as wide as its text.
        rect = resized(rect, rotation, |_, h| (UNWRAPPED, h));
        a.geometry = Geometry::Rect(rect);
        layout = a.text_layout().ok_or(DrawError::InvalidNumber)??;
    }
    let (frame, _) = a.text_frame().ok_or(DrawError::InvalidNumber)?;
    let padding = a.free_text.as_ref().map_or(0.0, |t| t.padding.max(0.0));
    let widest = layout.lines.iter().map(|l| l.width).fold(0.0, f64::max);
    let grow_w = widest + 2.0 * padding - frame.width();
    let grow_h = layout.overflow.height_needed - frame.height();
    let snug = |v: f64| ((v + FIT_SLACK) * 4.0).ceil() / 4.0;
    a.geometry = Geometry::Rect(resized(rect, rotation, |w, h| {
        let w = if fit == Fit::Both {
            snug(w + grow_w)
        } else {
            w
        };
        let h = if fit == Fit::GrowHeight && grow_h <= 0.0 {
            h
        } else {
            snug(h + grow_h)
        };
        (w, h)
    }));
    Ok(())
}

pub(crate) fn commit(
    state: &mut ToolState,
    input: &TextInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    let Phase::TextEntry {
        page,
        draft,
        companion,
        fit,
    } = &state.phase
    else {
        return;
    };
    let text = match input {
        TextInput::Cancel => {
            state.phase = Phase::Idle;
            return;
        }
        TextInput::Commit(text) => text,
    };
    // The style is read now, as for every other tool: the preset may have changed while the
    // editor was open.
    let mut companion = companion.as_deref().cloned();
    if let Some(c) = companion.as_mut() {
        cloud_style(state, c);
    }
    if text.trim().is_empty() {
        state.phase = Phase::Idle;
        if let Some(c) = companion {
            create(c, true, intents);
        }
        return;
    }
    let mut a = (**draft).clone();
    restyle(state, &mut a, unit(view, *page));
    a.meta.contents = Some(text.clone());
    if fitted(&mut a, *fit).is_err() {
        // Nothing is created and nothing is lost: the editor is asked for again.
        intents.push(Intent::RequestTextEdit(a.id));
        return;
    }
    state.phase = Phase::Idle;
    match companion {
        Some(c) => {
            let ids = vec![c.id.clone(), a.id.clone()];
            intents.push(Intent::Create(Box::new(c)));
            intents.push(Intent::Create(Box::new(a)));
            intents.push(Intent::Select(ids));
        }
        None => create(a, true, intents),
    }
}

// ---------------------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------------------

/// The leader placed so far, drawn as a line with the callout's ending at the tip.
fn leader_preview(
    state: &ToolState,
    page: usize,
    points: Vec<Point>,
    view: &dyn DocView,
) -> DrawList {
    if points.len() < 2 {
        return DrawList::new();
    }
    let mut a = Annotation::polyline(page, points, &view.origin());
    let preset = &state.preset.style;
    a.style = Style {
        stroke: preset.stroke,
        width: preset.width,
        dash: preset.dash.clone(),
        opacity: preset.opacity,
        line_start: preset.line_end,
        ..a.style
    };
    a.draw_list()
}

pub(crate) fn preview(state: &ToolState, view: &dyn DocView) -> DrawList {
    let mut list = DrawList::new();
    match &state.phase {
        Phase::Drag {
            page,
            anchor,
            current,
            moved: true,
            ..
        } => {
            let rect = Rect::from_points(*anchor, *current);
            if rect.width() > 0.0 && rect.height() > 0.0 {
                let a = if state.tool() == ToolKind::CloudCallout {
                    cloud_markup(state, *page, rect, view)
                } else {
                    draft(state, *page, rect, None, view)
                };
                list.extend(a.draw_list().ops);
            }
        }
        Phase::Leader {
            page,
            points,
            cloud,
            hover,
            drag,
        } => {
            if let Some(c) = cloud {
                list.extend(cloud_markup(state, *page, *c, view).draw_list().ops);
            }
            let rect = drag
                .filter(|d| d.moved)
                .map(|d| Rect::from_points(d.anchor, d.current))
                .filter(|r| r.width() > 0.0 && r.height() > 0.0);
            match (rect, points.first(), cloud) {
                (Some(rect), Some(tip), _) => {
                    let route = leader(rect, *tip, points.get(1).copied());
                    list.extend(draft(state, *page, rect, Some(route), view).draw_list().ops);
                }
                (Some(rect), None, Some(c)) => {
                    let tip = nearest_on_edge(*c, rect.center());
                    let route = leader(rect, tip, None);
                    list.extend(draft(state, *page, rect, Some(route), view).draw_list().ops);
                }
                _ => {
                    let mut line = points.clone();
                    if let Some(h) = hover {
                        if line.last() != Some(h) {
                            line.push(*h);
                        }
                    }
                    list.extend(leader_preview(state, *page, line, view).ops);
                }
            }
        }
        Phase::TextEntry {
            draft, companion, ..
        } => {
            if let Some(c) = companion {
                list.extend(c.draw_list().ops);
            }
            list.extend(draft.draw_list().ops);
        }
        _ => {}
    }
    list
}
