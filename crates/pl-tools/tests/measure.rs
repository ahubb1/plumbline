//! Scripted sequences for the measurement tools, with exact golden geometry and values, on all
//! eight page layouts. Scripts are in screen pixels (see `common`): at 100% zoom 72 screen
//! pixels are one inch of paper on every layout (the `/UserUnit 2` page included), so at the
//! scale used here, 1 in = 10 ft, 72 pixels measure ten feet whatever the page's rotation, box
//! or user unit.

mod common;

use common::*;
use pl_annot::{
    AnnotKind, Annotation, CountSymbol, Geometry, LineEnding, MeasureFormat, MeasureKind,
    MeasureSpec, PageViewports, ScaleRef,
};
use pl_geom::{
    polygon_area, LengthFormat, Point, Precision, Scale, Unit, VolumeFormat, VolumeUnit,
};
use pl_tools::{
    measure_handles, selection_measure_handles, step_measure, Intent, Key, MeasureGrip,
    MeasureInput, MeasurePreset, MeasureRequest, ScaleTarget, SnapKind, SnapKinds, ToolInput,
    ToolKind, ToolPreset, ToolState,
};

const FOOT: f64 = 0.3048;

/// 1 in = 10 ft: one inch of paper is 3.048 metres.
fn ten_feet() -> Scale {
    Scale::engineering(10.0).unwrap()
}

fn spec(l: &Layout, scale: &Scale) -> MeasureSpec {
    MeasureSpec::new(
        scale,
        l.geometry.user_unit(),
        &MeasureFormat::for_scale(scale),
    )
}

/// A rig whose page has the scale 1 in = 10 ft.
fn scaled(l: &Layout, tool: ToolKind) -> Rig {
    let mut rig = Rig::new(l, tool);
    let mut vps = PageViewports::new(PAGE, l.geometry.media_box());
    vps.page_scale = Some(spec(l, &ten_feet()));
    rig.view.viewports.push(vps);
    rig
}

#[test]
fn a_measurement_no_scale_covers_asks_for_one_instead_of_full_size() {
    for l in layouts() {
        for kind in [ToolKind::Length, ToolKind::Area] {
            // No page scale; one viewport at 1 in = 10 ft round screen (90..250, 90..250).
            let mut rig = Rig::new(&l, kind);
            let mut vps = PageViewports::new(PAGE, l.geometry.media_box());
            let corners = [l.at(90.0, 90.0), l.at(250.0, 250.0)];
            let bbox = pl_geom::Rect::from_points(corners[0], corners[1]);
            vps.add(pl_annot::Viewport::new(
                "V",
                bbox,
                Some(spec(&l, &ten_feet())),
            ))
            .unwrap();
            rig.view.viewports.push(vps);
            let pts: &[(f64, f64)] = if kind == ToolKind::Length {
                &[(100.0, 100.0), (172.0, 100.0)]
            } else {
                &SQUARE
            };
            click_all(&mut rig, pts);
            if kind == ToolKind::Area {
                rig.key(Key::Enter, NONE);
            }
            assert_eq!(rig.view.annotations.len(), 1, "{} {kind:?}", l.name);
            // Ending outside the viewport.
            let pts: &[(f64, f64)] = if kind == ToolKind::Length {
                &[(100.0, 100.0), (372.0, 100.0)]
            } else {
                &[(100.0, 100.0), (344.0, 100.0), (344.0, 244.0), (100.0, 244.0)]
            };
            let mut out = click_all(&mut rig, pts);
            if kind == ToolKind::Area {
                out.extend(rig.key(Key::Enter, NONE));
            }
            assert_eq!(rig.view.annotations.len(), 1, "{} {kind:?}", l.name);
            assert!(
                out.contains(&Intent::RequestMeasure(MeasureRequest::NoScale { page: PAGE })),
                "{} {kind:?}: {out:?}",
                l.name
            );
            assert!(!rig.state.is_busy());
        }
    }
}

fn tool(rig: &mut Rig, kind: ToolKind) {
    rig.state = ToolState::for_tool(kind);
}

fn with_preset(rig: &mut Rig, kind: ToolKind, f: impl FnOnce(&mut MeasurePreset)) {
    let mut preset = ToolPreset::default_for(kind);
    let mut m = preset
        .measure
        .expect("a measurement tool has measurement settings");
    f(&mut m);
    preset.measure = Some(m);
    rig.state = ToolState::new(preset);
}

#[track_caller]
fn close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() <= 1e-9 * want.abs().max(1.0),
        "{what}: got {got}, want {want}"
    );
}

fn screen_points(l: &Layout, pts: &[(f64, f64)]) -> Vec<Point> {
    pts.iter().map(|(x, y)| l.at(*x, *y)).collect()
}

#[track_caller]
fn assert_points(got: &[Point], want: &[Point], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: {got:?} against {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert_point(*g, *w, what);
    }
}

fn points_of(a: &Annotation) -> Vec<Point> {
    match &a.geometry {
        Geometry::Points(p) => p.clone(),
        other => panic!("not points: {other:?}"),
    }
}

fn line_of(a: &Annotation) -> (Point, Point) {
    match a.geometry {
        Geometry::Line { a, b } => (a, b),
        ref other => panic!("not a line: {other:?}"),
    }
}

fn contents(a: &Annotation) -> &str {
    a.meta.contents.as_deref().unwrap_or("")
}

/// Clicks every screen point in turn.
fn click_all(rig: &mut Rig, pts: &[(f64, f64)]) -> Vec<Intent> {
    let mut out = Vec::new();
    for (x, y) in pts {
        out.extend(rig.click(*x, *y, NONE));
    }
    out
}

/// The 2 x 2 inch square (20 ft x 20 ft) every area script outlines.
const SQUARE: [(f64, f64); 4] = [
    (100.0, 100.0),
    (244.0, 100.0),
    (244.0, 244.0),
    (100.0, 244.0),
];

// ---------------------------------------------------------------------------------------
// Length, Polylength, shift constraints, read-out
// ---------------------------------------------------------------------------------------

