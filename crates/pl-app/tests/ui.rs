//! Headless tests of the whole window, driven through `egui_kittest`. No window is opened.
//!
//! Most tests use `MockBackend`; `real_pdfium_*` tests use the vendored PDFium on fixtures from
//! pl-testkit. The harness itself fails any test in which an interactive control has no
//! accessible name or a character has no glyph in the embedded font.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use pl_app::{AppConfig, AppState, Command, LengthUnit, ScriptedDialogs, SidePage};
use pl_doc::pl_geom::{PageGeometry, Point, Rotation};
use pl_doc::pl_render::{
    MockBackend, OutlineItem, PageText, PdfiumBackend, RenderBackend, RenderError, Reply, Request,
    TextChar,
};
use pl_doc::FitMode;

type App = Harness<'static, AppState>;

/// Tests that render through wgpu take this lock: creating GPU devices from several test
/// threads at once crashed the test process (access violation) on the development machine.
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gpu() -> std::sync::MutexGuard<'static, ()> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ctrl as Windows reports it: both the control key and egui's "command" modifier.
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

/// Page text made of lines of `(string, baseline origin, font size)`; each character is half
/// its size wide.
fn synthetic_text(page: u32, lines: &[(&str, [f64; 2], f64)]) -> PageText {
    let mut chars = Vec::new();
    for (li, (s, origin, size)) in lines.iter().enumerate() {
        if li > 0 {
            for ch in ['\r', '\n'] {
                chars.push(TextChar {
                    ch,
                    bbox: [0.0; 4],
                    origin: [0.0; 2],
                    font_size: *size as f32,
                    generated: true,
                });
            }
        }
        for (k, ch) in s.chars().enumerate() {
            let x = origin[0] + k as f64 * 0.5 * size;
            chars.push(TextChar {
                ch,
                bbox: [
                    x,
                    origin[1] - 0.2 * size,
                    x + 0.5 * size,
                    origin[1] + 0.8 * size,
                ],
                origin: [x, origin[1]],
                font_size: *size as f32,
                generated: false,
            });
        }
    }
    PageText { page, chars }
}

fn state_with(backend: Arc<dyn RenderBackend>, data_dir: Option<PathBuf>) -> AppState {
    AppState::new(
        backend,
        AppConfig {
            data_dir,
            cache_bytes: 256 << 20,
        },
        Box::new(ScriptedDialogs::default()),
    )
}

fn harness_themed(state: AppState, theme: egui::Theme) -> App {
    Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .with_theme(theme)
        .with_max_steps(400)
        .with_context_setup(pl_app::theme::install)
        .build_ui_state(pl_app::ui, state)
}

fn harness(mut state: AppState) -> App {
    state.set_theme(egui::ThemePreference::Light);
    harness_themed(state, egui::Theme::Light)
}

/// Lets renders and queries finish and the interface come to rest. Everything the window gets
/// from the backend arrives a frame or more after it was asked for, so a test settles after
/// any action whose effect needs page text, page geometry or the outline.
fn settle(h: &mut App) {
    for _ in 0..6 {
        h.run();
        assert!(
            h.state_mut().wait_idle(Duration::from_secs(20)),
            "renders did not finish"
        );
    }
    h.run();
    assert_eq!(h.state().pending_queries(), 0);
}

/// A mock-backed app with one document of `pages` Letter pages open.
fn mock_app(pages: usize) -> (Arc<MockBackend>, App) {
    let mock = Arc::new(MockBackend::with_pages(&letters(pages)));
    let mut state = state_with(mock.clone(), None);
    state
        .open_bytes("fixture.pdf", Vec::new())
        .expect("mock open");
    let mut h = harness(state);
    settle(&mut h);
    (mock, h)
}

fn click(h: &mut App, name: &str) {
    h.get_by_role_and_label(Role::Button, name).click();
    h.run();
    h.remove_cursor();
    h.run();
}

fn shown_page(h: &App) -> u32 {
    h.state().active_view().expect("a view").shown_page()
}

/// A page-space point's position in window coordinates, for the active view.
fn on_screen(h: &App, page: u32, point: Point) -> egui::Pos2 {
    let dv = h.state().active_view().expect("a view");
    let p = dv.view.page_to_view(page, point).expect("page");
    egui::pos2(dv.rect.min.x + p.x as f32, dv.rect.min.y + p.y as f32)
}

fn wheel(h: &mut App, lines: f32, modifiers: Modifiers) {
    h.event_modifiers(
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers,
        },
        modifiers,
    );
    h.run();
}

#[test]
fn opens_a_fixture_through_injected_state() {
    let (mock, h) = mock_app(5);
    let state = h.state();
    assert_eq!(state.session().len(), 1);
    assert_eq!(state.view_ids().len(), 1);
    let info = state.status_info();
    assert_eq!(info.page, "Page 1 of 5");
    assert_eq!(info.size, "8.50 x 11.00 in");
    // The tab carries the document's name and the canvas has a name too.
    h.get_by_role_and_label(Role::Button, "fixture.pdf");
    h.get_by_label("Page view, fixture.pdf");
    // Tiles were rendered by the injected backend and uploaded as textures.
    assert!(!mock.rendered_tiles().is_empty());
    assert!(state.textures().tile_count() > 0);
    assert!(state.textures().thumb_count() > 0);
    assert!(state.tile_cache().bytes() <= state.tile_cache().budget());
}

#[test]
fn next_and_previous_page_buttons() {
    let (_mock, mut h) = mock_app(5);
    assert_eq!(shown_page(&h), 0);
    click(&mut h, "Next page");
    assert_eq!(shown_page(&h), 1);
    click(&mut h, "Next page");
    assert_eq!(shown_page(&h), 2);
    click(&mut h, "Previous page");
    assert_eq!(shown_page(&h), 1);
    assert_eq!(h.state().status_info().page, "Page 2 of 5");
}

