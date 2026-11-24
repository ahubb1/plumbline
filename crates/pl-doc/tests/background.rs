//! Saving and loading on a worker thread ([`Document::start_save`],
//! [`Document::start_load_annots`]) against real PDFium behind a deliberately slow wrapper:
//! a frame-like loop keeps running while the save is in flight, the document is read-only
//! meanwhile, and the store, the render mirror, the hidden set, the journal and the modified
//! flag are consistent after a success and after an injected failure. Headless.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use pl_doc::pl_annot::{AnnotId, Annotation, Origin, PdfDate};
use pl_doc::pl_geom::{PageGeometry, Rect, Vec2};
use pl_doc::pl_pdf::{FailPoint, PdfError, SaveOptions};
use pl_doc::pl_render::{Bitmap, PdfiumBackend, RenderBackend, RenderError, Reply, Request};
use pl_doc::{
    Busy, Command, DocError, Document, DocumentId, PendingJournal, SaveRequest, SaveTarget,
    Session, SessionOptions,
};
use pl_testkit::Fixture;

const SQUARE: &str = "fixture-square-1";
const LINE: &str = "fixture-line-1";
const FREETEXT: &str = "fixture-freetext-1";

/// How long the slow backend takes over each request a save makes.
const SLOW: Duration = Duration::from_millis(150);
/// The longest a frame may take: well under one slow request, so a frame that waited for the
/// backend (or for the worker) even once fails the test.
const FRAME_BUDGET: Duration = Duration::from_millis(100);

/// PDFium behind a wrapper that can be made slow, and that notes which thread calls it.
struct SlowBackend {
    inner: PdfiumBackend,
    delay_ms: AtomicU64,
    /// Calls from this thread are counted in `watched_calls`.
    watched: Mutex<Option<ThreadId>>,
    watched_calls: AtomicUsize,
    opens: AtomicUsize,
    closes: AtomicUsize,
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
            opens: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
        })
    }

    fn set_slow(&self, slow: bool) {
        let ms = if slow { SLOW.as_millis() as u64 } else { 0 };
        self.delay_ms.store(ms, Ordering::SeqCst);
    }

    /// Starts counting the calls this thread makes.
    fn watch_this_thread(&self) {
        *self.watched.lock().unwrap() = Some(std::thread::current().id());
        self.watched_calls.store(0, Ordering::SeqCst);
    }

    /// Stops counting and returns how many calls the watched thread made.
    fn unwatch(&self) -> usize {
        *self.watched.lock().unwrap() = None;
        self.watched_calls.load(Ordering::SeqCst)
    }

    fn open_documents(&self) -> isize {
        self.opens.load(Ordering::SeqCst) as isize - self.closes.load(Ordering::SeqCst) as isize
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
        let (is_open, is_close) = (
            matches!(request, Request::Open(_)),
            matches!(request, Request::Close { .. }),
        );
        let reply = self.inner.call(request);
        if reply.is_ok() {
            if is_open {
                self.opens.fetch_add(1, Ordering::SeqCst);
            }
            if is_close {
                self.closes.fetch_add(1, Ordering::SeqCst);
            }
        }
        reply
    }
}

fn on_disk(fixture: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    (dir, path)
}

fn session(backend: &Arc<SlowBackend>, journal: Option<&Path>, options: SessionOptions) -> Session {
    let mut s = Session::with_options(backend.clone(), options);
    s.set_journal_dir(journal.map(Path::to_path_buf));
    s
}

fn origin() -> Origin {
    Origin::new(
        "Plumbline Tester",
        PdfDate::parse("D:20261124101500Z").unwrap(),
    )
}

fn find(doc: &mut Document, id: &str) -> Annotation {
    doc.annots()
        .unwrap()
        .set()
        .get(&AnnotId::from(id))
        .unwrap_or_else(|| panic!("annotation {id}"))
        .clone()
}

fn moved(a: &Annotation, dx: f64, dy: f64) -> Annotation {
    let mut b = a.clone();
    b.translate(Vec2::new(dx, dy));
    b
}

fn pixel_box(geometry: &PageGeometry, scale: f64, rect: Rect, grow: f64) -> [u32; 4] {
    let t = pl_doc::pl_draw::raster::page_transform(geometry, scale);
    let r = t.transform_rect_bbox(rect).inflate(grow, grow);
    [
        r.x0.floor().max(0.0) as u32,
        r.y0.floor().max(0.0) as u32,
        r.x1.ceil().max(0.0) as u32,
        r.y1.ceil().max(0.0) as u32,
    ]
}

