//! The measurement tools: Calibrate, Viewport, Length, Polylength, Perimeter, Area, Cutout,
//! Volume, Angle, Radius, Diameter and Count, and the running read-out they offer.
//!
//! # Gestures
//!
//! | Tool | Gesture |
//! |---|---|
//! | Length | click both ends, or drag from one to the other |
//! | Polylength | click each point; double-click or Enter finishes; Backspace removes the last point |
//! | Perimeter, Area, Volume | click each corner (clicking the first closes the outline), or drag a rectangle |
//! | Cutout | click inside an area or volume, then click the hole's corners (or drag a rectangle) |
//! | Angle | click one arm's end, the vertex, the other arm's end |
//! | Radius, Diameter | click the centre and a point on the circle; with the preset's `three_point`, three points on it |
//! | Count | each click adds a marker to the count being made (the selected count); Alt-click on a marker removes it; Enter ends |
//! | Calibrate | optionally drag a region first (it becomes a new viewport); click two points; the application then supplies the real distance |
//! | Viewport | drag the region; the application then supplies its scale |
//!
//! Shift keeps a segment to steps of the preset's `angle_step` from the screen's horizontal
//! ([`constrain_step`]) and a dragged rectangle square. The snap point the caller found is used
//! wherever a point is placed unless Alt is held.
//!
//! # Scale
//!
//! A new measurement is taken at the scale [`DocView::viewports`] gives for its points: the
//! topmost viewport with a scale holding all of them, else the page's own. Where there is none
//! it is taken at full size (1:1) and follows the page's scale once one is set. Values are
//! shown in the units the preset names, or else in the scale's own.

use pl_annot::{AnnotId, Annotation, Geometry, MeasureKind, MeasureSpec, ScaleRef};
use pl_draw::shapes::{polyline_path, rect_path};
use pl_draw::{Dash, DrawList, ShapeStyle, Stroke};
use pl_geom::{
    circle_from_three_points, point_in_polygon, point_in_region, point_segment_distance,
    polygon_area, LengthFormat, Point, Rect, Rotation, Scale, Unit, VolumeFormat, VolumeUnit,
};

use crate::arrange::movable;
use crate::draw::{create, distance};
use crate::geom::{constrain_square, to_display};
use crate::input::{Key, MeasureInput, Pointer, ToolInput};
use crate::intent::{AnnotChange, Cursor, Intent, MeasureRequest, ScaleChange, ScaleTarget};
use crate::preset::{MeasurePreset, ToolKind};
use crate::select::GUIDE;
use crate::snap::constrain_step;
use crate::tool::{usable, Phase, ToolState, CLOSE_PX, DRAG_PX, HIT_PX};
use crate::view::DocView;

/// The button is down at the start of a measurement: it may be a click or a drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Press {
    /// Where the pointer itself went down.
    pub at: Point,
    /// Where the drag starts: the snap point there, unless Alt was held.
    pub anchor: Point,
    pub current: Point,
    /// The pointer itself has travelled at least [`DRAG_PX`].
    pub moved: bool,
}

impl Press {
    fn at(p: &Pointer) -> Press {
        let anchor = p.effective();
        Press {
            at: p.point,
            anchor,
            current: anchor,
            moved: false,
        }
    }
}

/// A measurement in progress.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Gesture {
    pub page: usize,
    /// The points placed so far.
    pub points: Vec<Point>,
    /// Where the next point would go.
    pub cursor: Option<Point>,
    /// The first press, while the button is down.
    pub press: Option<Press>,
    /// Calibrate: the region dragged first, which becomes a new viewport.
    pub region: Option<Rect>,
    /// Cutout: the area or volume the hole goes in.
    pub target: Option<AnnotId>,
    /// The tool asked the user for a value and waits for [`step_measure`].
    pub waiting: Option<MeasureRequest>,
}

impl Gesture {
    fn new(page: usize) -> Gesture {
        Gesture {
            page,
            points: Vec::new(),
            cursor: None,
            press: None,
            region: None,
            target: None,
            waiting: None,
        }
    }
}

/// One value of a [`Readout`].
#[derive(Debug, Clone, PartialEq)]
pub struct Value {
    /// In SI units (metres, square metres, cubic metres, radians); `None` when the scale's
    /// unit is not one that converts to metres.
    pub si: Option<f64>,
    /// As the measurement would show it.
    pub text: String,
}

/// What a measurement in progress amounts to so far, worked out through the scale it will be
/// taken at: for a tooltip beside the pointer. A field is `None` when it does not apply yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Readout {
    pub tool: ToolKind,
    pub page: usize,
    /// The scale the measurement is taken at.
    pub scale: ScaleRef,
    /// That scale as text (`/R`), such as `1 in = 20 ft`.
    pub ratio: String,
    /// The segment being drawn: from the last point placed to the pointer.
    pub segment: Option<Value>,
    /// The length so far: along the points, round the outline for a closed shape, the
    /// diameter of a Diameter.
    pub total: Option<Value>,
    /// The area enclosed so far (a cutout: the hole's own).
    pub area: Option<Value>,
    /// A volume whose depth the preset gives.
    pub volume: Option<Value>,
    /// Angle: the angle at the vertex.
    pub angle: Option<Value>,
    /// The direction of the segment being drawn, counter-clockwise from the screen's
    /// horizontal as the page is displayed, in `[0, 360)` degrees.
    pub direction: Option<Value>,
    /// Radius and Diameter: the radius of the circle.
    pub radius: Option<Value>,
    /// Count: the markers of the count being made.
    pub count: Option<usize>,
}