#[test]
fn length_by_two_clicks_or_a_drag() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Length);
        assert!(rig.click(100.0, 100.0, NONE).is_empty());
        assert!(rig.state.is_busy());
        let a = created(&rig.click(172.0, 100.0, NONE));
        assert_eq!(a.kind, AnnotKind::Line);
        assert_eq!(
            a.geometry,
            Geometry::Line {
                a: l.at(100.0, 100.0),
                b: l.at(172.0, 100.0)
            },
            "{}",
            l.name
        );
        let m = a.measurement().expect("a measurement");
        assert_eq!(m.kind, MeasureKind::Length);
        assert_eq!(m.scale, ScaleRef::Page);
        assert_eq!(m.spec.as_ref(), Some(&spec(&l, &ten_feet())));
        assert_eq!(m.rotation as i32, l.rotation().degrees(), "{}", l.name);
        // Text sizes are points on screen: 9 points on a /UserUnit 2 page is 4.5 units.
        close(m.text_size, 9.0 * l.px, l.name);
        assert_eq!(contents(&a), "10.00'", "{}", l.name);
        close(a.quantities().unwrap().length.unwrap(), 10.0 * FOOT, l.name);
        assert_eq!(a.meta.subject.as_deref(), Some("Length"));
        assert_eq!(
            (a.style.line_start, a.style.line_end),
            (LineEnding::ClosedArrow, LineEnding::ClosedArrow)
        );
        assert_eq!(rig.selection(), std::slice::from_ref(&a.id));
        assert!(!rig.state.is_busy() && rig.preview.is_empty());

        // A drag measures from the press to the release: two inches down the screen.
        let b = created(&rig.drag((300.0, 100.0), (300.0, 244.0), NONE));
        assert_eq!(
            b.geometry,
            Geometry::Line {
                a: l.at(300.0, 100.0),
                b: l.at(300.0, 244.0)
            }
        );
        assert_eq!(contents(&b), "20.00'", "{}", l.name);
    }
}

#[test]
fn shift_keeps_segments_to_the_screen_axes_and_diagonals() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Length);
        rig.click(100.0, 100.0, NONE);
        // 5 pixels off the horizontal: Shift puts it back on it.
        let a = created(&rig.click(172.0, 105.0, SHIFT));
        let (p, q) = line_of(&a);
        assert_point(p, l.at(100.0, 100.0), l.name);
        assert_point(q, l.at(172.0, 100.0), l.name);
        assert_eq!(contents(&a), "10.00'", "{}", l.name);

        // Near the diagonal: projected onto it, (60, -40) -> (50, -50).
        rig.click(100.0, 300.0, NONE);
        let a = created(&rig.click(160.0, 260.0, SHIFT));
        assert_point(line_of(&a).1, l.at(150.0, 250.0), l.name);

        // Ortho only (90 degree steps): the larger screen offset wins, never a diagonal.
        with_preset(&mut rig, ToolKind::Length, |m| m.angle_step = 90.0);
        rig.click(300.0, 300.0, NONE);
        let a = created(&rig.click(350.0, 260.0, SHIFT));
        assert_point(line_of(&a).1, l.at(350.0, 300.0), l.name);

        // A Shift drag is constrained the same way.
        tool(&mut rig, ToolKind::Length);
        let a = created(&rig.drag((100.0, 400.0), (103.0, 328.0), SHIFT));
        assert_point(line_of(&a).1, l.at(100.0, 328.0), l.name);
        assert_eq!(contents(&a), "10.00'", "{}", l.name);
    }
}

#[test]
fn polylength_with_backspace_and_both_ways_of_finishing() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Polylength);
        click_all(&mut rig, &[(100.0, 100.0), (172.0, 100.0), (172.0, 172.0)]);
        // Backspace takes back the last point.
        rig.key(Key::Backspace, NONE);
        rig.click(172.0, 244.0, NONE);
        let a = created(&rig.key(Key::Enter, NONE));
        assert_eq!(
            a.geometry,
            Geometry::Points(screen_points(
                &l,
                &[(100.0, 100.0), (172.0, 100.0), (172.0, 244.0)]
            ))
        );
        assert_eq!(a.measurement().unwrap().kind, MeasureKind::Polylength);
        // One inch across and two down: 30 feet.
        assert_eq!(contents(&a), "30.00'", "{}", l.name);

        // A double-click on the last point finishes, whether or not its click came first.
        click_all(&mut rig, &[(300.0, 100.0), (336.0, 100.0)]);
        rig.click(336.0, 136.0, NONE);
        let b = created(&rig.double(336.0, 136.0));
        assert_eq!(points_of(&b).len(), 3);
        assert_eq!(contents(&b), "10.00'", "{}", l.name);
        click_all(&mut rig, &[(300.0, 300.0), (372.0, 300.0)]);
        let c = created(&rig.double(372.0, 336.0));
        assert_eq!(
            points_of(&c),
            screen_points(&l, &[(300.0, 300.0), (372.0, 300.0), (372.0, 336.0)])
        );
        assert_eq!(contents(&c), "15.00'", "{}", l.name);

        // Enter with one point, or Esc, makes nothing.
        rig.click(400.0, 400.0, NONE);
        assert!(rig.key(Key::Enter, NONE).is_empty());
        assert!(rig.state.is_busy());
        rig.key(Key::Escape, NONE);
        assert!(!rig.state.is_busy());
    }
}

#[test]
fn the_read_out_follows_the_pointer_through_the_scale() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Polylength);
        assert_eq!(rig.state.readout(&rig.view), None, "nothing in progress");
        click_all(&mut rig, &[(100.0, 100.0), (172.0, 100.0)]);
        rig.mv(172.0, 136.0, NONE);
        let r = rig.state.readout(&rig.view).expect("a read-out");
        assert_eq!((r.tool, r.page), (ToolKind::Polylength, PAGE));
        assert_eq!(r.scale, ScaleRef::Page);
        assert_eq!(r.ratio, "1 in = 10 ft");
        let segment = r.segment.unwrap();
        assert_eq!(segment.text, "5.00'", "{}", l.name);
        close(segment.si.unwrap(), 5.0 * FOOT, l.name);
        let total = r.total.unwrap();
        assert_eq!(total.text, "15.00'", "{}", l.name);
        close(total.si.unwrap(), 15.0 * FOOT, l.name);
        // Straight down the screen, whatever the page's rotation.
        let direction = r.direction.unwrap();
        assert_eq!(direction.text, "270.0\u{b0}", "{}", l.name);
        assert_eq!((r.area, r.angle, r.count), (None, None, None));
        // The preview is the measurement as it would be made.
        assert!(!rig.preview.is_empty());

        // An area so far, closing edge included.
        let mut rig = scaled(&l, ToolKind::Area);
        click_all(&mut rig, &[(100.0, 100.0), (244.0, 100.0)]);
        rig.mv(244.0, 244.0, NONE);
        let r = rig.state.readout(&rig.view).unwrap();
        assert_eq!(r.area.as_ref().unwrap().text, "200.00 sq ft", "{}", l.name);
        close(r.area.unwrap().si.unwrap(), 200.0 * FOOT * FOOT, l.name);
        // 20 + 20 + the 28.28 ft diagonal back to the start.
        close(
            r.total.unwrap().si.unwrap(),
            (40.0 + 20.0 * 2f64.sqrt()) * FOOT,
            l.name,
        );
        assert_eq!(r.segment.unwrap().text, "20.00'");
        // A dragged rectangle reads as its area.
        let mut rig = scaled(&l, ToolKind::Area);
        rig.down(100.0, 100.0, NONE);
        rig.mv(172.0, 172.0, NONE);
        let r = rig.state.readout(&rig.view).unwrap();
        assert_eq!(r.area.unwrap().text, "100.00 sq ft", "{}", l.name);
        assert_eq!(r.total.unwrap().text, "40.00'");
        assert_eq!(r.segment, None);
    }
}