fn differing_inside(a: &Bitmap, b: &Bitmap, r: [u32; 4]) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut n = 0;
    for y in r[1]..r[3].min(a.height) {
        for x in r[0]..r[2].min(a.width) {
            if a.pixel(x, y) != b.pixel(x, y) {
                n += 1;
            }
        }
    }
    n
}

fn journal_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

/// An editing session that leaves something of every kind for a save to deal with: a new
/// markup (in the overlay), an edited foreign markup (hidden in the mirror, drawn by us), a
/// deleted one (hidden), a moved annotation we cannot draw (moved in the mirror), an undone
/// step (a redo stack) and a selection.
fn edit(doc: &mut Document) -> AnnotId {
    let new = Annotation::square(0, Rect::new(320.0, 600.0, 420.0, 680.0), &origin());
    let new_id = new.id.clone();
    doc.apply(Command::add(new)).unwrap();
    let square = find(doc, SQUARE);
    doc.apply(Command::modify(moved(&square, 30.0, -20.0)))
        .unwrap();
    doc.apply(Command::delete(AnnotId::from(LINE))).unwrap();
    let free = find(doc, FREETEXT);
    doc.apply(Command::modify(moved(&free, 0.0, -90.0)))
        .unwrap();
    let extra = Annotation::circle(0, Rect::new(60.0, 60.0, 120.0, 100.0), &origin());
    doc.apply(Command::add(extra)).unwrap();
    assert!(doc.undo().unwrap());
    doc.select([new_id.clone(), AnnotId::from(SQUARE)]).unwrap();
    new_id
}

/// Everything a save must leave alone until it is finished, and after a failure.
#[derive(Clone, PartialEq, Debug)]
struct Seen {
    model: Vec<Annotation>,
    dirty: bool,
    modified: bool,
    undo: usize,
    redo: usize,
    selection: Vec<AnnotId>,
    hidden: Vec<(AnnotId, u32)>,
    moved: Vec<(AnnotId, Rect)>,
    overlay: Vec<AnnotId>,
    revision: u64,
    render: pl_doc::pl_render::DocId,
    path: Option<PathBuf>,
    journal_len: Option<u64>,
}

fn see(doc: &Document, journal: &Path) -> Seen {
    let editor = doc.editor().expect("the store is loaded");
    Seen {
        model: editor.model(),
        dirty: editor.set().is_dirty(),
        modified: doc.is_modified(),
        undo: editor.undo_len(),
        redo: editor.redo_len(),
        selection: doc.selection().to_vec(),
        hidden: doc
            .hidden_annotations()
            .into_iter()
            .map(|(id, slot)| (id, slot.index))
            .collect(),
        moved: doc
            .moved_annotations()
            .into_iter()
            .map(|(id, slot)| (id, slot.shown))
            .collect(),
        overlay: doc.overlay(0).into_iter().map(|item| item.id).collect(),
        revision: doc.edit_revision(),
        render: doc.render_id(),
        path: doc.path().map(Path::to_path_buf),
        journal_len: journal_files(journal)
            .first()
            .map(|f| std::fs::metadata(f).unwrap().len()),
    }
}

struct Flight {
    result: pl_doc::Result<pl_doc::MarkupSave>,
    frames: usize,
    longest_frame: Duration,
    took: Duration,
    refused: usize,
}