fn user_unit(view: &dyn DocView, page: usize) -> f64 {
    view.page(page).map_or(1.0, |g| g.user_unit())
}

fn is_closed(tool: ToolKind) -> bool {
    matches!(
        tool,
        ToolKind::Perimeter | ToolKind::Area | ToolKind::Volume | ToolKind::Cutout
    )
}

fn three_point(state: &ToolState) -> bool {
    matches!(state.tool(), ToolKind::Radius | ToolKind::Diameter)
        && state.preset.measure_preset().three_point
}

/// How many points finish the measurement by themselves; `None` for the open-ended ones.
fn needed(state: &ToolState) -> Option<usize> {
    match state.tool() {
        ToolKind::Length | ToolKind::Calibrate => Some(2),
        ToolKind::Angle => Some(3),
        ToolKind::Radius | ToolKind::Diameter => Some(if three_point(state) { 3 } else { 2 }),
        _ => None,
    }
}

/// The fewest points the open-ended tools finish with.
fn min_points(state: &ToolState) -> usize {
    needed(state).unwrap_or(if is_closed(state.tool()) { 3 } else { 2 })
}

/// What a drag at the start of a measurement means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragAs {
    /// Both ends of a line.
    Line,
    /// A rectangular outline.
    Rect,
    /// A region of the page (a viewport).
    Region,
    /// The first two points; more follow.
    Points,
}

fn drag_as(state: &ToolState, g: &Gesture) -> DragAs {
    match state.tool() {
        ToolKind::Length => DragAs::Line,
        ToolKind::Radius | ToolKind::Diameter if !three_point(state) => DragAs::Line,
        ToolKind::Perimeter | ToolKind::Area | ToolKind::Volume | ToolKind::Cutout => DragAs::Rect,
        ToolKind::Calibrate if g.region.is_none() => DragAs::Region,
        ToolKind::Calibrate => DragAs::Line,
        ToolKind::Viewport => DragAs::Region,
        _ => DragAs::Points,
    }
}

/// The corners of a rectangle as an outline, counter-clockwise in page space from its
/// bottom-left.
pub(crate) fn rect_ring(r: Rect) -> Vec<Point> {
    vec![
        Point::new(r.x0, r.y0),
        Point::new(r.x1, r.y0),
        Point::new(r.x1, r.y1),
        Point::new(r.x0, r.y1),
    ]
}

/// Where a drag from `anchor` ends for this pointer.
fn drag_point(
    kind: DragAs,
    anchor: Point,
    p: &Pointer,
    rotation: Rotation,
    preset: &MeasurePreset,
) -> Point {
    let at = p.effective();
    match (p.modifiers.shift, kind) {
        (false, _) => at,
        (true, DragAs::Rect | DragAs::Region) => constrain_square(anchor, at, rotation),
        (true, _) => constrain_step(anchor, at, rotation, preset.angle_step),
    }
}

/// Where the next point goes for this pointer, and whether that closes the outline (three
/// points or more, the pointer on the first).
fn place(state: &ToolState, g: &Gesture, p: &Pointer, view: &dyn DocView) -> (Point, bool) {
    let near = CLOSE_PX * view.pixel_size();
    if is_closed(state.tool()) && g.points.len() >= 3 && distance(p.point, g.points[0]) <= near {
        return (g.points[0], true);
    }
    let at = p.effective();
    match g.points.last() {
        Some(last) if p.modifiers.shift => (
            constrain_step(
                *last,
                at,
                view.rotation(g.page),
                state.preset.measure_preset().angle_step,
            ),
            false,
        ),
        _ => (at, false),
    }
}

/// The scale a measurement over `points` is taken at, and its measure dictionary with the
/// preset's units.
pub(crate) fn scale_at(
    state: &ToolState,
    view: &dyn DocView,
    page: usize,
    points: &[Point],
) -> (ScaleRef, MeasureSpec) {
    let uu = user_unit(view, page);
    let preset = state.preset.measure_preset();
    let found = view.viewports(page).and_then(|v| {
        let (scale, spec) = v.scale_for(points);
        spec.map(|s| (scale, s.clone()))
    });
    match found {
        Some((scale, spec)) => {
            let own_units = preset.length.is_some() || preset.area.is_some();
            // A drawing with one scale on both axes is shown in the preset's units; one with
            // two keeps the dictionary it has.
            let spec = match spec.scale(uu) {
                Some(s) if own_units && spec.y.is_none() => {
                    MeasureSpec::new(&s, uu, &preset.format_for(&s))
                }
                _ => spec,
            };
            (scale, spec)
        }
        None => (
            ScaleRef::Page,
            MeasureSpec::new(&Scale::FULL_SIZE, uu, &preset.format_for(&Scale::FULL_SIZE)),
        ),
    }
}

