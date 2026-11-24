//! Measurement and takeoff in the window (M6, M7), headless: calibration, the measurement
//! tools, the scale control and panel, snapping with its marker, the measurement properties,
//! the takeoff columns of the markups list, formula columns, legends, spaces and numbered
//! counts. Values are checked against analytic ones; every change is undone again.

mod support;

use egui::accesskit::Role;
use egui::Key;
use egui_kittest::kittest::Queryable;
use pl_app::{Command, Tool};
use pl_doc::pl_annot::{Annotation, MeasureKind, ScaleRef};
use pl_doc::pl_geom::{Point, Unit};
use pl_doc::pl_takeoff::{Builtin, ColumnId};
use support::*;

/// Metres in a foot.
const FOOT: f64 = 0.3048;

/// Equal but for where a click lands: a point placed by a click (not snapped) comes through
/// the screen's single-precision coordinates, a few millionths of a point off.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-5 * b.abs().max(1.0)
}

/// Equal to rounding: for values from snapped (exact) points.
fn exact(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * b.abs().max(1.0)
}

/// Sets the scale of the page in view through the scale panel's own functions.
fn set_scale(h: &mut App, text: &str) {
    h.state_mut().open_scale_panel(None);
    assert!(h.state().scale_panel_open());
    h.state_mut().set_scale_panel(text, None, None, None);
    assert!(
        h.state_mut().apply_scale_panel(),
        "the scale {text} was refused: {:?}",
        h.state().status()
    );
    assert!(!h.state().scale_panel_open());
    h.run();
}

/// A mock-backed app whose markups have been read.
fn measuring_app(pages: usize) -> App {
    let mut h = mock_app(pages);
    // Reading the markups (outside a frame, so it finishes now).
    h.state_mut().select_markups(Vec::new());
    h.run();
    h
}

fn press_tool_key(h: &mut App, tool: Tool) {
    // The keys are the page's only while no control has the keyboard (a dialog's button that
    // was just clicked may have it).
    if let Some(id) = h.ctx.memory(|m| m.focused()) {
        h.ctx.memory_mut(|m| m.surrender_focus(id));
    }
    h.run();
    h.key_press_modifiers(tool.modifiers(), tool.key());
    h.run();
    assert_eq!(h.state().tool(), tool, "{tool:?} from its key");
}

fn measurements(h: &App) -> Vec<Annotation> {
    markups(h)
        .into_iter()
        .filter(|a| a.measure.is_some())
        .collect()
}

fn last_measurement(h: &App) -> Annotation {
    measurements(h).pop().expect("a measurement")
}

fn length_of(a: &Annotation) -> f64 {
    a.quantities().and_then(|q| q.length).expect("a length")
}

fn area_of(a: &Annotation) -> f64 {
    a.quantities().and_then(|q| q.area).expect("an area")
}

/// Clicks two page points with the active tool.
fn click_two(h: &mut App, page: u32, a: Point, b: Point) {
    click_on_page(h, page, a);
    click_on_page(h, page, b);
}

fn go_to(h: &mut App, page: u32) {
    h.state_mut().active_view_mut().unwrap().go_to_page(page);
    settle(h);
}

fn zoom(h: &mut App, z: f64) {
    h.state_mut().active_view_mut().unwrap().view.set_zoom(z);
    settle(h);
}

/// Gives a text box the keyboard and types into it, as a user clicking into it would.
fn type_in(h: &mut App, name: &str, text: &str) {
    h.get_by_label(name).focus();
    h.run();
    h.get_by_label(name).type_text(text);
    h.run();
}

// ---- calibration, then a measurement ------------------------------------------------------------

