//! Regressions found in review, driven through `egui_kittest`: keyboard focus on a control,
//! keys with a drawing tool active, locked markups, paste placement, Space during a gesture,
//! small markups at a low zoom, the presets file, Save As onto an open file, and layers. No
//! window is opened and the real clipboard is never touched.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use pl_app::{AppConfig, AppState, Command, SaveHow, ScriptedDialogs, SidePage, Tool};
use pl_doc::pl_annot::{Annotation, Flags, Geometry, Origin, PdfDate};
use pl_doc::pl_geom::{PageGeometry, Point, Rect, Rotation};
use pl_doc::pl_render::{MockBackend, PdfiumBackend, RenderBackend};
use pl_doc::Clipboard;
use pl_tools::ToolKind;

type App = Harness<'static, AppState>;

const CTRL: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    mac_cmd: false,
    command: true,
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

/// A mock-backed app with one document of the given pages open at 100%.
fn mock_pages(pages: &[PageGeometry], data: Option<PathBuf>) -> App {
    let mock = Arc::new(MockBackend::with_pages(pages));
    let mut state = state_with(mock, data);
    state
        .open_bytes("fixture.pdf", Vec::new())
        .expect("mock open");
    let mut h = harness_dt(state, 0.25);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    h
}

fn mock_app(pages: usize) -> App {
    mock_pages(&letters(pages), None)
}

fn click(h: &mut App, name: &str) {
    h.get_by_role_and_label(Role::Button, name).click();
    h.run();
    h.remove_cursor();
    h.run();
}

fn on_screen(h: &App, page: u32, point: Point) -> egui::Pos2 {
    let dv = h.state().active_view().expect("a view");
    let p = dv.view.page_to_view(page, point).expect("page");
    let pos = egui::pos2(dv.rect.min.x + p.x as f32, dv.rect.min.y + p.y as f32);
    assert!(dv.rect.contains(pos), "{point:?} is off screen at {pos:?}");
    pos
}

fn press(h: &mut App, pos: egui::Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
    h.run();
}

fn drag(h: &mut App, from: egui::Pos2, to: egui::Pos2) {
    h.hover_at(from);
    h.run();
    press(h, from, true);
    h.event(egui::Event::PointerMoved(from + (to - from) * 0.5));
    h.run();
    h.event(egui::Event::PointerMoved(to));
    h.run();
    press(h, to, false);
    h.remove_cursor();
    h.run();
}

fn drag_on_page(h: &mut App, page: u32, a: Point, b: Point) {
    let (from, to) = (on_screen(h, page, a), on_screen(h, page, b));
    drag(h, from, to);
}

fn click_on_page(h: &mut App, page: u32, point: Point) {
    let pos = on_screen(h, page, point);
    h.hover_at(pos);
    h.run();
    h.drag_at(pos);
    h.run();
    h.drop_at(pos);
    h.run();
}

fn set_tool(h: &mut App, tool: Tool) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::Tool(tool));
    h.run();
}

fn markups(h: &App) -> Vec<Annotation> {
    h.state().markups()
}

fn selected(h: &App) -> usize {
    h.state().selected_markups().len()
}

fn rect_of(a: &Annotation) -> Rect {
    match &a.geometry {
        Geometry::Rect(r) => *r,
        other => panic!("not a rectangle: {other:?}"),
    }
}

/// Draws a rectangle with the Rectangle tool; the tool stays active and the shape selected.
fn draw_rect_keep_tool(h: &mut App, page: u32, rect: Rect) -> Annotation {
    set_tool(h, Tool::Rectangle);
    drag_on_page(
        h,
        page,
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y0),
    );
    markups(h).pop().expect("the rectangle")
}

fn draw_rect(h: &mut App, page: u32, rect: Rect) -> Annotation {
    let a = draw_rect_keep_tool(h, page, rect);
    set_tool(h, Tool::Select);
    a
}

// ---- keyboard focus ----------------------------------------------------------------------------