/// Runs frames until the save is finished. Each frame does what a user interface does: asks
/// for the result, reads the document, and (the user hammering keys) tries to change it.
fn fly(doc: &mut Document, ticket: pl_doc::SaveTicket, journal: &Path, before: &Seen) -> Flight {
    let started = Instant::now();
    let mut frames = 0;
    let mut refused = 0;
    let mut longest_frame = Duration::ZERO;
    loop {
        let frame = Instant::now();
        if let Some(result) = doc.try_finish_save(ticket) {
            longest_frame = longest_frame.max(frame.elapsed());
            return Flight {
                result,
                frames,
                longest_frame,
                took: started.elapsed(),
                refused,
            };
        }
        // Still in flight: read-only, and exactly as it was when the save began.
        assert!(doc.is_saving());
        assert_eq!(doc.busy(), Busy::Saving);
        assert_eq!(doc.save_in_flight(), Some(ticket));
        assert_eq!(&see(doc, journal), before, "frame {frames}");
        doc.poll_background();
        let probe = Annotation::square(0, Rect::new(1.0, 1.0, 9.0, 9.0), &origin());
        for refusal in [
            doc.apply(Command::add(probe)).map(|_| ()),
            doc.apply(Command::delete(AnnotId::from(SQUARE)))
                .map(|_| ()),
            doc.undo().map(|_| ()),
            doc.redo().map(|_| ()),
            doc.set_text(
                &AnnotId::from(SQUARE),
                "typed during the save",
                origin().when,
            )
            .map(|_| ()),
            doc.save().map(|_| ()),
            doc.start_save().map(|_| ()),
            doc.start_load_annots().map(|_| ()),
            doc.set_layers_visible(&[]).map(|_| ()),
            doc.pdf().map(|_| ()),
        ] {
            assert!(
                matches!(refusal, Err(DocError::Saving)),
                "frame {frames}: {refusal:?}"
            );
            refused += 1;
        }
        longest_frame = longest_frame.max(frame.elapsed());
        frames += 1;
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn assert_never_blocked(flight: &Flight, backend: &SlowBackend, what: &str) {
    println!(
        "{what}: the save took {:?}; {} frames ran meanwhile, the longest {:?}; {} edits refused",
        flight.took, flight.frames, flight.longest_frame, flight.refused
    );
    assert!(
        flight.took >= SLOW * 2,
        "{what}: the save was not slow ({:?}), so the test shows nothing",
        flight.took
    );
    assert!(
        flight.frames >= 10,
        "{what}: only {} frames ran",
        flight.frames
    );
    assert!(
        flight.longest_frame < FRAME_BUDGET,
        "{what}: a frame took {:?}",
        flight.longest_frame
    );
    assert_eq!(
        backend.unwatch(),
        0,
        "{what}: the frame loop called the backend"
    );
}

fn open_with_edits(
    backend: &Arc<SlowBackend>,
    journal: &Path,
    path: &Path,
) -> (Session, DocumentId, AnnotId) {
    let mut s = session(backend, Some(journal), SessionOptions::complete());
    let id = s.open(path, None).unwrap();
    let new_id = edit(s.document_mut(id).unwrap());
    (s, id, new_id)
}

#[test]
fn the_frame_loop_keeps_running_while_a_slow_save_is_in_flight() {
    let backend = SlowBackend::new();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let (mut s, id, new_id) = open_with_edits(&backend, journal.path(), &path);
    let doc = s.document_mut(id).unwrap();
    let geometry = *doc.page_geometry(0).unwrap();
    let before = see(doc, journal.path());
    assert!(before.modified && before.dirty);
    assert_eq!((before.undo, before.redo), (4, 1));
    assert_eq!(
        before.hidden.len(),
        2,
        "the edited square and the deleted line"
    );
    assert_eq!(before.moved.len(), 1, "the text box we cannot draw");
    assert_eq!(before.overlay.len(), 2, "the new square and the edited one");
    assert!(before.journal_len.is_some());
    // What the user sees before the save, for the place the moved text box is drawn at.
    let free_now = before.moved[0].1;
    let picture_before = doc.render_page_composite(0, 2.0).unwrap();

    backend.set_slow(true);
    backend.watch_this_thread();
    let ticket = doc.start_save().unwrap();
    let flight = fly(doc, ticket, journal.path(), &before);
    assert_never_blocked(&flight, &backend, "save");
    backend.set_slow(false);

    // The result arrives once, and the document changed in one step.
    let saved = flight.result.unwrap();
    assert!(saved.wrote());
    assert_eq!((saved.added, saved.modified, saved.deleted), (1, 2, 1));
    assert_eq!(saved.path, path);
    assert!(doc.try_finish_save(ticket).is_none());
    assert!(matches!(
        doc.finish_save(ticket),
        Err(DocError::NoSuchTicket)
    ));
    assert_eq!(doc.busy(), Busy::Idle);
    let after = see(doc, journal.path());
    assert_eq!(after.model, before.model, "the same annotations");
    assert!(!after.modified && !after.dirty, "nothing left to save");
    assert!(after.hidden.is_empty() && after.moved.is_empty() && after.overlay.is_empty());
    assert_eq!(after.journal_len, None, "the journal is gone");
    assert_eq!(after.selection, before.selection);
    assert_ne!(
        after.render, before.render,
        "a new display, of the new file"
    );
    assert!(after.revision > before.revision);
    assert_eq!(doc.pending_journal(), PendingJournal::None);
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    assert_eq!(backend.open_documents(), 1, "the old display was closed");
    // Every annotation has its tie to the new file.
    assert!(doc
        .editor()
        .unwrap()
        .set()
        .iter()
        .all(|a| a.baseline().is_some()));
    // PDFium draws the moved text box where the mirror showed it before the save.
    let picture_after = doc.render_page_composite(0, 2.0).unwrap();
    let free_box = pixel_box(&geometry, 2.0, free_now, 2.0);
    assert_eq!(
        differing_inside(&picture_before, &picture_after, free_box),
        0,
        "the moved text box looks the same before and after the save"
    );

    // Editing goes on: history survived, and a second save has only the new change.
    assert!(doc.can_undo());
    doc.apply(Command::delete(new_id.clone())).unwrap();
    assert!(doc.is_modified());
    let again = doc.save().unwrap();
    assert_eq!((again.added, again.modified, again.deleted), (0, 0, 1));

    // A fresh session reads the same annotations from the file.
    let model = doc.editor().unwrap().model();
    drop(s);
    assert_eq!(backend.open_documents(), 0);
    let mut s = session(&backend, None, SessionOptions::complete());
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.annots().unwrap().model(), model);
    assert!(!doc.annots().unwrap().set().contains(&AnnotId::from(LINE)));
}

#[test]
fn a_failed_background_save_leaves_everything_as_it_was() {
    for fail_point in [FailPoint::AfterTmpWrite, FailPoint::BeforeReplace] {
        let backend = SlowBackend::new();
        let fixture = pl_testkit::annotations();
        let (_dir, path) = on_disk(&fixture);
        let journal = tempfile::tempdir().unwrap();
        let (mut s, id, _) = open_with_edits(&backend, journal.path(), &path);
        let doc = s.document_mut(id).unwrap();
        let before = see(doc, journal.path());
        let picture_before = doc.render_page_composite(0, 2.0).unwrap();
        let mirror_before = backend.annotations(doc.render_id(), 0).unwrap();

        backend.set_slow(true);
        backend.watch_this_thread();
        let ticket = doc
            .start_save_with(SaveRequest {
                target: SaveTarget::InPlace,
                options: SaveOptions {
                    fail_point: Some(fail_point),
                    ..SaveOptions::default()
                },
            })
            .unwrap();
        let flight = fly(doc, ticket, journal.path(), &before);
        assert_never_blocked(&flight, &backend, "failed save");
        backend.set_slow(false);

        let error = flight.result.unwrap_err();
        assert!(
            matches!(error, DocError::Pdf(PdfError::InjectedFailure { .. })),
            "{fail_point:?}: {error:?}"
        );
        // The file is untouched and no temporary file is left.
        assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );

        // The document is as it was: changes, history, selection, journal, modified flag.
        assert_eq!(doc.busy(), Busy::Idle);
        let mut after = see(doc, journal.path());
        // The display was closed for the replace and opened again on the old file; the
        // store goes on from the same revision.
        assert_ne!(after.render, before.render);
        after.render = before.render;
        assert_eq!(after, before, "{fail_point:?}");
        assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
        assert_eq!(backend.open_documents(), 1);
        // The new display has the same annotations hidden and moved, so the picture the
        // user sees is exactly the one from before the save.
        let mirror_after = backend.annotations(doc.render_id(), 0).unwrap();
        assert_eq!(mirror_after, mirror_before, "{fail_point:?}");
        assert_eq!(mirror_after.iter().filter(|a| a.hidden).count(), 2);
        let picture_after = doc.render_page_composite(0, 2.0).unwrap();
        assert!(
            picture_after == picture_before,
            "{fail_point:?}: the picture"
        );

        // Nothing is stuck: editing, undo and a save all work.
        assert!(doc.redo().unwrap());
        assert!(doc.undo().unwrap());
        let saved = doc.save().unwrap();
        assert!(saved.wrote());
        assert!(!doc.is_modified());
        assert!(journal_files(journal.path()).is_empty());
        assert_eq!(doc.editor().unwrap().model(), before.model);
    }
}