/// On the plain fixture (a 3 in horizontal and a 4 in vertical segment through the centre),
/// calibrating the horizontal one as 12 feet makes the page 1 in = 4 ft; the vertical one
/// then measures 16 feet. The points are picked a little off the segments' ends and snapped
/// onto them.
#[test]
fn calibrating_a_known_segment_then_measuring_another_gives_the_analytic_value() {
    let fixture = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fixture);
    let mut h = file_app(
        pdfium(),
        &path,
        None,
        pl_app::ScriptedDialogs::default(),
        0.25,
    );
    zoom(&mut h, 0.55);
    h.state_mut().select_markups(Vec::new());
    run_command(&mut h, Command::ToggleSnap);
    settle(&mut h);
    assert_eq!(h.state().status_info().scale, "Scale not set");

    // The Calibrate tool from its key, Shift+K.
    press_tool_key(&mut h, Tool::Calibrate);
    click_two(
        &mut h,
        0,
        Point::new(199.2, 396.9),
        Point::new(413.1, 395.4),
    );
    assert!(
        matches!(
            h.state().measure_request(),
            Some(pl_tools::MeasureRequest::Distance { a, b, .. })
                if *a == Point::new(198.0, 396.0) && *b == Point::new(414.0, 396.0)
        ),
        "the dialog asks for the distance between the snapped ends: {:?}",
        h.state().measure_request()
    );
    // The distance is typed into the in-app dialog.
    type_in(&mut h, "Known distance", "12'");
    click(&mut h, "OK");
    assert!(
        h.state().measure_request().is_none(),
        "{:?}",
        h.state().status()
    );
    let scale = h
        .state()
        .active_document()
        .and_then(|d| d.page_scales(0))
        .and_then(|v| v.page_scale.clone())
        .expect("the page has a scale now");
    let real = scale.scale(1.0).expect("a length scale");
    // 216 pt (3 in) is 12 ft: one inch of paper is four feet.
    assert!(exact(real.metres_per_page_unit(1.0) * 72.0, 4.0 * FOOT));

    // The vertical segment, from its key and two clicks near its ends.
    press_tool_key(&mut h, Tool::Length);
    click_on_page(&mut h, 0, Point::new(306.8, 252.9));
    // While the second point is being placed, the read-out beside the pointer runs.
    let pos = on_screen(&h, 0, Point::new(306.0, 400.0));
    h.hover_at(pos);
    h.run();
    let view = h.state().active().unwrap();
    let readout = h.state().readout(view).cloned().expect("a read-out");
    assert!(
        readout.segment.is_some() && readout.total.is_some(),
        "{readout:?}"
    );
    let lines = pl_app::measure::readout_lines(&readout);
    assert!(lines.iter().any(|l| l.starts_with("Segment ")), "{lines:?}");
    click_on_page(&mut h, 0, Point::new(305.4, 539.2));

    let m = last_measurement(&h);
    assert_eq!(m.measurement().unwrap().kind, MeasureKind::Length);
    assert!(
        exact(length_of(&m), 16.0 * FOOT),
        "16 ft is {} m, measured {}",
        16.0 * FOOT,
        length_of(&m)
    );
    // The value is in /Contents and drawn as the caption.
    let text = m.measure_text().expect("a caption");
    assert!(text.contains("16"), "{text}");
    assert_eq!(m.meta.contents.as_deref(), Some(text.as_str()));
    assert_eq!(m.caption().map(|c| c.text.clone()), Some(text.clone()));
    assert!(h.state().status_info().scale.contains('='));

    // Both are undone, one step each: the measurement, then the calibration.
    assert!(h.state_mut().undo());
    assert!(measurements(&h).is_empty());
    assert!(h.state_mut().undo());
    assert!(h
        .state()
        .active_document()
        .and_then(|d| d.page_scales(0))
        .is_some_and(|v| v.page_scale.is_none()));
}

// ---- area and cutout ---------------------------------------------------------------------------

/// A 3 in square at 1 in = 10 ft is 900 sq ft; a 1 in square cut out of it leaves 800.
#[test]
fn an_area_with_a_cutout_is_net_of_the_hole_and_both_undo() {
    let mut h = measuring_app(1);
    set_scale(&mut h, "1 in = 10 ft");
    press_tool_key(&mut h, Tool::Area);
    drag_on_page(
        &mut h,
        0,
        Point::new(150.0, 700.0),
        Point::new(366.0, 484.0),
    );
    let area = last_measurement(&h);
    assert_eq!(area.measurement().unwrap().kind, MeasureKind::Area);
    let sq_ft = FOOT * FOOT;
    assert!(close(area_of(&area), 900.0 * sq_ft), "{}", area_of(&area));

    press_tool_key(&mut h, Tool::Cutout);
    click_on_page(&mut h, 0, Point::new(200.0, 650.0));
    drag_on_page(
        &mut h,
        0,
        Point::new(200.0, 650.0),
        Point::new(272.0, 578.0),
    );
    let cut = last_measurement(&h);
    assert_eq!(cut.id, area.id);
    assert_eq!(cut.measurement().unwrap().cutouts.len(), 1);
    assert!(close(area_of(&cut), 800.0 * sq_ft), "{}", area_of(&cut));
    assert!(cut.measure_text().unwrap().contains("800"));

    assert!(h.state_mut().undo());
    assert!(close(area_of(&last_measurement(&h)), 900.0 * sq_ft));
    assert!(h.state_mut().undo());
    assert!(measurements(&h).is_empty());
}

// ---- count -------------------------------------------------------------------------------------