/// Gives a new measurement the preset's style and presentation, the scale it was taken at,
/// and a caption that reads upright on the page as displayed.
fn dress(state: &ToolState, view: &dyn DocView, page: usize, a: &mut Annotation, scale: ScaleRef) {
    let preset = state.preset.measure_preset();
    let uu = user_unit(view, page);
    state.preset.apply_to(a);
    if let Some(m) = a.measure.as_mut() {
        m.scale = scale;
        m.caption = preset.caption;
        m.caption_position = preset.caption_position;
        m.text_size = preset.text_size / uu;
        m.symbol = preset.symbol;
        m.symbol_size = preset.symbol_size / uu;
        if let Some(v) = preset.volume {
            m.volume = v;
        }
    }
    a.set_rotation(view.rotation(page).degrees());
    a.sync_measurement();
}

/// The centre and the point on the circle a Radius or Diameter is drawn to.
fn circle_of(points: &[Point], three: bool) -> Option<(Point, Point)> {
    let (centre, edge) = if three {
        let [a, b, c] = points else {
            return None;
        };
        (circle_from_three_points(*a, *b, *c)?.0, *c)
    } else {
        let [c, e] = points else {
            return None;
        };
        (*c, *e)
    };
    (centre != edge).then_some((centre, edge))
}

/// The measurement the tool makes of `points`, as it would be created; `None` when the points
/// do not make one yet. A Volume with no `depth` is shown as its area.
pub(crate) fn build(
    state: &ToolState,
    view: &dyn DocView,
    page: usize,
    points: &[Point],
    depth: Option<f64>,
) -> Option<Annotation> {
    let o = view.origin();
    let (scale, spec) = scale_at(state, view, page, points);
    let three = three_point(state);
    let pts = points.to_vec();
    let mut a = match (state.tool(), points.len()) {
        (ToolKind::Length, 2) => Annotation::measure_length(page, points[0], points[1], spec, &o),
        (ToolKind::Polylength, n) if n >= 2 => Annotation::measure_polylength(page, pts, spec, &o),
        (ToolKind::Perimeter, n) if n >= 3 => Annotation::measure_perimeter(page, pts, spec, &o),
        (ToolKind::Area, n) if n >= 3 => Annotation::measure_area(page, pts, Vec::new(), spec, &o),
        (ToolKind::Volume, n) if n >= 3 => {
            let mut a =
                Annotation::measure_volume(page, pts, Vec::new(), depth.unwrap_or(1.0), spec, &o);
            if let Some(m) = a.measure.as_mut() {
                m.depth = depth;
            }
            a
        }
        (ToolKind::Angle, 3) => {
            Annotation::measure_angle(page, points[0], points[1], points[2], Some(spec), &o)
        }
        (ToolKind::Radius, _) => {
            let (c, e) = circle_of(points, three)?;
            Annotation::measure_radius(page, c, e, spec, &o)
        }
        (ToolKind::Diameter, _) => {
            let (c, e) = circle_of(points, three)?;
            Annotation::measure_diameter(page, c + (c - e), e, spec, &o)
        }
        _ => return None,
    };
    dress(state, view, page, &mut a, scale);
    Some(a)
}

/// True when `points` are too close together to mean anything: a line shorter than
/// [`DRAG_PX`], an outline with no area, an angle with an arm of no length.
fn degenerate(state: &ToolState, points: &[Point], px: f64) -> bool {
    let short = |a: Point, b: Point| distance(a, b) < DRAG_PX * px;
    match state.tool() {
        ToolKind::Length | ToolKind::Calibrate => points.len() != 2 || short(points[0], points[1]),
        ToolKind::Polylength => {
            points.len() < 2 || points.windows(2).all(|w| distance(w[0], w[1]) == 0.0)
        }
        ToolKind::Perimeter | ToolKind::Area | ToolKind::Volume | ToolKind::Cutout => {
            points.len() < 3 || polygon_area(points) <= 1e-9 * px * px
        }
        ToolKind::Angle => points.len() != 3 || points[1] == points[0] || points[1] == points[2],
        ToolKind::Radius | ToolKind::Diameter => match circle_of(points, three_point(state)) {
            Some((c, e)) => short(c, e),
            None => true,
        },
        _ => true,
    }
}

/// The topmost area or volume of `page` that `p` is inside (and not inside one of its holes).
fn cutout_target(view: &dyn DocView, page: usize, p: Point) -> Option<&Annotation> {
    view.annotations(page).into_iter().rev().find(|a| {
        movable(a)
            && match (a.measurement(), &a.geometry) {
                (Some(m), Geometry::Points(outer)) => {
                    matches!(m.kind, MeasureKind::Area | MeasureKind::Volume)
                        && point_in_region(p, outer, &m.cutouts)
                }
                _ => false,
            }
    })
}

/// True when `p` is inside a ring or on its edge.
fn in_ring(p: Point, ring: &[Point]) -> bool {
    let n = ring.len();
    point_in_polygon(p, ring)
        || (0..n).any(|i| point_segment_distance(p, ring[i], ring[(i + 1) % n]) <= 1e-9)
}

