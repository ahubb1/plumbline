//! Headless tests of markup editing in the window, driven through `egui_kittest`: tools,
//! selection, properties, presets, undo, clipboard, save, close and recovery prompts, snapping
//! and the snapshot. No window is opened and the real clipboard is never touched.
//!
//! Tests named `real_pdfium_*` use the vendored PDFium on fixtures from pl-testkit; the others
//! use `MockBackend`, whose documents have no file (and so no annotations until some are
//! drawn).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::{by, Queryable};
use egui_kittest::Harness;
use pl_app::{
    AppConfig, AppState, Arrange, CloseChoice, Command, FakeClipboard, SaveHow, ScriptedDialogs,
    Tool,
};
use pl_doc::pl_annot::{AnnotKind, Annotation, BlendMode, Geometry, LineEnding, Rgb};
use pl_doc::pl_geom::{PageGeometry, Point, Rect, Rotation, Vec2};
use pl_doc::pl_render::{DocSource, MockBackend, PdfiumBackend, RenderBackend};
use pl_tools::ToolKind;

type App = Harness<'static, AppState>;

/// Tests that render through wgpu take this lock (see `tests/ui.rs`).
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gpu() -> std::sync::MutexGuard<'static, ()> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

const CTRL: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    mac_cmd: false,
    command: true,
};

const CTRL_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: true,
    mac_cmd: false,
    command: true,
};

const RED: Rgb = Rgb {
    r: 0.8,
    g: 0.0,
    b: 0.0,
};
const BLUE: Rgb = Rgb {
    r: 0.1,
    g: 0.3,
    b: 0.8,
};

fn letters(n: usize) -> Vec<PageGeometry> {
    vec![PageGeometry::simple(612.0, 792.0); n]
}

fn state_with(backend: Arc<dyn RenderBackend>, data_dir: Option<PathBuf>) -> AppState {
    let mut state = AppState::new(
        backend,
        AppConfig {
            data_dir,
            cache_bytes: 256 << 20,
        },
        Box::new(ScriptedDialogs::default()),
    );
    state.set_author("Test Author");
    state.set_theme(egui::ThemePreference::Light);
    state
}

fn harness(state: AppState) -> App {
    harness_dt(state, 0.25)
}

/// A harness whose frames are `dt` seconds apart. Two clicks are a double-click only when
/// they are close in time, so the tests that double-click use a short step.
fn harness_dt(state: AppState, dt: f32) -> App {
    Harness::builder()
        .with_step_dt(dt)
        .with_size(egui::vec2(1100.0, 720.0))
        .with_theme(egui::Theme::Light)
        .with_max_steps(400)
        .with_context_setup(pl_app::theme::install)
        .build_ui_state(pl_app::ui, state)
}

fn settle(h: &mut App) {
    for _ in 0..6 {
        h.run();
        assert!(
            h.state_mut().wait_idle(Duration::from_secs(30)),
            "renders did not finish"
        );
    }
    h.run();
    assert_eq!(h.state().pending_queries(), 0);
}

/// A mock-backed app with one document of `pages` Letter pages open at 100%.
fn mock_app(pages: usize) -> (Arc<MockBackend>, App) {
    mock_app_dt(pages, 0.25)
}

fn mock_app_dt(pages: usize, dt: f32) -> (Arc<MockBackend>, App) {
    let mock = Arc::new(MockBackend::with_pages(&letters(pages)));
    let mut state = state_with(mock.clone(), None);
    state
        .open_bytes("fixture.pdf", Vec::new())
        .expect("mock open");
    let mut h = harness_dt(state, dt);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    (mock, h)
}

fn click(h: &mut App, name: &str) {
    h.get_by_role_and_label(Role::Button, name).click();
    h.run();
    h.remove_cursor();
    h.run();
}

/// Clicks the item of an open menu whose text starts with `text` (an item's accessible name
/// goes on to say its shortcut).
fn click_item(h: &mut App, text: &str) {
    let wanted = text.to_owned();
    h.get_by(move |node| {
        node.role() == Role::Button && node.label().is_some_and(|l| l.starts_with(&wanted))
    })
    .click();
    h.run();
    h.remove_cursor();
    h.run();
}

/// A page-space point's position in window coordinates, for the active view.
fn on_screen(h: &App, page: u32, point: Point) -> egui::Pos2 {
    let dv = h.state().active_view().expect("a view");
    let p = dv.view.page_to_view(page, point).expect("page");
    let pos = egui::pos2(dv.rect.min.x + p.x as f32, dv.rect.min.y + p.y as f32);
    assert!(dv.rect.contains(pos), "{point:?} is off screen at {pos:?}");
    pos
}

/// Presses at `from`, moves through `via` to `to` and releases, a frame per event.
fn drag_with(h: &mut App, from: egui::Pos2, via: &[egui::Pos2], to: egui::Pos2, m: Modifiers) {
    h.hover_at(from);
    h.run();
    h.event_modifiers(
        egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: m,
        },
        m,
    );
    h.run();
    for stop in via.iter().chain([&to]) {
        h.event_modifiers(egui::Event::PointerMoved(*stop), m);
        h.run();
    }
    h.event_modifiers(
        egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: m,
        },
        m,
    );
    h.run();
    h.remove_cursor();
    h.run();
}

fn drag(h: &mut App, from: egui::Pos2, to: egui::Pos2) {
    let mid = from + (to - from) * 0.5;
    drag_with(h, from, &[mid], to, Modifiers::NONE);
}

/// Drags between two page points of `page`.
fn drag_on_page(h: &mut App, page: u32, a: Point, b: Point) {
    let (from, to) = (on_screen(h, page, a), on_screen(h, page, b));
    drag(h, from, to);
}

fn click_at(h: &mut App, pos: egui::Pos2) {
    h.hover_at(pos);
    h.run();
    h.drag_at(pos);
    h.run();
    h.drop_at(pos);
    h.run();
}

fn click_on_page(h: &mut App, page: u32, point: Point) {
    let pos = on_screen(h, page, point);
    click_at(h, pos);
}

fn set_tool(h: &mut App, tool: Tool) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::Tool(tool));
    h.run();
}

fn near(a: Point, b: Point) -> bool {
    (a - b).hypot() < 0.02
}

fn rect_near(a: Rect, b: Rect) -> bool {
    near(Point::new(a.x0, a.y0), Point::new(b.x0, b.y0))
        && near(Point::new(a.x1, a.y1), Point::new(b.x1, b.y1))
}

fn rect_of(a: &Annotation) -> Rect {
    match &a.geometry {
        Geometry::Rect(r) => *r,
        other => panic!("not a rectangle: {other:?}"),
    }
}

fn points_of(a: &Annotation) -> Vec<Point> {
    match &a.geometry {
        Geometry::Points(p) => p.clone(),
        other => panic!("not a point list: {other:?}"),
    }
}

fn markups(h: &App) -> Vec<Annotation> {
    h.state().markups()
}

fn selected_ids(h: &App) -> Vec<String> {
    h.state()
        .selected_markups()
        .iter()
        .map(|a| a.id.as_str().to_owned())
        .collect()
}

// ---- tools ---------------------------------------------------------------------------------