// ---------------------------------------------------------------------------------------
// Perimeter, Area, Volume, Cutout
// ---------------------------------------------------------------------------------------

#[test]
fn area_and_perimeter_by_clicks_or_a_rectangle() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Area);
        click_all(&mut rig, &SQUARE);
        // Clicking the first point closes the outline.
        let a = created(&rig.click(101.0, 101.0, NONE));
        assert_eq!(a.kind, AnnotKind::Polygon);
        assert_eq!(a.geometry, Geometry::Points(screen_points(&l, &SQUARE)));
        let m = a.measurement().unwrap();
        assert_eq!(m.kind, MeasureKind::Area);
        assert!(m.cutouts.is_empty());
        assert_eq!(contents(&a), "400.00 sq ft", "{}", l.name);
        close(
            a.quantities().unwrap().area.unwrap(),
            400.0 * FOOT * FOOT,
            l.name,
        );

        // The rectangle variant: a drag on the first point. The outline runs
        // counter-clockwise in page space from the rectangle's lower-left corner.
        let b = created(&rig.drag((300.0, 100.0), (372.0, 244.0), NONE));
        let r = l.rect(300.0, 100.0, 372.0, 244.0);
        assert_eq!(
            b.geometry,
            Geometry::Points(vec![
                Point::new(r.x0, r.y0),
                Point::new(r.x1, r.y0),
                Point::new(r.x1, r.y1),
                Point::new(r.x0, r.y1),
            ])
        );
        assert_eq!(contents(&b), "200.00 sq ft", "{}", l.name);
        // Shift makes it square on screen.
        let c = created(&rig.drag((100.0, 300.0), (172.0, 330.0), SHIFT));
        assert_eq!(contents(&c), "100.00 sq ft", "{}", l.name);

        let mut rig = scaled(&l, ToolKind::Perimeter);
        let p = created(&rig.drag((100.0, 100.0), (172.0, 244.0), NONE));
        assert_eq!(p.measurement().unwrap().kind, MeasureKind::Perimeter);
        assert_eq!(contents(&p), "60.00'", "{}", l.name);
        click_all(&mut rig, &SQUARE);
        let q = created(&rig.key(Key::Enter, NONE));
        assert_eq!(contents(&q), "80.00'", "{}", l.name);
    }
}

#[test]
fn volume_takes_its_depth_from_the_preset_or_asks_for_it() {
    for l in layouts() {
        // From the preset: one foot deep. 400 sq ft x 1 ft = 400 cu ft = 14.81 cu yd.
        let mut rig = scaled(&l, ToolKind::Volume);
        with_preset(&mut rig, ToolKind::Volume, |m| m.depth = Some(FOOT));
        let a = created(&rig.drag((100.0, 100.0), (244.0, 244.0), NONE));
        let m = a.measurement().unwrap();
        assert_eq!((m.kind, m.depth), (MeasureKind::Volume, Some(FOOT)));
        close(
            a.quantities().unwrap().volume.unwrap(),
            400.0 * FOOT.powi(3),
            l.name,
        );
        assert_eq!(contents(&a), "14.81 cu yd", "{}", l.name);

        // Without one the tool asks, and creates nothing until it is told.
        let mut rig = scaled(&l, ToolKind::Volume);
        click_all(&mut rig, &SQUARE);
        let asked = rig.key(Key::Enter, NONE);
        let Some(Intent::RequestMeasure(MeasureRequest::Depth { page, area })) = asked.first()
        else {
            panic!("expected a request for the depth, got {asked:?}");
        };
        assert_eq!(*page, PAGE);
        close(area.unwrap(), 400.0 * FOOT * FOOT, l.name);
        assert!(rig.state.measure_request().is_some());
        // While it waits it ignores the pointer, and a wrong kind of answer.
        assert!(rig.click(400.0, 400.0, NONE).is_empty());
        let (state, intents, _) = step_measure(
            rig.state.clone(),
            &MeasureInput::Scale(ten_feet()),
            &rig.view,
        );
        assert_eq!(intents, []);
        assert_eq!(state, rig.state);
        // A depth that is not above zero is no answer either.
        let (state, intents, _) =
            step_measure(rig.state.clone(), &MeasureInput::Depth(0.0), &rig.view);
        assert!(intents.is_empty() && state.measure_request().is_some());
        // Half a foot: 200 cu ft, in the preset's unit.
        rig.state = rig.state.clone().with_preset({
            let mut p = ToolPreset::default_for(ToolKind::Volume);
            p.measure.as_mut().unwrap().volume = Some(VolumeFormat::new(VolumeUnit::CuFoot, 1));
            p
        });
        let (state, intents, _) = step_measure(
            rig.state.clone(),
            &MeasureInput::Depth(FOOT / 2.0),
            &rig.view,
        );
        rig.view.apply(&intents);
        rig.state = state;
        let v = created(&intents);
        assert_eq!(v.geometry, Geometry::Points(screen_points(&l, &SQUARE)));
        assert_eq!(contents(&v), "200.0 cu ft", "{}", l.name);
        assert!(!rig.state.is_busy());

        // Cancel abandons it.
        click_all(&mut rig, &SQUARE);
        rig.key(Key::Enter, NONE);
        let (state, intents, _) = step_measure(rig.state.clone(), &MeasureInput::Cancel, &rig.view);
        assert!(intents.is_empty() && !state.is_busy());
    }
}