/// Three clicks make one count of three; Enter ends it. With numbering on, each new count
/// takes the next label of the document's sequence.
#[test]
fn a_count_counts_its_clicks_and_numbered_counts_take_the_next_label() {
    let mut h = measuring_app(1);
    // A count needs no scale.
    assert_eq!(h.state().status_info().scale, "Scale not set");
    assert!(h.state_mut().set_count_numbering(Some("D-")));
    press_tool_key(&mut h, Tool::Count);
    for x in [150.0, 200.0, 250.0] {
        click_on_page(&mut h, 0, Point::new(x, 600.0));
    }
    h.key_press(Key::Enter);
    h.run();
    let counts = measurements(&h);
    assert_eq!(counts.len(), 1);
    let first = &counts[0];
    assert_eq!(first.count_points().len(), 3);
    assert_eq!(first.quantities().and_then(|q| q.count), Some(3));
    // A second count.
    for x in [150.0, 200.0] {
        click_on_page(&mut h, 0, Point::new(x, 500.0));
    }
    h.key_press(Key::Enter);
    h.run();
    let counts = measurements(&h);
    assert_eq!(counts.len(), 2);
    assert_eq!(counts[1].count_points().len(), 2);
    let schema = h.state().list_schema().clone();
    let column = schema
        .columns
        .iter()
        .find(|c| c.title == pl_app::measure::NUMBER_COLUMN)
        .expect("the Number column")
        .clone();
    let labels: Vec<String> = counts
        .iter()
        .map(|a| pl_doc::pl_takeoff::custom_value(a, &column).display())
        .collect();
    assert_eq!(labels, ["D-01", "D-02"]);

    // Undo: the second count's markers one by one, then the first's, then the numbering.
    while !measurements(&h).is_empty() {
        assert!(h.state_mut().undo());
    }
    assert!(h.state().count_sequence().is_some());
    assert!(h.state_mut().undo());
    assert!(h.state().count_sequence().is_none());
}

/// With snapping on, a Count click a few pixels from a marker already placed is a new marker
/// where it was clicked, not a second one stacked on the first.
#[test]
fn count_clicks_near_a_marker_do_not_stack_on_it() {
    let mut h = measuring_app(1);
    h.state_mut().set_snap(true);
    press_tool_key(&mut h, Tool::Count);
    click_on_page(&mut h, 0, Point::new(150.0, 600.0));
    click_on_page(&mut h, 0, Point::new(154.0, 600.0));
    h.key_press(Key::Enter);
    h.run();
    let counts = measurements(&h);
    assert_eq!(counts.len(), 1);
    let points = counts[0].count_points();
    assert_eq!(points.len(), 2);
    assert!(
        points[0].distance(points[1]) > 2.0,
        "two markers apart: {points:?}"
    );
}

// ---- viewports -----------------------------------------------------------------------------------

/// On a page with no scale of its own, a measurement that starts in a viewport and ends
/// outside it has no scale: it is not made at full size, the scale panel asks for one.
#[test]
fn a_measurement_leaving_the_only_scaled_viewport_is_not_made_at_full_size() {
    let mut h = measuring_app(1);
    press_tool_key(&mut h, Tool::Viewport);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 700.0),
        Point::new(500.0, 500.0),
    );
    type_in(&mut h, "Viewport scale", "1 in = 20 ft");
    click(&mut h, "OK");
    assert_eq!(
        h.state()
            .active_document()
            .and_then(|d| d.page_scales(0))
            .map(|v| (v.viewports.len(), v.page_scale.is_some())),
        Some((1, false))
    );
    press_tool_key(&mut h, Tool::Length);
    // Inside: 20 ft.
    click_two(
        &mut h,
        0,
        Point::new(320.0, 600.0),
        Point::new(392.0, 600.0),
    );
    assert!(close(length_of(&last_measurement(&h)), 20.0 * FOOT));
    // From inside to outside.
    click_two(
        &mut h,
        0,
        Point::new(320.0, 600.0),
        Point::new(592.0, 600.0),
    );
    assert_eq!(measurements(&h).len(), 1, "nothing measured at full size");
    assert!(h.state().scale_panel_open());
    let prompt = h.state().scale_prompt().unwrap_or_default().to_owned();
    assert!(prompt.contains("no scale"), "{prompt}");
}

/// A viewport at 1 in = 20 ft on a page at 1 in = 10 ft: an inch measures 20 ft inside it and
/// 10 ft outside, and the status bar names the viewport where the pointer is over it.
#[test]
fn a_viewport_scale_differs_from_the_page_scale() {
    let mut h = measuring_app(1);
    set_scale(&mut h, "1 in = 10 ft");
    press_tool_key(&mut h, Tool::Viewport);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 700.0),
        Point::new(500.0, 500.0),
    );
    assert!(matches!(
        h.state().measure_request(),
        Some(pl_tools::MeasureRequest::Scale { .. })
    ));
    type_in(&mut h, "Viewport scale", "1 in = 20 ft");
    click(&mut h, "OK");
    let vps = h
        .state()
        .active_document()
        .and_then(|d| d.page_scales(0))
        .cloned()
        .unwrap();
    assert_eq!(vps.viewports.len(), 1);
    let name = vps.viewports[0].name.clone();

    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(320.0, 600.0),
        Point::new(392.0, 600.0),
    );
    let inside = last_measurement(&h);
    assert_eq!(
        inside.measurement().unwrap().scale,
        ScaleRef::Viewport(name.clone())
    );
    assert!(
        close(length_of(&inside), 20.0 * FOOT),
        "{}",
        length_of(&inside)
    );
    click_two(
        &mut h,
        0,
        Point::new(100.0, 600.0),
        Point::new(172.0, 600.0),
    );
    let outside = last_measurement(&h);
    assert_eq!(outside.measurement().unwrap().scale, ScaleRef::Page);
    assert!(
        close(length_of(&outside), 10.0 * FOOT),
        "{}",
        length_of(&outside)
    );

    let pos = on_screen(&h, 0, Point::new(400.0, 600.0));
    h.hover_at(pos);
    h.run();
    let shown = h.state().status_info().scale;
    assert!(shown.contains(&name) && shown.contains("20"), "{shown}");
    let pos = on_screen(&h, 0, Point::new(150.0, 600.0));
    h.hover_at(pos);
    h.run();
    let shown = h.state().status_info().scale;
    assert!(shown.contains("10") && !shown.contains(&name), "{shown}");

    // Undo takes the two lengths and the viewport away.
    for _ in 0..3 {
        assert!(h.state_mut().undo());
    }
    assert!(measurements(&h).is_empty());
    assert!(h
        .state()
        .active_document()
        .and_then(|d| d.page_scales(0))
        .is_some_and(|v| v.viewports.is_empty() && v.page_scale.is_some()));
}

