//! The interactive open, the save path with PDFium as verifier, and file release, against
//! real PDFium and the pl-testkit fixtures. Headless: nothing here opens a window.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use pl_doc::pl_geom::Size;
use pl_doc::pl_pdf::{
    Dictionary, EditSet, Object, ObjectId, PdfError, SaveOptions, StringFormat, VerifyError,
};
use pl_doc::pl_render::{
    JobKind, JobOutput, PdfiumBackend, RenderBackend, Scheduler, StreamId, TileCache,
};
use pl_doc::pl_scene::SceneLimits;
use pl_doc::{DocError, Document, OpenMode, SceneState, Session, SessionOptions, UserUnitPolicy};
use pl_testkit::{Fixture, Options};

fn vendor_dll() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    std::path::absolute(p).unwrap()
}

fn backend() -> Arc<PdfiumBackend> {
    Arc::new(PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll"))
}

fn on_disk(fixture: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    (dir, path)
}

fn small_fixtures() -> Vec<Fixture> {
    pl_testkit::all_fixtures_with(&Options::small())
}

const WAIT: Duration = Duration::from_secs(60);

// ---- the interactive open -----------------------------------------------------------------

#[test]
fn an_interactive_open_loads_no_page_whatever_is_drawn_on_them() {
    // The default session is the user interface's. Opening must cost the backend's open and
    // one request for page sizes: no page handle (PDFium parses a page's content when it
    // loads one), no lopdf parse, on any fixture, the dense sheet included.
    let backend = backend();
    let mut s = Session::new(backend.clone());
    assert_eq!(s.options().open, OpenMode::Interactive);
    for fixture in small_fixtures() {
        let m = &fixture.manifest;
        let (_dir, path) = on_disk(&fixture);
        let id = s
            .open(&path, None)
            .unwrap_or_else(|e| panic!("open {}: {e}", m.name));
        let doc = s.document_mut(id).unwrap();
        assert_eq!(
            backend.page_loads(doc.render_id()).unwrap(),
            0,
            "{}: the open loaded a page",
            m.name
        );
        assert!(
            !doc.pdf_is_loaded(),
            "{}: the open parsed with lopdf",
            m.name
        );
        assert_eq!(doc.page_count() as usize, m.page_count());

        // Every page lays out at its real displayed size (in user-space units; the unit
        // itself arrives in the background).
        for (i, truth) in m.pages.iter().enumerate() {
            let shown = doc.pages()[i].display_size();
            let want = truth.display_size();
            let want = [want[0] / truth.user_unit, want[1] / truth.user_unit];
            assert!(
                (shown.width - want[0]).abs() < 1e-3 && (shown.height - want[1]).abs() < 1e-3,
                "{} page {i}: {shown:?} vs {want:?}",
                m.name
            );
            assert!(matches!(
                doc.page_geometry(i as u32),
                Err(DocError::GeometryPending { .. })
            ));
        }

        // /UserUnit is found off the open path, by hayro-syntax on the mapped file: also
        // inside object streams, and still without lopdf having parsed anything.
        assert!(doc.wait_background(WAIT), "{}", m.name);
        assert!(!doc.background_pending());
        assert!(!doc.pdf_is_loaded(), "{}", m.name);
        for (i, truth) in m.pages.iter().enumerate() {
            assert_eq!(
                doc.pages()[i].user_unit(),
                truth.user_unit,
                "{} page {i}",
                m.name
            );
            let shown = doc.pages()[i].display_size_points();
            let want = truth.display_size();
            assert!((shown.width - want[0]).abs() < 1e-3 && (shown.height - want[1]).abs() < 1e-3);
        }
        assert_eq!(backend.page_loads(doc.render_id()).unwrap(), 0);
        assert!(
            doc.warnings().is_empty(),
            "{}: {:?}",
            m.name,
            doc.warnings()
        );

        // The real geometry on demand equals the manifest, one load per page.
        for (i, truth) in m.pages.iter().enumerate() {
            let g = *doc.load_geometry(i as u32).unwrap();
            let (crop, v) = (g.crop_box(), truth.visible_box());
            for (got, want) in [crop.x0, crop.y0, crop.x1, crop.y1].iter().zip(v) {
                assert!((got - want).abs() < 1e-3, "{} page {i}", m.name);
            }
            assert_eq!(g.rotate().degrees(), truth.rotate, "{} page {i}", m.name);
            assert_eq!(g.user_unit(), truth.user_unit, "{} page {i}", m.name);
            assert_eq!(doc.page_geometry(i as u32).unwrap(), &g);
        }
        assert_eq!(
            backend.page_loads(doc.render_id()).unwrap(),
            m.page_count() as u64
        );
        s.close(id).unwrap();
    }
}

#[test]
fn geometry_text_and_outline_arrive_through_the_scheduler() {
    // What the app does: nothing is asked of the backend directly; jobs are queued and their
    // results handed to the document.
    let backend = backend();
    let fixture = pl_testkit::inherited_boxes();
    let (_dir, path) = on_disk(&fixture);
    let mut s = Session::new(backend.clone());
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let render = doc.render_id();
    let dyn_backend: Arc<dyn RenderBackend> = backend.clone();
    let (scheduler, results) = Scheduler::new(dyn_backend, Arc::new(TileCache::default()));
    let stream = StreamId(9);
    for page in 0..doc.page_count() {
        assert!(scheduler.submit_query(stream, JobKind::PageInfo { doc: render, page }, page));
        assert!(scheduler.submit_query(stream, JobKind::PageText { doc: render, page }, page));
    }
    assert!(scheduler.submit_query(stream, JobKind::Outline { doc: render }, 0));
    assert!(scheduler.wait_idle(WAIT));

    let epoch = doc.geometry_epoch();
    let mut view = doc.new_view(Size::new(900.0, 700.0), 1.0);
    for result in results.try_iter() {
        match (result.job.kind, result.outcome.unwrap()) {
            (JobKind::PageInfo { doc: d, page }, JobOutput::PageInfo(info)) => {
                doc.accept_page_info(d, page, info);
            }
            (JobKind::PageText { doc: d, page }, JobOutput::Text(text)) => {
                assert!(doc.accept_text(d, page, text));
            }
            (JobKind::Outline { doc: d }, JobOutput::Outline(items)) => {
                assert!(doc.accept_outline(d, &items));
            }
            other => panic!("unexpected result {other:?}"),
        }
    }
    // Rotated pages and offset origins differ from their stand-ins: the layout epoch moved,
    // and a view that follows it lands on the real geometry.
    assert!(doc.geometry_epoch() > epoch);
    view.set_pages(doc.pages());
    for (i, truth) in fixture.manifest.pages.iter().enumerate() {
        let g = doc.page_geometry(i as u32).unwrap();
        assert_eq!(g.rotate().degrees(), truth.rotate);
        let v = truth.visible_box();
        let crop = g.crop_box();
        assert_eq!([crop.x0, crop.y0, crop.x1, crop.y1], v, "page {i}");
        assert_eq!(view.file_geometry(i as u32).unwrap(), g);
        // The text boxes are in that page space: the label sits where the manifest says.
        let text = doc.cached_text(i as u32).expect("text arrived");
        let label = &truth.texts[0];
        assert!(text.text().contains(&label.text), "page {i}");
        let first = text.chars.iter().find(|c| c.has_box()).unwrap();
        assert!(
            (first.origin[0] - label.origin[0]).abs() < 0.5
                && (first.origin[1] - label.origin[1]).abs() < 0.5,
            "page {i}: {:?} vs {:?}",
            first.origin,
            label.origin
        );
    }
    assert!(doc.cached_bookmarks().unwrap().is_empty());
    drop(scheduler);
    s.close(id).unwrap();
}

#[test]
fn the_unit_inside_an_object_stream_is_found_without_lopdf() {
    // A raw-byte scan cannot see this key, and "any file with object streams" must not be a
    // reason to parse the whole file at open.
    let fixture = pl_testkit::user_unit_in_object_stream();
    let (_dir, path) = on_disk(&fixture);
    for mode in [OpenMode::Interactive, OpenMode::Complete] {
        let mut s = Session::with_options(
            backend(),
            SessionOptions {
                open: mode,
                ..SessionOptions::default()
            },
        );
        let id = s.open(&path, None).unwrap();
        let doc = s.document_mut(id).unwrap();
        assert!(doc.wait_background(WAIT));
        assert_eq!(doc.pages()[0].user_unit(), 2.0, "{mode:?}");
        assert!(!doc.pdf_is_loaded(), "{mode:?}");
        assert_eq!(doc.load_geometry(0).unwrap().user_unit(), 2.0);
        let d = doc.pages()[0].display_size_points();
        assert_eq!((d.width, d.height), (612.0, 792.0));
        s.close(id).unwrap();
    }

    // An ordinary file with object streams and no unit: nothing is found, nothing is parsed.
    let fixture = pl_testkit::xref_object_streams();
    let (_dir, path) = on_disk(&fixture);
    let mut s = Session::new(backend());
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert!(doc.wait_background(WAIT));
    assert!(doc.pages().iter().all(|p| p.user_unit() == 1.0));
    assert!(!doc.pdf_is_loaded());
    assert!(doc.warnings().is_empty());

    // Never means never, in both modes.
    let mut never = Session::with_options(
        backend(),
        SessionOptions {
            user_unit: UserUnitPolicy::Never,
            ..SessionOptions::default()
        },
    );
    let (_dir, path) = on_disk(&pl_testkit::user_unit());
    let id = never.open(&path, None).unwrap();
    let doc = never.document_mut(id).unwrap();
    assert!(!doc.background_pending());
    assert_eq!(doc.pages()[0].user_unit(), 1.0);
}

#[test]
fn one_unloadable_page_does_not_stop_the_document_from_opening() {
    let fixture = pl_testkit::broken_page_tree();
    let broken = pl_testkit::BROKEN_PAGE_INDEX as u32;
    let (_dir, path) = on_disk(&fixture);
    for mode in [OpenMode::Interactive, OpenMode::Complete] {
        let mut s = Session::with_options(
            backend(),
            SessionOptions {
                open: mode,
                ..SessionOptions::default()
            },
        );
        let id = s
            .open(&path, None)
            .unwrap_or_else(|e| panic!("{mode:?}: the whole document failed to open: {e}"));
        let doc = s.document_mut(id).unwrap();
        assert_eq!(doc.page_count(), 3, "{mode:?}");
        // The broken page is reported, laid out as a placeholder of the previous page's
        // size, and fails on its own.
        assert!(
            doc.warnings().iter().any(|w| w.contains("page 2")),
            "{mode:?}: {:?}",
            doc.warnings()
        );
        let size = doc.pages()[broken as usize].display_size();
        assert_eq!((size.width, size.height), (612.0, 792.0));
        assert!(matches!(
            doc.page_geometry(broken),
            Err(DocError::PageUnreadable { page: 1 })
        ));
        assert!(doc.load_geometry(broken).is_err());
        assert!(doc.page_text(broken).is_err());
        // The pages around it are whole.
        for page in [0u32, 2] {
            let g = *doc.load_geometry(page).unwrap();
            assert_eq!(g.display_size(), Size::new(612.0, 792.0));
            let text = doc.page_text(page).unwrap().text();
            assert!(text.contains(&format!("BROKEN-TREE page {}", page + 1)));
            doc.backend()
                .render_page(doc.render_id(), page, 0.5)
                .unwrap();
        }
        let found = doc.search("BROKEN-TREE", Default::default());
        // A whole-document search through the backend stops at the broken page with an
        // error; fed page by page (as the app does) it skips the page and finishes.
        assert!(found.is_err(), "{mode:?}");
        let mut search = doc.start_search("BROKEN-TREE", Default::default());
        for page in 0..3 {
            let text = doc.page_text(page).ok();
            search.feed(page, text.as_deref());
        }
        assert!(search.is_finished());
        assert_eq!(
            search.hits().iter().map(|h| h.page).collect::<Vec<_>>(),
            [0, 2]
        );

        // The same after the mirror is reopened (the path a save or a layer toggle takes).
        doc.suspend_render().unwrap();
        doc.reopen_render(Vec::new())
            .unwrap_or_else(|e| panic!("{mode:?}: reopen failed: {e}"));
        assert_eq!(doc.page_count(), 3);
        assert!(doc.load_geometry(0).is_ok() && doc.load_geometry(broken).is_err());
        let warnings = doc
            .warnings()
            .iter()
            .filter(|w| w.contains("page 2"))
            .count();
        assert!(warnings >= 1);
        s.close(id).unwrap();
    }
}

// ---- save, with PDFium as the third reader -------------------------------------------------

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn text(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

fn ints(v: &[i64]) -> Object {
    Object::Array(v.iter().map(|i| Object::Integer(*i)).collect())
}

/// A 100 x 50 rectangle 30 units inside the lower-left corner of a page's visible box, so the
/// square is on the visible part of every fixture page (cropped and offset ones included).
fn square_rect(visible_box: [f64; 4]) -> [i64; 4] {
    let (x, y) = (visible_box[0] as i64 + 30, visible_box[1] as i64 + 30);
    [x, y, x + 100, y + 50]
}

/// Adds a red Square with a hand-built appearance to a page. Returns the annotation's id.
fn add_square(
    edits: &mut EditSet,
    page_id: ObjectId,
    page_index: usize,
    rect: [i64; 4],
) -> ObjectId {
    let mut ap = Dictionary::new();
    ap.set("Type", name("XObject"));
    ap.set("Subtype", name("Form"));
    ap.set("BBox", ints(&[0, 0, 100, 50]));
    ap.set("Resources", Object::Dictionary(Dictionary::new()));
    let ap_id = edits.add_stream(ap, b"q 1 0 0 RG 4 w 2 2 96 46 re S Q".to_vec());
    let mut n = Dictionary::new();
    n.set("N", Object::Reference(ap_id));
    let mut annot = Dictionary::new();
    annot.set("Type", name("Annot"));
    annot.set("Subtype", name("Square"));
    annot.set("Rect", ints(&rect));
    annot.set("C", ints(&[1, 0, 0]));
    annot.set("F", Object::Integer(4));
    annot.set("NM", text("pl-doc-test-0001"));
    annot.set("Contents", text("Plumbline save test"));
    annot.set("P", Object::Reference(page_id));
    annot.set("AP", Object::Dictionary(n));
    edits.add_annotation(page_index, annot)
}

/// `qpdf` if it can be found: `PLUMBLINE_QPDF` (a full path), or on the search path.
fn qpdf() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PLUMBLINE_QPDF").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    Command::new("qpdf")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from("qpdf"))
}

/// A page at 1 px per unit as the mirror shows it now, for before/after comparisons.
fn render(doc: &Document, page: u32) -> pl_doc::pl_render::Bitmap {
    doc.backend()
        .render_page(doc.render_id(), page, 1.0)
        .unwrap()
}

/// M0: "Incremental save on each fixture keeps the original bytes as an exact prefix and
/// reopens in PDFium, hayro-syntax and `qpdf --check`." pl-pdf's own tests cover the prefix,
/// lopdf and hayro-syntax; this is the PDFium third, through the session's save path, plus
/// qpdf where it is installed.
#[test]
fn incremental_save_on_every_fixture_reopens_in_pdfium() {
    let backend = backend();
    let qpdf = qpdf();
    let mut qpdf_checked = 0;
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    for fixture in small_fixtures() {
        let m = &fixture.manifest;
        let (_dir, path) = on_disk(&fixture);
        let id = s.open(&path, None).unwrap();
        let doc = s.document_mut(id).unwrap();
        let pages = doc.page_count();
        let target = pages - 1;
        let counts_before: Vec<usize> = (0..pages)
            .map(|p| doc.annotations(p).unwrap().len())
            .collect();
        let untouched_before: Vec<_> = (0..target).map(|p| render(doc, p)).collect();
        let target_before = render(doc, target);
        let old_render = doc.render_id();

        let rect = square_rect(m.pages[target as usize].visible_box());
        let (edits, annot_id) = {
            let pdf = doc.pdf().unwrap().document().unwrap();
            let mut edits = pdf.edit();
            let page_id = pdf.page_id(target as usize).unwrap();
            let id = add_square(&mut edits, page_id, target as usize, rect);
            (edits, id)
        };
        let outcome = doc
            .save_incremental(&edits, &[], &SaveOptions::default())
            .unwrap_or_else(|e| panic!("{}: {e}", m.name));

        // pl-pdf's part: appended, prefix intact, lopdf and hayro-syntax agreed.
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.len() > fixture.bytes.len(), "{}", m.name);
        assert_eq!(
            saved[..fixture.bytes.len()],
            fixture.bytes[..],
            "{}",
            m.name
        );
        assert!(outcome.report.verification.as_ref().unwrap().prefix_checked);
        // PDFium's part, before the replace: the touched page's count, and the untouched
        // sample's renders.
        assert!(
            outcome.pdfium.annotation_pages.contains(&target),
            "{}",
            m.name
        );
        assert_eq!(
            outcome.pdfium.render_pages.len(),
            (pages as usize - 1).min(3),
            "{}",
            m.name
        );
        assert!(!outcome.pdfium.render_pages.contains(&target));

        // And the document as it is now shown, from the saved file through PDFium.
        assert_ne!(
            doc.render_id(),
            old_render,
            "{}: the mirror was reopened",
            m.name
        );
        assert_eq!(doc.page_count(), pages, "{}", m.name);
        for page in 0..pages {
            let want = counts_before[page as usize] + usize::from(page == target);
            let found = doc.annotations(page).unwrap();
            assert_eq!(found.len(), want, "{} page {page}", m.name);
        }
        let added = doc.annotations(target).unwrap();
        let square = added.last().unwrap();
        assert_eq!(square.subtype, "Square", "{}", m.name);
        assert_eq!(square.name.as_deref(), Some("pl-doc-test-0001"));
        assert_eq!(square.rect, rect.map(|v| v as f64), "{}", m.name);
        // Untouched pages render exactly as before; the touched page shows the new markup.
        for (page, before) in untouched_before.iter().enumerate() {
            assert_eq!(&render(doc, page as u32), before, "{} page {page}", m.name);
        }
        let target_after = render(doc, target);
        assert_ne!(
            target_after, target_before,
            "{}: the square is drawn",
            m.name
        );
        // The geometry survived the reopen and the page's text is still there.
        for (i, truth) in m.pages.iter().enumerate() {
            let g = doc.page_geometry(i as u32).unwrap();
            assert_eq!(g.rotate().degrees(), truth.rotate, "{} page {i}", m.name);
            assert_eq!(g.user_unit(), truth.user_unit, "{} page {i}", m.name);
        }
        if let Some(label) = m.pages[target as usize].texts.first() {
            assert!(doc.page_text(target).unwrap().text().contains(&label.text));
        }
        // lopdf holds the verified new state: the annotation is where the edit put it.
        let pdf = doc.pdf().unwrap().document().unwrap();
        assert_eq!(
            pdf.annotations(target as usize).unwrap().last().unwrap().id,
            Some(annot_id)
        );

        if let Some(qpdf) = &qpdf {
            let mut command = Command::new(qpdf);
            command.arg("--check");
            if m.encryption.is_some() {
                command.arg("--password=");
            }
            let out = command.arg(&path).output().unwrap();
            assert!(
                out.status.success(),
                "{}: qpdf --check failed:\n{}\n{}",
                m.name,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            qpdf_checked += 1;
        }
        s.close(id).unwrap();
    }
    match qpdf {
        Some(_) => println!("QPDF --check passed on {qpdf_checked} saved fixtures"),
        None => println!(
            "QPDF SKIPPED: qpdf was not found (set PLUMBLINE_QPDF to qpdf.exe, or put it on the \
             search path); the saved fixtures were checked with lopdf, hayro-syntax and PDFium only"
        ),
    }
}

#[test]
fn a_save_that_changes_an_untouched_page_is_stopped_before_the_replace() {
    // The render comparison is what catches a save that damages something it did not mean to
    // touch. Here the edit rewrites page 1's content reference while claiming to touch only
    // the last page: PDFium sees page 1 change, and the original file stays as it was.
    let backend = backend();
    let fixture = pl_testkit::xref_classic();
    let (_dir, path) = on_disk(&fixture);
    let mut s = Session::with_options(backend, SessionOptions::complete());
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let before = render(doc, 0);

    let edits = {
        let pdf = doc.pdf().unwrap().document().unwrap();
        let mut edits = pdf.edit();
        add_square(&mut edits, pdf.page_id(2).unwrap(), 2, [100, 100, 200, 150]);
        // Collateral damage: page 1 now shows page 2's content.
        let other = pdf
            .object(pdf.page_id(1).unwrap())
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .clone();
        edits.patch(pdf.page_id(0).unwrap()).set("Contents", other);
        edits
    };
    let err = doc
        .save_incremental(&edits, &[], &SaveOptions::default())
        .unwrap_err();
    assert!(
        matches!(
            err,
            DocError::Pdf(PdfError::Verify(VerifyError::RenderChanged {
                parser: "PDFium",
                page_index: 0
            }))
        ),
        "{err:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
    assert!(!pl_doc::pl_pdf::tmp_path_for(&path).exists());
    // The document is shown again, from the untouched file.
    assert_eq!(render(doc, 0), before);
    assert_eq!(doc.annotations(2).unwrap().len(), 0);

    // Told that page 1 is touched too, the same edit is accepted: the check compares only
    // what the caller says is untouched.
    let edits = {
        let pdf = doc.pdf().unwrap().document().unwrap();
        let mut edits = pdf.edit();
        add_square(&mut edits, pdf.page_id(2).unwrap(), 2, [100, 100, 200, 150]);
        let other = pdf
            .object(pdf.page_id(1).unwrap())
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .clone();
        edits.patch(pdf.page_id(0).unwrap()).set("Contents", other);
        edits
    };
    let outcome = doc
        .save_incremental(&edits, &[0], &SaveOptions::default())
        .unwrap();
    assert_eq!(outcome.pdfium.render_pages, [1]);
    assert_ne!(render(doc, 0), before);
    assert_eq!(doc.annotations(2).unwrap().len(), 1);

    // An in-memory document has no file to save into.
    let mem = s.open_bytes("mem", fixture.bytes.clone(), None).unwrap();
    let doc = s.document_mut(mem).unwrap();
    assert!(matches!(doc.pdf(), Err(DocError::NoFile)));
}

// ---- releasing the file --------------------------------------------------------------------

#[test]
fn a_running_scene_build_holds_the_file_and_a_save_waits_for_it() {
    let backend = backend();
    let fixture = pl_testkit::dense_sheet();
    let (_dir, path) = on_disk(&fixture);
    let mut s = Session::new(backend);
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert!(doc.wait_background(WAIT));
    assert_eq!(doc.file_users(), 0);

    // A build of the 200,000-segment sheet takes a few hundred milliseconds. While its
    // worker runs it maps the file.
    assert!(matches!(doc.scene(0), SceneState::Pending));
    assert_eq!(doc.file_users(), 1);
    // With no time to wait, the release is refused and the mirror stays usable.
    match doc.suspend_render_within(Duration::ZERO) {
        Err(DocError::FileBusy { workers: 1, .. }) => {
            assert!(doc.annotations(0).is_ok(), "the mirror was closed anyway");
        }
        // The worker may have noticed the cancel between the two calls on a fast machine.
        Ok(()) => {}
        Err(other) => panic!("{other:?}"),
    }
    // The ordinary release cancels the build, waits for its thread to go, then closes the
    // mirror: nothing in this process maps the file any more, so it can be replaced.
    doc.suspend_render().unwrap();
    assert_eq!(doc.file_users(), 0);
    let replacement = pl_testkit::plain();
    let tmp = path.with_extension("pdf.new");
    std::fs::write(&tmp, &replacement.bytes).unwrap();
    std::fs::rename(&tmp, &path).expect("the file is free to be replaced");
    doc.reopen_render(Vec::new()).unwrap();
    assert_eq!(doc.page_count(), 1);

    // A build that has timed out is reported at once, but its worker still counts until it
    // has really exited.
    doc.set_scene_limits(SceneLimits {
        time_cap: Duration::ZERO,
        ..SceneLimits::default()
    });
    let state = doc.scene_wait(0);
    assert!(matches!(state, SceneState::Unavailable(_)), "{state:?}");
    doc.suspend_render().unwrap();
    assert_eq!(doc.file_users(), 0);
    std::fs::write(&path, &fixture.bytes).expect("the file is free again");
}