/// True when `p` may be a corner of a hole in `target`: inside its outline, not inside one of
/// its holes.
fn fits_target(view: &dyn DocView, target: Option<&AnnotId>, p: Point) -> bool {
    let Some(a) = target.and_then(|id| view.annotation(id)) else {
        return false;
    };
    match (a.measurement(), &a.geometry) {
        (Some(m), Geometry::Points(outer)) => {
            in_ring(p, outer) && !m.cutouts.iter().any(|c| point_in_polygon(p, c))
        }
        _ => false,
    }
}

fn select(view: &dyn DocView, ids: Vec<AnnotId>, out: &mut Vec<Intent>) {
    if view.selection() != ids.as_slice() {
        out.push(Intent::Select(ids));
    }
}

/// A name for a new viewport of `page`: `Viewport 1`, `Viewport 2`, ... whichever is free.
pub(crate) fn new_viewport_name(view: &dyn DocView, page: usize) -> String {
    let vps = view.viewports(page);
    (1..)
        .map(|i| format!("Viewport {i}"))
        .find(|n| vps.is_none_or(|v| v.get(n).is_none()))
        .unwrap_or_default()
}

/// Which scale a calibration sets: a new viewport over the dragged region; else the topmost
/// viewport holding both points; else the page's.
fn calibration_target(view: &dyn DocView, g: &Gesture) -> ScaleTarget {
    if let Some(bbox) = g.region {
        return ScaleTarget::NewViewport {
            name: new_viewport_name(view, g.page),
            bbox,
        };
    }
    view.viewports(g.page)
        .and_then(|v| {
            v.viewports
                .iter()
                .rev()
                .find(|vp| g.points.iter().all(|p| vp.contains(*p)))
        })
        .map_or(ScaleTarget::Page, |vp| {
            ScaleTarget::Viewport(vp.name.clone())
        })
}

/// The area outlined, in square metres, through the scale.
fn area_si(spec: &MeasureSpec, points: &[Point]) -> Option<f64> {
    let u = spec.metres_per_unit()?;
    Some(spec.real_area(polygon_area(points)) * u * u)
}

/// True for the tools whose value depends on a scale (not Angle, which reads the same at any
/// scale, nor Count).
fn needs_scale(tool: ToolKind) -> bool {
    matches!(
        tool,
        ToolKind::Length
            | ToolKind::Polylength
            | ToolKind::Perimeter
            | ToolKind::Area
            | ToolKind::Volume
            | ToolKind::Radius
            | ToolKind::Diameter
    )
}

/// True when the view knows the scales of `page` and none applies to all of `points`: not a
/// viewport holding them all, nor the page's own. A view that offers no scales at all (see
/// [`DocView::viewports`]) measures at full size and never misses one.
fn scale_missing(view: &dyn DocView, page: usize, points: &[Point]) -> bool {
    view.viewports(page)
        .is_some_and(|v| v.scale_for(points).1.is_none())
}

/// Ends the gesture: creates the measurement, cuts the hole, or asks for what is still needed.
fn finish(state: &mut ToolState, view: &dyn DocView, out: &mut Vec<Intent>) {
    let Phase::Measure(g) = std::mem::replace(&mut state.phase, Phase::Idle) else {
        return;
    };
    let mut g = *g;
    let px = view.pixel_size();
    if degenerate(state, &g.points, px) {
        return;
    }
    match state.tool() {
        ToolKind::Calibrate => {
            let request = MeasureRequest::Distance {
                page: g.page,
                a: g.points[0],
                b: g.points[1],
                target: calibration_target(view, &g),
            };
            g.cursor = None;
            g.waiting = Some(request.clone());
            state.phase = Phase::Measure(Box::new(g));
            out.push(Intent::RequestMeasure(request));
        }
        ToolKind::Cutout => {
            let Some(id) = g.target.clone() else {
                return;
            };
            let Some(a) = view.annotation(&id) else {
                return;
            };
            if !g.points.iter().all(|p| fits_target(view, Some(&id), *p)) {
                return;
            }
            if let (Some(m), Geometry::Points(outline)) = (a.measurement(), &a.geometry) {
                let mut m = m.clone();
                m.cutouts.push(g.points);
                // The whole hole, not only its corners, must be inside the outline and clear
                // of the other holes, or the area less the holes is not the area shown.
                if pl_geom::region_problem(outline, &m.cutouts).is_some() {
                    return;
                }
                out.push(Intent::Modify {
                    id,
                    change: AnnotChange::Measure(Box::new(m)),
                });
            }
        }
        // A value at the wrong scale is worse than none: a measurement over points no scale
        // covers (one that starts in a viewport and ends outside it, on a page with no scale
        // of its own) is not made at full size; the tool asks for a scale instead.
        tool if needs_scale(tool) && scale_missing(view, g.page, &g.points) => {
            out.push(Intent::RequestMeasure(MeasureRequest::NoScale {
                page: g.page,
            }));
        }
        ToolKind::Volume if state.preset.measure_preset().depth.is_none() => {
            let (_, spec) = scale_at(state, view, g.page, &g.points);
            let request = MeasureRequest::Depth {
                page: g.page,
                area: area_si(&spec, &g.points),
            };
            g.cursor = None;
            g.waiting = Some(request.clone());
            state.phase = Phase::Measure(Box::new(g));
            out.push(Intent::RequestMeasure(request));
        }
        _ => {
            let depth = state.preset.measure_preset().depth;
            if let Some(a) = build(state, view, g.page, &g.points, depth) {
                create(a, true, out);
            }
        }
    }
}