// ---- snapping ------------------------------------------------------------------------------------

/// With snapping on, the pointer near a fixture endpoint snaps exactly onto it and the marker
/// is the endpoint's; near the middle of a segment it is the midpoint's; turning a kind off
/// drops it.
#[test]
fn snapping_picks_the_exact_fixture_point_and_marks_its_kind() {
    let fixture = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fixture);
    let mut h = file_app(
        pdfium(),
        &path,
        None,
        pl_app::ScriptedDialogs::default(),
        0.25,
    );
    zoom(&mut h, 0.55);
    h.state_mut().select_markups(Vec::new());
    run_command(&mut h, Command::ToggleSnap);
    set_scale(&mut h, "1 in = 10 ft");
    press_tool_key(&mut h, Tool::Length);
    settle(&mut h);
    let view = h.state().active().unwrap();
    let hover = |h: &mut App, p: Point| {
        let pos = on_screen(h, 0, p);
        h.hover_at(pos);
        h.run();
        settle(h);
        h.hover_at(pos + egui::vec2(0.3, 0.0));
        h.run();
    };
    // The triangle's right-angle corner (244, 100) is the end of two segments.
    hover(&mut h, Point::new(245.3, 101.1));
    assert_eq!(
        h.state().snap_marker(view),
        Some((0, Point::new(244.0, 100.0)))
    );
    assert_eq!(h.state().snap_kind(view), Some(pl_doc::SnapKind::Endpoint));
    // The middle of the triangle's base (100,100)-(244,100).
    hover(&mut h, Point::new(172.6, 101.4));
    assert_eq!(
        h.state().snap_marker(view),
        Some((0, Point::new(172.0, 100.0)))
    );
    assert_eq!(h.state().snap_kind(view), Some(pl_doc::SnapKind::Midpoint));
    // Midpoints off: nothing there any more (no other point is that close).
    let mut toggles = h.state().snap_toggles();
    toggles.midpoint = false;
    h.state_mut().set_snap_toggles(toggles);
    hover(&mut h, Point::new(172.6, 101.4));
    assert_eq!(h.state().snap_marker(view), None);

    // A measurement drawn by snapped clicks has the exact points.
    toggles.midpoint = true;
    h.state_mut().set_snap_toggles(toggles);
    click_two(&mut h, 0, Point::new(100.9, 100.7), Point::new(243.2, 99.4));
    let m = last_measurement(&h);
    // 144 pt is 2 in: 20 ft.
    assert!(exact(length_of(&m), 20.0 * FOOT), "{}", length_of(&m));
    // A new point near the measurement's end snaps to it first, as a measurement point.
    hover(&mut h, Point::new(100.8, 101.2));
    assert_eq!(
        h.state().snap_marker(view),
        Some((0, Point::new(100.0, 100.0)))
    );
    assert_eq!(
        h.state().snap_kind(view),
        Some(pl_doc::SnapKind::MeasureEndpoint)
    );
}

// ---- no scale ------------------------------------------------------------------------------------

/// On a page with no scale a Length writes nothing: the scale panel opens with a prompt and
/// the status bar says so; the scale is then set through the panel's own controls.
#[test]
fn a_page_with_no_scale_prompts_for_one_instead_of_measuring() {
    let mut h = measuring_app(1);
    assert_eq!(h.state().status_info().scale, "Scale not set");
    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(150.0, 600.0),
        Point::new(222.0, 600.0),
    );
    assert!(
        measurements(&h).is_empty(),
        "nothing is measured at no scale"
    );
    assert!(h.state().scale_panel_open());
    let prompt = h.state().scale_prompt().unwrap_or_default().to_owned();
    assert!(prompt.contains("no scale"), "{prompt}");
    assert!(h.state().status().unwrap_or_default().contains("no scale"));
    assert_all_named(&h, "the scale panel");

    // The panel, typed into and applied.
    type_in(&mut h, "Drawing scale", "1/4 in = 1 ft");
    click(&mut h, "Apply scale");
    assert!(
        !h.state().scale_panel_open(),
        "{:?} / {:?}",
        h.state().scale_panel_error(),
        h.state().status()
    );
    let shown = h.state().status_info().scale;
    assert_ne!(shown, "Scale not set");
    // Now the same clicks measure: 72 pt is 1 in, which is 4 ft at a quarter inch to the foot.
    click_two(
        &mut h,
        0,
        Point::new(150.0, 600.0),
        Point::new(222.0, 600.0),
    );
    let m = last_measurement(&h);
    assert!(close(length_of(&m), 4.0 * FOOT), "{}", length_of(&m));

    // The status bar's scale control opens the panel too.
    click_label(&mut h, &format!("Scale: {shown}"));
    assert!(h.state().scale_panel_open());
    assert!(h.state().scale_prompt().is_none());
    click(&mut h, "Close scale panel");
    assert!(!h.state().scale_panel_open());
}