#[test]
fn typing_a_page_number_goes_to_that_page() {
    let (_mock, mut h) = mock_app(9);
    h.get_by_label("Page number").focus();
    h.run();
    h.get_by_label("Page number").type_text("7");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(shown_page(&h), 6);
    // The last page is short of a full viewport of scroll, yet the box still names it.
    h.get_by_label("Page number").focus();
    h.run();
    h.get_by_label("Page number").type_text("9");
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(shown_page(&h), 8);
    assert_eq!(h.get_by_label("Page number").value().as_deref(), Some("9"));
    // A number outside the document is refused with a message and nothing moves.
    h.get_by_label("Page number").focus();
    h.run();
    h.get_by_label("Page number").type_text("42");
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(shown_page(&h), 8);
    assert!(h.state().status().is_some_and(|s| s.contains("42")));
}

#[test]
fn ctrl_wheel_zooms_about_the_cursor() {
    let (_mock, mut h) = mock_app(4);
    // Zoomed in far enough that the scroll limits cannot intervene.
    h.state_mut().active_view_mut().unwrap().view.set_zoom(2.0);
    h.run();
    let rect = h.state().active_view().unwrap().rect;
    let cursor = rect.min + egui::vec2(310.0, 205.0);
    let local = Point::new(310.0, 205.0);
    h.hover_at(cursor);
    h.run();

    for lines in [1.0_f32, 1.0, -1.0, 2.0, -3.0] {
        let before = {
            let view = &h.state().active_view().unwrap().view;
            (view.zoom(), view.hit_test_nearest(local).expect("a page"))
        };
        wheel(&mut h, lines, CTRL);
        let view = &h.state().active_view().unwrap().view;
        let expected = before.0 * pl_app::canvas::WHEEL_ZOOM_STEP.powf(lines as f64);
        assert!(
            (view.zoom() - expected).abs() < 1e-9,
            "zoom {} after {lines} lines, expected {expected}",
            view.zoom()
        );
        assert_eq!(view.fit(), FitMode::None);
        let after = view
            .page_to_view(before.1.page, before.1.point)
            .expect("page");
        let error = (after - local).hypot();
        assert!(
            error < 0.5,
            "the page point under the cursor moved {error} px"
        );
    }
    settle(&mut h);
}

#[test]
fn wheel_scrolls_and_shift_wheel_scrolls_sideways() {
    let (_mock, mut h) = mock_app(4);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(2.0);
    h.run();
    let rect = h.state().active_view().unwrap().rect;
    h.hover_at(rect.center());
    h.run();
    let scroll = |h: &App| h.state().active_view().unwrap().view.scroll();
    let start = scroll(&h);
    wheel(&mut h, -3.0, Modifiers::NONE);
    let down = scroll(&h);
    assert!((down.y - start.y - 3.0 * pl_app::canvas::WHEEL_LINE_PX as f64).abs() < 1e-6);
    assert_eq!(down.x, start.x);
    wheel(&mut h, -2.0, Modifiers::SHIFT);
    let side = scroll(&h);
    assert!((side.x - down.x - 2.0 * pl_app::canvas::WHEEL_LINE_PX as f64).abs() < 1e-6);
    assert_eq!(side.y, down.y);
    settle(&mut h);
}