#[test]
fn each_drag_tool_draws_its_shape() {
    let (_mock, mut h) = mock_app(2);
    assert_eq!(h.state().tool(), Tool::Select);
    assert!(markups(&h).is_empty());

    // Rectangle, picked on the tool bar.
    click(&mut h, "Rectangle tool");
    assert_eq!(h.state().tool(), Tool::Rectangle);
    let (a, b) = (Point::new(100.0, 700.0), Point::new(220.0, 640.0));
    // Half-way through the drag the shape is previewed, and nothing exists yet.
    let (from, to) = (on_screen(&h, 0, a), on_screen(&h, 0, b));
    h.hover_at(from);
    h.run();
    h.drag_at(from);
    h.run();
    h.hover_at(from + (to - from) * 0.5);
    h.run();
    let view = h.state().active().unwrap();
    let (page, preview) = h.state().preview(view).expect("a live preview");
    assert_eq!(page, 0);
    let bounds = preview.bounds().expect("the preview draws something");
    let half_way = Rect::new(100.0, 670.0, 160.0, 700.0);
    assert!(
        (bounds.x0 - half_way.x0).abs() <= 1.0
            && (bounds.y0 - half_way.y0).abs() <= 1.0
            && (bounds.x1 - half_way.x1).abs() <= 1.0
            && (bounds.y1 - half_way.y1).abs() <= 1.0,
        "{bounds:?}"
    );
    assert!(markups(&h).is_empty());
    h.hover_at(to);
    h.run();
    h.drop_at(to);
    h.run();
    assert!(h.state().preview(view).is_none());
    let all = markups(&h);
    assert_eq!(all.len(), 1);
    let square = &all[0];
    assert_eq!(square.kind, AnnotKind::Square);
    assert_eq!(square.page, 0);
    assert!(rect_near(
        rect_of(square),
        Rect::new(100.0, 640.0, 220.0, 700.0)
    ));
    assert_eq!(square.meta.author.as_deref(), Some("Test Author"));
    assert_eq!(square.meta.subject.as_deref(), Some("Rectangle"));
    assert_eq!(square.style.stroke, Some(RED));
    assert!(square.meta.created.is_some() && square.meta.modified.is_some());
    // A new shape is selected.
    assert_eq!(selected_ids(&h), vec![square.id.as_str().to_owned()]);

    // Ellipse, picked with its key.
    h.key_press(Key::E);
    h.run();
    assert_eq!(h.state().tool(), Tool::Ellipse);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 700.0),
        Point::new(380.0, 650.0),
    );
    let circle = markups(&h).pop().unwrap();
    assert_eq!(circle.kind, AnnotKind::Circle);
    assert!(rect_near(
        rect_of(&circle),
        Rect::new(300.0, 650.0, 380.0, 700.0)
    ));

    // Line and Arrow.
    h.key_press(Key::L);
    h.run();
    assert_eq!(h.state().tool(), Tool::Line);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 600.0),
        Point::new(250.0, 560.0),
    );
    let line = markups(&h).pop().unwrap();
    assert_eq!(line.kind, AnnotKind::Line);
    match line.geometry {
        Geometry::Line { a, b } => {
            assert!(near(a, Point::new(100.0, 600.0)), "{a:?}");
            assert!(near(b, Point::new(250.0, 560.0)), "{b:?}");
        }
        ref other => panic!("{other:?}"),
    }
    assert_eq!(line.style.line_end, LineEnding::None);

    h.key_press(Key::A);
    h.run();
    assert_eq!(h.state().tool(), Tool::Arrow);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 600.0),
        Point::new(420.0, 600.0),
    );
    let arrow = markups(&h).pop().unwrap();
    assert_eq!(arrow.kind, AnnotKind::Line);
    assert_eq!(arrow.style.line_end, LineEnding::OpenArrow);
    assert_eq!(arrow.meta.subject.as_deref(), Some("Arrow"));

    // A press that goes nowhere draws nothing.
    click_on_page(&mut h, 0, Point::new(480.0, 500.0));
    assert_eq!(markups(&h).len(), 4);
    // The status bar names the tool and says what it does.
    let info = h.state().status_info();
    assert_eq!(info.tool, "Arrow");
    assert!(info.hint.contains("Drag to draw"));
    h.get_by_label("Arrow");
}

#[test]
fn click_tools_draw_polyline_polygon_cloud_and_note() {
    let (_mock, mut h) = mock_app_dt(1, 0.04);

    // Polyline: three clicks, then Enter.
    click(&mut h, "Polyline tool");
    let path = [
        Point::new(100.0, 700.0),
        Point::new(180.0, 660.0),
        Point::new(260.0, 700.0),
    ];
    for p in path {
        click_on_page(&mut h, 0, p);
    }
    assert!(
        markups(&h).is_empty(),
        "nothing exists until it is finished"
    );
    let view = h.state().active().unwrap();
    assert!(h.state().preview(view).is_some());
    h.key_press(Key::Enter);
    h.run();
    let polyline = markups(&h).pop().expect("a polyline");
    assert_eq!(polyline.kind, AnnotKind::PolyLine);
    let got = points_of(&polyline);
    assert_eq!(got.len(), 3);
    for (g, want) in got.iter().zip(path) {
        assert!(near(*g, want), "{g:?} vs {want:?}");
    }

    // Polygon: clicking the first point again closes it.
    h.key_press(Key::G);
    h.run();
    assert_eq!(h.state().tool(), Tool::Polygon);
    let corners = [
        Point::new(320.0, 700.0),
        Point::new(420.0, 700.0),
        Point::new(420.0, 620.0),
        Point::new(370.0, 660.0),
    ];
    for p in corners {
        click_on_page(&mut h, 0, p);
    }
    click_on_page(&mut h, 0, corners[0]);
    let polygon = markups(&h).pop().unwrap();
    assert_eq!(polygon.kind, AnnotKind::Polygon);
    assert_eq!(points_of(&polygon).len(), 4);
    assert!(polygon.style.cloud.is_none());

    // Cloud: a polygon with a cloud border, finished by a double-click on the last corner.
    h.key_press(Key::C);
    h.run();
    assert_eq!(h.state().tool(), Tool::Cloud);
    let corners = [
        Point::new(100.0, 560.0),
        Point::new(240.0, 560.0),
        Point::new(240.0, 460.0),
    ];
    click_on_page(&mut h, 0, corners[0]);
    click_on_page(&mut h, 0, corners[1]);
    let last = on_screen(&h, 0, corners[2]);
    h.hover_at(last);
    h.drag_at(last);
    h.drop_at(last);
    h.drag_at(last);
    h.drop_at(last);
    h.run();
    assert_eq!(markups(&h).len(), 3, "the double-click finished the cloud");
    let cloud = markups(&h).pop().unwrap();
    assert_eq!(cloud.kind, AnnotKind::Polygon);
    assert!(cloud.style.cloud.is_some());
    assert_eq!(cloud.meta.subject.as_deref(), Some("Cloud"));
    let got = points_of(&cloud);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(near(got[2], corners[2]));

    // Escape abandons a part-drawn shape.
    click_on_page(&mut h, 0, Point::new(300.0, 500.0));
    click_on_page(&mut h, 0, Point::new(340.0, 480.0));
    h.key_press(Key::Escape);
    h.run();
    assert!(h.state().preview(view).is_none());
    assert_eq!(markups(&h).len(), 3);
    assert_eq!(
        h.state().tool(),
        Tool::Cloud,
        "Escape cancelled the shape only"
    );

    // Note: one click; its text is typed under Properties, which opens for it.
    h.key_press(Key::N);
    h.run();
    click_on_page(&mut h, 0, Point::new(400.0, 500.0));
    let note = markups(&h).pop().unwrap();
    assert_eq!(note.kind, AnnotKind::Text);
    let r = rect_of(&note);
    assert!(near(r.center(), Point::new(400.0, 500.0)), "{r:?}");
    assert!((r.width() - pl_tools::NOTE_SIZE).abs() < 1e-6);
    assert!(h.state().properties_panel_open());
    h.run();
    assert!(h.get_by_label("Comment").is_focused());
    h.get_by_label("Comment").type_text("Check this beam");
    h.run();
    let note = markups(&h).pop().unwrap();
    assert_eq!(note.meta.contents.as_deref(), Some("Check this beam"));
    // Typing is one undo step, and the tool keys do not fire while typing ("C" is Cloud).
    assert_eq!(h.state().tool(), Tool::Note);
}

#[test]
fn pen_and_highlighter_draw_freehand_strokes() {
    let (_mock, mut h) = mock_app(1);
    h.key_press(Key::P);
    h.run();
    assert_eq!(h.state().tool(), Tool::Pen);
    let stops = [
        Point::new(100.0, 700.0),
        Point::new(130.0, 680.0),
        Point::new(160.0, 705.0),
        Point::new(190.0, 670.0),
        Point::new(220.0, 700.0),
    ];
    let screen: Vec<egui::Pos2> = stops.iter().map(|p| on_screen(&h, 0, *p)).collect();
    drag_with(&mut h, screen[0], &screen[1..4], screen[4], Modifiers::NONE);
    let ink = markups(&h).pop().expect("a pen stroke");
    assert_eq!(ink.kind, AnnotKind::Ink);
    let Geometry::Ink(strokes) = &ink.geometry else {
        panic!("{:?}", ink.geometry);
    };
    assert_eq!(strokes.len(), 1);
    assert!(near(strokes[0][0], stops[0]));
    assert!(near(*strokes[0].last().unwrap(), stops[4]));
    assert!(strokes[0].len() >= 4, "{:?}", strokes[0]);
    assert_eq!(ink.style.blend, BlendMode::Normal);
    // Freehand strokes are not selected, so the next one can follow at once.
    assert!(selected_ids(&h).is_empty());

    h.key_press(Key::M);
    h.run();
    assert_eq!(h.state().tool(), Tool::Highlighter);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 600.0),
        Point::new(260.0, 600.0),
    );
    let highlight = markups(&h).pop().unwrap();
    assert_eq!(highlight.kind, AnnotKind::Ink);
    assert_eq!(highlight.style.blend, BlendMode::Multiply);
    assert_eq!(highlight.meta.subject.as_deref(), Some("Highlighter"));
    assert_eq!(markups(&h).len(), 2);
}

