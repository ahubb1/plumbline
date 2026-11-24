//! The window keeps drawing while a document is saved or its markups and layers are read:
//! both run on a worker thread and are taken in by a later frame. Tested against real PDFium
//! behind a wrapper that makes the save's backend requests slow, so a frame that waited for
//! the save even once would be far over its budget.

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use egui::Key;
use egui_kittest::kittest::Queryable;
use pl_app::{ScriptedDialogs, SidePage, Tool};
use pl_doc::pl_geom::{Point, Rect};
use pl_doc::pl_render::{PdfiumBackend, RenderBackend, RenderError, Reply, Request};
use support::*;

/// How long the slow backend takes over each request a save makes of it.
const SLOW: Duration = Duration::from_millis(150);
/// The longest a frame may take while a save is in flight: well under one slow request.
const FRAME_BUDGET: Duration = Duration::from_millis(100);

/// PDFium behind a wrapper that can be made slow, and that counts the calls of one thread.
struct SlowBackend {
    inner: PdfiumBackend,
    delay_ms: AtomicU64,
    watched: Mutex<Option<ThreadId>>,
    watched_calls: AtomicUsize,
}

impl SlowBackend {
    fn new() -> Arc<SlowBackend> {
        let dll = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("vendor")
            .join("pdfium")
            .join("8125")
            .join("bin")
            .join("pdfium.dll");
        let dll = std::path::absolute(dll).unwrap();
        Arc::new(SlowBackend {
            inner: PdfiumBackend::new(Some(&dll)).expect("load vendored pdfium.dll"),
            delay_ms: AtomicU64::new(0),
            watched: Mutex::new(None),
            watched_calls: AtomicUsize::new(0),
        })
    }

    fn set_slow(&self, slow: bool) {
        let ms = if slow { SLOW.as_millis() as u64 } else { 0 };
        self.delay_ms.store(ms, Ordering::SeqCst);
    }

    fn watch_this_thread(&self) {
        *self.watched.lock().unwrap() = Some(std::thread::current().id());
        self.watched_calls.store(0, Ordering::SeqCst);
    }

    fn unwatch(&self) -> usize {
        *self.watched.lock().unwrap() = None;
        self.watched_calls.load(Ordering::SeqCst)
    }
}

impl RenderBackend for SlowBackend {
    fn call(&self, request: Request) -> Result<Reply, RenderError> {
        if *self.watched.lock().unwrap() == Some(std::thread::current().id()) {
            self.watched_calls.fetch_add(1, Ordering::SeqCst);
        }
        let delay = self.delay_ms.load(Ordering::SeqCst);
        let heavy = matches!(
            request,
            Request::Open(_) | Request::Close { .. } | Request::Thumbnail { .. }
        );
        if delay > 0 && heavy {
            std::thread::sleep(Duration::from_millis(delay));
        }
        self.inner.call(request)
    }
}