#[test]
fn a_background_save_as_moves_the_document_when_it_is_finished() {
    let backend = SlowBackend::new();
    let fixture = pl_testkit::annotations();
    let (dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let (mut s, id, _) = open_with_edits(&backend, journal.path(), &path);
    // Another open document's file is refused before anything starts.
    let (_other_dir, other_path) = on_disk(&pl_testkit::plain());
    let other = s.open(&other_path, None).unwrap();
    assert!(matches!(
        s.start_save_as(id, &other_path),
        Err(DocError::OpenElsewhere { .. })
    ));
    assert_eq!(s.document(id).unwrap().busy(), Busy::Idle);
    s.close(other).unwrap();

    let dest = dir.path().join("marked copy.pdf");
    let doc = s.document_mut(id).unwrap();
    let before = see(doc, journal.path());
    backend.set_slow(true);
    backend.watch_this_thread();
    let ticket = s.start_save_as(id, &dest).unwrap();
    let doc = s.document_mut(id).unwrap();
    // Until the save is finished the document is still the original file.
    assert_eq!(doc.path(), Some(path.as_path()));
    let flight = fly(doc, ticket, journal.path(), &before);
    assert_never_blocked(&flight, &backend, "save as");
    backend.set_slow(false);

    let saved = flight.result.unwrap();
    assert!(saved.wrote());
    assert_eq!(saved.path, dest);
    assert_eq!(doc.path(), Some(dest.as_path()));
    assert_eq!(doc.title(), "marked copy.pdf");
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes, "the original");
    let written = std::fs::read(&dest).unwrap();
    assert!(written.starts_with(&fixture.bytes), "an incremental update");
    let after = see(doc, journal.path());
    assert_eq!(after.model, before.model);
    assert!(!after.modified && !after.dirty);
    assert!(after.hidden.is_empty() && after.moved.is_empty() && after.overlay.is_empty());
    assert_eq!(after.journal_len, None);
    assert_eq!(backend.open_documents(), 1);
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    // The next save goes to the new file, and there is nothing to write.
    assert!(!doc.save().unwrap().wrote());
    assert_eq!(std::fs::read(&dest).unwrap(), written);
}

