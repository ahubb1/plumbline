//! Saving and loading off the caller's thread.
//!
//! A save parses the file with lopdf, writes it, reads it back with three parsers and reopens
//! the page display; the first load of the annotations parses it too. On a large drawing set
//! each is long enough to freeze a window (docs/measurements.md). So both are *jobs*: plain
//! owned data that a function turns into a plain owned result, with nothing of the
//! [`Document`] borrowed in between.
//!
//! * [`Document::save`], [`Document::save_as`], [`Document::save_optimised`],
//!   [`Document::save_incremental`] and [`Document::load_annots`] run the job on the calling
//!   thread and return when it is done: the command line and the tests use these.
//! * [`Document::start_save`] (and its siblings) and [`Document::start_load_annots`] hand the
//!   same job to a worker thread and return a ticket at once. A user interface then calls
//!   [`Document::try_finish_save`] / [`Document::try_finish_load`] once per frame; neither
//!   ever waits, parses or reads a file. They return `None` until the worker is done and the
//!   result exactly once.
//!
//! # While a save is in flight
//!
//! The document is read-only ([`Document::busy`] is [`Busy::Saving`]):
//!
//! * the annotations, the overlay, the selection, the undo history and the modified flag can
//!   be read and stay exactly as they were when the save began;
//! * everything that would change them ([`Document::apply`], undo, redo, paste, cut, a layer
//!   toggle, another save) is refused with [`DocError::Saving`] and changes nothing;
//! * the page display belongs to the worker. For an in-place save it is closed while the
//!   file is replaced, so render requests made with [`Document::render_id`] fail until the
//!   save is finished; tiles that are already cached stay valid until then. A user interface
//!   simply stops asking for new tiles while [`Document::is_saving`].
//!
//! When the result is taken, the document changes in one step: after a success the store
//! describes the new file (nothing is hidden, nothing is modified, the journal is gone,
//! [`Document::render_id`] is the display of the new file); after a failure everything is as
//! it was before the save began, changes, history and journal included, and the file is
//! untouched.
//!
//! Dropping a document (closing its tab) while its save is in flight waits for the save.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pl_annot::{AnnotSet, SavePlan};
use pl_pdf::{
    EditSet, Expected, LayerInfo, ObjectId, PdfDocument, PdfFile, SaveOptions, SignaturePolicy,
};
use pl_render::{DocId, RectArray, RenderBackend};
use pl_scene::SceneWorker;

use crate::editing::MarkupSave;
use crate::error::{DocError, Result};
use crate::journal::{FileStamp, Journal, JournalContents};
use crate::scales::ScaleStore;
use crate::session::{
    bitmap_hash, open_mirror, pdfium_check, sample, Document, DocumentId, NewMirror, Origin,
    PdfiumCheck, SaveOutcome, SAVE_RENDER_PX, SAVE_RENDER_SAMPLES,
};

/// What a document is doing in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// Nothing: every call is available.
    Idle,
    /// The annotations (or the layer list) are being read on a worker thread.
    Loading,
    /// A save is in flight on a worker thread; the document is read-only.
    Saving,
}

/// Where a save writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveTarget {
    /// An incremental update appended to the document's own file.
    InPlace,
    /// The whole file rewritten as one revision.
    Optimised,
    /// Another file, which the document then belongs to.
    As(PathBuf),
}

/// What [`Document::start_save_with`] is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRequest {
    pub target: SaveTarget,
    /// pl-pdf's options (tests use the fail points).
    pub options: SaveOptions,
}

impl SaveRequest {
    pub fn new(target: SaveTarget) -> SaveRequest {
        SaveRequest {
            target,
            options: SaveOptions::default(),
        }
    }
}

/// A save that was started with [`Document::start_save`]; give it back to
/// [`Document::try_finish_save`] or [`Document::finish_save`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SaveTicket {
    pub document: DocumentId,
    serial: u64,
}

/// A load that was started with [`Document::start_load_annots`] or
/// [`Document::start_load_layers`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LoadTicket {
    pub document: DocumentId,
    serial: u64,
}

/// The job a document has on a worker thread.
pub(crate) enum Flight {
    Save {
        serial: u64,
        rx: Receiver<Box<SaveDone>>,
        /// Where a Save As is writing: the file is spoken for until the save is finished.
        dest: Option<PathBuf>,
    },
    Load {
        serial: u64,
        rx: Receiver<Box<LoadDone>>,
    },
}

// ---- the save job ----------------------------------------------------------------------------

/// One thing to put back in a freshly opened display of the *old* file: an annotation the
/// store hides, or one it has moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MirrorFix {
    pub page: u32,
    pub index: u32,
    pub hide: bool,
    pub rect: Option<RectArray>,
}

/// Background work of the document that still maps its file.
#[derive(Default)]
pub(crate) struct FileUsers {
    pub scenes: Vec<SceneWorker>,
    pub probe_released: Option<Arc<AtomicBool>>,
}

impl FileUsers {
    fn count(&mut self) -> usize {
        self.scenes.retain(|w| !w.is_finished());
        let probe = self
            .probe_released
            .as_ref()
            .is_some_and(|p| !p.load(Ordering::Acquire));
        self.scenes.len() + usize::from(probe)
    }
}

/// Everything a save needs, owned.
pub(crate) struct SaveJob {
    pub backend: Arc<dyn RenderBackend>,
    pub origin: Origin,
    pub password: Option<String>,
    pub target: SaveTarget,
    pub options: SaveOptions,
    /// The document's lopdf handle, if it had one.
    pub pdf: Option<PdfFile>,
    pub was_loaded: bool,
    /// A copy of the annotation store; `None` when the annotations were never loaded.
    pub set: Option<AnnotSet>,
    /// A copy of the pages' scales and the document settings, beside `set`.
    pub scales: Option<ScaleStore>,
    /// The file as it was when the store was read.
    pub stamp: Option<FileStamp>,
    /// A caller's own edit set ([`Document::save_incremental`]) and the pages it touches.
    pub raw: Option<(EditSet, Vec<u32>)>,
    pub render: DocId,
    pub suspended: bool,
    pub page_count: u32,
    pub unreadable: Vec<bool>,
    /// Pages whose display differs from the file (something on them is hidden or moved).
    pub hidden_pages: Vec<u32>,
    pub fixes: Vec<MirrorFix>,
    pub users: FileUsers,
    pub release_wait: Duration,
    /// The layers this session has switched off, when that differs from the file's default.
    pub layers_off: Option<Vec<ObjectId>>,
    pub user_units: Vec<f64>,
    /// Fetch every page's real geometry for the new display ([`crate::OpenMode::Complete`]).
    pub complete: bool,
    /// The working copy of structural edits and whether the user agreed to invalidate
    /// signatures: the save is then the full rewrite of it (see [`crate::structure`]).
    pub working: Option<(PdfDocument, bool)>,
    /// `working` was given: the display shows the working copy's bytes, not the file.
    pub restructured: bool,
}