#[test]
fn a_focused_control_keeps_the_plain_keys() {
    let mut h = mock_app(2);
    let drawn = draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    assert_eq!(selected(&h), 1);
    click(&mut h, "Properties panel");
    assert!(drawn.style.cloud.is_none());

    // Enter on a focused checkbox ticks it; it does not open the comment editor.
    h.get_by_label("Cloud border").focus();
    h.run();
    h.key_press(Key::Enter);
    h.run();
    let now = markups(&h).pop().unwrap();
    assert!(now.style.cloud.is_some(), "Enter activated the checkbox");
    assert!(
        !h.get_by_label("Comment").is_focused(),
        "Enter was not taken for the comment editor"
    );
    // A tool letter does not change the tool, Delete does not delete.
    h.key_press(Key::E);
    h.run();
    assert_eq!(h.state().tool(), Tool::Select);
    h.key_press(Key::Delete);
    h.run();
    assert_eq!(markups(&h).len(), 1);

    // The arrow keys move a focused slider, not the selection and not the page.
    let before = markups(&h).pop().unwrap();
    assert_eq!(before.style.opacity, 1.0);
    h.get_by_role(Role::Slider).focus();
    h.run();
    h.key_press(Key::ArrowLeft);
    h.run();
    let after = markups(&h).pop().unwrap();
    assert!(after.style.opacity < 1.0, "the slider moved");
    assert_eq!(
        after.geometry, before.geometry,
        "the selection was not nudged"
    );
    assert_eq!(h.state().active_view().unwrap().shown_page(), 0);

    // Focus on the page itself is no one's: the keys act on the page again.
    h.get_by_label("Page view, fixture.pdf").focus();
    h.run();
    h.key_press(Key::ArrowRight);
    h.run();
    let nudged = markups(&h).pop().unwrap();
    assert_ne!(nudged.geometry, after.geometry, "the selection was nudged");
    h.key_press(Key::E);
    h.run();
    assert_eq!(h.state().tool(), Tool::Ellipse);

    // Shortcuts with Ctrl work wherever the focus is.
    h.get_by_label("Cloud border").focus();
    h.run();
    let count = markups(&h).len();
    h.key_press_modifiers(CTRL, Key::D);
    h.run();
    assert_eq!(markups(&h).len(), count + 1);
}

// ---- keys with a drawing tool active --------------------------------------------------------------

#[test]
fn delete_and_enter_act_on_the_selection_while_a_drawing_tool_is_active() {
    let mut h = mock_app(1);
    draw_rect_keep_tool(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    assert_eq!(h.state().tool(), Tool::Rectangle);
    assert_eq!(selected(&h), 1);

    // Enter opens the comment of the shape just drawn.
    assert!(!h.state().properties_panel_open());
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().properties_panel_open());
    h.run();
    assert!(h.get_by_label("Comment").is_focused());
    h.get_by_label("Page view, fixture.pdf").focus();
    h.run();

    // Delete removes it; the tool stays.
    h.key_press(Key::Delete);
    h.run();
    assert!(markups(&h).is_empty());
    assert_eq!(h.state().tool(), Tool::Rectangle);
    // And it is one undo step.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(markups(&h).len(), 1);

    // Backspace too, with another tool.
    set_tool(&mut h, Tool::Line);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 700.0),
        Point::new(380.0, 650.0),
    );
    assert_eq!(markups(&h).len(), 2);
    h.key_press(Key::Backspace);
    h.run();
    assert_eq!(markups(&h).len(), 1);

    // While a polygon is part drawn the keys are the tool's own.
    set_tool(&mut h, Tool::Polygon);
    click_on_page(&mut h, 0, Point::new(300.0, 500.0));
    click_on_page(&mut h, 0, Point::new(360.0, 500.0));
    click_on_page(&mut h, 0, Point::new(330.0, 440.0));
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(markups(&h).len(), 2, "Enter finished the polygon");
}

// ---- locked markups --------------------------------------------------------------------------------

/// Pastes a locked rectangle (as another Plumbline window would hand it over) and returns it.
fn paste_locked(h: &mut App, rect: Rect) -> Annotation {
    let origin = Origin::new("Someone", PdfDate::from_unix(1_700_000_000));
    let mut locked = Annotation::square(0, rect, &origin);
    locked.meta.flags = Flags(Flags::PRINT | Flags::LOCKED);
    let json = Clipboard::from_annotations([&locked]).to_json();
    h.event(egui::Event::Paste(json));
    h.run();
    let pasted = markups(h).pop().expect("the pasted rectangle");
    assert!(pasted.meta.flags.is_locked());
    pasted
}