#[test]
fn a_cutout_attaches_a_hole_to_the_area_clicked() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Area);
        click_all(&mut rig, &SQUARE);
        let area = created(&rig.key(Key::Enter, NONE));
        tool(&mut rig, ToolKind::Cutout);
        rig.view.selection.clear();

        // A click outside every area does nothing; one inside picks the area.
        assert!(rig.click(400.0, 400.0, NONE).is_empty());
        assert!(!rig.state.is_busy());
        let picked = rig.click(172.0, 172.0, NONE);
        assert_eq!(picked, [Intent::Select(vec![area.id.clone()])]);
        // The hole: one inch square in the middle. A corner outside the outline is ignored.
        let hole = [
            (136.0, 136.0),
            (208.0, 136.0),
            (208.0, 208.0),
            (136.0, 208.0),
        ];
        rig.click(136.0, 136.0, NONE);
        rig.click(300.0, 136.0, NONE);
        click_all(&mut rig, &hole[1..]);
        let done = rig.click(136.0, 136.0, NONE);
        let [Intent::Modify { id, .. }] = done.as_slice() else {
            panic!("expected one change, got {done:?}");
        };
        assert_eq!(*id, area.id);
        let a = rig.get(&area.id);
        let m = a.measurement().unwrap();
        assert_eq!(m.cutouts.len(), 1);
        assert_points(&m.cutouts[0], &screen_points(&l, &hole), l.name);
        // 400 sq ft less 100.
        assert_eq!(contents(a), "300.00 sq ft", "{}", l.name);
        close(
            a.quantities().unwrap().area.unwrap(),
            300.0 * FOOT * FOOT,
            l.name,
        );

        // A dragged rectangle is a hole too; a click inside the first hole does not pick the
        // area.
        assert!(rig.click(172.0, 172.0, NONE).is_empty());
        assert!(!rig.state.is_busy());
        rig.click(110.0, 110.0, NONE);
        rig.drag((110.0, 110.0), (128.0, 236.0), NONE);
        let a = rig.get(&area.id);
        assert_eq!(a.measurement().unwrap().cutouts.len(), 2);
        // 18 x 126 pixels: 2268 of the 5184 square pixels in a square inch.
        let want = 300.0 - 2268.0 / 5184.0 * 100.0;
        close(
            a.quantities().unwrap().area.unwrap(),
            want * FOOT * FOOT,
            l.name,
        );

        // A hole round the first one has no corner inside it, but takes the same area off
        // again: it is refused, and so is one that overlaps it.
        rig.click(230.0, 230.0, NONE);
        let around = rig.drag((130.0, 130.0), (214.0, 214.0), NONE);
        assert!(
            !around.iter().any(|i| matches!(i, Intent::Modify { .. })),
            "{}: {around:?}",
            l.name
        );
        rig.click(230.0, 230.0, NONE);
        let across = rig.drag((200.0, 200.0), (236.0, 236.0), NONE);
        assert!(
            !across.iter().any(|i| matches!(i, Intent::Modify { .. })),
            "{}: {across:?}",
            l.name
        );
        assert_eq!(
            rig.get(&area.id).measurement().unwrap().cutouts.len(),
            2,
            "{}",
            l.name
        );
    }
}

#[test]
fn a_cutout_whose_edge_leaves_an_l_shaped_area_is_refused() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Area);
        // An L: 2 x 2 inches with one inch square taken out of a corner.
        click_all(
            &mut rig,
            &[
                (100.0, 100.0),
                (172.0, 100.0),
                (172.0, 172.0),
                (244.0, 172.0),
                (244.0, 244.0),
                (100.0, 244.0),
            ],
        );
        let area = created(&rig.key(Key::Enter, NONE));
        tool(&mut rig, ToolKind::Cutout);
        rig.view.selection.clear();
        assert!(!rig.click(120.0, 220.0, NONE).is_empty());
        // Every corner is inside the L, but the long edge crosses the notch.
        let done = click_all(
            &mut rig,
            &[
                (110.0, 110.0),
                (110.0, 234.0),
                (234.0, 180.0),
                (110.0, 110.0),
            ],
        );
        assert!(
            !done.iter().any(|i| matches!(i, Intent::Modify { .. })),
            "{}: {done:?}",
            l.name
        );
        assert!(rig.get(&area.id).measurement().unwrap().cutouts.is_empty());
        // The same triangle kept inside the L's arms is a hole.
        rig.click(120.0, 220.0, NONE);
        let done = click_all(
            &mut rig,
            &[
                (110.0, 110.0),
                (110.0, 234.0),
                (234.0, 180.0),
                (180.0, 180.0),
                (110.0, 110.0),
            ],
        );
        assert!(
            done.iter().any(|i| matches!(i, Intent::Modify { .. })),
            "{}: {done:?}",
            l.name
        );
    }
}

// ---------------------------------------------------------------------------------------
// Angle, Radius, Diameter
// ---------------------------------------------------------------------------------------

#[test]
fn angle_from_three_clicks() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Angle);
        click_all(&mut rig, &[(172.0, 100.0), (100.0, 100.0)]);
        rig.mv(100.0, 28.0, NONE);
        let r = rig.state.readout(&rig.view).unwrap();
        assert_eq!(r.angle.as_ref().unwrap().text, "90.0\u{b0}", "{}", l.name);
        close(
            r.angle.unwrap().si.unwrap(),
            std::f64::consts::FRAC_PI_2,
            l.name,
        );
        let a = created(&rig.click(100.0, 28.0, NONE));
        assert_eq!(a.kind, AnnotKind::PolyLine);
        assert_eq!(
            a.geometry,
            Geometry::Points(screen_points(
                &l,
                &[(172.0, 100.0), (100.0, 100.0), (100.0, 28.0)]
            ))
        );
        assert_eq!(a.measurement().unwrap().kind, MeasureKind::Angle);
        assert_eq!(contents(&a), "90.0\u{b0}");
        close(
            a.quantities().unwrap().angle.unwrap(),
            std::f64::consts::FRAC_PI_2,
            l.name,
        );
        // Shift puts the last arm on the screen's diagonal: 45 degrees.
        click_all(&mut rig, &[(372.0, 300.0), (300.0, 300.0)]);
        let b = created(&rig.click(372.0, 232.0, SHIFT));
        assert_point(points_of(&b)[2], l.at(370.0, 230.0), l.name);
        assert_eq!(contents(&b), "45.0\u{b0}");
    }
}

#[test]
fn radius_and_diameter_from_the_centre_or_three_points() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Radius);
        click_all(&mut rig, &[(100.0, 100.0)]);
        let a = created(&rig.click(136.0, 100.0, NONE));
        assert_eq!(
            a.geometry,
            Geometry::Line {
                a: l.at(100.0, 100.0),
                b: l.at(136.0, 100.0)
            }
        );
        assert_eq!(a.measurement().unwrap().kind, MeasureKind::Radius);
        // /Contents is the bare value (Revu's); the caption drawn has the R.
        assert_eq!(contents(&a), "5.00'", "{}", l.name);
        assert_eq!(a.measure_text().unwrap(), "R 5.00'", "{}", l.name);
        // Dragged from the centre.
        let b = created(&rig.drag((300.0, 300.0), (300.0, 372.0), NONE));
        assert_eq!(contents(&b), "10.00'", "{}", l.name);

        // A diameter through the centre: from the far side to the point clicked.
        let mut rig = scaled(&l, ToolKind::Diameter);
        click_all(&mut rig, &[(100.0, 100.0)]);
        let d = created(&rig.click(136.0, 100.0, NONE));
        let (p, q) = line_of(&d);
        assert_point(p, l.at(64.0, 100.0), l.name);
        assert_point(q, l.at(136.0, 100.0), l.name);
        assert_eq!(contents(&d), "10.00'", "{}", l.name);
        assert_eq!(d.measure_text().unwrap(), "\u{d8} 10.00'", "{}", l.name);

        // Three points on the circle of centre (136, 136) and radius 36 pixels.
        for (kind, want) in [
            (ToolKind::Radius, "5.00'"),
            (ToolKind::Diameter, "10.00'"),
        ] {
            let mut rig = scaled(&l, kind);
            with_preset(&mut rig, kind, |m| m.three_point = true);
            click_all(&mut rig, &[(100.0, 136.0), (136.0, 100.0)]);
            rig.mv(172.0, 136.0, NONE);
            let r = rig.state.readout(&rig.view).unwrap();
            assert_eq!(r.radius.unwrap().text, "5.00'", "{}", l.name);
            let a = created(&rig.click(172.0, 136.0, NONE));
            assert_eq!(contents(&a), want, "{} {kind:?}", l.name);
            let (p, q) = line_of(&a);
            assert_point(q, l.at(172.0, 136.0), l.name);
            let centre = if kind == ToolKind::Radius {
                p
            } else {
                p.midpoint(q)
            };
            assert!((centre - l.at(136.0, 136.0)).hypot() < 1e-9, "{}", l.name);
            // Three points on one line make no circle and nothing is created.
            click_all(&mut rig, &[(300.0, 300.0), (336.0, 300.0)]);
            assert!(rig.click(372.0, 300.0, NONE).is_empty());
        }
    }
}