// ---- selection -----------------------------------------------------------------------------

/// Draws a rectangle with the Rectangle tool and returns to the Select tool.
fn draw_rect(h: &mut App, page: u32, rect: Rect) -> Annotation {
    set_tool(h, Tool::Rectangle);
    drag_on_page(
        h,
        page,
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y0),
    );
    set_tool(h, Tool::Select);
    markups(h).pop().expect("the rectangle")
}

#[test]
fn select_move_resize_nudge_and_delete() {
    let (_mock, mut h) = mock_app(1);
    let drawn = draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    // Escape drops the selection; a click on the outline selects again.
    h.key_press(Key::Escape);
    h.run();
    assert!(selected_ids(&h).is_empty());
    click_on_page(&mut h, 0, Point::new(160.0, 700.0));
    assert_eq!(selected_ids(&h), vec![drawn.id.as_str().to_owned()]);

    // Dragging the outline moves it: 30 units right and 20 down the page. (The middle of
    // an edge is a resize handle; a quarter of the way along is plain outline.)
    drag_on_page(
        &mut h,
        0,
        Point::new(130.0, 600.0),
        Point::new(160.0, 580.0),
    );
    let moved = markups(&h).pop().unwrap();
    assert!(
        rect_near(rect_of(&moved), Rect::new(130.0, 580.0, 250.0, 680.0)),
        "{:?}",
        moved.geometry
    );
    assert_eq!(moved.id, drawn.id);

    // The top-right handle resizes.
    drag_on_page(
        &mut h,
        0,
        Point::new(250.0, 680.0),
        Point::new(290.0, 695.0),
    );
    let resized = markups(&h).pop().unwrap();
    assert!(
        rect_near(rect_of(&resized), Rect::new(130.0, 580.0, 290.0, 695.0)),
        "{:?}",
        resized.geometry
    );

    // Arrow keys nudge by one unit (ten with Shift) instead of scrolling the view.
    let scroll = h.state().active_view().unwrap().view.scroll();
    h.key_press(Key::ArrowRight);
    h.run();
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowUp);
    h.run();
    let nudged = markups(&h).pop().unwrap();
    assert!(
        rect_near(rect_of(&nudged), Rect::new(131.0, 590.0, 291.0, 705.0)),
        "{:?}",
        nudged.geometry
    );
    assert_eq!(h.state().active_view().unwrap().view.scroll(), scroll);

    // Each of those was one undo step.
    for expected in [
        Rect::new(131.0, 580.0, 291.0, 695.0),
        Rect::new(130.0, 580.0, 290.0, 695.0),
        Rect::new(130.0, 580.0, 250.0, 680.0),
        Rect::new(100.0, 600.0, 220.0, 700.0),
    ] {
        h.key_press_modifiers(CTRL, Key::Z);
        h.run();
        let now = markups(&h).pop().unwrap();
        assert!(rect_near(rect_of(&now), expected), "{:?}", now.geometry);
    }
    // Ctrl+Y and Ctrl+Shift+Z redo.
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    h.key_press_modifiers(CTRL_SHIFT, Key::Z);
    h.run();
    let now = markups(&h).pop().unwrap();
    assert!(rect_near(
        rect_of(&now),
        Rect::new(130.0, 580.0, 290.0, 695.0)
    ));

    // Delete removes the selection; undo brings it back; the Edit menu does the same.
    h.key_press(Key::Delete);
    h.run();
    assert!(markups(&h).is_empty());
    click(&mut h, "Edit");
    click_item(&mut h, "Undo");
    assert_eq!(markups(&h).len(), 1);
    assert!(h.state().can_redo());
    // With nothing selected the arrow keys move the view again.
    h.key_press(Key::Escape);
    h.run();
    h.key_press(Key::ArrowDown);
    h.run();
    assert!(h.state().active_view().unwrap().view.scroll().y > scroll.y);
}

#[test]
fn marquee_shift_click_arrange_and_stacking() {
    let (_mock, mut h) = mock_app(1);
    let a = draw_rect(&mut h, 0, Rect::new(100.0, 640.0, 160.0, 700.0));
    let b = draw_rect(&mut h, 0, Rect::new(200.0, 600.0, 280.0, 660.0));
    let c = draw_rect(&mut h, 0, Rect::new(330.0, 560.0, 380.0, 620.0));
    h.key_press(Key::Escape);
    h.run();
    let ids = |list: &[&Annotation]| -> Vec<String> {
        list.iter().map(|a| a.id.as_str().to_owned()).collect()
    };

    // A box dragged left to right over the first two takes exactly those.
    drag_on_page(&mut h, 0, Point::new(80.0, 720.0), Point::new(300.0, 580.0));
    let mut selected = selected_ids(&h);
    selected.sort();
    let mut want = ids(&[&a, &b]);
    want.sort();
    assert_eq!(selected, want);

    // Shift-click adds the third.
    let third = on_screen(&h, 0, Point::new(355.0, 620.0));
    drag_with(&mut h, third, &[], third, Modifiers::SHIFT);
    assert_eq!(selected_ids(&h).len(), 3);

    // Align left through the Arrange menu: every left edge goes to the leftmost, 100.
    click(&mut h, "Arrange");
    click_item(&mut h, "Align left");
    let all = markups(&h);
    assert_eq!(all.len(), 3);
    for m in &all {
        assert!((rect_of(m).x0 - 100.0).abs() < 0.02, "{:?}", m.geometry);
    }
    // One undo step for the whole alignment.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    let all = markups(&h);
    assert!((rect_of(&all[1]).x0 - 200.0).abs() < 0.02);
    assert!((rect_of(&all[2]).x0 - 330.0).abs() < 0.02);
    assert_eq!(selected_ids(&h).len(), 3, "arranging keeps the selection");

    // Distribute across: the gaps between the three become equal.
    let ctx = h.ctx.clone();
    h.state_mut().run(
        &ctx,
        Command::Arrange(Arrange::Distribute(pl_tools::Axis::Horizontal)),
    );
    h.run();
    let all = markups(&h);
    let (ra, rb, rc) = (rect_of(&all[0]), rect_of(&all[1]), rect_of(&all[2]));
    assert!(
        ((rb.x0 - ra.x1) - (rc.x0 - rb.x1)).abs() < 0.05,
        "{ra:?} {rb:?} {rc:?}"
    );
    assert!((ra.x0 - 100.0).abs() < 0.02 && (rc.x1 - 380.0).abs() < 0.02);

    // Stacking: select the first drawn (the bottom one) and bring it to the front.
    h.key_press(Key::Escape);
    h.run();
    click_on_page(&mut h, 0, Point::new(130.0, 700.0));
    assert_eq!(selected_ids(&h), ids(&[&a]));
    h.state_mut()
        .run(&ctx, Command::Arrange(Arrange::BringToFront));
    h.run();
    let order: Vec<String> = markups(&h)
        .iter()
        .map(|m| m.id.as_str().to_owned())
        .collect();
    assert_eq!(order, ids(&[&b, &c, &a]));
    h.state_mut()
        .run(&ctx, Command::Arrange(Arrange::SendToBack));
    h.run();
    let order: Vec<String> = markups(&h)
        .iter()
        .map(|m| m.id.as_str().to_owned())
        .collect();
    assert_eq!(order, ids(&[&a, &b, &c]));
}

// ---- properties and presets ----------------------------------------------------------------