/// A range of pages takes the scale in one step, and undo takes it back from all of them.
#[test]
fn a_scale_applied_to_a_range_of_pages_is_one_undo_step() {
    let mut h = measuring_app(4);
    h.state_mut().open_scale_panel(None);
    h.state_mut()
        .set_scale_panel("1:100", Some((2, 4)), Some(Unit::Metre), None);
    assert!(h.state_mut().apply_scale_panel());
    let scaled = |h: &App| -> Vec<bool> {
        (0..4)
            .map(|p| {
                h.state()
                    .active_document()
                    .and_then(|d| d.page_scales(p))
                    .is_some_and(|v| v.page_scale.is_some())
            })
            .collect()
    };
    assert_eq!(scaled(&h), [false, true, true, true]);
    assert!(h.state_mut().undo());
    assert_eq!(scaled(&h), [false, false, false, false]);
    // A range that runs backwards is refused, with a message in the panel.
    h.state_mut().open_scale_panel(None);
    h.state_mut()
        .set_scale_panel("1:100", Some((3, 2)), None, None);
    assert!(!h.state_mut().apply_scale_panel());
    assert!(h.state().scale_panel_open());
}

// ---- the markups list ------------------------------------------------------------------------------

fn column_index(h: &App, column: &ColumnId) -> usize {
    h.state()
        .list_layout()
        .visible()
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("{column:?} is not shown"))
}

fn number(text: &str) -> f64 {
    text.trim()
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .replace(',', "")
        .parse()
        .unwrap_or_else(|_| panic!("not a number: {text:?}"))
}

/// A formula column added through its dialog: a formula that names a column the list does
/// not have is refused with the parser's message, the fixed one is added, computes per row
/// and adds up in the total line.
#[test]
fn a_formula_column_added_through_the_dialog_computes_and_totals() {
    let mut h = measuring_app(1);
    set_scale(&mut h, "1 in = 10 ft");
    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(150.0, 600.0),
        Point::new(222.0, 600.0),
    ); // 10 ft
    click_two(
        &mut h,
        0,
        Point::new(150.0, 500.0),
        Point::new(294.0, 500.0),
    ); // 20 ft
    run_command(&mut h, Command::TakeoffLayout);
    assert!(h.state().markups_list_open());
    assert_eq!(
        h.state()
            .list_layout()
            .group
            .first()
            .map(|g| g.column.clone()),
        Some(ColumnId::Builtin(Builtin::Subject))
    );

    run_command(&mut h, Command::FormulaColumn);
    assert!(h.state().formula_dialog_open());
    assert_all_named(&h, "the formula dialog");
    type_in(&mut h, "Formula column name", "Doubled");
    type_in(&mut h, "Formula text", "[Lenght] * 2");
    let message = h.state().formula_dialog_message().unwrap_or_default();
    assert!(
        message.contains("#REF!") && message.contains("Lenght"),
        "{message}"
    );
    click(&mut h, "Add formula column");
    assert!(
        h.state().formula_dialog_open(),
        "a bad formula is not added"
    );
    type_in(&mut h, "Formula text", " (");
    let message = h.state().formula_dialog_message().unwrap_or_default();
    assert!(message.contains("#SYNTAX!"), "{message}");
    // Closed and opened again, the dialog starts afresh.
    click(&mut h, "Close formula column");
    assert!(!h.state().formula_dialog_open());
    run_command(&mut h, Command::FormulaColumn);
    type_in(&mut h, "Formula column name", "Doubled");
    type_in(&mut h, "Formula text", "[Length] * 2");
    assert_eq!(h.state().formula_dialog_message(), None);
    click(&mut h, "Add formula column");
    assert!(!h.state().formula_dialog_open());

    // A second column of the same name is refused; once renamed, the refusal is gone.
    run_command(&mut h, Command::FormulaColumn);
    type_in(&mut h, "Formula column name", "Doubled");
    type_in(&mut h, "Formula text", "[Length] * 3");
    click(&mut h, "Add formula column");
    assert!(h.state().formula_dialog_open(), "a name in use is refused");
    let message = h.state().formula_dialog_message().unwrap_or_default();
    assert!(message.contains("Doubled"), "{message}");
    type_in(&mut h, "Formula column name", " again");
    assert_eq!(h.state().formula_dialog_message(), None);
    click(&mut h, "Close formula column");
    assert!(!h.state().formula_dialog_open());

    let id = h
        .state()
        .list_schema()
        .columns
        .iter()
        .find(|c| c.title == "Doubled")
        .expect("the formula column")
        .id
        .clone();
    let doubled = column_index(&h, &ColumnId::custom(&id));
    let length = column_index(&h, &ColumnId::Builtin(Builtin::Length));
    let lines = h.state_mut().list_lines();
    let rows: Vec<&pl_app::ListLineView> = lines.iter().filter(|l| l.id.is_some()).collect();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert!(
            close(
                number(&row.cells[doubled]),
                2.0 * number(&row.cells[length])
            ),
            "{:?}",
            row.cells
        );
    }
    let total = lines
        .iter()
        .find(|l| l.id.is_none() && l.group.is_none())
        .expect("a total line");
    assert!(
        close(number(&total.cells[length]), 30.0),
        "{:?}",
        total.cells
    );
    assert!(
        close(number(&total.cells[doubled]), 60.0),
        "{:?}",
        total.cells
    );
    // The group of the subject has the same subtotal.
    let group = lines.iter().find(|l| l.group.is_some()).expect("a group");
    assert!(
        close(number(&group.cells[doubled]), 60.0),
        "{:?}",
        group.cells
    );
}