// ---------------------------------------------------------------------------------------
// Count
// ---------------------------------------------------------------------------------------

#[test]
fn count_adds_and_removes_markers_on_one_annotation() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Count);
        let first = created(&rig.click(100.0, 100.0, NONE));
        assert_eq!(first.kind, AnnotKind::Ink);
        let m = first.measurement().unwrap();
        assert_eq!(
            (m.kind, m.symbol, m.caption),
            (MeasureKind::Count, CountSymbol::Dot, false)
        );
        close(m.symbol_size, 10.0 * l.px, l.name);
        assert_eq!(contents(&first), "1");
        // Each further click adds a marker to the same annotation.
        for (x, y) in [(150.0, 100.0), (200.0, 100.0)] {
            let i = rig.click(x, y, NONE);
            assert!(matches!(i.as_slice(), [Intent::Modify { id, .. }] if *id == first.id));
        }
        let count = rig.get(&first.id);
        assert_eq!(
            count.count_points(),
            screen_points(&l, &[(100.0, 100.0), (150.0, 100.0), (200.0, 100.0)])
        );
        assert_eq!(contents(count), "3");
        assert_eq!(rig.state.readout(&rig.view).unwrap().count, Some(3));

        // Alt-click on a marker takes it off.
        let i = rig.click(151.0, 99.0, ALT);
        assert!(matches!(i.as_slice(), [Intent::Modify { .. }]));
        let count = rig.get(&first.id);
        assert_eq!(
            count.count_points(),
            screen_points(&l, &[(100.0, 100.0), (200.0, 100.0)])
        );
        assert_eq!(contents(count), "2");

        // Enter ends the count; the next click starts another.
        assert_eq!(rig.key(Key::Enter, NONE), [Intent::Select(Vec::new())]);
        let second = created(&rig.click(300.0, 300.0, NONE));
        assert_ne!(second.id, first.id);
        assert_eq!(rig.on_page().len(), 2);
        // Alt-click on the last marker of a count deletes the count; the other one is still
        // reachable by its markers.
        assert_eq!(
            rig.click(300.0, 300.0, ALT),
            [Intent::Delete(vec![second.id.clone()])]
        );
        rig.click(200.0, 100.0, ALT);
        assert_eq!(rig.get(&first.id).count_points().len(), 1);
        // The preset's symbol.
        with_preset(&mut rig, ToolKind::Count, |m| m.symbol = CountSymbol::Star);
        rig.view.selection.clear();
        let star = created(&rig.click(400.0, 400.0, NONE));
        assert_eq!(star.measurement().unwrap().symbol, CountSymbol::Star);
    }
}

// ---------------------------------------------------------------------------------------
// Calibrate and Viewport
// ---------------------------------------------------------------------------------------

#[test]
fn calibrating_two_points_sets_the_page_scale() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Length);
        // Before any scale a length is taken at full size and kept in step later.
        let before = created(
            &rig.click(100.0, 100.0, NONE)
                .into_iter()
                .chain(rig.click(172.0, 100.0, NONE))
                .collect::<Vec<_>>(),
        );
        assert_eq!(contents(&before), "25 mm", "{}", l.name);

        tool(&mut rig, ToolKind::Calibrate);
        rig.click(100.0, 300.0, NONE);
        let asked = rig.click(244.0, 300.0, NONE);
        let want_request = MeasureRequest::Distance {
            page: PAGE,
            a: l.at(100.0, 300.0),
            b: l.at(244.0, 300.0),
            target: ScaleTarget::Page,
        };
        assert_eq!(asked, [Intent::RequestMeasure(want_request.clone())]);
        assert_eq!(rig.state.measure_request(), Some(&want_request));
        assert!(rig.view.request.is_some());

        // A useless distance changes nothing and the tool keeps waiting.
        let (state, intents, _) = step_measure(
            rig.state.clone(),
            &MeasureInput::Distance {
                metres: 0.0,
                unit: Unit::Foot,
            },
            &rig.view,
        );
        assert!(intents.is_empty() && state.measure_request().is_some());

        // Two inches are twenty feet: 1 in = 10 ft.
        let (state, intents, _) = step_measure(
            rig.state.clone(),
            &MeasureInput::Distance {
                metres: 20.0 * FOOT,
                unit: Unit::Foot,
            },
            &rig.view,
        );
        assert!(!state.is_busy());
        let [Intent::SetScale(change)] = intents.as_slice() else {
            panic!("expected a scale, got {intents:?}");
        };
        assert_eq!((change.page, &change.target), (PAGE, &ScaleTarget::Page));
        close(change.scale.factor(), 120.0, l.name);
        assert_eq!(change.spec.ratio, "1 in = 10 ft", "{}", l.name);
        close(
            change
                .spec
                .distance_between(l.at(100.0, 300.0), l.at(244.0, 300.0)),
            20.0,
            l.name,
        );
        assert_eq!(change.spec.format_distance(20.0), "20.00'");
        rig.view.apply(&intents);
        rig.state = state;
        // The length taken before is now ten feet.
        assert_eq!(contents(rig.get(&before.id)), "10.00'", "{}", l.name);
        // And new ones are taken at the new scale.
        tool(&mut rig, ToolKind::Length);
        rig.click(100.0, 400.0, NONE);
        assert_eq!(contents(&created(&rig.click(136.0, 400.0, NONE))), "5.00'");

        // Feet and inches are shown to a sixteenth.
        tool(&mut rig, ToolKind::Calibrate);
        rig.click(100.0, 300.0, NONE);
        rig.click(172.0, 300.0, NONE);
        let (_, intents, _) = step_measure(
            rig.state.clone(),
            &MeasureInput::Distance {
                metres: 4.0 * FOOT,
                unit: Unit::FeetInches,
            },
            &rig.view,
        );
        let [Intent::SetScale(change)] = intents.as_slice() else {
            panic!("expected a scale");
        };
        assert_eq!(change.spec.format_distance(4.5), "4'-6\"");
        close(change.scale.factor(), 48.0, l.name);
    }
}