#[test]
fn a_property_change_applies_to_the_selection_and_undoes() {
    let (_mock, mut h) = mock_app(1);
    let drawn = draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    assert_eq!(drawn.style.stroke, Some(RED));
    assert_eq!(selected_ids(&h).len(), 1);

    // The panel is opened from the tool bar and shows the selection.
    assert!(!h.state().properties_panel_open());
    click(&mut h, "Properties panel");
    assert!(h.state().properties_panel_open());
    h.get_by_label("Rectangle selected");

    click(&mut h, "Stroke colour Blue");
    let now = markups(&h).pop().unwrap();
    assert_eq!(now.style.stroke, Some(BLUE));
    assert_eq!(now.style.fill, None);
    click(&mut h, "Fill colour Yellow");
    assert!(markups(&h).pop().unwrap().style.fill.is_some());
    h.get_by_label("Cloud border").click();
    h.run();
    assert!(markups(&h).pop().unwrap().style.cloud.is_some());

    // Each change is its own undo step.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    let now = markups(&h).pop().unwrap();
    assert!(now.style.cloud.is_none() && now.style.fill.is_some());
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    let now = markups(&h).pop().unwrap();
    assert_eq!(now.style.stroke, Some(RED));
    assert_eq!(now.style.fill, None);
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    assert_eq!(markups(&h).pop().unwrap().style.stroke, Some(BLUE));

    // A hex colour typed in, and a subject: typing a word is one undo step.
    h.get_by_label("Stroke colour as hex").focus();
    h.run();
    h.key_press_modifiers(CTRL, Key::A);
    h.get_by_label("Stroke colour as hex").type_text("#204060");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(
        markups(&h).pop().unwrap().style.stroke,
        Some(Rgb::from_u8(0x20, 0x40, 0x60))
    );
    h.get_by_label("Subject").focus();
    h.run();
    h.key_press_modifiers(CTRL, Key::A);
    h.get_by_label("Subject").type_text("Beam");
    h.run();
    h.get_by_label("Subject").type_text(" B2");
    h.run();
    assert_eq!(
        markups(&h).pop().unwrap().meta.subject.as_deref(),
        Some("Beam B2")
    );
    // Leave the text box, then undo once: the whole subject edit goes.
    h.get_by_label("Page view, fixture.pdf").focus();
    h.run();
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(
        markups(&h).pop().unwrap().meta.subject.as_deref(),
        Some("Rectangle")
    );
}

#[test]
fn tool_style_and_presets_persist_in_the_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mock = Arc::new(MockBackend::with_pages(&letters(1)));
    let mut state = state_with(mock.clone(), Some(data.clone()));
    state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);

    // With a drawing tool and nothing selected, the panel edits the tool's own style.
    click(&mut h, "Rectangle tool");
    click(&mut h, "Properties panel");
    h.get_by_label("Rectangle tool style");
    click(&mut h, "Stroke colour Blue");
    click(&mut h, "Fill colour Yellow");
    let current = h.state().presets().current(ToolKind::Rectangle);
    assert_eq!(current.style.stroke, Some(BLUE));
    assert!(markups(&h).is_empty(), "no markup was touched");

    // Saved under a name.
    h.get_by_label("Preset name").focus();
    h.run();
    h.get_by_label("Preset name").type_text("Site notes");
    h.run();
    click(&mut h, "Save preset");
    assert!(h
        .state()
        .presets()
        .find(ToolKind::Rectangle, "Site notes")
        .is_some());
    let file = data.join(pl_app::markup::PRESETS_FILE_NAME);
    assert!(file.is_file(), "the presets were not written to {file:?}");
    // The strip offers it; another tool does not.
    h.get_by_role_and_label(Role::Button, "Site notes");
    click(&mut h, "Ellipse tool");
    assert!(h.query_by_label("Site notes").is_none());

    // The next rectangle is drawn in that style.
    click(&mut h, "Rectangle tool");
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 700.0),
        Point::new(200.0, 640.0),
    );
    let drawn = markups(&h).pop().unwrap();
    assert_eq!(drawn.style.stroke, Some(BLUE));
    assert!(drawn.style.fill.is_some());
    drop(h);

    // A new session in the same directory has the preset and the tool's current style.
    let mut state = state_with(mock, Some(data));
    let saved = state
        .presets()
        .find(ToolKind::Rectangle, "Site notes")
        .cloned()
        .expect("the preset was read back");
    assert_eq!(saved.style.stroke, Some(BLUE));
    assert_eq!(
        state.presets().current(ToolKind::Rectangle).style.stroke,
        Some(BLUE)
    );
    // Back to red, then the preset is used again by name.
    state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);
    click(&mut h, "Rectangle tool");
    click(&mut h, "Properties panel");
    click(&mut h, "Stroke colour Red");
    assert_eq!(
        h.state()
            .presets()
            .current(ToolKind::Rectangle)
            .style
            .stroke,
        Some(RED)
    );
    click(&mut h, "Site notes");
    assert_eq!(
        h.state()
            .presets()
            .current(ToolKind::Rectangle)
            .style
            .stroke,
        Some(BLUE)
    );
    // Removing it takes it off the strip and out of the file.
    click(&mut h, "Remove \"Site notes\"");
    assert!(h
        .state()
        .presets()
        .find(ToolKind::Rectangle, "Site notes")
        .is_none());
    let json = std::fs::read_to_string(file).unwrap();
    assert!(!json.contains("Site notes"));
}

// ---- clipboard -----------------------------------------------------------------------------

#[test]
fn copy_cut_paste_and_duplicate() {
    let (_mock, mut h) = mock_app(2);
    let source = draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    assert!(!h.state().can_paste());

    // Ctrl+C then Ctrl+V: a copy one step down and to the right, selected.
    h.key_press_modifiers(CTRL, Key::C);
    h.run();
    assert!(h.state().can_paste());
    h.key_press_modifiers(CTRL, Key::V);
    h.run();
    let all = markups(&h);
    assert_eq!(all.len(), 2);
    let pasted = &all[1];
    assert_ne!(pasted.id, source.id);
    assert!(rect_near(
        rect_of(pasted),
        Rect::new(112.0, 588.0, 232.0, 688.0)
    ));
    assert_eq!(pasted.style, source.style);
    assert_eq!(selected_ids(&h), vec![pasted.id.as_str().to_owned()]);
    // A second paste steps on again.
    h.key_press_modifiers(CTRL, Key::V);
    h.run();
    let all = markups(&h);
    assert!(rect_near(
        rect_of(&all[2]),
        Rect::new(124.0, 576.0, 244.0, 676.0)
    ));

    // Duplicate copies the selection without touching the clipboard.
    h.key_press_modifiers(CTRL, Key::D);
    h.run();
    assert_eq!(markups(&h).len(), 4);
    // Cut removes it and keeps it for pasting; the platform's events work like the keys.
    h.event(egui::Event::Cut);
    h.run();
    assert_eq!(markups(&h).len(), 3);
    h.state_mut().active_view_mut().unwrap().go_to_page(1);
    h.run();
    let json = String::from("not markups");
    h.event(egui::Event::Paste(json));
    h.run();
    let all = markups(&h);
    assert_eq!(all.len(), 4);
    assert_eq!(all[3].page, 1, "pasted on the page in view");
    // Paste is one undo step.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(markups(&h).len(), 3);
}

// ---- modified marker and close prompt ------------------------------------------------------

#[test]
fn a_modified_document_is_marked_and_asks_before_closing() {
    let (_mock, mut h) = mock_app(1);
    let view = h.state().active().unwrap();
    assert!(!h.state().is_modified(view));
    assert_eq!(h.state().view_title(view), "fixture.pdf");
    h.get_by_role_and_label(Role::Button, "fixture.pdf");

    draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    assert!(h.state().is_modified(view));
    assert_eq!(h.state().view_title(view), "fixture.pdf *");
    h.get_by_role_and_label(Role::Button, "fixture.pdf *");

    // Undo back to the file's state: the mark goes.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert!(!h.state().is_modified(view));
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    assert!(h.state().is_modified(view));

    // Ctrl+W asks instead of closing.
    h.key_press_modifiers(CTRL, Key::W);
    h.run();
    assert!(h.state().close_prompt_open());
    h.get_by_label("Save changes?");
    assert_eq!(h.state().session().len(), 1);
    // While the prompt is up, shortcuts do nothing.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert!(h.state().is_modified(view));
    click(&mut h, "Cancel");
    assert!(!h.state().close_prompt_open());
    assert_eq!(h.state().session().len(), 1);

    // "Save and close" on a document that cannot be saved (it has no file, and the save
    // dialog is cancelled) keeps it open.
    h.key_press_modifiers(CTRL, Key::W);
    h.run();
    click(&mut h, "Save and close");
    assert!(!h.state().close_prompt_open());
    assert_eq!(h.state().session().len(), 1);
    assert!(h.state().is_modified(view));

    // The tab's own close button asks too; Discard closes.
    let ctx = h.ctx.clone();
    h.state_mut().request_close(view);
    h.run();
    assert!(h.state().close_prompt_open());
    click(&mut h, "Discard changes");
    settle(&mut h);
    assert!(h.state().session().is_empty());
    assert!(!h.state().close_prompt_open());

    // An unmodified document closes without a question.
    h.state_mut().open_bytes("other.pdf", Vec::new()).unwrap();
    settle(&mut h);
    h.state_mut().run(&ctx, Command::CloseTab);
    settle(&mut h);
    assert!(h.state().session().is_empty());
    assert!(!h.state().close_prompt_open());
}

