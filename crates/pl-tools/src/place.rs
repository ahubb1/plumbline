//! The tools that place something ready-made or act on what is there: Stamp, Image, the four
//! text markup tools (highlight, underline, strike out, squiggly) and the Eraser.

use pl_annot::{AnnotId, AnnotKind, Annotation, Geometry, StampSpec};
use pl_draw::DrawList;
use pl_geom::{point_segment_distance, Point, Rect, Rotation, Vec2};

use crate::draw::create;
use crate::geom::{sign, to_display, to_page};
use crate::input::{Key, ToolInput};
use crate::intent::{AnnotChange, Cursor, Intent};
use crate::preset::ToolKind;
use crate::text::{box_around, press, track_drag, unit, Released};
use crate::tool::{usable, Phase, ToolState, ERASER_PX, IMAGE_PAGE_SHARE, IMAGE_POINTS_PER_PIXEL};
use crate::view::DocView;

// ---------------------------------------------------------------------------------------
// Stamp, Image
// ---------------------------------------------------------------------------------------

/// The stamp the Stamp tool places: the preset's, or APPROVED when the preset names none.
fn spec(state: &ToolState) -> StampSpec {
    state
        .preset
        .stamp
        .clone()
        .unwrap_or_else(StampSpec::approved)
}

/// The width and height on screen a click places the stamp or picture at, in page units;
/// `None` when there is nothing to place (no picture, or a stamp whose words cannot be set).
///
/// A stamp is placed at the size it is designed at. A picture is placed at
/// [`IMAGE_POINTS_PER_PIXEL`], made smaller if need be to cover no more than
/// [`IMAGE_PAGE_SHARE`] of the page's width and height as displayed.
fn natural(state: &ToolState, page: usize, view: &dyn DocView) -> Option<(f64, f64)> {
    let unit = unit(view, page);
    if state.tool() == ToolKind::Stamp {
        let (w, h) = spec(state).natural_size().ok()?;
        return Some((w / unit, h / unit));
    }
    let image = state.image.as_ref()?;
    let w = f64::from(image.width()) * IMAGE_POINTS_PER_PIXEL / unit;
    let h = f64::from(image.height()) * IMAGE_POINTS_PER_PIXEL / unit;
    let geometry = view.page(page)?;
    let crop = geometry.crop_box();
    let shown = to_display(geometry.rotate(), Vec2::new(crop.width(), crop.height()));
    let k = (IMAGE_PAGE_SHARE * shown.x.abs() / w)
        .min(IMAGE_PAGE_SHARE * shown.y.abs() / h)
        .min(1.0);
    (k.is_finite() && k > 0.0).then_some((w * k, h * k))
}

/// Where the stamp goes: centred on a click at its natural size; for a drag, from the press
/// to the pointer, in the proportions of `natural` (set by whichever of the two screen
/// offsets asks for more) unless `free`.
fn placed(
    anchor: Point,
    current: Point,
    dragged: bool,
    free: bool,
    natural: (f64, f64),
    rotation: Rotation,
) -> Rect {
    if !dragged {
        return box_around(anchor, natural, rotation);
    }
    if free {
        return Rect::from_points(anchor, current);
    }
    let (w, h) = natural;
    let d = to_display(rotation, current - anchor);
    let k = (d.x.abs() / w).max(d.y.abs() / h);
    let size = Vec2::new(sign(d.x) * k * w, sign(d.y) * k * h);
    Rect::from_points(anchor, anchor + to_page(rotation, size))
}

/// The stamp or picture filling `rect`, turned to read upright on the page as displayed.
fn stamp_markup(
    state: &ToolState,
    page: usize,
    rect: Rect,
    view: &dyn DocView,
) -> Option<Annotation> {
    if !(rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0) {
        return None;
    }
    let origin = view.origin();
    let mut a = if state.tool() == ToolKind::Stamp {
        Annotation::stamp(page, rect, spec(state), &origin)
    } else {
        Annotation::image_stamp(page, rect, state.image.clone()?, &origin)
    };
    state.preset.apply_to(&mut a);
    a.set_rotation(view.rotation(page).degrees());
    Some(a)
}

pub(crate) fn stamp_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => {
            if natural(state, p.page, view).is_some() {
                press(state, p, true);
            }
        }
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
            let (page, anchor, current, dragged, free) = match released {
                Released::Click { page, at } => (page, at, at, false, false),
                Released::Dragged {
                    page,
                    anchor,
                    current,
                    shift,
                } => (page, anchor, current, true, shift),
            };
            let Some(size) = natural(state, page, view) else {
                return;
            };
            let rect = placed(anchor, current, dragged, free, size, view.rotation(page));
            if let Some(a) = stamp_markup(state, page, rect, view) {
                create(a, true, intents);
            }
        }
        ToolInput::Key {
            key: Key::Escape, ..
        } => state.phase = Phase::Idle,
        _ => {}
    }
}