#[test]
fn calibrating_inside_a_viewport_or_a_dragged_region_scales_only_that_region() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Calibrate);
        // A dragged region first: it becomes a new viewport.
        rig.drag((50.0, 50.0), (300.0, 300.0), NONE);
        assert!(rig.state.is_busy());
        assert!(!rig.preview.is_empty(), "the region is shown");
        rig.click(100.0, 100.0, NONE);
        let asked = rig.click(172.0, 100.0, NONE);
        let region = l.rect(50.0, 50.0, 300.0, 300.0);
        let [Intent::RequestMeasure(MeasureRequest::Distance { target, .. })] = asked.as_slice()
        else {
            panic!("expected a request, got {asked:?}");
        };
        let ScaleTarget::NewViewport { name, bbox } = target else {
            panic!("expected a new viewport, got {target:?}");
        };
        assert_eq!(name, "Viewport 1");
        assert_rect(*bbox, region, l.name);
        // One inch is 50 ft in there: 1 in = 50 ft.
        let (state, intents, _) = step_measure(
            rig.state.clone(),
            &MeasureInput::Distance {
                metres: 50.0 * FOOT,
                unit: Unit::Foot,
            },
            &rig.view,
        );
        rig.view.apply(&intents);
        rig.state = state;
        let vps = &rig.view.viewports[0];
        assert_eq!(vps.viewports.len(), 1);
        assert_eq!(
            vps.viewports[0].measure.as_ref().unwrap().ratio,
            "1 in = 50 ft"
        );

        // Inside the region a length is at its scale; outside, at the page's.
        tool(&mut rig, ToolKind::Length);
        let inside = created(&rig.drag((100.0, 200.0), (172.0, 200.0), NONE));
        assert_eq!(
            inside.measurement().unwrap().scale,
            ScaleRef::Viewport("Viewport 1".into())
        );
        assert_eq!(contents(&inside), "50.00'", "{}", l.name);
        let outside = created(&rig.drag((100.0, 400.0), (172.0, 400.0), NONE));
        assert_eq!(outside.measurement().unwrap().scale, ScaleRef::Page);
        assert_eq!(contents(&outside), "10.00'");
        // One reaching out of the region is at the page's scale too.
        let across = created(&rig.drag((200.0, 200.0), (344.0, 200.0), NONE));
        assert_eq!(contents(&across), "20.00'");

        // Calibrating two points inside the existing viewport recalibrates it.
        tool(&mut rig, ToolKind::Calibrate);
        rig.click(100.0, 100.0, NONE);
        let asked = rig.click(244.0, 100.0, NONE);
        assert!(matches!(
            asked.as_slice(),
            [Intent::RequestMeasure(MeasureRequest::Distance {
                target: ScaleTarget::Viewport(name),
                ..
            })] if name == "Viewport 1"
        ));
        let (_, intents, _) = step_measure(
            rig.state.clone(),
            &MeasureInput::Distance {
                metres: 2.0,
                unit: Unit::Metre,
            },
            &rig.view,
        );
        rig.view.apply(&intents);
        // Two inches are two metres: 1:39.37..., shown in metres.
        assert_eq!(contents(rig.get(&inside.id)), "1.00 m", "{}", l.name);
        assert_eq!(contents(rig.get(&outside.id)), "10.00'");
    }
}

#[test]
fn a_viewport_is_a_dragged_region_given_a_scale() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Viewport);
        // A click is no region.
        assert!(rig.click(100.0, 100.0, NONE).is_empty());
        assert!(!rig.state.is_busy());
        let asked = rig.drag((100.0, 100.0), (300.0, 250.0), NONE);
        let want = MeasureRequest::Scale {
            page: PAGE,
            name: "Viewport 1".into(),
            bbox: l.rect(100.0, 100.0, 300.0, 250.0),
        };
        assert_eq!(asked, [Intent::RequestMeasure(want)]);
        assert!(!rig.preview.is_empty());
        let scale = Scale::ratio(100.0).unwrap();
        let (state, intents, _) =
            step_measure(rig.state.clone(), &MeasureInput::Scale(scale), &rig.view);
        assert!(!state.is_busy());
        let [Intent::SetScale(change)] = intents.as_slice() else {
            panic!("expected a scale");
        };
        assert_eq!(
            change.target,
            ScaleTarget::NewViewport {
                name: "Viewport 1".into(),
                bbox: l.rect(100.0, 100.0, 300.0, 250.0)
            }
        );
        assert_eq!(change.scale, scale);
        assert_eq!(change.spec, spec(&l, &scale));
        rig.view.apply(&intents);
        // The next one gets the next name.
        rig.state = ToolState::for_tool(ToolKind::Viewport);
        let asked = rig.drag((320.0, 100.0), (400.0, 200.0), NONE);
        assert!(matches!(
            asked.as_slice(),
            [Intent::RequestMeasure(MeasureRequest::Scale { name, .. })] if name == "Viewport 2"
        ));
        // At 1:100, one inch of paper is 2.54 m, shown in millimetres.
        tool(&mut rig, ToolKind::Length);
        let a = created(&rig.drag((110.0, 110.0), (182.0, 110.0), NONE));
        assert_eq!(contents(&a), "2540 mm", "{}", l.name);
    }
}

// ---------------------------------------------------------------------------------------
// Snapping and presets
// ---------------------------------------------------------------------------------------

#[test]
fn snap_points_are_taken_unless_alt_is_held() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Length);
        let snap = l.at(100.0, 100.0);
        let p = rig.pointer(103.0, 98.0, NONE).with_snap(snap);
        rig.send(ToolInput::PointerDown(p));
        rig.send(ToolInput::PointerUp(p));
        let q = rig.pointer(172.0, 100.0, ALT).with_snap(l.at(180.0, 100.0));
        let a = created(&rig.send(ToolInput::PointerDown(q)));
        rig.send(ToolInput::PointerUp(q));
        assert_eq!(
            a.geometry,
            Geometry::Line {
                a: snap,
                b: l.at(172.0, 100.0)
            }
        );
    }
    assert_eq!(ToolKind::Length.snap_kinds(), SnapKinds::ALL);
    assert!(ToolKind::Area.snap_kinds().contains(SnapKind::Nearest));
    assert_eq!(
        ToolKind::Count.snap_kinds().kinds(),
        [
            SnapKind::Endpoint,
            SnapKind::Intersection,
            SnapKind::Midpoint
        ]
    );
    assert_eq!(ToolKind::Pen.snap_kinds(), SnapKinds::NONE);
    assert_eq!(
        ToolState::for_tool(ToolKind::Viewport).snap_kinds(),
        SnapKinds::CORNERS
    );
}