#[test]
fn a_second_view_closes_without_a_prompt_and_quit_asks_per_document() {
    let mock = Arc::new(MockBackend::with_pages(&letters(1)));
    let mut state = state_with(mock, None);
    state.open_bytes("first.pdf", Vec::new()).unwrap();
    state.open_bytes("second.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::SplitSide);
    settle(&mut h);
    assert_eq!(h.state().view_ids().len(), 3);
    // Closing one of two views of the modified document loses nothing: no prompt.
    h.state_mut().run(&ctx, Command::CloseTab);
    settle(&mut h);
    assert!(!h.state().close_prompt_open());
    assert_eq!(h.state().view_ids().len(), 2);
    assert_eq!(h.state().session().len(), 2);

    // Exit asks about the modified document; Cancel leaves everything open.
    h.state_mut().run(&ctx, Command::Quit);
    h.run();
    assert!(h.state().close_prompt_open());
    h.state_mut().answer_close_prompt(&ctx, CloseChoice::Cancel);
    h.run();
    assert_eq!(h.state().session().len(), 2);
    // Discard lets the exit go on.
    h.state_mut().run(&ctx, Command::Quit);
    h.run();
    h.state_mut()
        .answer_close_prompt(&ctx, CloseChoice::Discard);
    settle(&mut h);
    assert!(!h.state().close_prompt_open());
    assert_eq!(
        h.state().session().len(),
        1,
        "only the unmodified one is left"
    );
}

// ---- accessibility -------------------------------------------------------------------------

#[test]
fn every_control_has_an_accessible_name() {
    let (_mock, mut h) = mock_app(2);
    draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    // The tool bar names every tool and tells its key in the tooltip.
    for tool in Tool::ALL {
        let name = format!("{} tool", tool.label());
        h.get_by_role_and_label(Role::Button, &name);
        assert!(tool.tooltip().contains(tool.key().name()));
        assert!(!tool.hint().is_empty());
    }
    h.get_by_role_and_label(Role::Button, "Snap to drawing content");

    let unnamed = |h: &App, what: &str| {
        let interactive = [
            Role::Button,
            Role::TextInput,
            Role::MultilineTextInput,
            Role::CheckBox,
            Role::RadioButton,
            Role::ComboBox,
            Role::Slider,
            Role::SpinButton,
            Role::MenuItem,
            Role::ScrollBar,
            Role::Document,
        ];
        let missing: Vec<String> = h
            .query_all(by().predicate(move |node| {
                interactive.contains(&node.role())
                    && node.label().is_none_or(|l| l.trim().is_empty())
                    && node.labelled_by().next().is_none()
            }))
            .map(|node| format!("{node:?}"))
            .collect();
        assert!(missing.is_empty(), "{what}: unnamed controls: {missing:#?}");
    };
    unnamed(&h, "viewer with a selected rectangle");

    // The properties panel, for a shape, a line and a tool.
    click(&mut h, "Properties panel");
    unnamed(&h, "properties of a rectangle");
    for name in [
        "Line width",
        "Dash style",
        "Opacity",
        "Author",
        "Subject",
        "Comment",
        "Stroke colour as hex",
        "Fill colour as hex",
        "No fill colour",
    ] {
        assert!(h.query_by_label(name).is_some(), "no control named {name}");
    }
    h.key_press(Key::Escape);
    h.run();
    assert!(selected_ids(&h).is_empty());
    set_tool(&mut h, Tool::Arrow);
    unnamed(&h, "properties of the arrow tool");
    for name in ["Line start", "Line end", "Preset name", "Save preset"] {
        assert!(h.query_by_label(name).is_some(), "no control named {name}");
    }
    // The menus.
    for menu in ["File", "Edit", "View", "Arrange"] {
        click(&mut h, menu);
        unnamed(&h, menu);
        h.key_press(Key::Escape);
        h.run();
    }
    // The dialogs.
    let view = h.state().active().unwrap();
    h.state_mut().request_close(view);
    h.run();
    unnamed(&h, "close prompt");
    click(&mut h, "Cancel");
}

// ---- snapshot ------------------------------------------------------------------------------

#[test]
fn copy_snapshot_puts_the_view_or_a_region_on_the_clipboard() {
    let (_mock, mut h) = mock_app(1);
    let clipboard = FakeClipboard::default();
    h.state_mut().set_clipboard(Box::new(clipboard.clone()));
    // A filled blue rectangle to look for.
    set_tool(&mut h, Tool::Rectangle);
    click(&mut h, "Properties panel");
    click(&mut h, "Stroke colour Blue");
    click(&mut h, "Fill colour Blue");
    click(&mut h, "Properties panel");
    settle(&mut h);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 700.0),
        Point::new(220.0, 600.0),
    );
    settle(&mut h);
    let rect = h.state().active_view().unwrap().rect;
    let inside = on_screen(&h, 0, Point::new(160.0, 650.0));
    let outside = on_screen(&h, 0, Point::new(300.0, 650.0));

    // The whole view.
    h.key_press_modifiers(CTRL_SHIFT, Key::C);
    h.run();
    let (w, hgt, pixels) = clipboard.contents().image.expect("an image was copied");
    assert_eq!(
        (w, hgt),
        (rect.width().round() as u32, rect.height().round() as u32)
    );
    assert_eq!(h.state().last_snapshot(), Some((w, hgt)));
    let at = |pos: egui::Pos2, w: u32, pixels: &[u8], origin: egui::Pos2| -> [u8; 4] {
        let (x, y) = ((pos.x - origin.x) as u32, (pos.y - origin.y) as u32);
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    assert_eq!(at(inside, w, &pixels, rect.min), [26, 77, 204, 255]);
    // Beside the rectangle the page's own pixels show: the mock's pattern at that place.
    let dv = h.state().active_view().unwrap();
    let scale = dv.view.visible_tiles()[0].grid.scale();
    let shown = dv.view.page_geometry(0).unwrap().page_to_display() * Point::new(300.0, 650.0);
    let want = MockBackend::pixel(0, (shown.x * scale) as u32, (shown.y * scale) as u32);
    let got = at(outside, w, &pixels, rect.min);
    for c in 0..3 {
        assert!(
            (got[c] as i32 - want[c] as i32).abs() <= 3,
            "{got:?} vs {want:?}"
        );
    }
    assert!(h.state().status().is_some_and(|s| s.contains("Copied")));

    // A dragged region: Edit > Copy snapshot of region, then a drag.
    click(&mut h, "Edit");
    click_item(&mut h, "Copy snapshot of region");
    assert!(h.state().status_info().hint.contains("Drag across"));
    let from = on_screen(&h, 0, Point::new(140.0, 680.0)).round();
    let to = from + egui::vec2(60.0, 40.0);
    drag(&mut h, from, to);
    let (w, hgt, pixels) = clipboard.contents().image.expect("the region was copied");
    assert_eq!((w, hgt), (60, 40));
    // The region lies wholly inside the blue rectangle, and no rectangle was drawn by the
    // drag although the Rectangle tool is active.
    assert!(pixels
        .as_chunks::<4>()
        .0
        .iter()
        .all(|p| *p == [26, 77, 204, 255]));
    assert_eq!(markups(&h).len(), 1);
    assert!(!h.state().status_info().hint.contains("Drag across"));
}

// ---- Multiply -----------------------------------------------------------------------------