/// Two documents must not be saved to one new file at the same time: both workers would
/// write the same temporary file and both documents would end up tied to it.
#[test]
fn the_destination_of_a_save_as_in_flight_is_spoken_for() {
    let backend = SlowBackend::new();
    let (dir, path) = on_disk(&pl_testkit::annotations());
    let journal = tempfile::tempdir().unwrap();
    let (mut s, a, _) = open_with_edits(&backend, journal.path(), &path);
    let (_other_dir, other_path) = on_disk(&pl_testkit::annotations());
    let b = s.open(&other_path, None).unwrap();
    edit(s.document_mut(b).unwrap());
    let dest = dir.path().join("Shared Name.pdf");

    let ticket = s.start_save_as(a, &dest).unwrap();
    assert_eq!(s.document(a).unwrap().saving_to(), Some(dest.as_path()));
    assert_eq!(s.document(a).unwrap().path(), Some(path.as_path()));
    // Until that save is taken in, nobody else gets the file: not a second background save,
    // not a blocking one, in whatever case the name is spelled.
    let spelled = dir.path().join("shared name.PDF");
    for target in [&dest, &spelled] {
        assert!(matches!(
            s.start_save_as(b, target),
            Err(DocError::OpenElsewhere { .. })
        ));
        assert!(matches!(
            s.save_as(b, target),
            Err(DocError::OpenElsewhere { .. })
        ));
    }
    assert_eq!(s.document(b).unwrap().busy(), Busy::Idle);
    assert_eq!(s.document(b).unwrap().path(), Some(other_path.as_path()));
    assert_eq!(s.find_by_path(&dest), Some(a));
    // Opening it leads to the document that is becoming it.
    assert_eq!(s.open(&dest, None).unwrap(), a);

    let saved = s.document_mut(a).unwrap().finish_save(ticket).unwrap();
    assert_eq!(saved.path, dest);
    assert_eq!(s.document(a).unwrap().saving_to(), None);
    assert_eq!(s.document(a).unwrap().path(), Some(dest.as_path()));
    // Still refused, now because it is A's file; another name is fine.
    assert!(matches!(
        s.start_save_as(b, &dest),
        Err(DocError::OpenElsewhere { .. })
    ));
    let elsewhere = dir.path().join("other copy.pdf");
    let ticket = s.start_save_as(b, &elsewhere).unwrap();
    s.document_mut(b).unwrap().finish_save(ticket).unwrap();
    assert_eq!(s.document(b).unwrap().path(), Some(elsewhere.as_path()));
}