#[test]
fn preset_units_and_presentation_reach_the_measurement() {
    let l = layouts()[1];
    let mut rig = scaled(&l, ToolKind::Length);
    with_preset(&mut rig, ToolKind::Length, |m| {
        m.length = Some(LengthFormat::new(Unit::Inch, Precision::Decimal(1)));
        m.caption = false;
        m.text_size = 12.0;
    });
    rig.click(100.0, 100.0, NONE);
    let a = created(&rig.click(172.0, 100.0, NONE));
    // Ten feet in inches, to one place; the caption is not drawn but the value is written.
    assert_eq!(contents(&a), "120.0\"");
    let m = a.measurement().unwrap();
    assert!(!m.caption);
    assert_eq!(m.text_size, 12.0);
    assert!(a.caption().is_none());
    // The area unit follows the length unit unless the preset names one.
    let mut rig = scaled(&l, ToolKind::Area);
    with_preset(&mut rig, ToolKind::Area, |m| {
        m.length = Some(LengthFormat::decimal(Unit::Inch, 0));
    });
    let a = created(&rig.drag((100.0, 100.0), (172.0, 172.0), NONE));
    assert_eq!(contents(&a), "14400.00 sq in");

    // Presets with measurement settings survive JSON.
    let mut p = ToolPreset::default_for(ToolKind::Count);
    p.name = "Fixtures".into();
    let m = p.measure.as_mut().unwrap();
    m.symbol = CountSymbol::Hexagon;
    m.symbol_size = 14.0;
    m.length = Some(LengthFormat::architectural(8));
    m.area = Some(pl_geom::AreaFormat::new(pl_geom::AreaUnit::SqYard, 1));
    m.volume = Some(VolumeFormat::new(VolumeUnit::Litre, 0));
    m.depth = Some(0.25);
    m.three_point = true;
    m.angle_step = 15.0;
    m.caption_position = pl_annot::CaptionPosition::Inline;
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains(r#""symbol":"Hexagon""#), "{json}");
    assert!(
        json.contains(r#""length":{"unit":"ft-in","fraction":8}"#),
        "{json}"
    );
    assert_eq!(serde_json::from_str::<ToolPreset>(&json).unwrap(), p);
    // An older preset without them gets the tool's defaults; unknown names are refused.
    let old = r#"{"tool":"Area","width":1}"#;
    let back: ToolPreset = serde_json::from_str(old).unwrap();
    assert_eq!(
        back.measure,
        Some(MeasurePreset::default_for(ToolKind::Area))
    );
    let bad = r#"{"tool":"Count","width":1,"measure":{"symbol":"Blob"}}"#;
    let err = serde_json::from_str::<ToolPreset>(bad)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Blob"), "{err}");
    let bad = r#"{"tool":"Area","width":1,"measure":{"text_size":0}}"#;
    assert!(serde_json::from_str::<ToolPreset>(bad).is_err());
    // Non-measurement tools carry none.
    assert_eq!(ToolPreset::default_for(ToolKind::Rectangle).measure, None);
    assert!(ToolKind::MEASURE.iter().all(|t| t.is_measure()));
    assert_eq!(ToolKind::Cutout.annot_kind(), None);
    assert_eq!(ToolKind::Count.measure_kind(), Some(MeasureKind::Count));
}

// ---------------------------------------------------------------------------------------
// Editing with the Select tool
// ---------------------------------------------------------------------------------------

#[test]
fn dragging_a_vertex_recomputes_the_value() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Polylength);
        click_all(&mut rig, &[(100.0, 100.0), (172.0, 100.0), (172.0, 172.0)]);
        let a = created(&rig.key(Key::Enter, NONE));
        assert_eq!(contents(&a), "20.00'");
        tool(&mut rig, ToolKind::Select);
        // The last vertex moves another inch down: 30 feet.
        rig.drag((172.0, 172.0), (172.0, 244.0), NONE);
        let b = rig.get(&a.id);
        assert_points(
            &points_of(b),
            &screen_points(&l, &[(100.0, 100.0), (172.0, 100.0), (172.0, 244.0)]),
            l.name,
        );
        assert_eq!(contents(b), "30.00'", "{}", l.name);

        // A count's markers are dragged one at a time.
        let mut rig = scaled(&l, ToolKind::Count);
        rig.click(100.0, 100.0, NONE);
        rig.click(200.0, 100.0, NONE);
        let id = rig.selection()[0].clone();
        tool(&mut rig, ToolKind::Select);
        rig.drag((200.0, 100.0), (230.0, 130.0), NONE);
        assert_points(
            &rig.get(&id).count_points(),
            &screen_points(&l, &[(100.0, 100.0), (230.0, 130.0)]),
            l.name,
        );
    }
}

#[test]
fn the_dimension_line_and_the_caption_are_dragged_by_their_own_handles() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Length);
        rig.click(100.0, 100.0, NONE);
        let a = created(&rig.click(172.0, 100.0, NONE));
        let handles = measure_handles(&a);
        assert_eq!(selection_measure_handles(&rig.view), handles);
        assert_eq!(handles[0].grip, MeasureGrip::Leader);
        assert_point(handles[0].at, l.at(136.0, 100.0), l.name);
        assert_eq!(handles[1].grip, MeasureGrip::Caption);

        tool(&mut rig, ToolKind::Select);
        // The dimension line 40 pixels up the screen: to the left of the line, which runs to
        // the right on screen, so a positive /LL.
        rig.drag((136.0, 100.0), (150.0, 60.0), NONE);
        let b = rig.get(&a.id);
        let m = b.measurement().unwrap();
        close(m.leader, 40.0 * l.px, l.name);
        assert_eq!(line_of(b), line_of(&a), "the measured points stay");
        let (p, q) = b.dimension_line().unwrap();
        assert_point(p, l.at(100.0, 60.0), l.name);
        assert_point(q, l.at(172.0, 60.0), l.name);
        assert_eq!(contents(b), "10.00'");

        // The caption, from wherever it now is, 20 pixels along and 10 further up.
        let c = b.caption().unwrap().centre();
        let at = l.screen(c);
        rig.drag((at.x, at.y), (at.x + 20.0, at.y - 10.0), NONE);
        let m = rig.get(&a.id).measurement().unwrap().clone();
        close(m.caption_offset.x, 20.0 * l.px, l.name);
        close(m.caption_offset.y, 10.0 * l.px, l.name);
        close(m.leader, 40.0 * l.px, l.name);
        let moved = rig.get(&a.id).caption().unwrap().centre();
        assert_point(
            l.screen(moved),
            Point::new(at.x + 20.0, at.y - 10.0),
            l.name,
        );
    }
}