/// A choice column whose options stand for numbers works in a formula.
#[test]
fn a_choice_column_with_values_feeds_a_formula() {
    let mut h = measuring_app(1);
    set_scale(&mut h, "1 in = 10 ft");
    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(150.0, 600.0),
        Point::new(222.0, 600.0),
    ); // 10 ft
    let rate = h
        .state_mut()
        .add_choice_column("Rate", &[("Low", Some(2.0)), ("High", Some(5.5))])
        .unwrap();
    let cost = h
        .state_mut()
        .add_formula_column("Cost", "[Length] * [Rate]")
        .unwrap();
    // A copy of the measurement with the High rate, added as one undoable step.
    let m = last_measurement(&h);
    let column = h.state().list_schema().get(&rate).unwrap().clone();
    let mut with_rate = m.clone();
    with_rate.id = pl_doc::pl_annot::AnnotId::generate();
    pl_doc::pl_takeoff::set_custom_value(
        &mut with_rate,
        &column,
        pl_doc::pl_takeoff::Value::text("High"),
    )
    .unwrap();
    assert!(h.state_mut().add_markups(vec![with_rate.clone()]));
    run_command(&mut h, Command::TakeoffLayout);
    let lines = h.state_mut().list_lines();
    let index = column_index(&h, &ColumnId::custom(&cost));
    let row = lines
        .iter()
        .find(|l| l.id.as_ref() == Some(&with_rate.id))
        .unwrap();
    // 10 ft at 5.5 a foot.
    assert!(close(number(&row.cells[index]), 55.0), "{:?}", row.cells);
    // The one without a rate counts as 0 (an empty cell is 0 in arithmetic).
    let total = lines
        .iter()
        .find(|l| l.id.is_none() && l.group.is_none())
        .unwrap();
    assert!(
        close(number(&total.cells[index]), 55.0),
        "{:?}",
        total.cells
    );
    assert!(h.state_mut().undo());
    assert_eq!(measurements(&h).len(), 1);
}

// ---- legends ---------------------------------------------------------------------------------------

/// A legend of lengths by subject is placed as a text box whose text follows the markups: a
/// new length changes it in the same undo step, and undo puts both back.
#[test]
fn a_legend_updates_after_an_edit_and_undoes_with_it() {
    let mut h = measuring_app(1);
    set_scale(&mut h, "1 in = 10 ft");
    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(150.0, 600.0),
        Point::new(222.0, 600.0),
    ); // 10 ft
    run_command(&mut h, Command::InsertLegend(Some(Builtin::Length)));
    let texts = h.state().legend_texts();
    assert_eq!(texts.len(), 1);
    let before = texts[0].1.clone();
    assert!(before.contains("Length legend"), "{before}");
    assert!(before.contains("10.00 ft"), "{before}");
    let legend = markups(&h)
        .into_iter()
        .find(|a| pl_app::measure::legend_id(a).is_some())
        .unwrap();
    assert_eq!(legend.meta.contents.as_deref(), Some(before.as_str()));

    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(150.0, 500.0),
        Point::new(294.0, 500.0),
    ); // 20 ft
    let after = h.state().legend_texts()[0].1.clone();
    assert_ne!(after, before);
    assert!(after.contains("30.00 ft"), "{after}");

    // One undo step for the length and the legend's new text.
    assert!(h.state_mut().undo());
    assert_eq!(measurements(&h).len(), 1);
    assert_eq!(h.state().legend_texts()[0].1, before);
    assert!(h.state_mut().redo());
    assert_eq!(h.state().legend_texts()[0].1, after);
    // Undo the length, then the legend itself.
    assert!(h.state_mut().undo());
    assert!(h.state_mut().undo());
    assert!(h.state().legend_texts().is_empty());
}

// ---- spaces ------------------------------------------------------------------------------------------