/// The highlighter is Multiply: it is composited against the page tile on the CPU and painted
/// as a texture over it, so the page under it keeps showing through as it will in the file.
#[test]
fn the_highlighter_multiplies_with_the_page_under_it() {
    let _gpu = gpu();
    let (_mock, mut h) = mock_app(1);
    set_tool(&mut h, Tool::Highlighter);
    // A horizontal stroke 200 units below the top of the page; the default pen is 12 wide.
    drag_on_page(&mut h, 0, Point::new(60.0, 592.0), Point::new(200.0, 592.0));
    settle(&mut h);
    let image = h.render().expect("headless render");
    let dv = h.state().active_view().unwrap();
    let geometry = *dv.view.page_geometry(0).unwrap();
    let scale = dv.view.visible_tiles()[0].grid.scale();
    let sample = |point: Point| -> ([u8; 4], [u8; 4]) {
        let shown = geometry.page_to_display() * point;
        let base = MockBackend::pixel(0, (shown.x * scale) as u32, (shown.y * scale) as u32);
        let p = dv.view.page_to_view(0, point).unwrap();
        let px = (
            (dv.rect.min.x as f64 + p.x) as u32,
            (dv.rect.min.y as f64 + p.y) as u32,
        );
        (image.get_pixel(px.0, px.1).0, base)
    };
    // On the stroke: the page's colour times the highlighter's (1.0, 0.9, 0.2).
    let (got, base) = sample(Point::new(130.5, 592.5));
    let want = [base[0] as f64, base[1] as f64 * 0.9, base[2] as f64 * 0.2];
    assert!(
        base[1] > 100,
        "the sample must have green to lose: {base:?}"
    );
    for c in 0..3 {
        assert!(
            (got[c] as f64 - want[c]).abs() <= 6.0,
            "on the stroke: {got:?}, expected about {want:?} (page {base:?})"
        );
    }
    // Beside the stroke the page is untouched.
    let (got, base) = sample(Point::new(130.5, 560.5));
    for c in 0..3 {
        assert!(
            (got[c] as i32 - base[c] as i32).abs() <= 6,
            "beside the stroke: {got:?} vs {base:?}"
        );
    }
}

/// The painter adapter puts a markup where the page says it is, whichever way the view is
/// turned: a filled rectangle is its own colour inside and the page's outside.
#[test]
fn painted_markups_follow_the_page_at_every_view_rotation() {
    let _gpu = gpu();
    let (_mock, mut h) = mock_app(1);
    set_tool(&mut h, Tool::Rectangle);
    click(&mut h, "Properties panel");
    click(&mut h, "Stroke colour Blue");
    click(&mut h, "Fill colour Blue");
    click(&mut h, "Properties panel");
    let rect = Rect::new(300.0, 420.0, 380.0, 470.0);
    drag_on_page(
        &mut h,
        0,
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y0),
    );
    h.key_press(Key::Escape);
    h.run();
    for rotation in Rotation::ALL {
        {
            let dv = h.state_mut().active_view_mut().unwrap();
            dv.view.set_rotation(rotation);
            dv.view.reveal_rect(0, rect, 120.0);
        }
        settle(&mut h);
        let image = h.render().expect("headless render");
        let pixel = |point: Point| -> [u8; 4] {
            let pos = on_screen(&h, 0, point);
            image.get_pixel(pos.x as u32, pos.y as u32).0
        };
        let blue = |p: [u8; 4]| {
            (p[0] as i32 - 26).abs() <= 3
                && (p[1] as i32 - 77).abs() <= 3
                && (p[2] as i32 - 204).abs() <= 3
        };
        for inside in [
            rect.center(),
            Point::new(rect.x0 + 3.0, rect.y0 + 3.0),
            Point::new(rect.x1 - 3.0, rect.y1 - 3.0),
        ] {
            assert!(
                blue(pixel(inside)),
                "{rotation:?}: {inside:?} is {:?}",
                pixel(inside)
            );
        }
        for outside in [
            Point::new(rect.x0 - 4.0, rect.center().y),
            Point::new(rect.x1 + 4.0, rect.center().y),
            Point::new(rect.center().x, rect.y0 - 4.0),
            Point::new(rect.center().x, rect.y1 + 4.0),
        ] {
            assert!(
                !blue(pixel(outside)),
                "{rotation:?}: {outside:?} is painted"
            );
        }
    }
}

// ---- real PDFium ---------------------------------------------------------------------------

fn pdfium() -> Arc<dyn RenderBackend> {
    let dll = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    Arc::new(PdfiumBackend::new(Some(&dll)).expect("vendored pdfium.dll loads"))
}

fn write_fixture(dir: &Path, fixture: &pl_testkit::Fixture) -> PathBuf {
    let path = dir.join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    path
}

/// Opens `path` with PDFium at 100% in a fresh app.
fn real_app(path: &Path, data: Option<PathBuf>) -> App {
    let mut state = state_with(pdfium(), data);
    state.open_path(path).expect("the file opens");
    let mut h = harness(state);
    settle(&mut h);
    if !h.state().recovery_prompt_open() {
        h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
        settle(&mut h);
    }
    h
}

fn go_to(h: &mut App, page: u32) {
    h.state_mut().active_view_mut().unwrap().go_to_page(page);
    settle(h);
}

/// A rectangle drawn with the pointer on `page`, given as fractions of the page's visible box
/// so that it suits every fixture layout. Returns the rectangle asked for, in page space.
fn draw_fraction_rect(h: &mut App, page: u32, fx: [f64; 2], fy: [f64; 2]) -> Rect {
    let crop = h
        .state()
        .active_view()
        .unwrap()
        .view
        .file_geometry(page)
        .unwrap()
        .crop_box();
    let x = |f: f64| crop.x0 + f * crop.width();
    let y = |f: f64| crop.y0 + f * crop.height();
    let rect = Rect::new(x(fx[0]), y(fy[0]), x(fx[1]), y(fy[1]));
    // Bring it into view first: on a rotated page it is not where the numbers suggest.
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .view
        .reveal_rect(page, rect, 60.0);
    settle(h);
    set_tool(h, Tool::Rectangle);
    drag_on_page(
        h,
        page,
        Point::new(rect.x0, rect.y0),
        Point::new(rect.x1, rect.y1),
    );
    set_tool(h, Tool::Select);
    rect
}

/// Counts the strongly red pixels of a file's page inside `rect` (page space), rendered by a
/// PDFium of its own at one pixel per unit times `scale`.
fn red_pixels_in_unit(path: &Path, page: u32, rect: Rect, scale: f64, unit: f64) -> usize {
    let backend = pdfium();
    let info = backend
        .open(DocSource::file(path.to_path_buf()), None)
        .unwrap();
    if unit != 1.0 {
        backend.set_page_user_unit(info.doc, page, unit).unwrap();
    }
    let geometry = backend.page_info(info.doc, page).unwrap().geometry();
    let bitmap = backend.render_page(info.doc, page, scale).unwrap();
    backend.close(info.doc).unwrap();
    let shown = (pl_doc::pl_geom::Affine::scale(scale) * geometry.page_to_display())
        .transform_rect_bbox(rect);
    let mut count = 0;
    for y in (shown.y0.floor().max(0.0) as u32)..(shown.y1.ceil() as u32).min(bitmap.height) {
        for x in (shown.x0.floor().max(0.0) as u32)..(shown.x1.ceil() as u32).min(bitmap.width) {
            let [r, g, b, _] = bitmap.pixel(x, y);
            if r > 150 && g < 90 && b < 90 {
                count += 1;
            }
        }
    }
    count
}

fn red_pixels_in(path: &Path, page: u32, rect: Rect, scale: f64) -> usize {
    red_pixels_in_unit(path, page, rect, scale, 1.0)
}