#[test]
fn double_click_inserts_and_deletes_vertices() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Area);
        click_all(&mut rig, &SQUARE);
        let a = created(&rig.key(Key::Enter, NONE));
        tool(&mut rig, ToolKind::Select);
        // On the top edge: a vertex there; the area is the same.
        let i = rig.double(172.0, 101.0);
        assert!(!i.iter().any(|i| matches!(i, Intent::RequestTextEdit(_))));
        let pts = points_of(rig.get(&a.id));
        assert_eq!(pts.len(), 5);
        assert_point(pts[1], l.at(172.0, 100.0), l.name);
        assert_eq!(contents(rig.get(&a.id)), "400.00 sq ft");
        // On a vertex: deleted. Without the bottom-right corner the outline is a triangle of
        // half the square.
        rig.double(244.0, 244.0);
        let pts = points_of(rig.get(&a.id));
        assert_eq!(pts.len(), 4);
        assert_eq!(contents(rig.get(&a.id)), "200.00 sq ft", "{}", l.name);
        rig.double(172.0, 100.0);
        assert_eq!(points_of(rig.get(&a.id)).len(), 3);
        // A triangle keeps its corners.
        assert!(rig
            .double(244.0, 100.0)
            .iter()
            .all(|i| !matches!(i, Intent::Modify { .. })));
        assert_eq!(points_of(rig.get(&a.id)).len(), 3);

        // A polylength gets a vertex on its segment, and keeps two at least.
        tool(&mut rig, ToolKind::Polylength);
        click_all(&mut rig, &[(300.0, 300.0), (444.0, 300.0)]);
        let p = created(&rig.key(Key::Enter, NONE));
        tool(&mut rig, ToolKind::Select);
        rig.double(372.0, 300.0);
        assert_eq!(points_of(rig.get(&p.id)).len(), 3);
        rig.double(372.0, 300.0);
        assert_eq!(points_of(rig.get(&p.id)).len(), 2);
        rig.double(300.0, 300.0);
        assert_eq!(points_of(rig.get(&p.id)).len(), 2);
        assert_eq!(contents(rig.get(&p.id)), "20.00'");
    }
}

#[test]
fn cutouts_are_edited_and_moved_with_their_area() {
    for l in layouts() {
        let mut rig = scaled(&l, ToolKind::Area);
        click_all(&mut rig, &SQUARE);
        let a = created(&rig.key(Key::Enter, NONE));
        tool(&mut rig, ToolKind::Cutout);
        rig.click(172.0, 172.0, NONE);
        rig.drag((136.0, 136.0), (208.0, 208.0), NONE);
        assert_eq!(contents(rig.get(&a.id)), "300.00 sq ft");
        tool(&mut rig, ToolKind::Select);
        rig.view.selection = vec![a.id.clone()];
        let grips: Vec<MeasureGrip> = selection_measure_handles(&rig.view)
            .iter()
            .map(|h| h.grip)
            .collect();
        assert_eq!(grips.len(), 4);
        assert!(grips.contains(&MeasureGrip::Cutout { ring: 0, index: 0 }));

        // A corner of the hole dragged out: the hole grows and the area shrinks.
        let ring = rig.get(&a.id).measurement().unwrap().cutouts[0].clone();
        let corner = l.screen(ring[2]);
        rig.drag((corner.x, corner.y), (corner.x + 18.0, corner.y), NONE);
        let hole: Vec<Point> = rig.get(&a.id).measurement().unwrap().cutouts[0]
            .iter()
            .map(|p| l.screen(*p))
            .collect();
        let pixels = polygon_area(&hole);
        let want = 400.0 - pixels / 5184.0 * 100.0;
        close(
            rig.get(&a.id).quantities().unwrap().area.unwrap(),
            want * FOOT * FOOT,
            l.name,
        );
        // The corner moved 18 pixels along one side of the 72-pixel square: half of an
        // 18 x 72 strip more or less, depending on which way that side runs on this page.
        assert!(
            ((pixels - 5184.0).abs() - 648.0).abs() < 1e-6,
            "{}: {pixels}",
            l.name
        );

        // Double-click on the hole's edge adds a corner there; on a corner, takes it off;
        // a hole left with two corners goes.
        let top = l.screen(rig.get(&a.id).measurement().unwrap().cutouts[0][0]);
        let next = l.screen(rig.get(&a.id).measurement().unwrap().cutouts[0][1]);
        let mid = top.midpoint(next);
        rig.double(mid.x, mid.y);
        assert_eq!(rig.get(&a.id).measurement().unwrap().cutouts[0].len(), 5);
        for _ in 0..3 {
            let c = l.screen(rig.get(&a.id).measurement().unwrap().cutouts[0][0]);
            rig.double(c.x, c.y);
        }
        assert!(rig.get(&a.id).measurement().unwrap().cutouts.is_empty());
        assert_eq!(contents(rig.get(&a.id)), "400.00 sq ft");

        // Moving the area moves its holes with it.
        tool(&mut rig, ToolKind::Cutout);
        rig.click(172.0, 172.0, NONE);
        rig.drag((136.0, 136.0), (208.0, 208.0), NONE);
        tool(&mut rig, ToolKind::Select);
        rig.view.selection = vec![a.id.clone()];
        rig.drag((172.0, 244.0), (202.0, 264.0), NONE);
        let b = rig.get(&a.id);
        assert_points(
            &points_of(b),
            &screen_points(
                &l,
                &[
                    (130.0, 120.0),
                    (274.0, 120.0),
                    (274.0, 264.0),
                    (130.0, 264.0),
                ],
            ),
            l.name,
        );
        let hole = l.rect(166.0, 156.0, 238.0, 228.0);
        assert_points(
            &b.measurement().unwrap().cutouts[0],
            &[
                Point::new(hole.x0, hole.y0),
                Point::new(hole.x1, hole.y0),
                Point::new(hole.x1, hole.y1),
                Point::new(hole.x0, hole.y1),
            ],
            l.name,
        );
        assert_eq!(contents(b), "300.00 sq ft");
    }
}

#[test]
fn measurement_tools_ignore_other_pages_while_busy() {
    let l = layouts()[0];
    for kind in ToolKind::MEASURE {
        if kind == ToolKind::Viewport {
            // A click is no region: it leaves the tool idle.
            continue;
        }
        let mut rig = scaled(&l, kind);
        rig.click(100.0, 100.0, NONE);
        let other = pl_tools::Pointer::new(0, Point::new(50.0, 50.0));
        let before = rig.state.clone();
        for input in [
            ToolInput::PointerDown(other),
            ToolInput::PointerMove(other),
            ToolInput::DoubleClick(other),
        ] {
            let (state, intents, _) = pl_tools::step(rig.state.clone(), &input, &rig.view);
            assert!(
                intents.iter().all(|i| matches!(i, Intent::SetCursor(_))),
                "{kind:?}"
            );
            assert_eq!(state, before, "{kind:?} {input:?}");
        }
    }
}