/// What became of the page display.
pub(crate) enum MirrorAfter {
    /// The document's display is the one it had.
    Kept,
    /// The old display is closed; this is the new one.
    New(NewMirror),
    /// The old display is closed and a new one could not be opened.
    Lost(String),
}

/// What became of the copy of the store.
pub(crate) enum StoreAfter {
    /// The store still describes the file.
    Unchanged,
    /// The copy, rebased on the file that was written.
    Rebased {
        set: AnnotSet,
        scales: Box<ScaleStore>,
        page_count: usize,
    },
    /// A structural save: the store read afresh from the file that was written, with a new
    /// history (structural edits are undoable until saved).
    Reloaded {
        set: AnnotSet,
        scales: Box<ScaleStore>,
        page_count: usize,
    },
    /// The file was written but could not be read back into the store: the store is dropped
    /// and read afresh when next needed.
    Stale,
}

pub(crate) struct Saved {
    pub path: PathBuf,
    pub outcome: Option<SaveOutcome>,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

/// What a save did, owned.
pub(crate) struct SaveDone {
    pub result: Result<Saved>,
    pub pdf: Option<PdfFile>,
    /// The file the document now belongs to, when that changed.
    pub moved_to: Option<PathBuf>,
    pub mirror: MirrorAfter,
    pub store: StoreAfter,
    /// The stamp the store now goes by, when it changed.
    pub stamp: Option<Option<FileStamp>>,
    /// The layer choices could not be applied to the new display.
    pub layers_reset: bool,
    pub warnings: Vec<String>,
}

enum Mode<'a> {
    Incremental(&'a EditSet),
    Full(&'a EditSet),
    IncrementalAs(&'a Path, &'a EditSet),
    /// The full rewrite of a restructured document over the file.
    Restructured(PdfDocument, SignaturePolicy),
    /// The same to another file; the handle's file is left as it is.
    RestructuredAs(PathBuf, PdfDocument, SignaturePolicy),
    /// A document with no file of its own (opened from memory) written to a new one.
    New(PathBuf, PdfDocument, SignaturePolicy),
}

struct Progress {
    /// Whether the document's display (`job.render`) is still open.
    mirror_open: bool,
    moved_to: Option<PathBuf>,
    stamp: Option<Option<FileStamp>>,
}

struct Written {
    outcome: Option<SaveOutcome>,
    counts: (usize, usize, usize),
}

fn nothing() -> Written {
    Written {
        outcome: None,
        counts: (0, 0, 0),
    }
}

fn open_pdf(path: &Path, password: Option<&str>) -> Result<PdfFile> {
    Ok(match password {
        Some(pw) => PdfFile::open_with_password(path, pw)?,
        None => PdfFile::open(path)?,
    })
}

/// Copies `source` to `dest` through `dest`'s temporary name, so `dest` is whole or absent.
fn copy_to(source: &Path, dest: &Path) -> Result<()> {
    let tmp = pl_pdf::tmp_path_for(dest);
    std::fs::copy(source, &tmp).map_err(|e| DocError::io("copy to", dest, e))?;
    pl_pdf::replace_file(&tmp, dest)?;
    Ok(())
}

/// "Verify, then replace" with PDFium as the third verifier (see
/// [`Document::save_incremental`]). For the in-place modes the display is closed first; the
/// caller opens a new one whatever the outcome.
fn write_checked(
    job: &mut SaveJob,
    progress: &mut Progress,
    pdf: Option<&mut PdfFile>,
    mode: Mode<'_>,
    touched: &[u32],
) -> Result<SaveOutcome> {
    let page_count = job.page_count;
    let mut touched: Vec<u32> = touched
        .iter()
        .copied()
        .filter(|p| *p < page_count)
        .collect();
    touched.sort_unstable();
    touched.dedup();
    let untouched: Vec<u32> = (0..page_count)
        .filter(|p| !touched.contains(p) && !job.unreadable[*p as usize])
        .collect();
    let samples = sample(&untouched, SAVE_RENDER_SAMPLES);

    // What the untouched sample looks like now, in the display that is about to close.
    let backend = job.backend.clone();
    let mut before = Vec::with_capacity(samples.len());
    for page in &samples {
        let bitmap = backend.thumbnail(job.render, *page, SAVE_RENDER_PX)?;
        before.push((*page, bitmap_hash(&bitmap)));
    }
    let mut count_pages = touched.clone();
    count_pages.extend(sample(&untouched, SAVE_RENDER_SAMPLES));
    count_pages.sort_unstable();
    count_pages.dedup();

    if matches!(
        mode,
        Mode::Incremental(_) | Mode::Full(_) | Mode::Restructured(..)
    ) {
        // The file is about to be replaced: nothing of this document may still map it.
        let deadline = Instant::now() + job.release_wait;
        while job.users.count() > 0 {
            if Instant::now() >= deadline {
                return Err(DocError::FileBusy {
                    path: pdf
                        .as_ref()
                        .map(|p| p.path().to_path_buf())
                        .unwrap_or_default(),
                    workers: job.users.count(),
                });
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // A restructured document's display shows its bytes in memory, not the file: it
        // stays open (and is still right if the save fails).
        if progress.mirror_open && !matches!(mode, Mode::Restructured(..)) {
            backend.close(job.render)?;
            progress.mirror_open = false;
        }
    }
    let password = job.password.clone();
    let mut check = |tmp: &Path, expected: &Expected| {
        pdfium_check(
            backend.as_ref(),
            tmp,
            password.as_deref(),
            expected,
            &count_pages,
            &before,
        )
    };
    let report =
        match mode {
            Mode::Incremental(edits) => pdf.ok_or(DocError::NoFile)?.save_incremental_checked(
                edits,
                &job.options,
                &mut check,
            )?,
            Mode::Full(edits) => {
                pdf.ok_or(DocError::NoFile)?
                    .save_full_checked(edits, &job.options, &mut check)?
            }
            Mode::IncrementalAs(dest, edits) => pdf
                .ok_or(DocError::NoFile)?
                .save_incremental_as_checked(dest, edits, &job.options, &mut check)?,
            Mode::Restructured(doc, policy) => pdf
                .ok_or(DocError::NoFile)?
                .save_restructured_checked(doc, &job.options, policy, &mut check)?,
            Mode::RestructuredAs(dest, doc, policy) => pdf
                .ok_or(DocError::NoFile)?
                .save_restructured_as_checked(&dest, doc, &job.options, policy, &mut check)?,
            Mode::New(dest, doc, policy) => {
                doc.save_new_checked(&dest, &job.options, policy, &mut check)?
            }
        };
    Ok(SaveOutcome {
        report,
        pdfium: PdfiumCheck {
            annotation_pages: count_pages,
            render_pages: samples,
        },
    })
}

/// Plans and writes. `pdf` is the handle of the file `path`.
fn plan_and_write(
    job: &mut SaveJob,
    progress: &mut Progress,
    pdf: &mut PdfFile,
    path: &Path,
    target: &SaveTarget,
) -> Result<Written> {
    let full = matches!(target, SaveTarget::Optimised);
    let (edits, counts, touched): (EditSet, (usize, usize, usize), Vec<u32>) =
        if let Some((edits, also)) = job.raw.take() {
            (edits, (0, 0, 0), also)
        } else if let Some(set) = &job.set {
            // The parsed file is released between load and save to keep memory down, so
            // pl-pdf's own check would compare the file with itself: compare with the stamp
            // taken when the store was read. A new modification time over the same bytes is
            // not a change.
            if let Some(then) = job.stamp {
                let now = FileStamp::of_file(path).map_err(|e| DocError::io("read", path, e))?;
                if (now.len, now.ends_hash) != (then.len, then.ends_hash) {
                    return Err(pl_pdf::PdfError::ChangedOnDisk {
                        path: path.to_path_buf(),
                    }
                    .into());
                }
            }
            let doc = pdf.document()?;
            let mut plan: SavePlan = set.plan_save(doc)?;
            // A page's scales are a patch of its /VP; the document settings one of the catalog.
            let scale_pages = match &job.scales {
                Some(scales) => scales.plan(doc, &mut plan.edits)?,
                None => Vec::new(),
            };
            if plan.is_empty() && !full {
                if let SaveTarget::As(dest) = target {
                    // Nothing to add: the new file is a copy.
                    copy_to(path, dest)?;
                    progress.moved_to = Some(dest.clone());
                }
                return Ok(nothing());
            }
            let mut touched: Vec<u32> = plan.touched_pages().iter().map(|p| *p as u32).collect();
            touched.extend(job.hidden_pages.iter().copied());
            touched.extend(scale_pages.iter().map(|p| *p as u32));
            let counts = (plan.added.len(), plan.modified.len(), plan.deleted.len());
            (plan.edits, counts, touched)
        } else {
            // Nothing was ever edited.
            match target {
                SaveTarget::InPlace => return Ok(nothing()),
                SaveTarget::Optimised => (pdf.edit()?, (0, 0, 0), Vec::new()),
                SaveTarget::As(dest) => {
                    copy_to(path, dest)?;
                    progress.moved_to = Some(dest.clone());
                    return Ok(nothing());
                }
            }
        };
    let mode = match target {
        SaveTarget::InPlace => Mode::Incremental(&edits),
        SaveTarget::Optimised => Mode::Full(&edits),
        SaveTarget::As(dest) => Mode::IncrementalAs(dest, &edits),
    };
    let touched: Vec<u32> = edits
        .annotation_pages()
        .into_iter()
        .map(|p| p as u32)
        .chain(touched)
        .collect();
    let outcome = write_checked(job, progress, Some(pdf), mode, &touched)?;
    if let SaveTarget::As(dest) = target {
        progress.moved_to = Some(dest.clone());
    }
    Ok(Written {
        outcome: Some(outcome),
        counts,
    })
}

/// The save of a working copy of structural edits: the store's changes applied to it in
/// memory, then the verified full rewrite (see [`crate::structure`]).
fn save_working(job: &mut SaveJob, progress: &mut Progress) -> Result<Written> {
    let (doc, accepted) = job.working.take().ok_or(DocError::NoFile)?;
    let policy = if accepted {
        SignaturePolicy::Invalidate
    } else {
        SignaturePolicy::Refuse
    };
    let set = job.set.as_ref().ok_or(DocError::NoFile)?;
    let mut plan = set.plan_save(&doc)?;
    let scale_pages = match &job.scales {
        Some(scales) => scales.plan(&doc, &mut plan.edits)?,
        None => Vec::new(),
    };
    let counts = (plan.added.len(), plan.modified.len(), plan.deleted.len());
    let mut touched: Vec<u32> = plan.touched_pages().iter().map(|p| *p as u32).collect();
    touched.extend(plan.edits.annotation_pages().iter().map(|p| *p as u32));
    touched.extend(job.hidden_pages.iter().copied());
    touched.extend(scale_pages.iter().map(|p| *p as u32));
    let doc = if plan.edits.is_empty() {
        doc
    } else {
        doc.with_edits(&plan.edits)?
    };
    let (mode, dest) = match (&job.origin, job.target.clone()) {
        (Origin::File(path), SaveTarget::InPlace | SaveTarget::Optimised) => {
            // As for any save in place: not over a file another program changed since.
            if let Some(then) = job.stamp {
                let now = FileStamp::of_file(path).map_err(|e| DocError::io("read", path, e))?;
                if (now.len, now.ends_hash) != (then.len, then.ends_hash) {
                    return Err(pl_pdf::PdfError::ChangedOnDisk { path: path.clone() }.into());
                }
            }
            (Mode::Restructured(doc, policy), None)
        }
        (Origin::File(_), SaveTarget::As(dest)) => {
            (Mode::RestructuredAs(dest.clone(), doc, policy), Some(dest))
        }
        (Origin::Memory { .. }, SaveTarget::As(dest)) => {
            (Mode::New(dest.clone(), doc, policy), Some(dest))
        }
        (Origin::Memory { .. }, _) => return Err(DocError::NoFile),
    };
    let mut pdf = match (&job.origin, job.pdf.take()) {
        (Origin::File(_), Some(pdf)) => Some(pdf),
        (Origin::File(path), None) => Some(open_pdf(path, job.password.as_deref())?),
        (Origin::Memory { .. }, _) => None,
    };
    let outcome = write_checked(job, progress, pdf.as_mut(), mode, &touched);
    job.pdf = pdf;
    let outcome = outcome?;
    if let Some(dest) = dest {
        progress.moved_to = Some(dest);
    }
    Ok(Written {
        outcome: Some(outcome),
        counts,
    })
}

fn save_steps(job: &mut SaveJob, progress: &mut Progress) -> Result<Written> {
    if job.restructured {
        return save_working(job, progress);
    }
    let mut target = job.target.clone();
    if let Origin::Memory { bytes, .. } = &job.origin {
        // A document held in memory is written out as it is, then saved in place like any
        // file. Its display maps no file, so it can stay open until the end.
        let SaveTarget::As(dest) = &target else {
            return Err(DocError::NoFile);
        };
        let dest = dest.clone();
        let tmp = pl_pdf::tmp_path_for(&dest);
        std::fs::write(&tmp, &bytes[..]).map_err(|e| DocError::io("write", &dest, e))?;
        pl_pdf::replace_file(&tmp, &dest)?;
        job.origin = Origin::File(dest.clone());
        job.pdf = None;
        job.was_loaded = false;
        job.stamp = FileStamp::of_file(&dest).ok();
        progress.stamp = Some(job.stamp);
        progress.moved_to = Some(dest);
        target = SaveTarget::InPlace;
    }
    let Origin::File(path) = job.origin.clone() else {
        return Err(DocError::NoFile);
    };
    let mut pdf = match job.pdf.take() {
        Some(pdf) => pdf,
        None => open_pdf(&path, job.password.as_deref())?,
    };
    let written = plan_and_write(job, progress, &mut pdf, &path, &target);
    job.pdf = Some(pdf);
    written
}

/// Runs a save to its end. Never panics on a failed step: every outcome is in the result.
pub(crate) fn run_save(mut job: SaveJob) -> SaveDone {
    let mut progress = Progress {
        mirror_open: !job.suspended,
        moved_to: None,
        stamp: None,
    };
    let original_path = match &job.origin {
        Origin::File(path) => Some(path.clone()),
        Origin::Memory { .. } => None,
    };
    let written = save_steps(&mut job, &mut progress);
    let mut warnings = Vec::new();
    let final_path = progress.moved_to.clone().or(original_path);
    let moved_file = progress.moved_to.is_some();
    // The handle must be the final file's.
    if let (Some(dest), Some(pdf)) = (&progress.moved_to, &job.pdf) {
        if pdf.path() != dest.as_path() {
            job.pdf = None;
            job.was_loaded = false;
        }
    }
    let mut pdf = job.pdf.take();
    if pdf.is_none() && (moved_file || !progress.mirror_open) {
        if let Some(path) = &final_path {
            pdf = open_pdf(path, job.password.as_deref()).ok();
        }
    }

    // The store describes the file that now exists.
    let mut store = StoreAfter::Unchanged;
    let changed_file = written
        .as_ref()
        .is_ok_and(|w| w.outcome.is_some() || moved_file);
    if changed_file {
        let stamp = final_path
            .as_deref()
            .and_then(|p| FileStamp::of_file(p).ok());
        progress.stamp = Some(stamp);
        if job.restructured {
            // The pages may all be new: the store is read afresh from the file written.
            job.set = None;
            let parsed = match pdf.as_mut() {
                Some(file) => file.document().map_err(DocError::from),
                None => Err(DocError::NoFile),
            };
            store = match parsed {
                Ok(doc) => StoreAfter::Reloaded {
                    set: AnnotSet::load(doc),
                    scales: Box::new(ScaleStore::load(doc)),
                    page_count: doc.page_count(),
                },
                Err(e) => {
                    warnings.push(format!(
                        "the saved file could not be read back into the markup list ({e}); the \
                         markups are read again from the file"
                    ));
                    StoreAfter::Stale
                }
            };
        }
        if let Some(mut set) = job.set.take() {
            let parsed = match pdf.as_mut() {
                Some(file) => file.document().map_err(DocError::from),
                None => Err(DocError::NoFile),
            };
            store = match parsed {
                Ok(doc) => {
                    set.rebase(doc);
                    StoreAfter::Rebased {
                        set,
                        scales: Box::new(ScaleStore::load(doc)),
                        page_count: doc.page_count(),
                    }
                }
                Err(e) => {
                    warnings.push(format!(
                        "the saved file could not be read back into the markup list ({e}); the \
                         markups are read again from the file"
                    ));
                    StoreAfter::Stale
                }
            };
        }
    }

    // The display: a new one when the old one was closed for the replace, or shows another
    // file than the document now is.
    let mut layers_reset = false;
    // After a structural save the display, which showed the working copy's bytes, is opened
    // on the file that now holds them.
    let reopen_restructured = job.restructured && changed_file;
    let mirror = if progress.mirror_open && !moved_file && !reopen_restructured {
        MirrorAfter::Kept
    } else if let Some(path) = &final_path {
        let tail = match &job.layers_off {
            None => Vec::new(),
            Some(off) => {
                let tail = pdf
                    .as_mut()
                    .ok_or_else(|| "the file could not be opened".to_string())
                    .and_then(|file| file.document().map_err(|e| e.to_string()))
                    .and_then(|doc| doc.layer_override_tail(off).map_err(|e| e.to_string()));
                match tail {
                    Ok(tail) => tail,
                    Err(e) => {
                        warnings.push(format!(
                            "the layer choices could not be applied to the saved file ({e}); \
                             every layer is shown as the file has it"
                        ));
                        layers_reset = true;
                        Vec::new()
                    }
                }
            }
        };
        let source = Origin::File(path.clone());
        match open_mirror(job.backend.as_ref(), &source, job.password.as_deref(), tail) {
            Ok(mut new) => {
                let backend = job.backend.as_ref();
                for (i, unit) in job.user_units.iter().enumerate() {
                    if *unit != 1.0 && i < new.sizes.len() {
                        if let Err(e) = backend.set_page_user_unit(new.doc, i as u32, *unit) {
                            warnings.push(format!("/UserUnit could not be applied: {e}"));
                        }
                    }
                }
                if written.is_err() {
                    // The same pages as before the save: what the store hides or has moved
                    // is hidden and moved again.
                    for fix in &job.fixes {
                        let done = match fix.rect {
                            Some(rect) => {
                                backend.set_annotation_rect(new.doc, fix.page, fix.index, rect)
                            }
                            None => Ok(()),
                        }
                        .and_then(|()| match fix.hide {
                            true => {
                                backend.set_annotation_hidden(new.doc, fix.page, fix.index, true)
                            }
                            false => Ok(()),
                        });
                        if let Err(e) = done {
                            warnings.push(format!(
                                "an edited annotation on page {} could not be put back in the \
                                 page render: {e}",
                                fix.page + 1
                            ));
                        }
                    }
                }
                if job.complete {
                    new.infos = Some(
                        (0..new.sizes.len() as u32)
                            .map(|page| match new.sizes[page as usize] {
                                Some(_) => {
                                    backend.page_info(new.doc, page).map_err(|e| e.to_string())
                                }
                                None => Err("its size could not be read".to_string()),
                            })
                            .collect(),
                    );
                }
                if progress.mirror_open {
                    let _ = backend.close(job.render);
                }
                MirrorAfter::New(new)
            }
            Err(e) if progress.mirror_open => {
                warnings.push(format!(
                    "the page display could not be opened on {} ({e}); the pages are shown \
                     from the previous file",
                    path.display()
                ));
                MirrorAfter::Kept
            }
            Err(e) => MirrorAfter::Lost(e.to_string()),
        }
    } else {
        MirrorAfter::Kept
    };

    // A loaded lopdf document is about twice the file's size in memory
    // (docs/measurements.md): released unless it was loaded before the save began.
    if !job.was_loaded {
        if let Some(file) = pdf.as_mut() {
            file.unload();
        }
    }
    let result = written.map(|w| Saved {
        path: final_path.clone().unwrap_or_default(),
        outcome: w.outcome,
        added: w.counts.0,
        modified: w.counts.1,
        deleted: w.counts.2,
    });
    SaveDone {
        result,
        pdf,
        moved_to: progress.moved_to,
        mirror,
        store,
        stamp: progress.stamp,
        layers_reset,
        warnings,
    }
}

// ---- the load job ----------------------------------------------------------------------------

pub(crate) struct LoadJob {
    pub origin: Origin,
    pub password: Option<String>,
    /// The crash journal to read along with the annotations.
    pub journal: Option<PathBuf>,
    pub annots: bool,
}

pub(crate) struct LoadDone {
    /// The annotations, when they were asked for.
    pub set: Option<AnnotSet>,
    /// The pages' scales and the document settings, read with the annotations.
    pub scales: Option<ScaleStore>,
    pub stamp: Option<FileStamp>,
    /// Every page's `/UserUnit` as lopdf reads it.
    pub units: Option<Vec<f64>>,
    pub layers: Option<Vec<LayerInfo>>,
    pub journal: Option<std::io::Result<JournalContents>>,
    pub warning: Option<String>,
    pub error: Option<DocError>,
}

fn read_parsed(doc: &PdfDocument, annots: bool, done: &mut LoadDone) {
    if annots {
        done.set = Some(AnnotSet::load(doc));
        done.scales = Some(ScaleStore::load(doc));
    }
    done.layers = Some(doc.layers());
}

/// Parses the file and reads what was asked for. The parsed file is dropped again before
/// this returns; the annotations stay.
pub(crate) fn run_load(job: LoadJob) -> LoadDone {
    let mut done = LoadDone {
        set: None,
        scales: None,
        stamp: None,
        units: None,
        layers: None,
        journal: None,
        warning: None,
        error: None,
    };
    match &job.origin {
        Origin::File(path) => {
            done.stamp = FileStamp::of_file(path).ok();
            let parsed = open_pdf(path, job.password.as_deref()).and_then(|mut file| {
                let doc = file.document()?;
                read_parsed(doc, job.annots, &mut done);
                done.units = doc
                    .pages()
                    .ok()
                    .map(|pages| pages.iter().map(|p| p.user_unit).collect());
                Ok(())
            });
            if let Err(e) = parsed {
                done.error = Some(e);
            }
        }
        Origin::Memory { bytes, .. } => {
            match PdfDocument::from_bytes(bytes.to_vec(), job.password.as_deref()) {
                Ok(doc) => read_parsed(&doc, job.annots, &mut done),
                Err(e) if job.annots => {
                    done.warning = Some(format!(
                        "the annotations could not be read ({e}); starting with none"
                    ));
                    done.set = Some(AnnotSet::new());
                }
                Err(e) => done.error = Some(e.into()),
            }
        }
    }
    if let (Some(journal), true) = (&job.journal, done.set.is_some()) {
        if journal.exists() {
            done.journal = Some(Journal::read(journal));
        }
    }
    done
}

// ---- the document's side ---------------------------------------------------------------------

fn worker_lost() -> DocError {
    DocError::Worker("the worker thread stopped without a result".into())
}

impl Document {
    /// What the document is doing in the background (see the module documentation).
    pub fn busy(&self) -> Busy {
        match &self.flight {
            None => Busy::Idle,
            Some(Flight::Save { .. }) => Busy::Saving,
            Some(Flight::Load { .. }) => Busy::Loading,
        }
    }

    /// Whether a save is in flight: the document is read-only until it is finished.
    pub fn is_saving(&self) -> bool {
        matches!(self.flight, Some(Flight::Save { .. }))
    }

    /// Whether the annotations or the layer list are being read on a worker thread.
    pub fn is_loading(&self) -> bool {
        matches!(self.flight, Some(Flight::Load { .. }))
    }

    /// The file a Save As in flight is writing, which becomes the document's file when the
    /// save is finished. Until then the document's [`path`](Document::path) is still the old
    /// one, so whoever hands out files ([`Session`](crate::Session)) asks here as well.
    pub fn saving_to(&self) -> Option<&Path> {
        match &self.flight {
            Some(Flight::Save { dest, .. }) => dest.as_deref(),
            _ => None,
        }
    }

    /// The save in flight, if there is one.
    pub fn save_in_flight(&self) -> Option<SaveTicket> {
        match &self.flight {
            Some(Flight::Save { serial, .. }) => Some(SaveTicket {
                document: self.id(),
                serial: *serial,
            }),
            _ => None,
        }
    }

    /// The load in flight, if there is one.
    pub fn load_in_flight(&self) -> Option<LoadTicket> {
        match &self.flight {
            Some(Flight::Load { serial, .. }) => Some(LoadTicket {
                document: self.id(),
                serial: *serial,
            }),
            _ => None,
        }
    }

    /// [`DocError::Saving`] while a save is in flight: what every call that would change the
    /// document, its file or its display starts with.
    pub(crate) fn refuse_while_saving(&self) -> Result<()> {
        if self.is_saving() {
            Err(DocError::Saving)
        } else {
            Ok(())
        }
    }

    fn refuse_while_busy(&self) -> Result<()> {
        match self.busy() {
            Busy::Idle => Ok(()),
            Busy::Saving => Err(DocError::Saving),
            Busy::Loading => Err(DocError::Loading),
        }
    }

    fn next_serial(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }

    // ---- save --------------------------------------------------------------------------------

    /// Gathers what a save needs. From here until [`adopt_save`](Document::adopt_save) the
    /// document's lopdf handle is with the job.
    fn save_job(
        &mut self,
        target: SaveTarget,
        options: &SaveOptions,
        raw: Option<(EditSet, Vec<u32>)>,
    ) -> Result<SaveJob> {
        self.refuse_while_busy()?;
        let target = match target {
            SaveTarget::As(dest) => {
                let dest =
                    std::path::absolute(&dest).map_err(|e| DocError::io("resolve", &dest, e))?;
                if self
                    .path()
                    .is_some_and(|p| crate::journal::same_path(p, &dest))
                {
                    // Saving "as" the document's own file is a save.
                    SaveTarget::InPlace
                } else {
                    SaveTarget::As(dest)
                }
            }
            other => other,
        };
        if self.path().is_none() && !matches!(target, SaveTarget::As(_)) {
            return Err(DocError::NoFile);
        }
        let in_place = !matches!(target, SaveTarget::As(_)) || self.path().is_none();
        // A plain save of a document whose annotations were never read writes nothing.
        let writes =
            raw.is_some() || self.edit.state.is_some() || !matches!(target, SaveTarget::InPlace);
        let users = if in_place && writes {
            // Running scene builds map the file: they are told to stop now and waited for
            // by the job. Finished scenes hold nothing of the file.
            self.cancel_file_users()
        } else {
            FileUsers::default()
        };
        let (set, scales, stamp, fixes) = match self.edit.state.as_ref() {
            Some(state) => (
                Some(state.editor.set().clone()),
                Some(state.editor.scales().clone()),
                state.stamp,
                state.mirror_fixes(),
            ),
            None => (None, None, None, Vec::new()),
        };
        let mut hidden_pages: Vec<u32> = fixes.iter().map(|f| f.page).collect();
        hidden_pages.sort_unstable();
        hidden_pages.dedup();
        Ok(SaveJob {
            backend: self.backend.clone(),
            origin: self.origin.clone(),
            password: self.password.clone(),
            target,
            options: options.clone(),
            was_loaded: self.pdf_is_loaded(),
            pdf: self.pdf.take(),
            set,
            scales,
            stamp,
            raw,
            render: self.render,
            suspended: self.suspended,
            page_count: self.page_count(),
            unreadable: self.unreadable_pages().to_vec(),
            hidden_pages,
            fixes,
            users,
            release_wait: crate::session::FILE_RELEASE_WAIT,
            layers_off: self.layers_switched_off(),
            user_units: self.pages().iter().map(|p| p.user_unit()).collect(),
            complete: self.settles_at_open(),
            working: self
                .structure
                .working
                .as_ref()
                .map(|w| (w.doc.clone(), w.accepted)),
            restructured: self.structure.working.is_some(),
        })
    }

    /// Takes a finished save into the document, in one step. Makes no backend call and reads
    /// no file.
    pub(crate) fn adopt_save(&mut self, done: SaveDone) -> Result<MarkupSave> {
        let old_journal = self.journal_file_path();
        self.pdf = done.pdf;
        if let Some(dest) = &done.moved_to {
            self.origin = Origin::File(dest.clone());
        }
        match done.mirror {
            MirrorAfter::Kept => {}
            MirrorAfter::New(new) => self.adopt_mirror(new),
            MirrorAfter::Lost(why) => {
                self.suspended = true;
                self.warnings.push(format!(
                    "the page display could not be reopened after the save: {why}"
                ));
            }
        }
        if done.layers_reset {
            self.reset_layer_choices();
        }
        self.warnings.extend(done.warnings);
        if let (Some(stamp), Some(state)) = (done.stamp, self.edit.state.as_mut()) {
            state.stamp = stamp;
        }
        match done.store {
            StoreAfter::Unchanged => {}
            StoreAfter::Rebased {
                set,
                scales,
                page_count,
            } => {
                if let Some(state) = self.edit.state.as_mut() {
                    state.editor.replace_set(set);
                    state.editor.set_page_count(page_count);
                    state.editor.replace_scales(*scales);
                    // The new display shows the new file: nothing is hidden or moved, nothing
                    // is ours to paint, and every page has a new revision already.
                    state.clear_mirror_state();
                }
            }
            StoreAfter::Reloaded {
                set,
                scales,
                page_count,
            } => {
                let old_total = self.edit_revision();
                if let Some(state) = self.edit.state.as_mut() {
                    let selection = state.editor.selection().to_vec();
                    state.editor = crate::command::Editor::with_scales(set, *scales, page_count);
                    state.editor.select(selection);
                    state.clear_mirror_state();
                }
                self.rebase_edit_revision(old_total);
            }
            StoreAfter::Stale => self.drop_store(),
        }
        let moved = done.moved_to.is_some();
        let saved = done.result?;
        if saved.outcome.is_some() || moved {
            // The file is the truth now: the working copy is written and structural steps
            // (whose snapshots are of the old file) are no longer undoable.
            self.structure.clear_all();
        }
        if let Some(state) = self.edit.state.as_mut() {
            state.settle_saved();
        }
        for file in [old_journal, self.journal_file_path()]
            .into_iter()
            .flatten()
        {
            let _ = crate::journal::remove_if_present(&file);
        }
        Ok(MarkupSave {
            path: saved.path,
            outcome: saved.outcome,
            added: saved.added,
            modified: saved.modified,
            deleted: saved.deleted,
        })
    }

    /// Runs a save on the calling thread.
    pub(crate) fn save_blocking(
        &mut self,
        target: SaveTarget,
        options: &SaveOptions,
        raw: Option<(EditSet, Vec<u32>)>,
    ) -> Result<MarkupSave> {
        // A load on a worker thread is waited for: the save is of what it reads.
        self.settle_load()?;
        let job = self.save_job(target, options, raw)?;
        let done = run_save(job);
        self.adopt_save(done)
    }

    /// Starts saving the annotation changes into the document's file on a worker thread, as
    /// [`save`](Document::save) does on the calling one. Returns at once.
    pub fn start_save(&mut self) -> Result<SaveTicket> {
        self.start_save_with(SaveRequest::new(SaveTarget::InPlace))
    }

    /// Starts [`save_as`](Document::save_as) on a worker thread. (Use
    /// [`Session::start_save_as`](crate::Session::start_save_as), which refuses the file of
    /// another open document.)
    pub fn start_save_as(&mut self, dest: impl AsRef<Path>) -> Result<SaveTicket> {
        self.start_save_with(SaveRequest::new(SaveTarget::As(
            dest.as_ref().to_path_buf(),
        )))
    }

    /// Starts [`save_optimised`](Document::save_optimised) on a worker thread.
    pub fn start_save_optimised(&mut self) -> Result<SaveTicket> {
        self.start_save_with(SaveRequest::new(SaveTarget::Optimised))
    }

    /// Starts a save on a worker thread and returns at once. Until the ticket is finished the
    /// document is read-only (see the module documentation). An error here means nothing was
    /// started and nothing changed: the document is busy, or has no file to save into.
    pub fn start_save_with(&mut self, request: SaveRequest) -> Result<SaveTicket> {
        let dest = match &request.target {
            SaveTarget::As(dest) => {
                Some(std::path::absolute(dest).unwrap_or_else(|_| dest.clone()))
            }
            _ => None,
        };
        let job = self.save_job(request.target, &request.options, None)?;
        let (tx, rx) = channel();
        let serial = self.next_serial();
        let closer = self.backend.clone();
        let spawned = std::thread::Builder::new()
            .name("pl-doc-save".into())
            .spawn(move || {
                let done = Box::new(run_save(job));
                if let Err(unsent) = tx.send(done) {
                    // Nobody is left to take the new display: close it.
                    let unsent: Box<SaveDone> = unsent.0;
                    if let MirrorAfter::New(new) = &unsent.mirror {
                        let _ = closer.close(new.doc);
                    }
                }
            });
        if let Err(e) = spawned {
            // The job never ran: nothing was closed and nothing was written.
            return Err(DocError::Worker(format!(
                "a worker thread could not be started: {e}"
            )));
        }
        self.flight = Some(Flight::Save { serial, rx, dest });
        Ok(SaveTicket {
            document: self.id(),
            serial,
        })
    }

    fn take_save(&mut self, done: std::result::Result<Box<SaveDone>, ()>) -> Result<MarkupSave> {
        self.flight = None;
        match done {
            Ok(done) => self.adopt_save(*done),
            Err(()) => {
                // The worker died. Whatever it did to the display, show the file again.
                self.pdf = None;
                let _ = self.backend.close(self.render);
                self.suspended = true;
                if let Err(e) = self.reopen_render(Vec::new()) {
                    self.warnings
                        .push(format!("the page display could not be reopened: {e}"));
                }
                Err(worker_lost())
            }
        }
    }

    /// Takes the result of a save started with [`start_save`](Document::start_save) if the
    /// worker has finished: `None` while it has not (or when `ticket` is not the save in
    /// flight). Never waits; call it once per frame. The result is returned once.
    pub fn try_finish_save(&mut self, ticket: SaveTicket) -> Option<Result<MarkupSave>> {
        let Some(Flight::Save { serial, rx, .. }) = &self.flight else {
            return None;
        };
        if *serial != ticket.serial || ticket.document != self.id() {
            return None;
        }
        let done = match rx.try_recv() {
            Ok(done) => Ok(done),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(()),
        };
        Some(self.take_save(done))
    }

    /// As [`try_finish_save`](Document::try_finish_save) for whichever save is in flight.
    pub fn poll_save(&mut self) -> Option<Result<MarkupSave>> {
        let ticket = self.save_in_flight()?;
        self.try_finish_save(ticket)
    }

    /// Waits for a save started with [`start_save`](Document::start_save) and returns its
    /// result. An error of [`DocError::NoSuchTicket`] means `ticket` is not the save in
    /// flight (it was finished already).
    pub fn finish_save(&mut self, ticket: SaveTicket) -> Result<MarkupSave> {
        let Some(Flight::Save { serial, rx, .. }) = &self.flight else {
            return Err(DocError::NoSuchTicket);
        };
        if *serial != ticket.serial || ticket.document != self.id() {
            return Err(DocError::NoSuchTicket);
        }
        let done = rx.recv().map_err(|_| ());
        self.take_save(done)
    }

    /// Waits for the save in flight, if there is one, and takes its result in (a failure
    /// becomes a warning). For the calls that must not run beside a save and cannot refuse.
    pub(crate) fn settle_save(&mut self) {
        if let Some(ticket) = self.save_in_flight() {
            if let Err(e) = self.finish_save(ticket) {
                self.warnings.push(format!("the save failed: {e}"));
            }
        }
    }

    // ---- load --------------------------------------------------------------------------------

    fn load_job(&self, annots: bool) -> LoadJob {
        LoadJob {
            origin: self.origin.clone(),
            password: self.password.clone(),
            journal: annots.then(|| self.journal_file_path()).flatten(),
            annots,
        }
    }

    fn start_load(&mut self, annots: bool) -> Result<LoadTicket> {
        self.refuse_while_busy()?;
        let serial = self.next_serial();
        let ticket = LoadTicket {
            document: self.id(),
            serial,
        };
        let (tx, rx) = channel();
        if !annots && self.cached_layers().is_none() {
            // The layers of a working copy are at hand (and the file's would be wrong).
            if let Some(layers) = self.structure.working.as_ref().map(|w| w.doc.layers()) {
                self.install_layers(layers);
            }
        }
        let wanted = if annots {
            !self.annots_loaded()
        } else {
            self.cached_layers().is_none()
        };
        if !wanted {
            // Nothing to read: the ticket is finished already.
            let _ = tx.send(Box::new(LoadDone {
                set: None,
                scales: None,
                stamp: None,
                units: None,
                layers: None,
                journal: None,
                warning: None,
                error: None,
            }));
        } else {
            let job = self.load_job(annots);
            std::thread::Builder::new()
                .name("pl-doc-load".into())
                .spawn(move || {
                    let _ = tx.send(Box::new(run_load(job)));
                })
                .map_err(|e| {
                    DocError::Worker(format!("a worker thread could not be started: {e}"))
                })?;
        }
        self.flight = Some(Flight::Load { serial, rx });
        Ok(ticket)
    }

    /// Starts reading the document's annotations (and its layer list, which the same parse
    /// gives for free) on a worker thread, as [`load_annots`](Document::load_annots) does on
    /// the calling one. Returns at once; finish with
    /// [`try_finish_load`](Document::try_finish_load). When the annotations are loaded
    /// already the ticket is finished at once.
    ///
    /// Until the load is finished the document can be viewed as before. A call that needs the
    /// annotations ([`apply`](Document::apply), [`annots`](Document::annots), ...) waits for
    /// the worker instead of parsing the file a second time.
    pub fn start_load_annots(&mut self) -> Result<LoadTicket> {
        self.start_load(true)
    }

    /// Starts reading the document's layer list on a worker thread, as
    /// [`layers`](Document::layers) does on the calling one. The annotations are not read.
    pub fn start_load_layers(&mut self) -> Result<LoadTicket> {
        self.start_load(false)
    }

    /// Takes a finished load into the document. It parses nothing and reads no file. With a
    /// crash journal to replay it makes the short display calls an edit makes (hiding what
    /// the recovered changes touched).
    fn adopt_load(&mut self, done: LoadDone, tell_backend: bool) -> Result<()> {
        if let Some(layers) = done.layers {
            self.install_layers(layers);
        }
        if let Some(warning) = done.warning {
            self.warnings.push(warning);
        }
        if let Some(set) = done.set {
            if !self.annots_loaded() {
                // lopdf knows every page's /UserUnit, which the open may not have found.
                if let Some(units) = done.units {
                    if let Err(e) = self.apply_user_units(&units, tell_backend) {
                        self.warnings
                            .push(format!("/UserUnit could not be applied: {e}"));
                    }
                }
                self.install_store(set, done.scales, done.stamp);
                self.recover_journal_from(done.journal);
            }
        }
        match done.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn take_load(
        &mut self,
        done: std::result::Result<Box<LoadDone>, ()>,
        tell_backend: bool,
    ) -> Result<()> {
        self.flight = None;
        match done {
            Ok(done) => self.adopt_load(*done, tell_backend),
            Err(()) => Err(worker_lost()),
        }
    }

    /// Takes the result of a load started with
    /// [`start_load_annots`](Document::start_load_annots) or
    /// [`start_load_layers`](Document::start_load_layers) if the worker has finished: `None`
    /// while it has not (or when `ticket` is not the load in flight). Never waits for the
    /// worker. After `Some(Ok(()))` the annotations ([`Document::editor`]) or the layers
    /// ([`Document::cached_layers`]) are there.
    pub fn try_finish_load(&mut self, ticket: LoadTicket) -> Option<Result<()>> {
        let Some(Flight::Load { serial, rx }) = &self.flight else {
            return None;
        };
        if *serial != ticket.serial || ticket.document != self.id() {
            return None;
        }
        let done = match rx.try_recv() {
            Ok(done) => Ok(done),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(()),
        };
        Some(self.take_load(done, false))
    }

    /// As [`try_finish_load`](Document::try_finish_load) for whichever load is in flight.
    pub fn poll_load(&mut self) -> Option<Result<()>> {
        let ticket = self.load_in_flight()?;
        self.try_finish_load(ticket)
    }

    /// Waits for a load and takes its result in.
    pub fn finish_load(&mut self, ticket: LoadTicket) -> Result<()> {
        let Some(Flight::Load { serial, rx }) = &self.flight else {
            return Err(DocError::NoSuchTicket);
        };
        if *serial != ticket.serial || ticket.document != self.id() {
            return Err(DocError::NoSuchTicket);
        }
        let done = rx.recv().map_err(|_| ());
        self.take_load(done, true)
    }

    /// Forgets a load in flight: the worker finishes by itself and its result is dropped.
    pub(crate) fn abandon_load(&mut self) {
        if self.is_loading() {
            self.flight = None;
        }
    }

    /// Waits for the load in flight, if there is one.
    pub(crate) fn settle_load(&mut self) -> Result<()> {
        match self.load_in_flight() {
            Some(ticket) => self.finish_load(ticket),
            None => Ok(()),
        }
    }
}