#[test]
fn middle_drag_and_space_drag_pan() {
    let (_mock, mut h) = mock_app(4);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(2.0);
    h.run();
    let rect = h.state().active_view().unwrap().rect;
    let from = rect.center();
    let to = from + egui::vec2(-120.0, -90.0);
    let scroll = |h: &App| h.state().active_view().unwrap().view.scroll();

    let start = scroll(&h);
    h.hover_at(from);
    for pressed in [true, false] {
        if !pressed {
            h.hover_at(from + egui::vec2(-60.0, -45.0));
            h.hover_at(to);
        }
        h.event(egui::Event::PointerButton {
            pos: if pressed { from } else { to },
            button: egui::PointerButton::Middle,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run();
    let after_middle = scroll(&h);
    assert!((after_middle.x - start.x - 120.0).abs() < 1.0);
    assert!((after_middle.y - start.y - 90.0).abs() < 1.0);

    // Space held: the primary button pans instead of selecting text.
    h.key_down(Key::Space);
    h.hover_at(from);
    h.drag_at(from);
    h.hover_at(from + egui::vec2(-60.0, -45.0));
    h.hover_at(to);
    h.drop_at(to);
    h.key_up(Key::Space);
    h.run();
    let after_space = scroll(&h);
    assert!((after_space.x - after_middle.x - 120.0).abs() < 1.0);
    assert!((after_space.y - after_middle.y - 90.0).abs() < 1.0);
    assert!(h.state().active_view().unwrap().selection.is_none());
    settle(&mut h);
}

#[test]
fn fit_modes_zoom_buttons_and_rotation() {
    let (_mock, mut h) = mock_app(3);
    let view = |h: &App| h.state().active_view().unwrap().view.clone();
    assert_eq!(
        view(&h).fit(),
        FitMode::Width,
        "documents open at fit width"
    );

    click(&mut h, "Fit page");
    let fitted = view(&h);
    assert_eq!(fitted.fit(), FitMode::Page);
    let page = fitted.page_rect_in_view(0).unwrap();
    assert!(page.height() <= fitted.viewport().height + 1e-6);

    click(&mut h, "100%");
    assert_eq!(view(&h).fit(), FitMode::None);
    assert!((view(&h).zoom() - 1.0).abs() < 1e-12);
    click(&mut h, "Zoom in");
    assert!((view(&h).zoom() - 1.25).abs() < 1e-12);
    click(&mut h, "Zoom out");
    click(&mut h, "Zoom out");
    assert!((view(&h).zoom() - 0.75).abs() < 1e-12);

    // The zoom box takes a typed percentage.
    h.get_by_label("Zoom percent").focus();
    h.run();
    h.get_by_label("Zoom percent").type_text("150");
    h.key_press(Key::Enter);
    h.run();
    assert!((view(&h).zoom() - 1.5).abs() < 1e-12);

    click(&mut h, "Rotate view clockwise");
    assert_eq!(view(&h).rotation(), Rotation::R90);
    assert_eq!(h.state().status_info().size, "11.00 x 8.50 in");
    click(&mut h, "Rotate view anticlockwise");
    click(&mut h, "Rotate view anticlockwise");
    assert_eq!(view(&h).rotation(), Rotation::R270);
    settle(&mut h);
    assert!(h.state().textures().tile_count() > 0);
}

#[test]
fn search_finds_hits_and_steps_through_them() {
    let mock = Arc::new(MockBackend::with_pages(&letters(4)));
    mock.set_page_text(synthetic_text(
        0,
        &[
            ("alpha beta alpha", [72.0, 700.0], 12.0),
            ("gamma", [72.0, 680.0], 12.0),
        ],
    ));
    mock.set_page_text(synthetic_text(2, &[("delta alpha", [72.0, 300.0], 12.0)]));
    let mut state = state_with(mock.clone(), None);
    state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);

    // Ctrl+F shows the find bar with the keyboard in its text box.
    assert!(!h.state().find_bar_open());
    h.key_press_modifiers(CTRL, Key::F);
    h.run();
    assert!(h.state().find_bar_open());
    assert!(h.get_by_label("Find text").is_focused());
    h.get_by_label("Find text").type_text("alpha");
    h.run();
    h.key_press(Key::Enter);
    settle(&mut h);

    let position = |h: &App| h.state().active_view().unwrap().search_position();
    let current_page = |h: &App| {
        let search = h.state().active_view().unwrap().search.as_ref().unwrap();
        search.current().unwrap().page
    };
    assert_eq!(position(&h), Some((1, 3)));
    assert_eq!(current_page(&h), 0);
    h.get_by_label("1 of 3");

    // Enter again steps on; so do the buttons and F3, and they wrap.
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(position(&h), Some((2, 3)));
    assert_eq!(current_page(&h), 0);
    click(&mut h, "Next match");
    assert_eq!(position(&h), Some((3, 3)));
    assert_eq!(current_page(&h), 2);
    // The hit on page 3 was scrolled into view.
    let hit_at = on_screen(&h, 2, Point::new(110.0, 303.0));
    assert!(h.state().active_view().unwrap().rect.contains(hit_at));
    click(&mut h, "Next match");
    assert_eq!(position(&h), Some((1, 3)));
    click(&mut h, "Previous match");
    assert_eq!(position(&h), Some((3, 3)));
    h.key_press_modifiers(Modifiers::SHIFT, Key::F3);
    h.run();
    assert_eq!(position(&h), Some((2, 3)));

    // A query with no match says so.
    h.state_mut().set_find_text("omega");
    h.state_mut().find(true);
    settle(&mut h);
    assert_eq!(position(&h), Some((0, 0)));
    h.get_by_label("No matches");

    // Escape closes the bar and clears the highlights.
    h.key_press(Key::Escape);
    h.run();
    assert!(!h.state().find_bar_open());
    assert!(h.state().active_view().unwrap().search.is_none());
    settle(&mut h);
}

#[test]
fn dragging_selects_text_and_copy_puts_it_on_the_clipboard() {
    let mock = Arc::new(MockBackend::with_pages(&letters(2)));
    mock.set_page_text(synthetic_text(
        0,
        &[
            ("Hello world", [72.0, 700.0], 12.0),
            ("second line", [72.0, 680.0], 12.0),
        ],
    ));
    let mut state = state_with(mock.clone(), None);
    state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    h.run();

    // Each character is 6 pt wide: drag from inside "H" to inside the "o" of "Hello".
    let from = on_screen(&h, 0, Point::new(74.0, 703.0));
    let to = on_screen(&h, 0, Point::new(99.0, 703.0));
    // Pointing at the page asks for its text; it is there by the time the drag starts.
    h.hover_at(from);
    settle(&mut h);
    h.drag_at(from);
    h.hover_at(from + (to - from) * 0.5);
    h.hover_at(to);
    h.drop_at(to);
    h.run();
    let selection = h.state().active_view().unwrap().selection.clone().unwrap();
    assert_eq!(selection.text, "Hello");
    assert_eq!(selection.page, 0);
    assert_eq!(selection.quads.len(), 1);

    // The platform's copy event puts exactly that string on the clipboard.
    h.event(egui::Event::Copy);
    h.step();
    let copied: Vec<String> = h
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(copied, vec!["Hello".to_owned()]);
    assert_eq!(h.state().last_copied(), Some("Hello"));

    // A drag across both lines, copied with Ctrl+C.
    let to = on_screen(&h, 0, Point::new(72.0 + 6.0 * 5.5, 683.0));
    h.hover_at(from);
    h.drag_at(from);
    h.hover_at(to);
    h.drop_at(to);
    h.run();
    h.key_press_modifiers(CTRL, Key::C);
    h.run();
    assert_eq!(h.state().last_copied(), Some("Hello world\r\nsecond"));

    // Ctrl+A takes the whole page; a plain click drops the selection.
    h.key_press_modifiers(CTRL, Key::A);
    h.run();
    let all = h.state().active_view().unwrap().selection.clone().unwrap();
    assert_eq!(all.text, "Hello world\r\nsecond line");
    let empty = on_screen(&h, 0, Point::new(300.0, 520.0));
    assert!(h.state().active_view().unwrap().rect.contains(empty));
    h.hover_at(empty);
    h.drag_at(empty);
    h.drop_at(empty);
    h.run();
    assert!(h.state().active_view().unwrap().selection.is_none());
}

#[test]
fn status_bar_reports_page_size_and_cursor_position() {
    let (_mock, mut h) = mock_app(2);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    h.run();
    assert_eq!(h.state().status_info().cursor, None);
    // One inch in from the left edge and two down from the top of the page.
    let at = on_screen(&h, 0, Point::new(72.0, 792.0 - 144.0));
    h.hover_at(at);
    h.run();
    let info = h.state().status_info();
    assert_eq!(info.cursor.as_deref(), Some("x 1.00  y 2.00 in"));
    assert_eq!(info.zoom, "100%");
    h.get_by_label("x 1.00  y 2.00 in");
    h.get_by_label("8.50 x 11.00 in");

    h.state_mut().set_unit(LengthUnit::Millimetres);
    h.run();
    let info = h.state().status_info();
    assert_eq!(info.size, "215.9 x 279.4 mm");
    assert_eq!(info.cursor.as_deref(), Some("x 25.4  y 50.8 mm"));
    h.state_mut().set_unit(LengthUnit::Points);
    assert_eq!(h.state().status_info().size, "612.0 x 792.0 pt");
}

#[test]
fn thumbnails_and_bookmarks_navigate() {
    let mock = Arc::new(MockBackend::with_pages(&letters(6)));
    let item = |title: &str, level: u32, page: u32| OutlineItem {
        title: title.to_owned(),
        level,
        page: Some(page),
        x: None,
        y: None,
        zoom: None,
    };
    mock.set_outline(vec![
        item("Ground floor", 0, 1),
        item("Stair detail", 1, 4),
        item("Roof", 0, 5),
    ]);
    let mut state = state_with(mock.clone(), None);
    state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);

    // Thumbnails are requested for the rows in view and arrive as textures.
    assert!(h.state().side_panel_open());
    assert!(h.state().textures().thumb_count() >= 3);
    click(&mut h, "Page 3 thumbnail");
    assert_eq!(shown_page(&h), 2);

    click(&mut h, "Bookmarks");
    settle(&mut h);
    click(&mut h, "Stair detail");
    assert_eq!(shown_page(&h), 4);
    click(&mut h, "Ground floor");
    assert_eq!(shown_page(&h), 1);

    // Ctrl+B and the toolbar button hide and show the panel.
    h.key_press_modifiers(CTRL, Key::B);
    h.run();
    assert!(!h.state().side_panel_open());
    assert!(h.query_by_label("Ground floor").is_none());
    click(&mut h, "Side panel");
    assert!(h.state().side_panel_open());
    h.state_mut().set_side_page(SidePage::Thumbnails);
    settle(&mut h);
}

#[test]
fn keyboard_shortcuts_move_the_view() {
    let (_mock, mut h) = mock_app(5);
    h.key_press(Key::ArrowRight);
    h.key_press(Key::ArrowRight);
    h.run();
    assert_eq!(shown_page(&h), 2);
    h.key_press(Key::ArrowLeft);
    h.run();
    assert_eq!(shown_page(&h), 1);
    h.key_press(Key::End);
    h.run();
    assert_eq!(shown_page(&h), 4);
    h.key_press(Key::Home);
    h.run();
    assert_eq!(shown_page(&h), 0);
    let scroll = |h: &App| h.state().active_view().unwrap().view.scroll().y;
    let top = scroll(&h);
    h.key_press(Key::ArrowDown);
    h.run();
    assert!((scroll(&h) - top - 60.0).abs() < 1e-6);
    h.key_press(Key::PageDown);
    h.run();
    assert!(scroll(&h) > top + 300.0);

    h.key_press_modifiers(CTRL, Key::Num0);
    h.run();
    assert_eq!(h.state().active_view().unwrap().view.fit(), FitMode::Page);
    h.key_press_modifiers(CTRL, Key::Num2);
    h.run();
    assert_eq!(h.state().active_view().unwrap().view.fit(), FitMode::Width);
    h.key_press_modifiers(CTRL, Key::Num1);
    h.run();
    assert!((h.state().active_view().unwrap().view.zoom() - 1.0).abs() < 1e-12);
    h.key_press_modifiers(CTRL, Key::R);
    h.run();
    assert_eq!(
        h.state().active_view().unwrap().view.rotation(),
        Rotation::R90
    );
    // Ctrl+G puts the keyboard in the page box; typing there does not move the view.
    h.key_press_modifiers(CTRL, Key::G);
    h.run();
    assert!(h.get_by_label("Page number").is_focused());
    let before = scroll(&h);
    h.key_press(Key::ArrowDown);
    h.run();
    assert_eq!(scroll(&h), before);
    settle(&mut h);
}

#[test]
fn tabs_splits_and_closing() {
    let mock = Arc::new(MockBackend::with_pages(&letters(3)));
    let mut state = state_with(mock.clone(), None);
    let first = state.open_bytes("first.pdf", Vec::new()).unwrap();
    let second = state.open_bytes("second.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);
    assert_eq!(h.state().session().len(), 2);
    assert_eq!(h.state().active(), Some(second));

    // Clicking a tab makes its view the active one.
    click(&mut h, "first.pdf");
    assert_eq!(h.state().active(), Some(first));
    h.state_mut().active_view_mut().unwrap().go_to_page(1);

    // A split is a second view of the same document, starting where the first one is.
    click(&mut h, "Split side by side");
    settle(&mut h);
    let ids = h.state().view_ids();
    assert_eq!(ids.len(), 3);
    let split = *ids.last().unwrap();
    assert_eq!(h.state().active(), Some(split));
    let (a, b) = (
        h.state().view(first).unwrap(),
        h.state().view(split).unwrap(),
    );
    assert_eq!(a.doc, b.doc);
    assert_eq!(b.shown_page(), 1);
    assert_eq!(h.state().session().len(), 2, "no document was opened twice");
    // Both halves are on screen side by side.
    assert!(a.rect.width() > 100.0 && b.rect.width() > 100.0);
    assert!(a.rect.right() <= b.rect.left() + 1.0);

    // The views scroll independently.
    h.state_mut().view_mut(split).unwrap().go_to_page(2);
    h.run();
    assert_eq!(h.state().view(first).unwrap().shown_page(), 1);
    assert_eq!(h.state().view(split).unwrap().shown_page(), 2);

    h.state_mut()
        .run(&egui::Context::default(), Command::SplitStack);
    settle(&mut h);
    assert_eq!(h.state().view_ids().len(), 4);

    // Closing one view keeps the document; closing its last view closes it.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::CloseTab);
    h.state_mut().close_view(split);
    settle(&mut h);
    assert_eq!(h.state().session().len(), 2);
    h.state_mut().close_view(first);
    settle(&mut h);
    assert_eq!(h.state().session().len(), 1);
    assert_eq!(h.state().active(), Some(second));
    h.key_press_modifiers(CTRL, Key::W);
    settle(&mut h);
    assert!(h.state().session().is_empty());
    assert!(h.state().view_ids().is_empty());
    // With nothing open the start page offers to open a file.
    h.get_by_role_and_label(Role::Button, "Open a file\u{2026}");
}

/// A backend whose every document needs a password.
struct Locked;

impl RenderBackend for Locked {
    fn call(&self, _request: Request) -> Result<Reply, RenderError> {
        Err(RenderError::PasswordRequired)
    }
}

#[test]
fn a_protected_file_asks_for_its_password() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.pdf");
    std::fs::write(&path, b"%PDF-1.7\n").unwrap();
    let mut state = state_with(Arc::new(Locked), None);
    assert!(state.open_path(&path).is_none());
    assert!(state.password_prompt_open());
    let mut h = harness(state);
    h.run();
    h.get_by_label("Password needed");
    h.get_by_label("Password").type_text("guess");
    h.run();
    click(&mut h, "Open");
    // Still locked: the prompt stays and says the password was wrong.
    assert!(h.state().password_prompt_open());
    h.get_by_label("That password did not open the file.");
    click(&mut h, "Cancel");
    assert!(!h.state().password_prompt_open());
    assert!(h.state().session().is_empty());
}

#[test]
fn a_missing_file_is_reported_not_opened() {
    let mut state = state_with(Arc::new(MockBackend::new()), None);
    assert!(state
        .open_path(Path::new("C:\\no\\such\\folder\\absent.pdf"))
        .is_none());
    assert!(state.status().is_some_and(|s| s.contains("absent.pdf")));
    let mut h = harness(state);
    h.run();
    assert!(h.state().session().is_empty());
}

// ---- real PDFium --------------------------------------------------------------------------

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

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

#[test]
fn real_pdfium_drop_open_search_select_and_recent() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let fixture = pl_testkit::multi_page();
    let path = write_fixture(dir.path(), &fixture);

    let mut h = harness(state_with(pdfium(), Some(data.clone())));
    h.run();
    h.get_by_role_and_label(Role::Button, "Open a file\u{2026}");

    // Dropping the file on the window opens it.
    h.input_mut()
        .dropped_files
        .push(Arc::new(Dropped(path.clone())));
    h.step();
    settle(&mut h);
    assert_eq!(h.state().session().len(), 1);
    assert_eq!(
        h.state().status_info().page,
        format!("Page 1 of {}", pl_testkit::MULTI_PAGE_COUNT)
    );
    assert!(h.state().status().is_none(), "{:?}", h.state().status());
    assert!(h.state().textures().tile_count() > 0);

    // Its bookmarks are listed, and one takes the view to its page.
    let outline = &fixture.manifest.outline;
    assert!(!outline.is_empty());
    let target = outline
        .iter()
        .find(|o| o.page_index >= 3)
        .expect("a bookmark beyond the first pages");
    click(&mut h, "Bookmarks");
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, &target.title).click();
    settle(&mut h);
    assert_eq!(shown_page(&h), target.page_index as u32);

    // Search for a token that is on exactly one page.
    let token = pl_testkit::multi_page_token(17);
    h.state_mut().run(&egui::Context::default(), Command::Find);
    h.run();
    h.get_by_label("Find text").type_text(&token);
    h.run();
    h.key_press(Key::Enter);
    settle(&mut h);
    let dv = h.state().active_view().unwrap();
    assert_eq!(dv.search_position(), Some((1, 1)));
    let hit = dv.search.as_ref().unwrap().current().unwrap().clone();
    assert_eq!(hit.page, 17);
    h.key_press(Key::Escape);
    h.run();

    // Select that token by dragging across it; the copied text is what PDFium extracts.
    let bounds = pl_doc::quads_bounds(&hit.quads).unwrap();
    let mid_y = (bounds[1] + bounds[3]) / 2.0;
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.5);
    h.run();
    h.state_mut().active_view_mut().unwrap().view.reveal_rect(
        17,
        pl_doc::pl_geom::Rect::new(bounds[0], bounds[1], bounds[2], bounds[3]),
        80.0,
    );
    settle(&mut h);
    let from = on_screen(&h, 17, Point::new(bounds[0] + 0.5, mid_y));
    let to = on_screen(&h, 17, Point::new(bounds[2] - 0.5, mid_y));
    assert!(h.state().active_view().unwrap().rect.contains(from));
    assert!(h.state().active_view().unwrap().rect.contains(to));
    h.hover_at(from);
    settle(&mut h);
    h.drag_at(from);
    h.hover_at(from + (to - from) * 0.5);
    h.hover_at(to);
    h.drop_at(to);
    h.run();
    h.key_press_modifiers(CTRL, Key::C);
    h.run();
    assert_eq!(h.state().last_copied(), Some(token.as_str()));
    let extracted = pdfium_text(&path, 17);
    let start = hit.char_start as usize;
    let expected: String = extracted
        .chars
        .iter()
        .skip(start)
        .take(hit.char_len as usize)
        .map(|c| c.ch)
        .collect();
    assert_eq!(h.state().last_copied(), Some(expected.as_str()));

    // The file is in the recent list, and closing records the page it was left on.
    let recent = h.state().recent().unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent.entries()[0].path, path);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::CloseTab);
    settle(&mut h);
    assert!(h.state().session().is_empty());
    drop(h);

    // A new session lists it on the start page and reopens it where it was.
    let mut h = harness(state_with(pdfium(), Some(data)));
    h.run();
    h.get_by_role_and_label(Role::Button, "multi_page.pdf")
        .click();
    settle(&mut h);
    assert_eq!(h.state().session().len(), 1);
    assert_eq!(shown_page(&h), 17);
}