#[test]
fn the_menu_delete_and_cut_leave_locked_markups() {
    let mut h = mock_app(1);
    let free = draw_rect(&mut h, 0, Rect::new(100.0, 600.0, 220.0, 700.0));
    let locked = paste_locked(&mut h, Rect::new(300.0, 500.0, 380.0, 560.0));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::SelectAllMarkups);
    h.run();
    assert_eq!(selected(&h), 2);

    // Edit > Delete.
    h.state_mut().run(&ctx, Command::Delete);
    h.run();
    let left = markups(&h);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, locked.id, "the locked one stays");
    assert_eq!(
        h.state().status(),
        Some("1 locked markup(s) were left"),
        "and the status bar says so"
    );

    // Undo, then cut: the same.
    h.state_mut().run(&ctx, Command::Undo);
    h.run();
    assert_eq!(markups(&h).len(), 2);
    h.state_mut().run(&ctx, Command::SelectAllMarkups);
    h.run();
    h.state_mut().run(&ctx, Command::Cut);
    h.run();
    let left = markups(&h);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, locked.id);
    assert!(h.state().can_paste());
    // What was cut is the free one only.
    h.state_mut().run(&ctx, Command::Paste);
    h.run();
    let all = markups(&h);
    assert_eq!(all.len(), 2);
    assert!(!all[1].meta.flags.is_locked());
    assert_eq!(rect_of(&all[1]).width(), rect_of(&free).width());

    // Only a locked markup selected: nothing happens.
    let border = rect_of(&locked);
    click_on_page(&mut h, 0, Point::new(border.x0 + 0.5, border.center().y));
    assert_eq!(selected(&h), 1);
    assert!(!h.state_mut().delete_selection());
    assert_eq!(markups(&h).len(), 2);
}

// ---- paste placement -------------------------------------------------------------------------------

fn inside(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 - 1e-6
        && inner.y0 >= outer.y0 - 1e-6
        && inner.x1 <= outer.x1 + 1e-6
        && inner.y1 <= outer.y1 + 1e-6
}

#[test]
fn a_paste_lands_on_the_visible_page() {
    // Page 0 is Letter; page 1 is a small sheet whose visible box is offset from the origin.
    let crop = Rect::new(700.0, 900.0, 900.0, 1050.0);
    let small = PageGeometry::new(
        Rect::new(650.0, 850.0, 950.0, 1100.0),
        Some(crop),
        Rotation::R0,
        1.0,
    );
    let mut h = mock_pages(&[PageGeometry::simple(612.0, 792.0), small], None);
    let source = draw_rect(&mut h, 0, Rect::new(400.0, 600.0, 520.0, 700.0));
    h.key_press_modifiers(CTRL, Key::C);
    h.run();

    // On the same page a paste is a step away, as before.
    h.key_press_modifiers(CTRL, Key::V);
    h.run();
    let pasted = markups(&h).pop().unwrap();
    let r = rect_of(&pasted);
    assert!(
        (r.x0 - 412.0).abs() < 0.02 && (r.y1 - 688.0).abs() < 0.02,
        "{r:?}"
    );

    // On the small page the copied coordinates are nowhere near the sheet: the copy goes to
    // the part of the page in view, inside the visible box, at its own size.
    h.state_mut().active_view_mut().unwrap().go_to_page(1);
    settle(&mut h);
    h.key_press_modifiers(CTRL, Key::V);
    h.run();
    let pasted = markups(&h).pop().unwrap();
    assert_eq!(pasted.page, 1);
    let r = rect_of(&pasted);
    assert!(inside(crop, r), "{r:?} is outside the visible box {crop:?}");
    let s = rect_of(&source);
    assert!((r.width() - s.width()).abs() < 1e-6 && (r.height() - s.height()).abs() < 1e-6);
    assert_eq!(pasted.style, source.style);

    // Pasting again and again never walks off the page.
    h.state_mut().active_view_mut().unwrap().go_to_page(0);
    settle(&mut h);
    let page = Rect::new(0.0, 0.0, 612.0, 792.0);
    for n in 0..40 {
        h.key_press_modifiers(CTRL, Key::V);
        h.run();
        let pasted = markups(&h).pop().unwrap();
        if pasted.page == 0 {
            assert!(
                inside(page, rect_of(&pasted)),
                "paste {n}: {:?}",
                rect_of(&pasted)
            );
        }
    }
}

// ---- Space during a gesture ---------------------------------------------------------------------