fn pointer_down(
    state: &mut ToolState,
    p: &Pointer,
    double: bool,
    view: &dyn DocView,
    out: &mut Vec<Intent>,
) {
    let tool = state.tool();
    let slop = CLOSE_PX * view.pixel_size();
    let want = needed(state);
    let finishes = match &state.phase {
        Phase::Measure(g) => {
            if g.page != p.page || g.waiting.is_some() {
                return;
            }
            let mut g = g.clone();
            // A double-click that came before its release: the press was a click.
            if let Some(press) = g.press.take() {
                g.points.push(press.anchor);
            }
            if g.points.is_empty() {
                if !double {
                    g.press = Some(Press::at(p));
                }
                state.phase = Phase::Measure(g);
                return;
            }
            let (at, closes) = place(state, &g, p, view);
            // A click on the last point is the first half of a double-click, or its repeat;
            // so is a click on the first point of an outline that cannot close yet.
            let repeat = g.points.last().is_some_and(|l| distance(*l, at) <= slop)
                || g.points
                    .last()
                    .is_some_and(|l| distance(*l, p.point) <= slop)
                || (is_closed(tool) && distance(g.points[0], p.point) <= slop);
            let fits = tool != ToolKind::Cutout || fits_target(view, g.target.as_ref(), at);
            if !closes && !repeat && fits {
                g.points.push(at);
            }
            g.cursor = None;
            let complete = want.is_some_and(|n| g.points.len() >= n);
            let enough = g.points.len() >= min_points(state);
            state.phase = Phase::Measure(g);
            closes || complete || (double && enough && want.is_none())
        }
        _ => {
            if double {
                return;
            }
            if tool == ToolKind::Cutout {
                if let Some(a) = cutout_target(view, p.page, p.point) {
                    let id = a.id.clone();
                    select(view, vec![id.clone()], out);
                    state.phase = Phase::Measure(Box::new(Gesture {
                        target: Some(id),
                        ..Gesture::new(p.page)
                    }));
                }
                return;
            }
            state.phase = Phase::Measure(Box::new(Gesture {
                press: Some(Press::at(p)),
                ..Gesture::new(p.page)
            }));
            false
        }
    };
    if finishes {
        finish(state, view, out);
    }
}

/// Follows the pointer: the end of a first drag, or where the next point would go.
fn track(state: &mut ToolState, p: &Pointer, view: &dyn DocView) {
    let Phase::Measure(g) = &state.phase else {
        return;
    };
    if g.page != p.page || g.waiting.is_some() || !p.is_finite() {
        return;
    }
    let mut g = g.clone();
    let kind = drag_as(state, &g);
    let preset = state.preset.measure_preset();
    let rotation = view.rotation(g.page);
    let threshold = DRAG_PX * view.pixel_size();
    if let Some(press) = g.press.as_mut() {
        press.moved |= distance(press.at, p.point) >= threshold;
        press.current = drag_point(kind, press.anchor, p, rotation, &preset);
    } else {
        g.cursor = Some(place(state, &g, p, view).0);
    }
    state.phase = Phase::Measure(g);
}

fn pointer_up(state: &mut ToolState, p: &Pointer, view: &dyn DocView, out: &mut Vec<Intent>) {
    let Phase::Measure(g) = &state.phase else {
        return;
    };
    if g.press.is_none() {
        return;
    }
    track(state, p, view);
    let Phase::Measure(g) = &state.phase else {
        return;
    };
    let mut g = g.clone();
    let Some(press) = g.press.take() else {
        return;
    };
    let tool = state.tool();
    let dragged =
        press.moved && distance(press.anchor, press.current) >= DRAG_PX * view.pixel_size();
    let mut finishes = false;
    if dragged {
        let (a, b) = (press.anchor, press.current);
        let r = Rect::from_points(a, b);
        match drag_as(state, &g) {
            DragAs::Line => {
                g.points = vec![a, b];
                finishes = true;
            }
            DragAs::Points => g.points.extend([a, b]),
            DragAs::Rect => {
                let ring = rect_ring(r);
                let fits = tool != ToolKind::Cutout
                    || ring
                        .iter()
                        .all(|c| fits_target(view, g.target.as_ref(), *c));
                if r.width() > 0.0 && r.height() > 0.0 && fits {
                    g.points = ring;
                    finishes = true;
                }
            }
            DragAs::Region if r.width() > 0.0 && r.height() > 0.0 => {
                if tool == ToolKind::Viewport {
                    let request = MeasureRequest::Scale {
                        page: g.page,
                        name: new_viewport_name(view, g.page),
                        bbox: r,
                    };
                    g.waiting = Some(request.clone());
                    out.push(Intent::RequestMeasure(request));
                } else {
                    g.region = Some(r);
                }
            }
            DragAs::Region => {}
        }
    } else if tool != ToolKind::Viewport
        && (tool != ToolKind::Cutout || fits_target(view, g.target.as_ref(), press.anchor))
    {
        g.points.push(press.anchor);
        finishes = needed(state).is_some_and(|n| g.points.len() >= n);
    }
    let idle = tool == ToolKind::Viewport && g.waiting.is_none();
    state.phase = if idle { Phase::Idle } else { Phase::Measure(g) };
    if finishes {
        finish(state, view, out);
    }
}