fn pdfium_text(path: &Path, page: u32) -> PageText {
    let backend = pdfium();
    let info = backend
        .open(pl_doc::pl_render::DocSource::file(path.to_path_buf()), None)
        .unwrap();
    let text = backend.page_text(info.doc, page).unwrap();
    backend.close(info.doc).unwrap();
    text
}

#[test]
fn real_pdfium_file_dialog_opens_rotated_and_cropped_fixtures() {
    let dir = tempfile::tempdir().unwrap();
    let rotated = write_fixture(dir.path(), &pl_testkit::rotated_pages());
    let cropped = write_fixture(dir.path(), &pl_testkit::crop_box());
    let unit = write_fixture(dir.path(), &pl_testkit::user_unit());
    let state = AppState::new(
        pdfium(),
        AppConfig::default(),
        Box::new(ScriptedDialogs::new([vec![rotated.clone(), cropped, unit]])),
    );
    let mut h = harness(state);
    h.run();
    // File > Open goes through the (scripted) dialog and opens every file picked.
    h.key_press_modifiers(CTRL, Key::O);
    settle(&mut h);
    assert_eq!(h.state().session().len(), 3);
    assert_eq!(h.state().view_ids().len(), 3);
    assert!(h.state().status().is_none(), "{:?}", h.state().status());
    // The last one opened is in front: a 306 x 396 pt page with /UserUnit 2 is Letter size.
    assert_eq!(h.state().status_info().size, "8.50 x 11.00 in");

    // Page 2 of the rotated fixture is shown landscape, and every visible tile arrives.
    click(&mut h, "rotated_pages.pdf");
    h.state_mut().active_view_mut().unwrap().go_to_page(1);
    settle(&mut h);
    assert_eq!(h.state().status_info().size, "11.00 x 8.50 in");
    assert!(h.state().textures().tile_count() > 0);

    // Opening a file that is already open goes to its tab instead of opening it again.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Command::OpenPath(rotated));
    settle(&mut h);
    assert_eq!(h.state().session().len(), 3);
    assert_eq!(h.state().view_ids().len(), 3);
}