#[test]
fn space_during_a_drag_does_not_strand_the_gesture() {
    let mut h = mock_app(1);
    set_tool(&mut h, Tool::Rectangle);
    let view = h.state().active().unwrap();
    let from = on_screen(&h, 0, Point::new(100.0, 700.0));
    let mid = on_screen(&h, 0, Point::new(160.0, 670.0));
    let to = on_screen(&h, 0, Point::new(220.0, 640.0));
    let scroll = h.state().active_view().unwrap().view.scroll();
    h.hover_at(from);
    h.run();
    press(&mut h, from, true);
    h.event(egui::Event::PointerMoved(mid));
    h.run();
    assert!(h.state().preview(view).is_some());

    // Space goes down in the middle of the drag; the button is released while it is held.
    h.key_down(Key::Space);
    h.run();
    h.event(egui::Event::PointerMoved(to));
    h.run();
    press(&mut h, to, false);
    h.key_up(Key::Space);
    h.run();

    // The gesture ended with the release: the shape exists, and moving the pointer with no
    // button held drags nothing.
    assert_eq!(markups(&h).len(), 1, "the drag finished as a rectangle");
    assert!(h.state().preview(view).is_none());
    assert_eq!(
        h.state().active_view().unwrap().view.scroll(),
        scroll,
        "the page did not pan under a drag that was already the tool's"
    );
    let away = on_screen(&h, 0, Point::new(400.0, 400.0));
    h.event(egui::Event::PointerMoved(away));
    h.run();
    assert!(
        h.state().preview(view).is_none(),
        "nothing follows the pointer"
    );
    assert_eq!(markups(&h).len(), 1);

    // Space held before the press still pans.
    set_tool(&mut h, Tool::Select);
    h.key_down(Key::Space);
    h.run();
    h.state_mut().active_view_mut().unwrap().view.set_zoom(3.0);
    h.run();
    let scroll = h.state().active_view().unwrap().view.scroll();
    let centre = h.state().active_view().unwrap().rect.center();
    h.hover_at(centre);
    h.run();
    h.drag_at(centre);
    h.run();
    h.hover_at(centre + egui::vec2(-60.0, -40.0));
    h.run();
    h.drop_at(centre + egui::vec2(-60.0, -40.0));
    h.run();
    h.key_up(Key::Space);
    h.run();
    assert_ne!(h.state().active_view().unwrap().view.scroll(), scroll);
}

// ---- a small markup at a low zoom ------------------------------------------------------------------

#[test]
fn a_small_markup_at_a_low_zoom_moves_when_dragged_by_its_middle() {
    let mut h = mock_app(1);
    let drawn = draw_rect(&mut h, 0, Rect::new(200.0, 400.0, 320.0, 500.0));
    // At 5% the 120 x 100 pt rectangle is 6 x 5 pixels: all of it within reach of a handle.
    h.state_mut().active_view_mut().unwrap().view.set_zoom(0.05);
    settle(&mut h);
    assert!(h.state().active_view().unwrap().view.zoom() < 0.06);
    let centre = on_screen(&h, 0, Point::new(260.0, 450.0));
    // Select it (it was deselected by nothing, but be sure), then drag by its middle.
    click_on_page(&mut h, 0, Point::new(260.0, 450.0));
    assert_eq!(selected(&h), 1);
    // Page units per pixel at this zoom (100% is the sheet at its real size, not 1 px/pt).
    let per_px = 100.0 / f64::from(on_screen(&h, 0, Point::new(360.0, 450.0)).x - centre.x);
    assert!(per_px > 12.0, "{per_px}");
    assert!(
        120.0 / per_px < 2.0 * pl_tools::HANDLE_PX,
        "the rectangle is smaller on screen than two handle reaches"
    );
    drag(&mut h, centre, centre + egui::vec2(10.0, 6.0));
    let moved = markups(&h).pop().unwrap();
    let (a, b) = (rect_of(&drawn), rect_of(&moved));
    assert!(
        (b.width() - a.width()).abs() < 1e-6 && (b.height() - a.height()).abs() < 1e-6,
        "moved, not resized: {a:?} -> {b:?}"
    );
    // Ten pixels to the right and six down, in page units of this zoom.
    assert!((b.x0 - a.x0 - 10.0 * per_px).abs() < 2.0, "{a:?} -> {b:?}");
    assert!((a.y0 - b.y0 - 6.0 * per_px).abs() < 2.0, "{a:?} -> {b:?}");

    // From just outside a corner it is still resized: the handles reach beyond the markup.
    let corner = on_screen(&h, 0, Point::new(b.x1, b.y0));
    drag(
        &mut h,
        corner + egui::vec2(2.0, 2.0),
        corner + egui::vec2(22.0, 17.0),
    );
    let resized = rect_of(&markups(&h).pop().unwrap());
    assert!(
        resized.width() > b.width() + 10.0 * per_px
            && resized.height() > b.height() + 10.0 * per_px,
        "a handle drag resizes: {b:?} -> {resized:?}"
    );
    assert!((resized.x0 - b.x0).abs() < 1e-6 && (resized.y1 - b.y1).abs() < 1e-6);
}

