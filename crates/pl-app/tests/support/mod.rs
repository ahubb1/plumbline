//! What the M4/M5 test files share: building an app without a window, and driving it with the
//! pointer and the keyboard through `egui_kittest`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::Modifiers;
use egui_kittest::kittest::{by, Queryable};
use egui_kittest::Harness;
use pl_app::{AppConfig, AppState, Command, ScriptedDialogs, Tool};
use pl_doc::pl_annot::{Annotation, Geometry};
use pl_doc::pl_geom::{PageGeometry, Point, Rect};
use pl_doc::pl_render::{MockBackend, PdfiumBackend, RenderBackend};

pub type App = Harness<'static, AppState>;

/// Tests that render through wgpu take this lock (see `tests/ui.rs`).
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn gpu() -> std::sync::MutexGuard<'static, ()> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

pub const CTRL: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    mac_cmd: false,
    command: true,
};

pub const SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: false,
    shift: true,
    mac_cmd: false,
    command: false,
};

pub fn letters(n: usize) -> Vec<PageGeometry> {
    vec![PageGeometry::simple(612.0, 792.0); n]
}

pub fn state_with(
    backend: Arc<dyn RenderBackend>,
    data_dir: Option<PathBuf>,
    dialogs: ScriptedDialogs,
) -> AppState {
    let mut state = AppState::new(
        backend,
        AppConfig {
            data_dir,
            cache_bytes: 256 << 20,
        },
        Box::new(dialogs),
    );
    state.set_author("Test Author");
    state.set_theme(egui::ThemePreference::Light);
    state
}

/// A harness whose frames are `dt` seconds apart (two clicks are a double-click only when they
/// are close in time).
pub fn harness_dt(state: AppState, dt: f32) -> App {
    harness_opts(state, dt, false)
}

/// As [`harness_dt`]; with `tofu`, a character the interface font cannot draw is shown as a
/// box, as the window does, instead of failing the test.
pub fn harness_opts(state: AppState, dt: f32, tofu: bool) -> App {
    let mut builder = Harness::builder()
        .with_step_dt(dt)
        .with_size(egui::vec2(1100.0, 720.0))
        .with_theme(egui::Theme::Light)
        .with_max_steps(400)
        .with_context_setup(pl_app::theme::install);
    if tofu {
        builder = builder.allow_missing_glyphs();
    }
    builder.build_ui_state(pl_app::ui, state)
}

/// A mock-backed app, as [`mock_app`], that may be given text its font cannot draw.
pub fn mock_app_tofu(pages: usize, dt: f32) -> App {
    let mock = Arc::new(MockBackend::with_pages(&letters(pages)));
    let mut state = state_with(mock, None, ScriptedDialogs::default());
    state
        .open_bytes("fixture.pdf", Vec::new())
        .expect("mock open");
    let mut h = harness_opts(state, dt, true);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    h
}