/// The painter must put every tile where the view says it is, also with the view rotated
/// (tiles are rendered in the file's orientation and turned when drawn). The mock backend's
/// pixels are a known function of their position on the page, so rendered pixels can be checked
/// against the page point the view reports under them.
#[test]
fn rendered_pixels_match_the_page_under_them_at_every_view_rotation() {
    let _gpu = gpu();
    let (_mock, mut h) = mock_app(2);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.5);
    for rotation in Rotation::ALL {
        h.state_mut()
            .active_view_mut()
            .unwrap()
            .view
            .set_rotation(rotation);
        settle(&mut h);
        let image = h.render().expect("headless render");
        let dv = h.state().active_view().unwrap();
        let tiles = dv.view.visible_tiles();
        let mut checked = 0;
        for sy in (40..560).step_by(37) {
            for sx in (30..780).step_by(41) {
                let local = Point::new(sx as f64 + 0.5, sy as f64 + 0.5);
                let Some(hit) = dv.view.hit_test(local) else {
                    continue;
                };
                let Some(page_tiles) = tiles.iter().find(|t| t.page == hit.page) else {
                    continue;
                };
                let shown = page_tiles.render_geometry.page_to_display() * hit.point;
                let scale = page_tiles.grid.scale();
                let (gx, gy) = (shown.x * scale, shown.y * scale);
                // Stay clear of page edges and of the pattern's wrap-around every 256 px.
                let clear = |g: f64, max: u32| {
                    let m = g.rem_euclid(256.0);
                    g > 8.0 && g < max as f64 - 8.0 && m > 8.0 && m < 247.0
                };
                if !clear(gx, page_tiles.grid.width_px()) || !clear(gy, page_tiles.grid.height_px())
                {
                    continue;
                }
                let expected = MockBackend::pixel(hit.page, gx as u32, gy as u32);
                let px = (dv.rect.min.x as u32 + sx, dv.rect.min.y as u32 + sy);
                let got = image.get_pixel(px.0, px.1).0;
                for channel in 0..2 {
                    let diff = (got[channel] as i32 - expected[channel] as i32).abs();
                    assert!(
                        diff <= 6,
                        "{rotation:?}: pixel {px:?} is {got:?}, the page says {expected:?}"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 60, "{rotation:?}: only {checked} samples");
    }
}

// ---- the window never waits for the backend ---------------------------------------------------

/// Resumes a paused mock after a while and remembers that it had to, so a frame that does
/// wait for the backend fails the test instead of hanging it.
struct Watchdog {
    tripped: Arc<std::sync::atomic::AtomicBool>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn guard(mock: Arc<MockBackend>, limit: Duration) -> Watchdog {
        use std::sync::atomic::{AtomicBool, Ordering};
        let tripped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (t, s) = (tripped.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            while !s.load(Ordering::Acquire) {
                if started.elapsed() > limit {
                    t.store(true, Ordering::Release);
                    mock.resume();
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Watchdog {
            tripped,
            stop,
            thread: Some(thread),
        }
    }

    /// Stops the watchdog; returns whether it had to step in.
    fn finish(mut self) -> bool {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        self.tripped.load(Ordering::Acquire)
    }
}

/// The backend is one thread, and a tile of a dense sheet keeps it busy for a long time. With
/// the mock refusing to answer *anything*, every part of a frame that used to call it
/// directly (hover, text selection, search, select all, the bookmarks panel) must still
/// return at once, and catch up when the backend answers.
#[test]
fn a_frame_never_waits_for_the_backend() {
    let mock = Arc::new(MockBackend::with_pages(&letters(5)));
    mock.set_page_text(synthetic_text(
        0,
        &[
            ("Hello world", [72.0, 700.0], 12.0),
            ("alpha line", [72.0, 680.0], 12.0),
        ],
    ));
    mock.set_page_text(synthetic_text(3, &[("more alpha", [72.0, 300.0], 12.0)]));
    mock.set_outline(vec![OutlineItem {
        title: "Ground floor".to_owned(),
        level: 0,
        page: Some(1),
        x: None,
        y: None,
        zoom: None,
    }]);
    let mut state = state_with(mock.clone(), None);
    // The open itself is one request for the page sizes: no page is loaded for it.
    let view = state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    {
        let doc = state.session().documents().next().unwrap();
        assert_eq!(mock.page_loads(doc.render_id()).unwrap(), 0);
        assert!(!mock
            .calls()
            .iter()
            .any(|r| matches!(r, Request::PageInfo { .. } | Request::PageText { .. })));
        assert!(state.view(view).is_some());
    }
    let mut h = harness(state);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    let over_text = on_screen(&h, 0, Point::new(74.0, 703.0));
    let over_word = on_screen(&h, 0, Point::new(110.0, 703.0));
    assert!(h.state().active_view().unwrap().selection.is_none());

    // From here the backend answers nothing.
    mock.pause_all();
    let answered = mock.calls().len();
    let watchdog = Watchdog::guard(mock.clone(), Duration::from_secs(20));
    let started = std::time::Instant::now();

    // Hover over text, double-click a word, select all, start a search, open the bookmarks
    // panel, scroll and zoom: one frame (or more) each.
    h.hover_at(over_text);
    h.run();
    h.hover_at(over_word);
    h.drag_at(over_word);
    h.drop_at(over_word);
    h.drag_at(over_word);
    h.drop_at(over_word);
    h.run();
    h.key_press_modifiers(CTRL, Key::A);
    h.run();
    h.state_mut().set_find_text("alpha");
    h.state_mut().find(true);
    h.run();
    h.state_mut().set_side_page(SidePage::Bookmarks);
    h.run();
    wheel(&mut h, -3.0, Modifiers::NONE);
    wheel(&mut h, 2.0, CTRL);
    h.run();
    let frames_took = started.elapsed();

    // Nothing was answered, and nothing was waited for.
    assert_eq!(
        mock.calls().len(),
        answered,
        "the mock answered while paused"
    );
    assert!(h.state().pending_queries() > 0, "the requests are queued");
    let dv = h.state().active_view().unwrap();
    assert!(dv.search.as_ref().is_some_and(|s| !s.is_finished()));
    h.get_by_label("Reading bookmarks...");
    assert!(
        !watchdog.finish(),
        "a frame waited for the backend ({frames_took:?})"
    );
    assert!(frames_took < Duration::from_secs(10), "{frames_took:?}");

    // When the backend answers, everything that was asked for arrives.
    mock.resume();
    settle(&mut h);
    let dv = h.state().active_view().unwrap();
    let search = dv.search.as_ref().unwrap();
    assert!(search.is_finished());
    assert_eq!(
        search.hits().iter().map(|h| h.page).collect::<Vec<_>>(),
        [0, 3]
    );
    // The text action that was waiting for page 1's text (the last one asked for: select
    // all) has been applied.
    assert_eq!(
        dv.selection.as_ref().map(|s| s.text.as_str()),
        Some("Hello world\r\nalpha line")
    );
    h.get_by_label("Ground floor");
    assert!(h.query_by_label("Reading bookmarks...").is_none());
    // Each page's text went to the backend once, although hover, selection and search all
    // wanted page 1's.
    let texts = |page: u32| {
        mock.calls()
            .iter()
            .filter(|r| matches!(r, Request::PageText { page: p, .. } if *p == page))
            .count()
    };
    assert_eq!(texts(0), 1);
    assert!((1..5).all(|page| texts(page) <= 1));
    assert!(!mock
        .calls()
        .iter()
        .any(|r| matches!(r, Request::Search { .. })));
}

/// A change on one page must cost that page's tiles and nothing else: the tile cache's content
/// revision is per page.
#[test]
fn invalidating_one_page_re_renders_that_page_only() {
    let (mock, mut h) = mock_app(3);
    {
        // Small pages, so the first two are on screen together.
        let dv = h.state_mut().active_view_mut().unwrap();
        dv.view.set_zoom(0.4);
        dv.go_to_page(0);
    }
    settle(&mut h);
    let doc = h.state().active_view().unwrap().doc;
    let visible = h.state().active_view().unwrap().view.visible_pages();
    assert!(visible.contains(&0) && visible.contains(&1), "{visible:?}");
    let tiles_of = |mock: &MockBackend, page: u32| {
        mock.rendered_tiles()
            .iter()
            .filter(|t| t.page == page)
            .count()
    };
    let (first, second) = (tiles_of(&mock, 0), tiles_of(&mock, 1));
    assert!(first > 0 && second > 0);
    let cached = h.state().tile_cache().len();
    let textures = h.state().textures().tile_count();

    h.state_mut().invalidate_page(doc, 0);
    // The first page's tiles (of every zoom level seen so far) are gone at once, before
    // anything is rendered again; nothing unreachable is left to wait for LRU pressure.
    let after_drop = h.state().tile_cache().len();
    assert!(after_drop < cached);
    assert!(h.state().textures().tile_count() < textures);
    settle(&mut h);

    // The first page was rendered again: exactly the tiles that are now back in the cache.
    // The second page, on screen beside it, was not rendered again at all.
    let again = tiles_of(&mock, 0) - first;
    assert!(again > 0, "the changed page was not re-rendered");
    assert_eq!(again, h.state().tile_cache().len() - after_drop);
    assert_eq!(
        tiles_of(&mock, 1),
        second,
        "another page was rendered again"
    );
    assert_eq!(tiles_of(&mock, 2), 0);
    let session_doc = h.state().session().document(doc).unwrap();
    let (render, revision) = (session_doc.render_id(), session_doc.page_revision(0));
    assert!(revision > 0);
    assert_eq!(session_doc.page_revision(1), 0);
    assert_eq!(session_doc.page_revision(2), 0);
    // Every cached tile of the first page is of its current revision.
    assert_eq!(h.state().tile_cache().remove_stale(render, 0, revision), 0);

    // A page that does not exist is ignored.
    h.state_mut().invalidate_page(doc, 99);
    settle(&mut h);
    assert_eq!(tiles_of(&mock, 0), first + again);
}

/// The app's own page geometry on pages whose boxes are inherited and rotated: the open
/// loads no page, the geometry arrives through the scheduler, and a drag over a label selects
/// exactly that label (which only works if page coordinates on screen are the file's).
#[test]
fn real_pdfium_lazy_geometry_is_right_on_rotated_pages_with_inherited_boxes() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::inherited_boxes();
    let path = write_fixture(dir.path(), &fixture);
    let backend = pdfium();
    let mut state = state_with(backend.clone(), None);
    state.open_path(&path).expect("opens");
    {
        let doc = state.session().documents().next().unwrap();
        assert_eq!(
            backend.page_loads(doc.render_id()).unwrap(),
            0,
            "opening loaded a page"
        );
        assert!(!doc.geometry_is_exact(0));
    }
    let mut h = harness(state);
    settle(&mut h);
    assert!(h.state().status().is_none(), "{:?}", h.state().status());

    for (page, truth) in fixture.manifest.pages.iter().enumerate() {
        let page = page as u32;
        {
            let dv = h.state_mut().active_view_mut().unwrap();
            dv.view.set_zoom(1.5);
            dv.go_to_page(page);
        }
        settle(&mut h);
        let dv = h.state().active_view().unwrap();
        let geometry = *dv.view.file_geometry(page).unwrap();
        assert_eq!(geometry.rotate().degrees(), truth.rotate, "page {page}");
        let (crop, want) = (geometry.crop_box(), truth.visible_box());
        assert_eq!([crop.x0, crop.y0, crop.x1, crop.y1], want, "page {page}");

        // Bring the label into view (on a rotated page it is not at the top), then drag along
        // its baseline, from its first to its last character.
        let label = &truth.texts[0];
        let b = label.bbox();
        h.state_mut().active_view_mut().unwrap().view.reveal_rect(
            page,
            pl_doc::pl_geom::Rect::new(b[0], b[1], b[2], b[3]),
            60.0,
        );
        settle(&mut h);
        let y = label.origin[1] + 0.3 * label.font_size;
        let a = on_screen(&h, page, Point::new(label.origin[0] + 1.0, y));
        let b = on_screen(&h, page, Point::new(label.origin[0] + label.width - 1.0, y));
        let rect = h.state().active_view().unwrap().rect;
        assert!(
            rect.contains(a) && rect.contains(b),
            "page {page}: label off screen"
        );
        h.hover_at(a);
        settle(&mut h);
        h.drag_at(a);
        h.hover_at(a + (b - a) * 0.5);
        h.hover_at(b);
        h.drop_at(b);
        h.run();
        let selection = h.state().active_view().unwrap().selection.clone();
        assert_eq!(
            selection.as_ref().map(|s| s.text.as_str()),
            Some(label.text.as_str()),
            "page {page}"
        );
        assert_eq!(selection.unwrap().page, page);
        assert!(h.state().textures().tile_count() > 0);
    }
    assert!(h.state().status().is_none(), "{:?}", h.state().status());
}

/// One page that cannot be loaded must not keep the document from opening in the window.
#[test]
fn real_pdfium_a_document_with_an_unloadable_page_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::broken_page_tree();
    let path = write_fixture(dir.path(), &fixture);
    let mut state = state_with(pdfium(), None);
    assert!(state.open_path(&path).is_some(), "{:?}", state.status());
    let mut h = harness(state);
    settle(&mut h);
    assert_eq!(h.state().session().len(), 1);
    assert_eq!(h.state().status_info().page, "Page 1 of 3");
    let doc = h.state().session().documents().next().unwrap();
    assert!(doc.warnings().iter().any(|w| w.contains("page 2")));
    assert!(doc.geometry_is_exact(0));
    assert!(h.state().textures().tile_count() > 0);
    // The last page is reachable and drawn; the broken one in between is a placeholder.
    h.state_mut().active_view_mut().unwrap().go_to_page(2);
    settle(&mut h);
    assert_eq!(shown_page(&h), 2);
    let doc = h.state().session().documents().next().unwrap();
    assert!(doc.geometry_is_exact(2));
    assert!(!doc.geometry_is_exact(1));
}

// ---- snapshots (few: goldens depend on the GPU adapter) ---------------------------------------

#[test]
fn snapshot_viewer_light() {
    let _gpu = gpu();
    let mock = Arc::new(MockBackend::with_pages(&letters(3)));
    mock.set_page_text(synthetic_text(
        0,
        &[("Plumbline sample text", [72.0, 640.0], 24.0)],
    ));
    let mut state = state_with(mock.clone(), None);
    state.open_bytes("sample.pdf", Vec::new()).unwrap();
    let mut h = harness(state);
    settle(&mut h);
    h.state_mut().run(&egui::Context::default(), Command::Find);
    h.state_mut().set_find_text("sample");
    h.state_mut().find(true);
    settle(&mut h);
    assert_eq!(
        h.state().active_view().unwrap().search_position(),
        Some((1, 1))
    );
    h.snapshot("viewer_light");
}

#[test]
fn snapshot_start_page_dark() {
    let _gpu = gpu();
    let mut state = state_with(Arc::new(MockBackend::new()), None);
    state.set_theme(egui::ThemePreference::Dark);
    let mut h = harness_themed(state, egui::Theme::Dark);
    h.run();
    h.snapshot("start_page_dark");
}