// ---- the presets file ------------------------------------------------------------------------------

#[test]
fn a_tool_style_change_is_written_once_it_has_settled() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let file = data.join("presets.json");
    let mock = Arc::new(MockBackend::with_pages(&letters(1)));
    let mut state = state_with(mock, Some(data.clone()));
    state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    // Frames a hundredth of a second apart, so the delay is many frames long.
    let mut h = harness_dt(state, 0.01);
    settle(&mut h);
    click(&mut h, "Rectangle tool");
    click(&mut h, "Properties panel");
    for _ in 0..80 {
        h.step();
    }
    assert!(!h.state().presets_unsaved());
    assert!(!file.exists(), "nothing has changed yet");

    // Five steps of the Opacity slider in five frames: the style follows at once, the file
    // does not.
    h.get_by_role(Role::Slider).focus();
    // One frame gives it the focus, the next has it hold the arrow keys.
    h.step();
    h.step();
    assert!(h.get_by_role(Role::Slider).is_focused());
    for _ in 0..5 {
        h.key_press(Key::ArrowLeft);
        h.step();
        h.step();
    }
    let opacity = h
        .state()
        .presets()
        .current(ToolKind::Rectangle)
        .style
        .opacity;
    assert!(opacity < 0.9, "{opacity}");
    assert!(h.state().presets_unsaved());
    assert!(
        !file.exists(),
        "not written while the control is still moving"
    );

    // Left alone for the delay, it is written, once, with the final value.
    for _ in 0..80 {
        h.step();
    }
    assert!(!h.state().presets_unsaved());
    let json = std::fs::read_to_string(&file).expect("the presets were written");
    let stored: pl_tools::PresetLibrary = serde_json::from_str(&json).unwrap();
    assert_eq!(stored.current(ToolKind::Rectangle).style.opacity, opacity);

    // A change that has not settled when the window closes is written by the shutdown.
    h.key_press(Key::ArrowLeft);
    h.step();
    h.step();
    assert!(h.state().presets_unsaved());
    let latest = h
        .state()
        .presets()
        .current(ToolKind::Rectangle)
        .style
        .opacity;
    assert!(latest < opacity);
    h.state_mut().shutdown();
    let json = std::fs::read_to_string(&file).unwrap();
    let stored: pl_tools::PresetLibrary = serde_json::from_str(&json).unwrap();
    assert_eq!(stored.current(ToolKind::Rectangle).style.opacity, latest);
}

// ---- real PDFium -----------------------------------------------------------------------------------

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

#[test]
fn real_pdfium_save_as_onto_a_file_open_in_another_tab_is_refused() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::plain();
    let path_a = dir_a.path().join(fixture.manifest.file_name());
    let path_b = dir_b.path().join(fixture.manifest.file_name());
    std::fs::write(&path_a, &fixture.bytes).unwrap();
    std::fs::write(&path_b, &fixture.bytes).unwrap();
    let mut state = AppState::new(
        pdfium(),
        AppConfig::default(),
        Box::new(ScriptedDialogs::default().with_saves([path_a.clone()])),
    );
    state.set_author("Test Author");
    state.set_theme(egui::ThemePreference::Light);
    state.open_path(&path_a).unwrap();
    let view_b = state.open_path(&path_b).unwrap();
    let mut h = harness_dt(state, 0.25);
    settle(&mut h);
    assert_eq!(h.state().active(), Some(view_b));
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    draw_rect(&mut h, 0, Rect::new(360.0, 600.0, 440.0, 650.0));
    assert!(h.state().is_modified(view_b));

    // Save As, and the dialog names the file the other tab has open.
    assert!(!h.state_mut().save_active(SaveHow::As));
    h.run();
    let status = h.state().status().unwrap_or_default().to_owned();
    assert!(status.contains("open in another tab"), "{status}");
    assert_eq!(std::fs::read(&path_a).unwrap(), fixture.bytes);
    assert_eq!(std::fs::read(&path_b).unwrap(), fixture.bytes);
    assert!(
        h.state().is_modified(view_b),
        "the changes are still unsaved"
    );
    assert_eq!(
        h.state().active_document().unwrap().path(),
        Some(path_b.as_path())
    );
    // A plain save still goes to its own file.
    assert!(h.state_mut().save_active(SaveHow::InPlace));
    settle(&mut h);
    assert!(!h.state().is_modified(view_b));
    assert_eq!(std::fs::read(&path_a).unwrap(), fixture.bytes);
    assert_ne!(std::fs::read(&path_b).unwrap(), fixture.bytes);
}