/// Draw on every page layout, save into a temporary copy, and open that copy afresh: the
/// markups are in the file where they were drawn, PDFium draws them there, and the model
/// reads back as it was.
#[test]
fn real_pdfium_markups_are_saved_and_shown_after_reload_on_every_page_layout() {
    for fixture in [
        pl_testkit::rotated_pages(),
        pl_testkit::user_unit(),
        pl_testkit::crop_box(),
    ] {
        let name = fixture.manifest.file_name();
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &fixture);
        let original = fixture.bytes.clone();
        let mut h = real_app(&path, None);
        let view = h.state().active().unwrap();
        let pages = fixture.manifest.pages.len() as u32;
        let mut drawn: Vec<(u32, Rect)> = Vec::new();
        for page in 0..pages {
            go_to(&mut h, page);
            // A clear patch of every fixture page, away from its marks and labels.
            let rect = draw_fraction_rect(&mut h, page, [0.55, 0.75], [0.30, 0.42]);
            drawn.push((page, rect));
        }
        let before = markups(&h);
        assert_eq!(before.len(), pages as usize, "{name}");
        for ((page, rect), m) in drawn.iter().zip(&before) {
            assert_eq!(m.page, *page as usize, "{name}");
            assert_eq!(m.kind, AnnotKind::Square);
            assert!(
                rect_near(rect_of(m), *rect),
                "{name} page {page}: drew {:?}, asked for {rect:?}",
                m.geometry
            );
        }
        assert!(h.state().is_modified(view));
        assert!(h.state().status().is_none(), "{:?}", h.state().status());

        // Ctrl+S.
        h.key_press_modifiers(CTRL, Key::S);
        settle(&mut h);
        assert!(
            h.state().status().is_some_and(|s| s.starts_with("Saved")),
            "{name}: {:?}",
            h.state().status()
        );
        assert!(!h.state().is_modified(view), "{name}");
        assert_eq!(h.state().view_title(view), name);
        // An incremental save: the original bytes are an exact prefix.
        let saved = std::fs::read(&path).unwrap();
        assert!(
            saved.len() > original.len() && saved.starts_with(&original),
            "{name}"
        );
        // The window shows the saved file without complaint, and a second save writes nothing.
        assert!(h.state().textures().tile_count() > 0);
        h.key_press_modifiers(CTRL, Key::S);
        settle(&mut h);
        assert_eq!(h.state().status(), Some("Nothing to save"));
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        drop(h);

        // PDFium on its own draws each rectangle where it was drawn.
        for (page, rect) in &drawn {
            let unit = fixture.manifest.pages[*page as usize].user_unit;
            let red = red_pixels_in_unit(&path, *page, rect.inflate(1.0, 1.0), 2.0, unit);
            let outline = 2.0 * 2.0 * (rect.width() + rect.height());
            assert!(
                (red as f64) > outline * 0.6,
                "{name} page {page}: {red} red pixels in {rect:?}, expected about {outline}"
            );
            let beside = red_pixels_in_unit(&path, *page, rect.inflate(-6.0, -6.0), 2.0, unit);
            assert_eq!(beside, 0, "{name} page {page}: red inside the outline");
        }

        // A fresh app on the saved copy reads the same model back.
        let mut h = real_app(&path, None);
        assert!(markups(&h).is_empty(), "nothing is read until it is needed");
        h.state_mut().select_all_markups();
        h.run();
        let after = markups(&h);
        let ours: Vec<&Annotation> = after
            .iter()
            .filter(|a| a.meta.author.as_deref() == Some("Test Author"))
            .collect();
        assert_eq!(ours.len(), before.len(), "{name}");
        for (a, b) in ours.iter().zip(&before) {
            assert_eq!(a.id, b.id, "{name}");
            assert!(
                rect_near(rect_of(a), rect_of(b)),
                "{name}: {:?} vs {:?}",
                a.geometry,
                b.geometry
            );
            assert_eq!(a.style, b.style, "{name}");
            assert_eq!(a.meta.subject, b.meta.subject);
        }
        let view = h.state().active().unwrap();
        assert!(!h.state().is_modified(view));
    }
}

/// A markup that is already in the file is drawn by PDFium until it is edited; then it is
/// hidden there, its tiles are rendered again and the window paints it itself.
#[test]
fn real_pdfium_editing_a_file_markup_hides_it_in_the_page_render() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::annotations();
    let path = write_fixture(dir.path(), &fixture);
    let truth = fixture.manifest.pages[0]
        .annotations
        .iter()
        .find(|a| a.subtype == "Square")
        .expect("the fixture has a square")
        .clone();
    let mut h = real_app(&path, None);
    let view = h.state().active().unwrap();
    let [x0, y0, x1, y1] = truth.rect;
    let rect = Rect::new(x0, y0, x1, y1);
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .view
        .reveal_rect(0, rect, 120.0);
    settle(&mut h);

    // A click on its border selects it; nothing is hidden or modified by that.
    click_on_page(&mut h, 0, Point::new((x0 + x1) / 2.0, y0 + 0.5));
    let selected = h.state().selected_markups();
    assert_eq!(selected.len(), 1, "the file's square was not picked");
    assert_eq!(selected[0].kind, AnnotKind::Square);
    let id = selected[0].id.clone();
    let doc = h.state().active_document().unwrap();
    assert!(doc.hidden_annotations().is_empty());
    assert!(doc.overlay(0).is_empty());
    assert!(!h.state().is_modified(view));

    // Dragging it moves it: now it is ours to draw. (Not by the middle of the edge, which
    // is a resize handle once the square is selected.)
    let grab = Point::new(x0 + (x1 - x0) / 4.0, y0 + 0.5);
    drag_on_page(&mut h, 0, grab, grab + Vec2::new(40.0, 0.0));
    settle(&mut h);
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.hidden_annotations().len(), 1);
    let overlay = doc.overlay(0);
    assert_eq!(overlay.len(), 1);
    assert_eq!(overlay[0].id, id);
    let moved = doc.annotation(&id).unwrap();
    assert!((moved.geometry.bounds().x0 - (x0 + 40.0)).abs() < 0.6);
    assert!(h.state().is_modified(view));
    assert!(h.state().status().is_none(), "{:?}", h.state().status());
    assert!(h.state().textures().tile_count() > 0);

    // Undo: it is back where the file has it and there is nothing to save. Once edited it
    // stays ours to draw (pl-doc's rule), so it is still in the overlay, at its old place.
    h.key_press_modifiers(CTRL, Key::Z);
    settle(&mut h);
    let doc = h.state().active_document().unwrap();
    assert!(!h.state().is_modified(view));
    let back = doc.annotation(&id).unwrap();
    assert!((back.geometry.bounds().x0 - x0).abs() < 0.01);
    assert_eq!(doc.overlay(0).len(), 1);

    // Recolour it from the properties panel and save; the copy on disk has the new colour.
    click(&mut h, "Properties panel");
    click(&mut h, "Stroke colour Blue");
    assert!(h.state().is_modified(view));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::Save);
    settle(&mut h);
    assert!(
        h.state().status().is_some_and(|s| s.starts_with("Saved")),
        "{:?}",
        h.state().status()
    );
    let doc = h.state().active_document().unwrap();
    assert!(
        doc.hidden_annotations().is_empty(),
        "after a save the file is the truth"
    );
    drop(h);
    let mut h = real_app(&path, None);
    h.state_mut().select_all_markups();
    h.run();
    let again = markups(&h);
    let square = again.iter().find(|a| a.id == id).expect("the square");
    assert_eq!(square.style.stroke, Some(BLUE));
}

#[test]
fn real_pdfium_save_as_moves_the_tab_to_the_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::plain();
    let path = write_fixture(dir.path(), &fixture);
    let copy = dir.path().join("marked up.pdf");
    let mut state = AppState::new(
        pdfium(),
        AppConfig::default(),
        Box::new(ScriptedDialogs::default().with_saves([copy.clone()])),
    );
    state.set_author("Test Author");
    state.set_theme(egui::ThemePreference::Light);
    state.open_path(&path).unwrap();
    let mut h = harness(state);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    let view = h.state().active().unwrap();
    draw_rect(&mut h, 0, Rect::new(360.0, 600.0, 440.0, 650.0));

    // File > Save as: the dialog names the copy.
    click(&mut h, "File");
    click_item(&mut h, "Save as");
    settle(&mut h);
    assert!(copy.is_file(), "{:?}", h.state().status());
    assert_eq!(h.state().view_title(view), "marked up.pdf");
    assert!(!h.state().is_modified(view));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        fixture.bytes,
        "the original was written to"
    );
    assert_eq!(
        h.state().active_document().unwrap().path(),
        Some(copy.as_path())
    );
    assert!(red_pixels_in(&copy, 0, Rect::new(359.0, 599.0, 441.0, 651.0), 2.0) > 300);
    assert_eq!(
        red_pixels_in(&path, 0, Rect::new(359.0, 599.0, 441.0, 651.0), 2.0),
        0
    );
    // A second "save as" with the dialog cancelled changes nothing.
    assert!(!h.state_mut().save_active(SaveHow::As));
    // Save optimised rewrites the copy as one revision and it still opens.
    assert!(h.state_mut().save_active(SaveHow::Optimised));
    settle(&mut h);
    assert!(red_pixels_in(&copy, 0, Rect::new(359.0, 599.0, 441.0, 651.0), 2.0) > 300);
    assert!(h.state().textures().tile_count() > 0);
}