/// A space drawn with the Space tool is named, and the list groups measurements by it.
#[test]
fn a_space_names_a_region_and_the_list_groups_by_it() {
    let mut h = measuring_app(1);
    set_scale(&mut h, "1 in = 10 ft");
    press_tool_key(&mut h, Tool::Space);
    for p in [
        Point::new(100.0, 700.0),
        Point::new(300.0, 700.0),
        Point::new(300.0, 500.0),
        Point::new(100.0, 500.0),
        Point::new(100.0, 700.0),
    ] {
        click_on_page(&mut h, 0, p);
    }
    let space = markups(&h).pop().unwrap();
    assert_eq!(
        pl_doc::pl_takeoff::space_name(&space).as_deref(),
        Some("Space 1")
    );
    // Renamed through the properties.
    h.state_mut().select_markups(vec![space.id.clone()]);
    assert!(h.state_mut().set_space_name("Kitchen"));
    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(150.0, 600.0),
        Point::new(222.0, 600.0),
    );
    click_two(
        &mut h,
        0,
        Point::new(350.0, 600.0),
        Point::new(422.0, 600.0),
    );
    h.state_mut().set_markups_list_open(true);
    h.state_mut().edit_list_layout(|l| {
        l.group = vec![pl_doc::pl_takeoff::GroupKey::asc(Builtin::Space)];
        l.set_visible(&ColumnId::Builtin(Builtin::Space), true);
    });
    let lines = h.state_mut().list_lines();
    let groups: Vec<String> = lines.iter().filter_map(|l| l.group.clone()).collect();
    assert!(groups.iter().any(|g| g == "Space: Kitchen"), "{groups:?}");
    let space_col = column_index(&h, &ColumnId::Builtin(Builtin::Space));
    let kitchen: Vec<&pl_app::ListLineView> = lines
        .iter()
        .filter(|l| l.id.is_some() && l.cells[space_col] == "Kitchen")
        .collect();
    // The inner length and the space's own outline.
    assert_eq!(kitchen.len(), 2, "{lines:#?}");
}

// ---- rotated, UserUnit and offset pages ----------------------------------------------------------------

/// On a page shown with /Rotate 90 or 270, on a /UserUnit 2 page and on an offset MediaBox, an
/// inch of the shown page measures 10 ft at 1 in = 10 ft, and the caption is turned to read
/// upright on the page as shown.
#[test]
fn measurements_on_rotated_user_unit_and_offset_pages_are_upright_and_right() {
    for (fixture, page) in [
        (pl_testkit::rotated_pages(), 1u32),
        (pl_testkit::rotated_pages(), 3),
        (pl_testkit::user_unit(), 0),
        (pl_testkit::crop_box(), 1),
        (pl_testkit::crop_box(), 2),
    ] {
        let name = fixture.manifest.file_name();
        let truth = &fixture.manifest.pages[page as usize];
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &fixture);
        let mut h = file_app(
            pdfium(),
            &path,
            None,
            pl_app::ScriptedDialogs::default(),
            0.25,
        );
        zoom(&mut h, 0.6);
        go_to(&mut h, page);
        h.state_mut().select_markups(Vec::new());
        set_scale(&mut h, "1 in = 10 ft");
        let geometry = *h
            .state()
            .active_view()
            .unwrap()
            .view
            .page_geometry(page)
            .unwrap();
        let uu = geometry.user_unit();
        let crop = geometry.crop_box();
        let c = crop.center();
        // One shown inch, along the page's own x axis.
        let half = 36.0 / uu;
        press_tool_key(&mut h, Tool::Length);
        click_two(
            &mut h,
            page,
            Point::new(c.x - half, c.y + 20.0 / uu),
            Point::new(c.x + half, c.y + 20.0 / uu),
        );
        let m = last_measurement(&h);
        assert_eq!(m.page, page as usize, "{name}");
        assert!(
            close(length_of(&m), 10.0 * FOOT),
            "{name} page {page}: {}",
            length_of(&m)
        );
        let rotation = truth.rotate.rem_euclid(360) as u16;
        assert_eq!(
            m.measurement().unwrap().rotation,
            rotation,
            "{name} page {page}: the caption reads upright"
        );
        assert!(m.measure_text().unwrap().contains("10"), "{name}");
        assert!(h.state_mut().undo());
        assert!(h.state_mut().undo());
    }
}

// ---- properties --------------------------------------------------------------------------------------