#[test]
fn closing_a_document_in_mid_save_finishes_the_save_and_leaves_nothing_open() {
    let backend = SlowBackend::new();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let (mut s, id, _) = open_with_edits(&backend, journal.path(), &path);
    let model = s.document(id).unwrap().editor().unwrap().model();
    backend.set_slow(true);
    s.document_mut(id).unwrap().start_save().unwrap();
    // The tab is closed at once.
    s.close(id).unwrap();
    backend.set_slow(false);
    assert_eq!(backend.open_documents(), 0, "no display is left open");
    assert!(
        journal_files(journal.path()).is_empty(),
        "the finished save took its journal with it"
    );
    assert_ne!(std::fs::read(&path).unwrap(), fixture.bytes, "the save ran");
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );

    let mut s = session(&backend, None, SessionOptions::complete());
    let id = s.open(&path, None).unwrap();
    assert_eq!(s.document_mut(id).unwrap().annots().unwrap().model(), model);
}

#[test]
fn annotations_and_layers_load_on_a_worker_and_a_journal_is_replayed() {
    let backend = SlowBackend::new();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();

    // A run that is killed with unsaved changes.
    let (s, id, _) = open_with_edits(&backend, journal.path(), &path);
    let before = see(s.document(id).unwrap(), journal.path());
    drop(s);
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);

    // The next run, as a user interface opens it: nothing is parsed at open.
    let mut s = session(&backend, Some(journal.path()), SessionOptions::default());
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert!(!doc.annots_loaded() && doc.cached_layers().is_none());
    assert_eq!(doc.pending_journal(), PendingJournal::Recoverable);
    let ticket = doc.start_load_annots().unwrap();
    assert_eq!(doc.busy(), Busy::Loading);
    assert_eq!(doc.load_in_flight(), Some(ticket));
    // A save cannot start beside the load, and a second load neither.
    assert!(matches!(doc.start_save(), Err(DocError::Loading)));
    assert!(matches!(doc.start_load_layers(), Err(DocError::Loading)));
    let mut frames = 0;
    let loaded = loop {
        // Viewing goes on meanwhile: nothing here waits for the worker.
        assert!(doc.overlay(0).is_empty() || doc.annots_loaded());
        doc.poll_background();
        if let Some(result) = doc.try_finish_load(ticket) {
            break result;
        }
        frames += 1;
        assert!(frames < 20_000, "the load never finished");
        std::thread::sleep(Duration::from_millis(1));
    };
    loaded.unwrap();
    assert_eq!(doc.busy(), Busy::Idle);
    assert!(
        doc.try_finish_load(ticket).is_none(),
        "the result is given once"
    );
    assert!(doc.annots_loaded());
    assert_eq!(
        doc.cached_layers().map(<[_]>::len),
        Some(0),
        "listed by the same parse"
    );
    assert!(doc.recovered_changes() >= 6, "{}", doc.recovered_changes());
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    // The recovered state is the state before the kill: annotations, history, what the
    // mirror hides and moves, what the overlay paints.
    doc.load_all_geometry();
    doc.select(before.selection.clone()).unwrap();
    let mut after = see(doc, journal.path());
    after.render = before.render;
    after.revision = before.revision;
    assert_eq!(after, before);
    assert_eq!(
        backend
            .annotations(doc.render_id(), 0)
            .unwrap()
            .iter()
            .filter(|a| a.hidden)
            .count(),
        2
    );

    // Loading again is finished at once and changes nothing.
    let again = doc.start_load_annots().unwrap();
    assert_eq!(doc.finish_load(again).unwrap(), ());
    assert_eq!(see(doc, journal.path()).model, before.model);
    drop(s);

    // A blocking call that needs the annotations waits for a load in flight instead of
    // parsing the file a second time, and the layer list arrives on a worker too.
    let layered = pl_testkit::layers();
    let (_dir2, layered_path) = on_disk(&layered);
    let mut s = session(&backend, None, SessionOptions::default());
    let id = s.open(&layered_path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let ticket = doc.start_load_layers().unwrap();
    let names: Vec<String> = doc
        .layers()
        .unwrap()
        .iter()
        .map(|l| l.name.clone())
        .collect();
    assert_eq!(names.len(), layered.manifest.layers.len());
    assert!(names.contains(&"Layer A".to_string()));
    assert!(!doc.is_loading() && doc.try_finish_load(ticket).is_none());
    assert!(!doc.annots_loaded(), "only the layers were asked for");
    let ticket = doc.start_load_annots().unwrap();
    let square = Annotation::square(0, Rect::new(100.0, 100.0, 200.0, 180.0), &origin());
    assert!(doc.apply(Command::add(square)).unwrap().changed);
    assert!(doc.try_finish_load(ticket).is_none());
    assert_eq!(doc.editor().unwrap().set().len(), 1);
}