/// Snapping on: the pointer near a drawn endpoint gives the tool that endpoint exactly, bit
/// for bit, and the marker shows where.
#[test]
fn real_pdfium_snapping_picks_drawing_endpoints_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::plain();
    let path = write_fixture(dir.path(), &fixture);
    let mut h = real_app(&path, None);
    let view = h.state().active().unwrap();
    // The fixture has a short segment from (400.25, 150.5) to (450.75, 150.5) and a triangle
    // with a corner at (244, 208).
    let a = Point::new(400.25, 150.5);
    let b = Point::new(244.0, 208.0);
    h.state_mut().active_view_mut().unwrap().view.reveal_rect(
        0,
        Rect::new(230.0, 140.0, 460.0, 220.0),
        60.0,
    );
    settle(&mut h);

    // Off: the line starts where the pointer is.
    assert!(!h.state().snap_enabled());
    set_tool(&mut h, Tool::Line);
    let near_a = on_screen(&h, 0, a) + egui::vec2(-3.0, 4.0);
    let near_b = on_screen(&h, 0, b) + egui::vec2(2.0, -3.0);
    drag(&mut h, near_a, near_b);
    let free = markups(&h).pop().unwrap();
    let Geometry::Line { a: fa, .. } = free.geometry else {
        panic!()
    };
    assert!(fa != a && (fa - a).hypot() > 2.0);
    assert!(h.state().snap_marker(view).is_none());

    // On, with its key. The scene is built in the background and is ready after a settle.
    h.key_press(Key::S);
    h.run();
    assert!(h.state().snap_enabled());
    h.hover_at(near_a);
    settle(&mut h);
    h.hover_at(near_a);
    h.run();
    assert_eq!(h.state().snap_marker(view), Some((0, a)));
    drag(&mut h, near_a, near_b);
    let snapped = markups(&h).pop().unwrap();
    assert_eq!(
        snapped.geometry,
        Geometry::Line { a, b },
        "both ends are the drawing's own points"
    );
    // Away from any geometry nothing snaps.
    let nowhere = on_screen(&h, 0, Point::new(350.0, 180.0));
    h.hover_at(nowhere);
    h.run();
    assert!(h.state().snap_marker(view).is_none());
    // Alt lets go of the snap for one event.
    let alt = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    drag_with(&mut h, near_a, &[], near_b, alt);
    let loose = markups(&h).pop().unwrap();
    let Geometry::Line { a: la, .. } = loose.geometry else {
        panic!()
    };
    assert!(la != a);
    // The toolbar button turns it off again.
    click(&mut h, "Snap to drawing content");
    assert!(!h.state().snap_enabled());
}

/// Markups that were not saved when the app died are offered back on the next open.
#[test]
fn real_pdfium_unsaved_markups_are_offered_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let fixture = pl_testkit::plain();
    let path = write_fixture(dir.path(), &fixture);

    let mut h = real_app(&path, Some(data.clone()));
    assert!(!h.state().recovery_prompt_open());
    let first = draw_rect(&mut h, 0, Rect::new(360.0, 600.0, 440.0, 650.0));
    let second = draw_rect(&mut h, 0, Rect::new(330.0, 420.0, 420.0, 480.0));
    // The process "dies": no save, no close prompt, no discard.
    std::mem::forget(h);
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);

    // Next run: the prompt comes up with the document; Recover brings both rectangles back.
    let mut h = real_app(&path, Some(data.clone()));
    assert!(h.state().recovery_prompt_open());
    h.get_by_label("Recover unsaved markups?");
    assert!(markups(&h).is_empty());
    click(&mut h, "Recover");
    settle(&mut h);
    assert!(!h.state().recovery_prompt_open());
    let back = markups(&h);
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].id, first.id);
    assert_eq!(back[1].detached(), second.detached());
    let view = h.state().active().unwrap();
    assert!(h.state().is_modified(view));
    assert!(h.state().status().is_some_and(|s| s.contains("Recovered")));
    // The recovered history can be undone.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(markups(&h).len(), 1);
    std::mem::forget(h);

    // Dying again and declining: the record is thrown away and is not offered a third time.
    let mut h = real_app(&path, Some(data.clone()));
    assert!(h.state().recovery_prompt_open());
    click(&mut h, "Discard them");
    settle(&mut h);
    assert!(!h.state().recovery_prompt_open());
    h.state_mut().select_all_markups();
    h.run();
    assert!(markups(&h).is_empty());
    drop(h);
    let h = real_app(&path, Some(data));
    assert!(!h.state().recovery_prompt_open());
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
}

/// The real sample drawing set, when it is there: a markup on its first and last sheet is
/// saved to a temporary copy (never to the sample) and read back.
#[test]
fn real_sample_markups_save_to_a_copy_and_reload() {
    let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("samples")
        .join("site-development-plans.pdf");
    if !sample.is_file() {
        println!("skipped: the sample file is not present");
        return;
    }
    let stamp = |p: &Path| {
        let meta = std::fs::metadata(p).unwrap();
        (meta.len(), meta.modified().unwrap())
    };
    let before = stamp(&sample);
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("sample copy.pdf");
    let mut state = AppState::new(
        pdfium(),
        AppConfig::default(),
        Box::new(ScriptedDialogs::default().with_saves([copy.clone()])),
    );
    state.set_author("Test Author");
    state.set_theme(egui::ThemePreference::Light);
    state.open_path(&sample).expect("the sample opens");
    let mut h = harness(state);
    settle(&mut h);
    let pages = h.state().active_view().unwrap().view.page_count();
    assert!(pages >= 2);
    let mut drawn = Vec::new();
    for page in [0, pages - 1] {
        go_to(&mut h, page);
        h.state_mut().active_view_mut().unwrap().view.set_zoom(0.25);
        settle(&mut h);
        drawn.push((
            page,
            draw_fraction_rect(&mut h, page, [0.40, 0.50], [0.40, 0.50]),
        ));
    }
    assert_eq!(
        markups(&h)
            .iter()
            .filter(|a| a.baseline().is_none())
            .count(),
        2
    );
    assert!(
        h.state_mut().save_active(SaveHow::As),
        "{:?}",
        h.state().status()
    );
    settle(&mut h);
    assert!(copy.is_file());
    assert_eq!(stamp(&sample), before, "the sample itself was touched");
    drop(h);

    let mut h = real_app(&copy, None);
    h.state_mut().select_all_markups();
    h.run();
    let ours: Vec<Annotation> = markups(&h)
        .into_iter()
        .filter(|a| a.meta.author.as_deref() == Some("Test Author"))
        .collect();
    assert_eq!(ours.len(), 2);
    for (a, (page, rect)) in ours.iter().zip(&drawn) {
        assert_eq!(a.page, *page as usize);
        let got = rect_of(a);
        assert!(
            (got.x0 - rect.x0).abs() < 0.05 && (got.y1 - rect.y1).abs() < 0.05,
            "{got:?} vs {rect:?}"
        );
    }
    assert_eq!(stamp(&sample), before);
}

// ---- view rotation -------------------------------------------------------------------------

/// With the view turned, a drag still draws where the pointer is on the page, and the arrow
/// keys nudge the way they point on screen.
#[test]
fn tools_follow_the_view_rotation() {
    let (_mock, mut h) = mock_app(1);
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .view
        .set_rotation(Rotation::R90);
    settle(&mut h);
    let rect = Rect::new(300.0, 300.0, 380.0, 360.0);
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .view
        .reveal_rect(0, rect, 80.0);
    settle(&mut h);
    let drawn = draw_rect(&mut h, 0, rect);
    assert!(rect_near(rect_of(&drawn), rect), "{:?}", drawn.geometry);
    // Right on screen is down the page's own y axis when the page is turned clockwise.
    h.key_press(Key::ArrowRight);
    h.run();
    let nudged = markups(&h).pop().unwrap();
    let by = rect_of(&nudged).center() - rect.center();
    let on_screen_right = pl_tools::to_page(Rotation::R90, Vec2::new(1.0, 0.0));
    assert!((by - on_screen_right).hypot() < 0.02, "{by:?}");
    assert!(by.x.abs() < 0.02 && by.y.abs() > 0.9, "{by:?}");
}

// ---- snapshot of the interface (goldens depend on the GPU adapter) -------------------------

#[test]
fn snapshot_markup_editing_light() {
    let _gpu = gpu();
    let (_mock, mut h) = mock_app(2);
    set_tool(&mut h, Tool::Cloud);
    for p in [
        Point::new(330.0, 700.0),
        Point::new(480.0, 700.0),
        Point::new(480.0, 610.0),
        Point::new(330.0, 610.0),
    ] {
        click_on_page(&mut h, 0, p);
    }
    h.key_press(Key::Enter);
    h.run();
    set_tool(&mut h, Tool::Arrow);
    drag_on_page(
        &mut h,
        0,
        Point::new(120.0, 560.0),
        Point::new(300.0, 640.0),
    );
    set_tool(&mut h, Tool::Highlighter);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 500.0),
        Point::new(300.0, 500.0),
    );
    draw_rect(&mut h, 0, Rect::new(100.0, 660.0, 240.0, 740.0));
    click(&mut h, "Properties panel");
    click(&mut h, "Fill colour Yellow");
    settle(&mut h);
    h.snapshot("markup_editing_light");
}