/// How many pixels of the first page, as the render mirror shows it, are mostly red.
fn red_pixels(h: &App) -> usize {
    let doc = h.state().active_document().expect("a document");
    let bitmap = doc
        .backend()
        .render_page(doc.render_id(), 0, 1.0)
        .expect("render");
    bitmap
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 180 && px[1] < 90 && px[2] < 90)
        .count()
}

#[test]
fn real_pdfium_layers_are_switched_from_the_side_panel() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::layers();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    let mut state = state_with(pdfium(), None);
    state.open_path(&path).expect("the file opens");
    let mut h = harness_dt(state, 0.25);
    settle(&mut h);
    let view = h.state().active().unwrap();
    assert!(h.state().layers().is_empty(), "not read until asked for");
    let with_red = red_pixels(&h);
    assert!(with_red > 500, "{with_red}");

    // The Layers page of the side panel lists them, each with a named box.
    h.state_mut().set_side_page(SidePage::Layers);
    settle(&mut h);
    assert_eq!(
        h.state().layers(),
        [("Layer A".to_owned(), true), ("Layer B".to_owned(), true)]
    );
    h.get_by_role_and_label(Role::CheckBox, "Layer B");
    let tiles_before = h.state().textures().tile_count();
    assert!(tiles_before > 0);

    // Unticking Layer A (the red lines) takes it out of the page.
    h.get_by_role_and_label(Role::CheckBox, "Layer A").click();
    h.run();
    h.remove_cursor();
    settle(&mut h);
    assert_eq!(
        h.state().layers(),
        [("Layer A".to_owned(), false), ("Layer B".to_owned(), true)]
    );
    assert_eq!(red_pixels(&h), 0, "the red layer is hidden");
    assert!(
        h.state().textures().tile_count() > 0,
        "the page was drawn again"
    );
    assert!(
        !h.state().is_modified(view),
        "a layer toggle is not an edit"
    );
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);

    // Ticking it again brings it back.
    h.get_by_role_and_label(Role::CheckBox, "Layer A").click();
    h.run();
    h.remove_cursor();
    settle(&mut h);
    assert_eq!(red_pixels(&h), with_red);

    // "Show as saved" undoes every toggle at once.
    h.get_by_role_and_label(Role::CheckBox, "Layer B").click();
    h.run();
    h.remove_cursor();
    settle(&mut h);
    assert!(!h.state().layers()[1].1);
    click(&mut h, "Show as saved");
    settle(&mut h);
    assert!(h.state().layers().iter().all(|(_, shown)| *shown));
    assert_eq!(red_pixels(&h), with_red);

    // A markup drawn with a layer off is saved, and the layer is still off afterwards.
    h.get_by_role_and_label(Role::CheckBox, "Layer A").click();
    h.run();
    h.remove_cursor();
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    set_tool(&mut h, Tool::Ellipse);
    drag_on_page(
        &mut h,
        0,
        Point::new(380.0, 560.0),
        Point::new(460.0, 620.0),
    );
    assert_eq!(markups(&h).len(), 1);
    assert!(h.state_mut().save_active(SaveHow::InPlace));
    settle(&mut h);
    assert!(!h.state().layers()[0].1, "layer A is still off");
    assert!(
        red_pixels(&h) < with_red / 2,
        "only the new ellipse is red: {}",
        red_pixels(&h)
    );
}

#[test]
fn the_layers_page_says_so_when_there_are_none() {
    let mut h = mock_app(1);
    h.state_mut().set_side_page(SidePage::Layers);
    h.run();
    // A document without a file has nothing to read layers from.
    assert!(h.state().layers().is_empty());
    assert!(
        h.query_by_label("The layers could not be read.").is_some()
            || h.query_by_label("This document has no layers.").is_some()
    );
}