/// The properties of a selected measurement: its units, caption, wall height and depth, each
/// one undo step; and every control of the measuring interface is named.
#[test]
fn measurement_properties_change_the_measurement_and_every_control_is_named() {
    let mut h = measuring_app(1);
    set_scale(&mut h, "1 in = 10 ft");
    run_command(&mut h, Command::TogglePropertiesPanel);
    for tool in Tool::MEASURE {
        set_tool(&mut h, tool);
        assert_all_named(&h, &format!("the {tool:?} tool"));
        let name = format!("{} tool", tool.label());
        h.get_by_role_and_label(Role::Button, &name);
        assert_eq!(
            tool.tooltip(),
            format!("{} (Shift+{})", tool.label(), tool.key().name())
        );
    }
    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(150.0, 600.0),
        Point::new(222.0, 600.0),
    );
    let m = last_measurement(&h);
    h.state_mut().select_markups(vec![m.id.clone()]);
    set_tool(&mut h, Tool::Select);
    assert_all_named(&h, "a selected measurement's properties");

    assert!(h
        .state_mut()
        .set_measure_units(Unit::Metre, pl_doc::pl_geom::Precision::Decimal(3)));
    let metric = last_measurement(&h);
    assert!(
        metric.measure_text().unwrap().ends_with('m'),
        "{:?}",
        metric.measure_text()
    );
    assert!(metric.measure_text().unwrap().contains("3.048"));
    // The value itself is the same.
    assert!(close(length_of(&metric), 10.0 * FOOT));
    assert!(h.state_mut().undo());
    assert_eq!(last_measurement(&h).measure_text(), m.measure_text());

    // Wall height (in the list's feet): Wall Area is 10 ft x 9 ft.
    assert!(h.state_mut().set_wall_height(Some(9.0)));
    run_command(&mut h, Command::TakeoffLayout);
    let wall = column_index(&h, &ColumnId::Builtin(Builtin::WallArea));
    let lines = h.state_mut().list_lines();
    let row = lines.iter().find(|l| l.id.is_some()).unwrap();
    assert!(close(number(&row.cells[wall]), 90.0), "{:?}", row.cells);
    assert!(h.state_mut().undo());

    // An area given a depth becomes a volume.
    press_tool_key(&mut h, Tool::Area);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 700.0),
        Point::new(172.0, 628.0),
    );
    let area = last_measurement(&h);
    h.state_mut().select_markups(vec![area.id.clone()]);
    assert!(h.state_mut().set_measure_depth(3.0 * FOOT));
    let volume = last_measurement(&h);
    assert_eq!(volume.measurement().unwrap().kind, MeasureKind::Volume);
    // 10 ft x 10 ft x 3 ft.
    let cubic = volume.quantities().and_then(|q| q.volume).unwrap();
    assert!(close(cubic, 300.0 * FOOT * FOOT * FOOT), "{cubic}");
    assert!(h.state_mut().undo());
    assert_eq!(
        last_measurement(&h).measurement().unwrap().kind,
        MeasureKind::Area
    );

    // The dialogs and menus of the measuring interface.
    press_tool_key(&mut h, Tool::Volume);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 700.0),
        Point::new(372.0, 628.0),
    );
    assert!(matches!(
        h.state().measure_request(),
        Some(pl_tools::MeasureRequest::Depth { .. })
    ));
    assert_all_named(&h, "the depth dialog");
    type_in(&mut h, "Depth of the volume", "2'");
    click(&mut h, "OK");
    let v = last_measurement(&h);
    assert_eq!(v.measurement().unwrap().kind, MeasureKind::Volume);
    assert!(close(
        v.quantities().and_then(|q| q.volume).unwrap(),
        200.0 * FOOT * FOOT * FOOT
    ));
    run_command(&mut h, Command::ScalePanel);
    assert_all_named(&h, "the scale panel");
    run_command(&mut h, Command::FormulaColumn);
    assert_all_named(&h, "the formula dialog");
}

// ---- the real sample sheet -----------------------------------------------------------------------------

/// On the owner's sample sheet (34 x 22 in civil plans), a scale set through the panel makes a
/// shown inch measure its real length. Works on a copy under `target`, prints nothing from
/// the file, saves nothing; skipped when the sample is not there.
#[test]
fn the_real_sample_sheet_takes_a_scale_and_a_measurement() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let sample = root.join("samples").join("site-development-plans.pdf");
    if !sample.exists() {
        eprintln!("SKIPPED: the sample sheet is not in samples/");
        return;
    }
    let dir = root.join("target").join("measure-sample");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sheet.pdf");
    std::fs::copy(&sample, &path).unwrap();
    let mut h = file_app(
        pdfium(),
        &path,
        None,
        pl_app::ScriptedDialogs::default(),
        0.25,
    );
    zoom(&mut h, 0.25);
    h.state_mut().select_markups(Vec::new());
    set_scale(&mut h, "1 in = 40 ft");
    let geometry = *h
        .state()
        .active_view()
        .unwrap()
        .view
        .page_geometry(0)
        .unwrap();
    let uu = geometry.user_unit();
    let c = geometry.crop_box().center();
    let half = 36.0 / uu;
    press_tool_key(&mut h, Tool::Length);
    click_two(
        &mut h,
        0,
        Point::new(c.x - half, c.y),
        Point::new(c.x + half, c.y),
    );
    let m = last_measurement(&h);
    // At a quarter zoom a click lands within a few hundredths of a point.
    let error = (length_of(&m) - 40.0 * FOOT).abs();
    assert!(error < 40.0 * FOOT * 1e-3, "{}", length_of(&m));
    assert!(h.state_mut().undo());
    assert!(h.state_mut().undo());
    drop(h);
    let _ = std::fs::remove_file(&path);
}