pub(crate) fn stamp_preview(state: &ToolState, view: &dyn DocView) -> DrawList {
    let Phase::Drag {
        page,
        anchor,
        current,
        centred,
        moved: true,
        ..
    } = &state.phase
    else {
        return DrawList::new();
    };
    if anchor == current {
        return DrawList::new();
    }
    natural(state, *page, view)
        .map(|size| {
            placed(
                *anchor,
                *current,
                true,
                *centred,
                size,
                view.rotation(*page),
            )
        })
        .and_then(|rect| stamp_markup(state, *page, rect, view))
        .map(|a| a.draw_list())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// Highlight, Underline, Strikeout, Squiggly over selected text
// ---------------------------------------------------------------------------------------

/// The markup over the text between `anchor` and `current`, one quad per line; `None` when
/// the view finds no text there.
fn text_markup(
    state: &ToolState,
    page: usize,
    anchor: Point,
    current: Point,
    view: &dyn DocView,
) -> Option<Annotation> {
    let kind = state.tool().annot_kind().unwrap_or(AnnotKind::Highlight);
    let quads = view.text_quads(page, anchor, current);
    let mut a = Annotation::text_markup_from_quads(kind, page, &quads, &view.origin())?;
    state.preset.apply_to(&mut a);
    Some(a)
}

pub(crate) fn markup_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    match input {
        // Text is picked where the pointer is: snap points are for drawing, not for reading.
        ToolInput::PointerDown(p) if usable(view, p) => press(state, p, false),
        ToolInput::PointerMove(p) | ToolInput::PointerUp(p) => {
            let up = matches!(input, ToolInput::PointerUp(_));
            if !matches!(state.phase, Phase::Drag { .. }) {
                if !up && usable(view, p) {
                    intents.push(Intent::SetCursor(Cursor::Crosshair));
                }
                return;
            }
            if let Some(Released::Dragged {
                page,
                anchor,
                current,
                ..
            }) = track_drag(state, p, up, false, view)
            {
                if let Some(a) = text_markup(state, page, anchor, current, view) {
                    create(a, false, intents);
                }
            }
        }
        ToolInput::Key {
            key: Key::Escape, ..
        } => state.phase = Phase::Idle,
        _ => {}
    }
}

pub(crate) fn markup_preview(state: &ToolState, view: &dyn DocView) -> DrawList {
    match &state.phase {
        Phase::Drag {
            page,
            anchor,
            current,
            moved: true,
            ..
        } => text_markup(state, *page, *anchor, *current, view)
            .map(|a| a.draw_list())
            .unwrap_or_default(),
        _ => DrawList::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Eraser
// ---------------------------------------------------------------------------------------

/// The shortest distance between two segments (either may be a single point).
fn segment_distance(a0: Point, a1: Point, b0: Point, b1: Point) -> f64 {
    let side = |p: Point, q: Point, r: Point| (q - p).cross(r - p);
    let crossing =
        side(b0, b1, a0) * side(b0, b1, a1) < 0.0 && side(a0, a1, b0) * side(a0, a1, b1) < 0.0;
    if crossing {
        return 0.0;
    }
    point_segment_distance(a0, b0, b1)
        .min(point_segment_distance(a1, b0, b1))
        .min(point_segment_distance(b0, a0, a1))
        .min(point_segment_distance(b1, a0, a1))
}

fn touches(stroke: &[Point], from: Point, to: Point, reach: f64) -> bool {
    match stroke {
        [] => false,
        [only] => point_segment_distance(*only, from, to) <= reach,
        _ => stroke
            .windows(2)
            .any(|w| segment_distance(w[0], w[1], from, to) <= reach),
    }
}

/// Rubs out every pen stroke on `page` that the eraser's move from `from` to `to` touches: a
/// drawing that keeps some strokes is changed, one that loses them all is deleted. Locked and
/// hidden drawings, and ink this crate cannot redraw, are left alone.
fn rub(view: &dyn DocView, page: usize, from: Point, to: Point, intents: &mut Vec<Intent>) {
    let px = view.pixel_size();
    let mut gone: Vec<AnnotId> = Vec::new();
    for a in view.annotations(page) {
        let Geometry::Ink(strokes) = &a.geometry else {
            continue;
        };
        if a.kind != AnnotKind::Ink
            || !a.is_modelled()
            || a.meta.flags.is_locked()
            || a.meta.flags.is_hidden()
        {
            continue;
        }
        let half = if a.style.stroke.is_some() {
            a.style.width.max(0.0) / 2.0
        } else {
            0.0
        };
        let reach = ERASER_PX * px + half;
        let kept: Vec<Vec<Point>> = strokes
            .iter()
            .filter(|s| !touches(s, from, to, reach))
            .cloned()
            .collect();
        if kept.len() == strokes.len() {
            continue;
        }
        if kept.is_empty() {
            gone.push(a.id.clone());
        } else {
            intents.push(Intent::Modify {
                id: a.id.clone(),
                change: AnnotChange::Geometry(Geometry::Ink(kept)),
            });
        }
    }
    if !gone.is_empty() {
        intents.push(Intent::Delete(gone));
    }
}

/// The eraser works as it goes: every move with the button down rubs out what it touched
/// since the last one, so the page shows the result at once. Esc ends the drag; what is
/// already rubbed out is undone with undo.
pub(crate) fn eraser_step(
    state: &mut ToolState,
    input: &ToolInput,
    view: &dyn DocView,
    intents: &mut Vec<Intent>,
) {
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => {
            state.phase = Phase::Stroke {
                page: p.page,
                points: vec![p.point],
                end: p.point,
                straight: false,
            };
            rub(view, p.page, p.point, p.point, intents);
        }
        ToolInput::PointerMove(p) | ToolInput::PointerUp(p) => {
            let up = matches!(input, ToolInput::PointerUp(_));
            let Phase::Stroke {
                page, points, end, ..
            } = &mut state.phase
            else {
                if !up && usable(view, p) {
                    intents.push(Intent::SetCursor(Cursor::Crosshair));
                }
                return;
            };
            if p.page == *page && p.is_finite() {
                rub(view, *page, *end, p.point, intents);
                *end = p.point;
                *points = vec![p.point];
            }
            if up {
                state.phase = Phase::Idle;
            }
        }
        ToolInput::Key {
            key: Key::Escape, ..
        } => state.phase = Phase::Idle,
        _ => {}
    }
}