/// The real drawing set, when it is there: reading its annotations and saving a marked copy
/// on worker threads, with a frame loop that must never be held up. The copy goes under
/// `target` and is deleted with the test; nothing of the sample's content is printed.
#[test]
fn the_real_sample_loads_and_saves_in_the_background_without_holding_a_frame_up() {
    let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    if !sample.is_file() {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    }
    let sample = std::path::absolute(sample).unwrap();
    let size_before = std::fs::metadata(&sample).unwrap().len();
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("tmp");
    std::fs::create_dir_all(&work).unwrap();
    let dir = tempfile::tempdir_in(std::path::absolute(work).unwrap()).unwrap();
    let marked = dir.path().join("marked.pdf");

    let backend = SlowBackend::new();
    let mut s = session(&backend, None, SessionOptions::default());
    let id = s.open(&sample, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let pages = doc.page_count();
    assert!(pages >= 2);

    // The first read of the annotations: a lopdf parse of the whole file, off this thread.
    let started = Instant::now();
    let ticket = doc.start_load_annots().unwrap();
    let (mut frames, mut longest) = (0usize, Duration::ZERO);
    loop {
        let frame = Instant::now();
        let done = doc.try_finish_load(ticket);
        doc.poll_background();
        longest = longest.max(frame.elapsed());
        if let Some(result) = done {
            result.unwrap();
            break;
        }
        frames += 1;
        std::thread::sleep(Duration::from_millis(1));
    }
    println!(
        "SAMPLE load: {} bytes, {pages} sheets: annotations read in {:?} on a worker; {frames} frames meanwhile, the longest {longest:?}",
        size_before,
        started.elapsed()
    );
    assert!(
        longest < FRAME_BUDGET,
        "a frame took {longest:?} during the load"
    );
    assert!(doc.annots_loaded() && doc.cached_layers().is_some());
    let already = doc.editor().unwrap().set().len();

    // A markup on the first and the last sheet, saved as a copy on a worker.
    for page in [0, pages as usize - 1] {
        let a = Annotation::square(page, Rect::new(200.0, 200.0, 420.0, 340.0), &origin());
        doc.apply(Command::add(a)).unwrap();
    }
    backend.watch_this_thread();
    let started = Instant::now();
    let ticket = doc.start_save_as(&marked).unwrap();
    let (mut frames, mut longest) = (0usize, Duration::ZERO);
    let saved = loop {
        let frame = Instant::now();
        let done = doc.try_finish_save(ticket);
        // A frame reads what it paints and is refused what it must not do.
        let painted = doc.overlay(0).len();
        if done.is_none() {
            assert_eq!(painted, 1);
            assert!(matches!(doc.undo(), Err(DocError::Saving)));
        }
        longest = longest.max(frame.elapsed());
        if let Some(result) = done {
            break result.unwrap();
        }
        frames += 1;
        std::thread::sleep(Duration::from_millis(1));
    };
    println!(
        "SAMPLE save: verified incremental save-as in {:?} on a worker; {frames} frames meanwhile, the longest {longest:?}",
        started.elapsed()
    );
    assert_eq!(backend.unwatch(), 0, "the frame loop called the backend");
    assert!(
        longest < FRAME_BUDGET,
        "a frame took {longest:?} during the save"
    );
    assert!(saved.wrote());
    assert_eq!((saved.added, saved.modified, saved.deleted), (2, 0, 0));
    assert_eq!(doc.path(), Some(marked.as_path()));
    assert!(!doc.is_modified() && doc.overlay(0).is_empty());
    assert_eq!(doc.editor().unwrap().set().len(), already + 2);
    assert_eq!(std::fs::metadata(&sample).unwrap().len(), size_before);
    assert!(std::fs::metadata(&marked).unwrap().len() > size_before);
    // The copy has to be released before its folder can be deleted.
    drop(s);
    assert_eq!(backend.open_documents(), 0);
}