fn key(state: &mut ToolState, key: Key, view: &dyn DocView, out: &mut Vec<Intent>) {
    match key {
        Key::Escape => state.phase = Phase::Idle,
        Key::Enter => {
            let ready = match &state.phase {
                Phase::Measure(g) => {
                    g.waiting.is_none()
                        && g.press.is_none()
                        && needed(state).is_none()
                        && g.points.len() >= min_points(state)
                }
                _ => false,
            };
            if ready {
                finish(state, view, out);
            }
        }
        Key::Backspace => {
            let Phase::Measure(g) = &mut state.phase else {
                return;
            };
            if g.waiting.is_some() || g.press.is_some() {
                return;
            }
            g.cursor = None;
            let mut idle = false;
            if g.points.pop().is_none() && g.region.take().is_none() {
                idle = true;
            }
            if g.points.is_empty() && g.region.is_none() && g.target.is_none() {
                idle = true;
            }
            if idle {
                state.phase = Phase::Idle;
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
    if state.tool() == ToolKind::Count {
        return count_step(state, input, view, out);
    }
    match input {
        ToolInput::PointerDown(p) | ToolInput::DoubleClick(p) if usable(view, p) => {
            pointer_down(
                state,
                p,
                matches!(input, ToolInput::DoubleClick(_)),
                view,
                out,
            );
        }
        ToolInput::PointerMove(p) if usable(view, p) => match state.phase {
            Phase::Measure(_) => track(state, p, view),
            _ => out.push(Intent::SetCursor(Cursor::Crosshair)),
        },
        ToolInput::PointerUp(p) => pointer_up(state, p, view, out),
        ToolInput::Key { key: k, .. } => key(state, *k, view, out),
        _ => {}
    }
}

/// Gives a measurement tool the answer to its [`Intent::RequestMeasure`].
pub(crate) fn answer(
    state: &mut ToolState,
    input: &MeasureInput,
    view: &dyn DocView,
    out: &mut Vec<Intent>,
) {
    let Phase::Measure(g) = &state.phase else {
        return;
    };
    let Some(request) = g.waiting.clone() else {
        return;
    };
    let points = g.points.clone();
    let preset = state.preset.measure_preset();
    match (request, *input) {
        (_, MeasureInput::Cancel) => state.phase = Phase::Idle,
        (
            MeasureRequest::Distance { page, a, b, target },
            MeasureInput::Distance { metres, unit },
        ) => {
            let uu = user_unit(view, page);
            // A scale that cannot be made (no distance, no length) leaves the tool waiting.
            let Ok(scale) = Scale::calibrate(a, b, metres, uu, unit) else {
                return;
            };
            let mut format = preset.format_for(&scale);
            if preset.length.is_none() && unit == Unit::FeetInches {
                format.length = LengthFormat::architectural(16);
            }
            let spec = MeasureSpec::new(&scale, uu, &format);
            out.push(Intent::SetScale(Box::new(ScaleChange {
                page,
                target,
                scale,
                spec,
            })));
            state.phase = Phase::Idle;
        }
        (MeasureRequest::Scale { page, name, bbox }, MeasureInput::Scale(scale)) => {
            let uu = user_unit(view, page);
            let spec = MeasureSpec::new(&scale, uu, &preset.format_for(&scale));
            out.push(Intent::SetScale(Box::new(ScaleChange {
                page,
                target: ScaleTarget::NewViewport { name, bbox },
                scale,
                spec,
            })));
            state.phase = Phase::Idle;
        }
        (MeasureRequest::Depth { page, .. }, MeasureInput::Depth(depth))
            if depth.is_finite() && depth > 0.0 =>
        {
            state.phase = Phase::Idle;
            if let Some(a) = build(state, view, page, &points, Some(depth)) {
                create(a, true, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Count
// ---------------------------------------------------------------------------------------

fn is_count(a: &Annotation) -> bool {
    a.measurement()
        .is_some_and(|m| m.kind == MeasureKind::Count)
}

/// The count being made: the one selected count on `page`.
pub(crate) fn active_count(view: &dyn DocView, page: usize) -> Option<&Annotation> {
    match view.selection() {
        [only] => view
            .annotation(only)
            .filter(|a| a.page == page && is_count(a) && movable(a)),
        _ => None,
    }
}

/// The marker of a count under `p`: the nearest within its own size plus [`HIT_PX`].
fn marker_at(a: &Annotation, p: Point, px: f64) -> Option<usize> {
    let size = a.measurement()?.symbol_size;
    a.count_points()
        .iter()
        .enumerate()
        .map(|(i, c)| (distance(*c, p), i))
        .filter(|(d, _)| *d <= size / 2.0 + HIT_PX * px)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i)| i)
}

fn count_step(state: &mut ToolState, input: &ToolInput, view: &dyn DocView, out: &mut Vec<Intent>) {
    match input {
        ToolInput::PointerDown(p) if usable(view, p) => {
            if let Phase::Measure(g) = &state.phase {
                if g.page != p.page {
                    return;
                }
            }
            let px = view.pixel_size();
            let active = active_count(view, p.page);
            state.phase = Phase::Measure(Box::new(Gesture::new(p.page)));
            if p.modifiers.alt {
                let on_page = view
                    .annotations(p.page)
                    .into_iter()
                    .rev()
                    .filter(|a| is_count(a) && movable(a));
                for c in active.into_iter().chain(on_page) {
                    let Some(i) = marker_at(c, p.point, px) else {
                        continue;
                    };
                    let Geometry::Ink(strokes) = &c.geometry else {
                        continue;
                    };
                    if strokes.len() <= 1 {
                        out.push(Intent::Delete(vec![c.id.clone()]));
                    } else {
                        let mut strokes = strokes.clone();
                        strokes.remove(i);
                        out.push(Intent::Modify {
                            id: c.id.clone(),
                            change: AnnotChange::Geometry(Geometry::Ink(strokes)),
                        });
                    }
                    return;
                }
            }
            let at = p.effective();
            match active {
                Some(c) => {
                    if let Geometry::Ink(strokes) = &c.geometry {
                        let mut strokes = strokes.clone();
                        strokes.push(vec![at, at]);
                        out.push(Intent::Modify {
                            id: c.id.clone(),
                            change: AnnotChange::Geometry(Geometry::Ink(strokes)),
                        });
                    }
                }
                None => {
                    let preset = state.preset.measure_preset();
                    let mut a = Annotation::count(p.page, &[at], preset.symbol, &view.origin());
                    dress(state, view, p.page, &mut a, ScaleRef::Page);
                    create(a, true, out);
                }
            }
        }
        ToolInput::PointerMove(p) if usable(view, p) => {
            out.push(Intent::SetCursor(Cursor::Crosshair));
        }
        ToolInput::Key {
            key: Key::Enter | Key::Escape,
            ..
        } => {
            if let Phase::Measure(g) = &state.phase {
                if active_count(view, g.page).is_some() {
                    out.push(Intent::Select(Vec::new()));
                }
            }
            state.phase = Phase::Idle;
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
// Read-out and preview
// ---------------------------------------------------------------------------------------

/// The points of the measurement as it stands: those placed and the pointer, or the shape of
/// a first drag.
fn live_points(state: &ToolState, g: &Gesture) -> Vec<Point> {
    if let Some(press) = g.press.filter(|p| p.moved && p.anchor != p.current) {
        return match drag_as(state, g) {
            DragAs::Rect | DragAs::Region => {
                rect_ring(Rect::from_points(press.anchor, press.current))
            }
            DragAs::Line | DragAs::Points => {
                let mut pts = g.points.clone();
                pts.extend([press.anchor, press.current]);
                pts
            }
        };
    }
    let mut pts = g.points.clone();
    if let Some(c) = g.cursor.filter(|_| g.waiting.is_none()) {
        if pts.last() != Some(&c) && !(is_closed(state.tool()) && pts.first() == Some(&c)) {
            pts.push(c);
        }
    }
    pts
}

fn distance_value(spec: &MeasureSpec, units: f64) -> Value {
    Value {
        si: spec.metres_per_unit().map(|u| units * u),
        text: spec.format_distance(units),
    }
}

fn angle_value(spec: &MeasureSpec, radians: f64) -> Value {
    Value {
        si: Some(radians),
        text: spec.format_angle(radians.to_degrees()),
    }
}

pub(crate) fn readout(state: &ToolState, view: &dyn DocView) -> Option<Readout> {
    let tool = state.tool();
    if !tool.is_measure() {
        return None;
    }
    let preset = state.preset.measure_preset();
    if tool == ToolKind::Count {
        let page = match &state.phase {
            Phase::Measure(g) => g.page,
            _ => {
                view.selection()
                    .first()
                    .and_then(|id| view.annotation(id))?
                    .page
            }
        };
        let count = active_count(view, page)?;
        return Some(Readout {
            tool,
            page,
            scale: ScaleRef::Page,
            ratio: String::new(),
            segment: None,
            total: None,
            area: None,
            volume: None,
            angle: None,
            direction: None,
            radius: None,
            count: Some(count.count_points().len()),
        });
    }
    let Phase::Measure(g) = &state.phase else {
        return None;
    };
    let pts = live_points(state, g);
    let (scale, spec) = scale_at(state, view, g.page, &pts);
    let mut r = Readout {
        tool,
        page: g.page,
        scale,
        ratio: spec.ratio.clone(),
        segment: None,
        total: None,
        area: None,
        volume: None,
        angle: None,
        direction: None,
        radius: None,
        count: None,
    };
    let rect_drag = g.press.is_some_and(|p| p.moved)
        && matches!(drag_as(state, g), DragAs::Rect | DragAs::Region);
    let n = pts.len();
    if n >= 2 && !rect_drag {
        let (a, b) = (pts[n - 2], pts[n - 1]);
        r.segment = Some(distance_value(&spec, spec.distance_between(a, b)));
        let d = to_display(view.rotation(g.page), b - a);
        if d.x != 0.0 || d.y != 0.0 {
            let degrees = (-d.y).atan2(d.x).to_degrees().rem_euclid(360.0);
            r.direction = Some(Value {
                si: Some(degrees.to_radians()),
                text: spec.format_angle(degrees),
            });
        }
    }
    let run: f64 = pts
        .windows(2)
        .map(|w| spec.distance_between(w[0], w[1]))
        .sum();
    match tool {
        ToolKind::Length | ToolKind::Calibrate | ToolKind::Polylength | ToolKind::Angle => {
            if n >= 2 {
                r.total = Some(distance_value(&spec, run));
            }
            if tool == ToolKind::Angle && n == 3 {
                let (u, v) = (pts[0] - pts[1], pts[2] - pts[1]);
                if u.hypot2() > 0.0 && v.hypot2() > 0.0 {
                    r.angle = Some(angle_value(&spec, u.cross(v).abs().atan2(u.dot(v))));
                }
            }
        }
        ToolKind::Perimeter | ToolKind::Area | ToolKind::Volume | ToolKind::Cutout => {
            if n >= 3 {
                let closing = spec.distance_between(pts[n - 1], pts[0]);
                r.total = Some(distance_value(&spec, run + closing));
                let units = spec.real_area(polygon_area(&pts));
                let si = area_si(&spec, &pts);
                r.area = Some(Value {
                    si,
                    text: spec.format_area(units),
                });
                if let (ToolKind::Volume, Some(depth), Some(area)) = (tool, preset.depth, si) {
                    let format = preset.volume.unwrap_or_else(|| {
                        VolumeFormat::new(
                            if spec.is_imperial() {
                                VolumeUnit::CuYard
                            } else {
                                VolumeUnit::CuMetre
                            },
                            2,
                        )
                    });
                    r.volume = Some(Value {
                        si: Some(area * depth),
                        text: format.format(area * depth),
                    });
                }
            } else if n == 2 {
                r.total = Some(distance_value(&spec, run));
            }
        }
        ToolKind::Radius | ToolKind::Diameter => {
            if let Some((c, e)) = circle_of(&pts, three_point(state)) {
                let radius = spec.distance_between(c, e);
                r.radius = Some(distance_value(&spec, radius));
                let across = if tool == ToolKind::Diameter {
                    2.0 * radius
                } else {
                    radius
                };
                r.total = Some(distance_value(&spec, across));
            }
        }
        _ => {}
    }
    Some(r)
}

fn guide(px: f64) -> Stroke {
    Stroke::new(px, GUIDE)
}

pub(crate) fn preview(state: &ToolState, view: &dyn DocView) -> DrawList {
    let mut list = DrawList::new();
    let Phase::Measure(g) = &state.phase else {
        return list;
    };
    let tool = state.tool();
    if tool == ToolKind::Count {
        return list;
    }
    let px = view.pixel_size();
    let dashed = ShapeStyle::stroked(guide(px).with_dash(Dash::even(4.0 * px)));
    if let Some(r) = g.region {
        list.push_shape(rect_path(r), &dashed);
    }
    let pts = live_points(state, g);
    let region_drag = g.press.is_some_and(|p| p.moved) && drag_as(state, g) == DragAs::Region;
    if region_drag || tool == ToolKind::Viewport {
        if let (true, Some(r)) = (pts.len() == 4, pts.first().zip(pts.get(2))) {
            list.push_shape(rect_path(Rect::from_points(*r.0, *r.1)), &dashed);
        }
        if let Some(MeasureRequest::Scale { bbox, .. }) = &g.waiting {
            list.push_shape(rect_path(*bbox), &dashed);
        }
        return list;
    }
    let line = |list: &mut DrawList, pts: &[Point]| {
        if pts.len() >= 2 {
            list.push_shape(polyline_path(pts), &ShapeStyle::stroked(guide(px)));
        }
    };
    match tool {
        ToolKind::Calibrate => line(&mut list, &pts),
        ToolKind::Cutout => {
            let target = g.target.as_ref().and_then(|id| view.annotation(id));
            match target {
                Some(a) if pts.len() >= 3 => {
                    let mut b = a.detached();
                    if let Some(m) = b.measure.as_mut() {
                        m.cutouts.push(pts);
                    }
                    b.sync_measurement();
                    list.extend(b.draw_list().ops);
                }
                _ => line(&mut list, &pts),
            }
        }
        _ => {
            let depth = state.preset.measure_preset().depth;
            match build(state, view, g.page, &pts, depth) {
                Some(a) => {
                    list.extend(a.draw_list().ops);
                }
                None => line(&mut list, &pts),
            }
        }
    }
    list
}