pub fn settle(h: &mut App) {
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
pub fn mock_app(pages: usize) -> App {
    mock_app_with(pages, 0.25, None, ScriptedDialogs::default())
}

pub fn mock_app_with(
    pages: usize,
    dt: f32,
    data_dir: Option<PathBuf>,
    dialogs: ScriptedDialogs,
) -> App {
    let mock = Arc::new(MockBackend::with_pages(&letters(pages)));
    let mut state = state_with(mock, data_dir, dialogs);
    state
        .open_bytes("fixture.pdf", Vec::new())
        .expect("mock open");
    let mut h = harness_dt(state, dt);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    h
}

pub fn pdfium() -> Arc<dyn RenderBackend> {
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

pub fn write_fixture(dir: &Path, fixture: &pl_testkit::Fixture) -> PathBuf {
    let path = dir.join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    path
}

/// Opens `path` with `backend` at 100% in a fresh app.
pub fn file_app(
    backend: Arc<dyn RenderBackend>,
    path: &Path,
    data: Option<PathBuf>,
    dialogs: ScriptedDialogs,
    dt: f32,
) -> App {
    let mut state = state_with(backend, data, dialogs);
    state.open_path(path).expect("the file opens");
    let mut h = harness_dt(state, dt);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    h
}

pub fn real_app(path: &Path) -> App {
    file_app(pdfium(), path, None, ScriptedDialogs::default(), 0.25)
}

pub fn click(h: &mut App, name: &str) {
    h.get_by_role_and_label(Role::Button, name).click();
    h.run();
    h.remove_cursor();
    h.run();
}

/// Clicks the one control whose accessible name is `name`, whatever its role.
pub fn click_label(h: &mut App, name: &str) {
    h.get_by_label(name).click();
    h.run();
    h.remove_cursor();
    h.run();
}

/// Clicks the control whose accessible name starts with `text`.
pub fn click_starting(h: &mut App, text: &str) {
    let wanted = text.to_owned();
    h.get_by(move |node| node.label().is_some_and(|l| l.starts_with(&wanted)))
        .click();
    h.run();
    h.remove_cursor();
    h.run();
}

/// A page-space point's position in window coordinates, for the active view.
pub fn on_screen(h: &App, page: u32, point: Point) -> egui::Pos2 {
    let dv = h.state().active_view().expect("a view");
    let p = dv.view.page_to_view(page, point).expect("page");
    let pos = egui::pos2(dv.rect.min.x + p.x as f32, dv.rect.min.y + p.y as f32);
    assert!(dv.rect.contains(pos), "{point:?} is off screen at {pos:?}");
    pos
}

/// Presses at `from`, moves through the middle to `to` and releases, a frame per event.
pub fn drag_with(h: &mut App, from: egui::Pos2, to: egui::Pos2, m: Modifiers) {
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
    for stop in [from + (to - from) * 0.5, to] {
        h.event_modifiers(egui::Event::PointerMoved(stop), m);
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

pub fn drag(h: &mut App, from: egui::Pos2, to: egui::Pos2) {
    drag_with(h, from, to, Modifiers::NONE);
}

/// Drags between two page points of `page`.
pub fn drag_on_page(h: &mut App, page: u32, a: Point, b: Point) {
    let (from, to) = (on_screen(h, page, a), on_screen(h, page, b));
    drag(h, from, to);
}

/// A click with modifier keys held from before the press until after the release.
pub fn click_at_with(h: &mut App, pos: egui::Pos2, m: Modifiers) {
    h.event(egui::Event::ModifiersChanged(m));
    h.hover_at(pos);
    h.run();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: m,
        });
        h.run();
    }
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

pub fn click_at(h: &mut App, pos: egui::Pos2) {
    click_at_with(h, pos, Modifiers::NONE);
}

/// Two clicks in one frame: a double-click.
pub fn double_click_at(h: &mut App, pos: egui::Pos2) {
    h.hover_at(pos);
    h.run();
    h.drag_at(pos);
    h.drop_at(pos);
    h.drag_at(pos);
    h.drop_at(pos);
    h.run();
    h.remove_cursor();
    h.run();
}

pub fn click_on_page(h: &mut App, page: u32, point: Point) {
    let pos = on_screen(h, page, point);
    click_at(h, pos);
}

pub fn set_tool(h: &mut App, tool: Tool) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::Tool(tool));
    h.run();
}

pub fn run_command(h: &mut App, command: Command) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, command);
    h.run();
}

pub fn near(a: Point, b: Point) -> bool {
    (a - b).hypot() < 0.05
}

pub fn rect_near(a: Rect, b: Rect) -> bool {
    near(Point::new(a.x0, a.y0), Point::new(b.x0, b.y0))
        && near(Point::new(a.x1, a.y1), Point::new(b.x1, b.y1))
}

pub fn rect_of(a: &Annotation) -> Rect {
    match &a.geometry {
        Geometry::Rect(r) => *r,
        other => panic!("not a rectangle: {other:?}"),
    }
}

pub fn markups(h: &App) -> Vec<Annotation> {
    h.state().markups()
}

pub fn selected_ids(h: &App) -> Vec<String> {
    h.state()
        .selected_markups()
        .iter()
        .map(|a| a.id.as_str().to_owned())
        .collect()
}

/// Draws a rectangle with the Rectangle tool and goes back to Select.
pub fn draw_rect(h: &mut App, page: u32, rect: Rect) -> Annotation {
    set_tool(h, Tool::Rectangle);
    drag_on_page(
        h,
        page,
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y0),
    );
    set_tool(h, Tool::Select);
    markups(h).pop().expect("a rectangle")
}

/// Fails if a control of an interactive role on screen has no accessible name.
pub fn assert_all_named(h: &App, what: &str) {
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
        Role::Cell,
        Role::ProgressIndicator,
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
}

/// Types into the focused text box, a frame per call.
pub fn type_into(h: &mut App, name: &str, text: &str) {
    h.get_by_label(name).type_text(text);
    h.run();
}