#[test]
fn a_slow_save_runs_in_the_background_while_frames_keep_coming() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::plain();
    let path = write_fixture(dir.path(), &fixture);
    let backend = SlowBackend::new();
    let mut h = file_app(
        backend.clone(),
        &path,
        None,
        ScriptedDialogs::default(),
        0.25,
    );
    let view = h.state().active().unwrap();
    draw_rect(&mut h, 0, Rect::new(360.0, 600.0, 440.0, 650.0));
    assert_eq!(markups(&h).len(), 1);
    assert!(h.state().is_modified(view));
    assert!(h.state().background_activity().is_none());
    settle(&mut h);
    let jobs_before = h.state().finished_jobs();

    // Ctrl+S with a backend that takes its time: the frame that starts the save returns at
    // once, and the save is in flight.
    backend.set_slow(true);
    backend.watch_this_thread();
    h.key_press_modifiers(CTRL, Key::S);
    let started = Instant::now();
    h.step();
    let first = started.elapsed();
    assert!(
        first < FRAME_BUDGET,
        "the frame that started the save took {first:?}"
    );
    assert!(h.state().is_saving(view));
    assert_eq!(
        h.state().background_activity().as_deref(),
        Some("Saving plain.pdf")
    );

    // Frames keep running. Meanwhile the document is read-only: a rectangle dragged out with
    // the pointer is not drawn, undo does nothing, the tab does not close, and a small mark
    // in the status bar says what is going on.
    let mut frames = 0;
    let mut longest = Duration::ZERO;
    let mut saw_mark = false;
    let (from, to) = (
        on_screen(&h, 0, Point::new(100.0, 700.0)),
        on_screen(&h, 0, Point::new(200.0, 640.0)),
    );
    while h.state().is_saving(view) {
        match frames {
            1 => {
                h.state_mut().set_tool(Tool::Rectangle);
                h.hover_at(from);
            }
            2 => h.drag_at(from),
            3 => h.hover_at(to),
            4 => h.drop_at(to),
            5 => h.key_press_modifiers(CTRL, Key::Z),
            6 => h.state_mut().request_close(view),
            _ => {}
        }
        let started = Instant::now();
        h.step();
        longest = longest.max(started.elapsed());
        frames += 1;
        if frames == 3 {
            saw_mark = h
                .query_by_role_and_label(
                    egui::accesskit::Role::ProgressIndicator,
                    "Saving plain.pdf\u{2026}",
                )
                .is_some();
            assert_all_named(&h, "while saving");
        }
        assert_eq!(
            markups(&h).len(),
            1,
            "frame {frames}: nothing changes during a save"
        );
        assert!(frames < 20_000, "the save never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
    let frame_calls = backend.unwatch();
    backend.set_slow(false);
    assert!(
        frames >= 10,
        "only {frames} frames ran while the slow save was in flight"
    );
    assert!(
        longest < FRAME_BUDGET,
        "a frame took {longest:?} while the save was in flight ({frames} frames)"
    );
    assert_eq!(
        frame_calls, 0,
        "the frames made no backend call of their own during the save"
    );
    assert!(saw_mark, "the status bar showed the save");
    assert!(h.state().view(view).is_some(), "the tab stayed open");

    // Taken in: the file has the markup, the document is clean and can be edited again.
    assert_eq!(h.state().finished_jobs(), jobs_before + 1);
    assert!(
        h.state().status().is_some_and(|s| s.starts_with("Saved")),
        "{:?}",
        h.state().status()
    );
    assert!(h.state().background_activity().is_none());
    assert!(!h.state().is_modified(view));
    let saved = std::fs::read(&path).unwrap();
    assert!(saved.len() > fixture.bytes.len() && saved.starts_with(&fixture.bytes));
    settle(&mut h);
    assert!(
        h.state().textures().tile_count() > 0,
        "the page is drawn again"
    );
    h.state_mut().set_tool(Tool::Select);
    let second = draw_rect(&mut h, 0, Rect::new(100.0, 640.0, 200.0, 700.0));
    assert_eq!(markups(&h).len(), 2);
    assert!(h.state().is_modified(view));

    // "Save and close" from the close prompt: the tab closes when the save has succeeded.
    h.state_mut().request_close(view);
    h.run();
    assert!(h.state().close_prompt_open());
    backend.set_slow(true);
    h.get_by_label("Save and close").click();
    h.step();
    h.step();
    assert!(!h.state().close_prompt_open());
    assert!(
        h.state().view(view).is_some(),
        "still open while it is being saved"
    );
    assert!(h.state().is_saving(view));
    let mut frames = 0;
    while h.state().view(view).is_some() {
        h.step();
        frames += 1;
        assert!(frames < 20_000, "the save never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
    backend.set_slow(false);
    assert!(frames >= 10, "{frames} frames");
    assert!(h.state().session().is_empty());
    // The copy on disk has both rectangles.
    let mut h = file_app(
        backend.clone(),
        &path,
        None,
        ScriptedDialogs::default(),
        0.25,
    );
    h.state_mut().select_all_markups();
    h.run();
    let back = markups(&h);
    assert_eq!(back.len(), 2);
    assert_eq!(back[1].id, second.id);
}

#[test]
fn markups_and_layers_are_read_on_a_worker_thread_when_they_are_first_needed() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::annotations();
    let path = write_fixture(dir.path(), &fixture);
    let mut h = real_app(&path);
    let loaded = |h: &App| h.state().active_document().unwrap().annots_loaded();
    assert!(!loaded(&h), "nothing is read at open");
    assert_eq!(h.state().finished_jobs(), 0);

    // The pointer over the page with a markup tool: the read starts, on a worker thread, in
    // the frame that sees it, and that frame does not wait for it.
    let over = h.state().active_view().unwrap().rect.center();
    h.hover_at(over);
    h.step();
    assert_eq!(
        h.state().background_activity().as_deref(),
        Some("Reading markups")
    );
    assert!(
        !loaded(&h),
        "the frame that started the read did not wait for it"
    );
    // Frames keep coming until it is in.
    h.run();
    assert!(loaded(&h));
    assert_eq!(h.state().finished_jobs(), 1);
    assert!(h.state().background_activity().is_none());
    let expected: usize = fixture
        .manifest
        .pages
        .iter()
        .map(|p| p.annotations.len())
        .sum();
    assert!(expected > 0);
    assert_eq!(markups(&h).len(), expected);
    // Hovering again starts nothing.
    h.hover_at(over + egui::vec2(5.0, 5.0));
    h.run();
    assert_eq!(h.state().finished_jobs(), 1);

    // The Pan tool has no use for the markups: hovering with it reads nothing.
    let other = write_fixture(dir.path(), &pl_testkit::plain());
    let mut h = real_app(&other);
    h.state_mut().set_tool(Tool::Pan);
    h.hover_at(over);
    h.run();
    assert!(!loaded(&h));
    // The markups list does: opening it reads them, and says so until they are in.
    h.remove_cursor();
    h.state_mut().set_markups_list_open(true);
    h.step();
    assert!(!loaded(&h));
    h.get_by_label("Reading this document's markups\u{2026}");
    h.run();
    assert!(loaded(&h));

    // The layer list, for the Layers page.
    let layered = pl_testkit::layers();
    let path = write_fixture(dir.path(), &layered);
    let mut h = real_app(&path);
    h.state_mut().set_side_page(SidePage::Layers);
    h.step();
    assert_eq!(
        h.state().background_activity().as_deref(),
        Some("Reading layers")
    );
    assert!(h.state().layers().is_empty());
    h.get_by_label("Reading layers\u{2026}");
    h.run();
    assert_eq!(
        h.state().layers(),
        [("Layer A".to_owned(), true), ("Layer B".to_owned(), true)]
    );
    assert!(!loaded(&h), "the layer list was read without the markups");
    assert_all_named(&h, "the Layers page");
}

/// The real sample drawing set, when it is there: its markups are read and a markup is saved
/// to a copy under `target` without a frame being held up.
#[test]
fn real_sample_is_read_and_saved_without_holding_a_frame_up() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let sample = root.join("samples").join("site-development-plans.pdf");
    if !sample.is_file() {
        eprintln!("SAMPLE SKIPPED: {} is not there", sample.display());
        return;
    }
    let out = root.join("target").join("tmp").join("pl-app-background");
    std::fs::create_dir_all(&out).unwrap();
    let copy = out.join("sample-background-save.pdf");
    let _ = std::fs::remove_file(&copy);
    let dialogs = ScriptedDialogs::default().with_saves([copy.clone()]);
    let mut h = file_app(pdfium(), &sample, None, dialogs, 0.25);
    let view = h.state().active().unwrap();

    // Reading the markups: frames keep coming, none of them long.
    let over = h.state().active_view().unwrap().rect.center();
    h.hover_at(over);
    let mut longest = Duration::ZERO;
    let mut frames = 0;
    loop {
        let started = Instant::now();
        h.step();
        longest = longest.max(started.elapsed());
        frames += 1;
        if h.state().active_document().unwrap().annots_loaded() {
            break;
        }
        assert!(frames < 100_000, "the markups were never read");
    }
    h.remove_cursor();
    assert!(
        longest < Duration::from_millis(250),
        "a frame took {longest:?} while the markups were read"
    );

    // A markup, saved to a copy (never to the sample): frames keep coming meanwhile.
    let geometry = *h
        .state()
        .active_view()
        .unwrap()
        .view
        .page_geometry(0)
        .unwrap();
    let crop = geometry.crop_box();
    let rect = Rect::new(
        crop.x0 + crop.width() * 0.45,
        crop.y0 + crop.height() * 0.45,
        crop.x0 + crop.width() * 0.55,
        crop.y0 + crop.height() * 0.55,
    );
    h.state_mut()
        .active_view_mut()
        .unwrap()
        .view
        .reveal_rect(0, rect, 40.0);
    settle(&mut h);
    let drawn = draw_rect(&mut h, 0, rect);
    let original = std::fs::metadata(&sample).unwrap().len();
    assert!(h.state_mut().start_save_active(pl_app::SaveHow::As));
    let mut longest = Duration::ZERO;
    let mut frames = 0;
    while h.state().is_saving(view) {
        let started = Instant::now();
        h.step();
        longest = longest.max(started.elapsed());
        frames += 1;
        assert!(frames < 1_000_000, "the save never finished");
    }
    assert!(
        longest < Duration::from_millis(250),
        "a frame took {longest:?} while the sample was saved ({frames} frames)"
    );
    assert!(frames > 1, "{frames} frames ran during the save");
    assert!(
        h.state().status().is_some_and(|s| s.starts_with("Saved")),
        "{:?}",
        h.state().status()
    );
    assert_eq!(std::fs::metadata(&sample).unwrap().len(), original);
    assert!(std::fs::metadata(&copy).unwrap().len() > original);
    drop(h);
    let mut h = file_app(pdfium(), &copy, None, ScriptedDialogs::default(), 0.25);
    h.state_mut().select_all_markups();
    h.run();
    assert!(markups(&h).iter().any(|a| a.id == drawn.id));
    drop(h);
    let _ = std::fs::remove_file(&copy);
}
