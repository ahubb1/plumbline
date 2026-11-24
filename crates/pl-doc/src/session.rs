//! [`Session`] and [`Document`]: the open documents and what is lazily derived from each.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pl_geom::{PageGeometry, Point, Size};
use pl_pdf::{EditSet, Expected, PdfDocument, PdfFile, SaveOptions, SaveReport, VerifyError};
use pl_render::{
    AnnotInfo, Bitmap, DocId, DocSource, OutlineItem, PageInfo, PageText, RenderBackend,
    SearchOptions,
};
use pl_scene::{
    spawn_scene_build, NoSnapReason, Scene, SceneJob, SceneLimits, SceneSource, SceneWorker,
};

use crate::bookmarks::BookmarkTree;
use crate::editing::EditSlot;
use crate::error::{DocError, Result};
use crate::search::DocSearch;
use crate::selection::{select_between, TextSelection};
use crate::view::ViewState;

/// How many pages of extracted text a document keeps.
pub const TEXT_CACHE_PAGES: usize = 64;
/// How many finished content-snap scenes a document keeps.
pub const SCENE_CACHE_PAGES: usize = 6;
/// How long [`Document::suspend_render`] waits for background work that still maps the file.
pub const FILE_RELEASE_WAIT: Duration = Duration::from_secs(3);
/// The size given to a page whose own size cannot be read, when no earlier page has one.
const PLACEHOLDER_PAGE: [f64; 2] = [612.0, 792.0];
/// How many untouched pages a save renders before and after to compare.
pub(crate) const SAVE_RENDER_SAMPLES: usize = 3;
/// Longer side, in pixels, of the renders a save compares.
pub(crate) const SAVE_RENDER_PX: u32 = 256;

/// Identifies one open document within a [`Session`]. It stays the same when the render mirror
/// is reopened (which changes the backend's [`DocId`]). Ids are not reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(pub u32);

/// How `/UserUnit` is found. PDFium 8125 cannot report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UserUnitPolicy {
    /// Read the page dictionaries with hayro-syntax, which works straight from the mapped file
    /// (no copy, no object graph) and sees into object streams. Only when hayro-syntax cannot
    /// read the file (it is encrypted, or malformed) *and* the raw bytes contain `/UserUnit`
    /// is the file parsed with lopdf. An encrypted file whose page dictionaries sit in object
    /// streams is the one case left at 1.0; [`Document::refresh_user_units`] settles it once
    /// lopdf has the file loaded anyway.
    ///
    /// In [`OpenMode::Interactive`] this runs on a worker thread after the open has returned
    /// and is applied by [`Document::poll_background`]; it never costs the open anything.
    #[default]
    Scan,
    /// Always parse with lopdf, during the open. Slow and memory-hungry on a large file.
    Always,
    /// Never look; every page reports 1.0 until [`Document::refresh_user_units`] is called.
    Never,
}

/// How much an open settles before it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OpenMode {
    /// For a user interface. The open asks the backend for the page *sizes* only, which costs
    /// the same however much is drawn on the pages, and returns. Each page then has a stand-in
    /// geometry of the right displayed size ([`Document::pages`]); its real boxes and rotation
    /// arrive when the page is first needed ([`Document::accept_page_info`] from a scheduler
    /// job, or [`Document::load_geometry`]), and `/UserUnit` arrives from a worker thread
    /// ([`Document::poll_background`]).
    #[default]
    Interactive,
    /// For the command line and tests. The open loads every page for its real geometry and
    /// waits for `/UserUnit` before it returns, so every query answers at once. The time this
    /// takes grows with the amount of drawing in the file.
    Complete,
}

/// Session-wide settings.
#[derive(Clone, Copy, Debug, Default)]
pub struct SessionOptions {
    /// Caps for content-snap scene builds.
    pub scene_limits: SceneLimits,
    pub user_unit: UserUnitPolicy,
    pub open: OpenMode,
}

impl SessionOptions {
    /// Everything settled at open: see [`OpenMode::Complete`].
    pub fn complete() -> SessionOptions {
        SessionOptions {
            open: OpenMode::Complete,
            ..SessionOptions::default()
        }
    }
}

/// The state of a page's content-snap scene.
#[derive(Clone, Debug)]
pub enum SceneState {
    /// A build is running on a worker thread; ask again later.
    Pending,
    Ready(Arc<Scene>),
    /// There is no content snapping on this page, and why.
    Unavailable(NoSnapReason),
}

impl SceneState {
    pub fn scene(&self) -> Option<&Arc<Scene>> {
        match self {
            SceneState::Ready(scene) => Some(scene),
            _ => None,
        }
    }
}

enum SceneSlot {
    Building(SceneJob),
    Ready(Arc<Scene>),
    Unavailable(NoSnapReason),
}

#[derive(Clone)]
pub(crate) enum Origin {
    File(PathBuf),
    Memory { name: String, bytes: Arc<Vec<u8>> },
}

/// What the `/UserUnit` probe found: the unit of every page, or `None` when it could not tell.
type ProbeOutcome = std::result::Result<Option<Vec<f64>>, String>;

/// The `/UserUnit` probe running on a worker thread.
struct UnitProbe {
    rx: Receiver<ProbeOutcome>,
    /// Set once the worker no longer maps the file.
    released: Arc<AtomicBool>,
}

/// What a save checked through PDFium before the file was replaced.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PdfiumCheck {
    /// Pages whose annotation count PDFium confirmed.
    pub annotation_pages: Vec<u32>,
    /// Untouched pages whose render was the same before and after the save.
    pub render_pages: Vec<u32>,
}

/// The result of [`Document::save_incremental`].
#[derive(Clone, Debug)]
pub struct SaveOutcome {
    /// What pl-pdf wrote and verified with lopdf and hayro-syntax.
    pub report: SaveReport,
    /// What PDFium confirmed about the written file.
    pub pdfium: PdfiumCheck,
}

/// One open document.
///
/// * Its render mirror lives in the backend ([`Document::render_id`]).
/// * Page sizes are read at open; a page's full geometry when it is first needed (or all at
///   open, in [`OpenMode::Complete`]).
/// * The lopdf document ([`Document::pdf`]) is created on first use.
/// * Page text, bookmarks and content-snap scenes are fetched on first use and cached.
///
/// # Calls that wait for the backend
///
/// The backend is one thread. [`load_geometry`](Document::load_geometry),
/// [`page_text`](Document::page_text), [`select_text`](Document::select_text),
/// [`step_search`](Document::step_search), [`search`](Document::search),
/// [`bookmarks`](Document::bookmarks) and [`annotations`](Document::annotations) send it a
/// request and wait for the answer, behind whatever tile is rendering. They are for the
/// command line and tests. A user interface asks through the scheduler instead
/// (`JobKind::PageText`, `PageInfo`, `Outline`) and hands the results to
/// [`accept_text`](Document::accept_text), [`accept_page_info`](Document::accept_page_info)
/// and [`accept_outline`](Document::accept_outline); in between it reads
/// [`cached_text`](Document::cached_text), [`page_geometry`](Document::page_geometry) and
/// [`cached_bookmarks`](Document::cached_bookmarks), none of which waits.
///
/// Dropping the document closes it in the backend.
pub struct Document {
    id: DocumentId,
    pub(crate) backend: Arc<dyn RenderBackend>,
    pub(crate) render: DocId,
    pub(crate) suspended: bool,
    pub(crate) origin: Origin,
    pub(crate) password: Option<String>,
    open_mode: OpenMode,
    /// Layout geometry: the real one where `exact`, otherwise a stand-in of the right size.
    pages: Vec<PageGeometry>,
    exact: Vec<bool>,
    /// Pages the backend could not load.
    unreadable: Vec<bool>,
    geometry_epoch: u64,
    pub(crate) pdf: Option<PdfFile>,
    texts: HashMap<u32, Arc<PageText>>,
    text_order: VecDeque<u32>,
    bookmarks: Option<Arc<BookmarkTree>>,
    scenes: HashMap<u32, SceneSlot>,
    scene_order: VecDeque<u32>,
    scene_limits: SceneLimits,
    /// Workers of scene builds that may still be running and mapping the file.
    scene_workers: Vec<SceneWorker>,
    unit_probe: Option<UnitProbe>,
    revisions: Vec<u64>,
    revision: u64,
    pub(crate) warnings: Vec<String>,
    /// The annotation store and what goes with it (see `editing.rs`); empty until first used.
    pub(crate) edit: EditSlot,
    /// The layers and which of them are switched off (see `layers.rs`); empty until asked for.
    pub(crate) layers: Option<crate::layers::LayerState>,
    /// The save or load running on a worker thread (see `background.rs`).
    pub(crate) flight: Option<crate::background::Flight>,
    pub(crate) next_ticket: u64,
}

/// A freshly opened render mirror, before a document has taken it.
pub(crate) struct NewMirror {
    pub doc: DocId,
    pub sizes: Vec<Option<[f64; 2]>>,
    /// Every page's real geometry (or why it could not be loaded), when it was fetched along.
    pub infos: Option<Vec<std::result::Result<PageInfo, String>>>,
}

/// Opens a render mirror on `origin`, with `appended` presented after its bytes, and reads the
/// page sizes. No page is loaded.
pub(crate) fn open_mirror(
    backend: &dyn RenderBackend,
    origin: &Origin,
    password: Option<&str>,
    appended: Vec<u8>,
) -> Result<NewMirror> {
    let source = match origin {
        Origin::File(path) => DocSource::File {
            path: path.clone(),
            appended,
        },
        Origin::Memory { bytes, .. } => {
            let mut all = bytes.to_vec();
            all.extend_from_slice(&appended);
            DocSource::Bytes(all)
        }
    };
    let info = backend.open(source, password)?;
    let sizes = match backend.page_sizes(info.doc) {
        Ok(sizes) if sizes.len() == info.page_count as usize => sizes,
        Ok(_) => {
            let _ = backend.close(info.doc);
            return Err(pl_render::RenderError::UnexpectedReply.into());
        }
        Err(e) => {
            let _ = backend.close(info.doc);
            return Err(e.into());
        }
    };
    Ok(NewMirror {
        doc: info.doc,
        sizes,
        infos: None,
    })
}

impl Document {
    pub fn id(&self) -> DocumentId {
        self.id
    }

    /// The document's id in the render backend. It changes when the mirror is reopened.
    pub fn render_id(&self) -> DocId {
        self.render
    }

    pub fn backend(&self) -> &Arc<dyn RenderBackend> {
        &self.backend
    }

    /// The file's absolute path; `None` for a document opened from memory.
    pub fn path(&self) -> Option<&Path> {
        match &self.origin {
            Origin::File(path) => Some(path),
            Origin::Memory { .. } => None,
        }
    }

    /// A short name for a tab: the file name, or the name given to an in-memory document.
    pub fn title(&self) -> String {
        match &self.origin {
            Origin::File(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            Origin::Memory { name, .. } => name.clone(),
        }
    }

    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    // ---- geometry ----------------------------------------------------------------------------

    /// Every page's *layout* geometry: the real geometry of the pages whose
    /// [`geometry_is_exact`](Document::geometry_is_exact), and for the others a stand-in with
    /// the right displayed size (and so the right place in a page layout and the right tile
    /// grid) but not the file's page space. Use it to lay pages out and to request tiles; use
    /// [`page_geometry`](Document::page_geometry) for anything in page coordinates.
    pub fn pages(&self) -> &[PageGeometry] {
        &self.pages
    }

    /// Whether the page's real boxes and rotation are known.
    pub fn geometry_is_exact(&self, page: u32) -> bool {
        self.exact.get(page as usize).copied().unwrap_or(false)
    }

    /// A page's real geometry, as the file has it. Never waits: a page whose geometry has not
    /// arrived yet is [`DocError::GeometryPending`], and one the backend cannot load is
    /// [`DocError::PageUnreadable`]. After an [`OpenMode::Complete`] open every readable page
    /// answers.
    pub fn page_geometry(&self, page: u32) -> Result<&PageGeometry> {
        let i = self.check_page(page)?;
        if self.exact[i] {
            Ok(&self.pages[i])
        } else if self.unreadable[i] {
            Err(DocError::PageUnreadable { page })
        } else {
            Err(DocError::GeometryPending { page })
        }
    }

    /// A page's real geometry, loading the page in the backend now if it is not known yet.
    /// This waits for the backend.
    pub fn load_geometry(&mut self, page: u32) -> Result<&PageGeometry> {
        let i = self.check_page(page)?;
        if !self.exact[i] {
            if self.unreadable[i] {
                return Err(DocError::PageUnreadable { page });
            }
            // The display is the save's while one is in flight; a failed request then says
            // nothing about the page.
            self.refuse_while_saving()?;
            match self.backend.page_info(self.render, page) {
                Ok(info) => {
                    self.set_exact(i, info);
                }
                Err(e) => {
                    self.mark_unreadable(i, &e.to_string());
                    return Err(e.into());
                }
            }
        }
        Ok(&self.pages[i])
    }

    /// Loads the real geometry of every page that does not have it yet. A page that cannot be
    /// loaded becomes a warning and keeps its stand-in; the others are not affected. This
    /// waits for the backend once per page, and the backend parses each page's content.
    pub fn load_all_geometry(&mut self) {
        for page in 0..self.page_count() {
            let _ = self.load_geometry(page);
        }
    }

    /// Takes a page's geometry from a finished `JobKind::PageInfo` job. `render` is the
    /// backend document the job ran on; an answer for a mirror that has since been reopened is
    /// ignored. Returns whether the page's layout geometry changed (views then need
    /// [`ViewState::set_pages`]).
    pub fn accept_page_info(&mut self, render: DocId, page: u32, info: PageInfo) -> bool {
        if render != self.render || page >= self.page_count() {
            return false;
        }
        self.set_exact(page as usize, info)
    }

    /// Records that the backend could not load a page (a failed `JobKind::PageInfo` job).
    pub fn note_unreadable(&mut self, render: DocId, page: u32, why: &str) {
        if render == self.render
            && page < self.page_count()
            && !self.exact[page as usize]
            && !self.is_saving()
        {
            self.mark_unreadable(page as usize, why);
        }
    }

    /// Goes up whenever any page's layout geometry changes (real geometry arrived with another
    /// rotation or origin, `/UserUnit` was found, the mirror was reopened). A view compares it
    /// with the value it last saw and calls [`ViewState::set_pages`] when it differs.
    pub fn geometry_epoch(&self) -> u64 {
        self.geometry_epoch
    }

    fn check_page(&self, page: u32) -> Result<usize> {
        if page < self.page_count() {
            Ok(page as usize)
        } else {
            Err(DocError::PageOutOfRange {
                page,
                count: self.page_count(),
            })
        }
    }

    fn set_exact(&mut self, i: usize, mut info: PageInfo) -> bool {
        // The unit is this document's to know (the backend only repeats what it was told).
        info.user_unit = self.pages[i].user_unit();
        let geometry = info.geometry();
        let changed = geometry != self.pages[i];
        if changed {
            self.pages[i] = geometry;
            self.geometry_epoch += 1;
        }
        self.exact[i] = true;
        self.unreadable[i] = false;
        changed
    }

    fn mark_unreadable(&mut self, i: usize, why: &str) {
        if !self.unreadable[i] {
            self.unreadable[i] = true;
            self.warnings
                .push(format!("page {} could not be read: {why}", i + 1));
        }
    }

    /// A counter that goes up whenever anything rendered may have changed. It is *not* the
    /// tile cache's content revision: that is per page, [`page_revision`](Document::page_revision).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The content revision of one page: the `revision` of its tile keys. It changes only when
    /// that page's rendered content does, so an edit on one page leaves every other page's
    /// cached tiles valid.
    pub fn page_revision(&self, page: u32) -> u64 {
        self.revisions.get(page as usize).copied().unwrap_or(0)
    }

    /// Things that went wrong without stopping the document from opening (a page that could
    /// not be read, `/UserUnit` that could not be found out).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// A new view of this document at 100%, showing the top of the first page.
    pub fn new_view(&self, viewport: Size, dpr: f64) -> ViewState {
        ViewState::new(&self.pages, viewport, dpr)
    }

    // ---- lopdf side ------------------------------------------------------------------------

    /// The lopdf handle, created on first call. The object graph is still only parsed when the
    /// handle is asked for something that needs it. Not available for in-memory documents.
    pub fn pdf(&mut self) -> Result<&mut PdfFile> {
        // The handle is with the save while one is in flight, and the file is being replaced.
        self.refuse_while_saving()?;
        if self.pdf.is_none() {
            let Origin::File(path) = &self.origin else {
                return Err(DocError::NoFile);
            };
            let file = match &self.password {
                Some(pw) => PdfFile::open_with_password(path, pw)?,
                None => PdfFile::open(path)?,
            };
            self.pdf = Some(file);
        }
        Ok(self.pdf.as_mut().expect("just set"))
    }

    /// Whether the lopdf handle exists and has parsed the file.
    pub fn pdf_is_loaded(&self) -> bool {
        self.pdf.as_ref().is_some_and(PdfFile::is_loaded)
    }

    /// Parses the file with lopdf (now, on this thread), reads every page's `/UserUnit`,
    /// passes the values to the backend and updates the page geometries. The parsed document
    /// is dropped afterwards unless it was already loaded. Returns whether any page has a unit
    /// other than 1. Call it once lopdf has the file loaded for an edit, to settle the files
    /// [`UserUnitPolicy::Scan`] could not read.
    pub fn refresh_user_units(&mut self) -> Result<bool> {
        let units: Vec<f64> = match &self.origin {
            Origin::File(_) => {
                let was_loaded = self.pdf_is_loaded();
                let file = self.pdf()?;
                let units = file
                    .document()
                    .and_then(|d| d.pages())
                    .map(|pages| pages.iter().map(|p| p.user_unit).collect());
                if !was_loaded {
                    file.unload();
                }
                units?
            }
            Origin::Memory { bytes, .. } => {
                PdfDocument::from_bytes(bytes.to_vec(), self.password.as_deref())?
                    .pages()?
                    .iter()
                    .map(|p| p.user_unit)
                    .collect()
            }
        };
        self.apply_user_units(&units, true)?;
        Ok(self.pages.iter().any(|p| p.user_unit() != 1.0))
    }

    /// Sets the pages' units. `tell_backend` also records them in the backend, which waits for
    /// it; the backend needs them for nothing but its own [`PageInfo`] replies (tiles are laid
    /// out in user-space units), so the non-waiting path leaves it out.
    pub(crate) fn apply_user_units(&mut self, units: &[f64], tell_backend: bool) -> Result<bool> {
        if units.len() != self.pages.len() {
            self.warnings.push(format!(
                "the page tree read for /UserUnit has {} pages where the renderer sees {}; \
                 /UserUnit ignored",
                units.len(),
                self.pages.len()
            ));
            return Ok(false);
        }
        let mut changed = false;
        for (i, unit) in units.iter().enumerate() {
            if !unit.is_finite() || *unit <= 0.0 || *unit == self.pages[i].user_unit() {
                continue;
            }
            if tell_backend && !self.suspended {
                self.backend
                    .set_page_user_unit(self.render, i as u32, *unit)?;
            }
            let mut info = PageInfo::from_geometry(&self.pages[i]);
            info.user_unit = *unit;
            self.pages[i] = info.geometry();
            changed = true;
        }
        if changed {
            self.geometry_epoch += 1;
        }
        Ok(changed)
    }

    /// Picks up finished background work: at present the `/UserUnit` probe that an
    /// [`OpenMode::Interactive`] open leaves running. Never waits. Returns whether the page
    /// layout changed (see [`geometry_epoch`](Document::geometry_epoch)). A user interface
    /// calls this once per frame.
    pub fn poll_background(&mut self) -> bool {
        let Some(probe) = &self.unit_probe else {
            return false;
        };
        let outcome = match probe.rx.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the worker stopped without a result".into()),
        };
        self.unit_probe = None;
        self.take_probe_outcome(outcome, false)
    }

    /// Whether background work started at open is still running.
    pub fn background_pending(&self) -> bool {
        self.unit_probe.is_some()
    }

    /// Waits for the background work [`poll_background`](Document::poll_background) would pick
    /// up, for at most `timeout`. Returns whether it has all been applied.
    pub fn wait_background(&mut self, timeout: Duration) -> bool {
        let Some(probe) = self.unit_probe.take() else {
            return true;
        };
        match probe.rx.recv_timeout(timeout) {
            Ok(outcome) => {
                self.take_probe_outcome(outcome, false);
                true
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.unit_probe = Some(probe);
                false
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.take_probe_outcome(Err("the worker stopped without a result".into()), false);
                true
            }
        }
    }

    fn take_probe_outcome(&mut self, outcome: ProbeOutcome, tell_backend: bool) -> bool {
        match outcome {
            Ok(Some(units)) => match self.apply_user_units(&units, tell_backend) {
                Ok(changed) => changed,
                Err(e) => {
                    self.warnings
                        .push(format!("/UserUnit could not be applied: {e}"));
                    false
                }
            },
            // Not knowable without lopdf; stays 1.0 until `refresh_user_units`.
            Ok(None) => false,
            Err(e) => {
                self.warnings
                    .push(format!("/UserUnit could not be read: {e}"));
                false
            }
        }
    }

    // ---- render mirror ---------------------------------------------------------------------

    /// How many background workers of this document still map its file (scene builds that
    /// have not exited yet, and the `/UserUnit` probe). While this is not zero the file cannot
    /// be replaced.
    pub fn file_users(&mut self) -> usize {
        self.scene_workers.retain(|w| !w.is_finished());
        let probe = self
            .unit_probe
            .as_ref()
            .is_some_and(|p| !p.released.load(Ordering::Acquire));
        self.scene_workers.len() + usize::from(probe)
    }

    /// Releases the file so it can be replaced (save): running scene builds are cancelled and
    /// waited for, then the render mirror is closed. Until
    /// [`reopen_render`](Document::reopen_render), backend calls for this document fail.
    ///
    /// A scene build stuck inside hayro cannot be interrupted, and it holds a mapping of the
    /// file until its thread exits. If one is still running after [`FILE_RELEASE_WAIT`] this
    /// returns [`DocError::FileBusy`] and leaves the mirror open: the save has to be deferred
    /// or written under another name, because Windows would refuse the replace.
    pub fn suspend_render(&mut self) -> Result<()> {
        self.suspend_render_within(FILE_RELEASE_WAIT)
    }

    /// As [`suspend_render`](Document::suspend_render) with a chosen wait.
    pub fn suspend_render_within(&mut self, wait: Duration) -> Result<()> {
        self.refuse_while_saving()?;
        if matches!(self.origin, Origin::File(_)) {
            // Dropping a running job cancels it; finished scenes hold nothing of the file.
            self.scenes
                .retain(|_, slot| !matches!(slot, SceneSlot::Building(_)));
            let scenes = &self.scenes;
            self.scene_order.retain(|page| scenes.contains_key(page));
            for worker in &self.scene_workers {
                worker.cancel();
            }
            let deadline = Instant::now() + wait;
            while self.file_users() > 0 {
                if Instant::now() >= deadline {
                    return Err(DocError::FileBusy {
                        path: self.path().map(Path::to_path_buf).unwrap_or_default(),
                        workers: self.file_users(),
                    });
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if !self.suspended {
            self.backend.close(self.render)?;
            self.suspended = true;
        }
        Ok(())
    }

    /// Opens the render mirror again from the file (after a save), or rebuilds it with
    /// `appended` presented after the file's bytes (the in-memory layer override). Page sizes
    /// are re-read; a page keeps its real geometry if its size is unchanged. Cached text and
    /// scenes are dropped and every page's revision goes up. The annotations the store has
    /// hidden ([`Document::hidden_annotations`]) are hidden again in the new mirror.
    ///
    /// Like the open, this loads no page (except in [`OpenMode::Complete`]).
    pub fn reopen_render(&mut self, appended: Vec<u8>) -> Result<()> {
        self.refuse_while_saving()?;
        let new = open_mirror(
            self.backend.as_ref(),
            &self.origin,
            self.password.as_deref(),
            appended,
        )?;
        if !self.suspended {
            let _ = self.backend.close(self.render);
        }
        let doc = new.doc;
        self.adopt_mirror(new);
        for (i, page) in self.pages.iter().enumerate() {
            if page.user_unit() != 1.0 {
                self.backend
                    .set_page_user_unit(doc, i as u32, page.user_unit())?;
            }
        }
        self.reapply_hidden();
        if self.open_mode == OpenMode::Complete {
            self.load_all_geometry();
        }
        Ok(())
    }

    /// Makes `new` the document's render mirror: the page list is rebuilt from its sizes (a
    /// page keeps its real geometry if its size is unchanged), cached text and scenes are
    /// dropped and every page's revision goes up. The mirror the document had is not closed
    /// here, and no backend call is made.
    pub(crate) fn adopt_mirror(&mut self, new: NewMirror) {
        self.render = new.doc;
        self.suspended = false;
        let old = PageSet {
            pages: std::mem::take(&mut self.pages),
            exact: std::mem::take(&mut self.exact),
        };
        let fresh = layout_from_sizes(&new.sizes, Some(&old), &mut self.warnings);
        self.pages = fresh.pages;
        self.exact = fresh.exact;
        self.unreadable = fresh.unreadable;
        self.geometry_epoch += 1;
        self.texts.clear();
        self.text_order.clear();
        self.scenes.clear();
        self.scene_order.clear();
        self.bookmarks = None;
        self.revision += 1;
        let revision = self.revision;
        self.revisions.clear();
        self.revisions.resize(self.pages.len(), revision);
        for (i, info) in new.infos.into_iter().flatten().enumerate() {
            if i >= self.pages.len() || self.unreadable[i] {
                continue;
            }
            match info {
                Ok(info) => {
                    self.set_exact(i, info);
                }
                Err(why) if !self.exact[i] => self.mark_unreadable(i, &why),
                Err(_) => {}
            }
        }
    }

    /// Tells the scene builds that are still running to stop and hands back what a save has
    /// to wait for before the file can be replaced: their workers and the `/UserUnit` probe.
    /// Never waits. Finished scenes are kept; they hold nothing of the file.
    pub(crate) fn cancel_file_users(&mut self) -> crate::background::FileUsers {
        if !matches!(self.origin, Origin::File(_)) {
            return crate::background::FileUsers::default();
        }
        // Dropping a running job cancels it.
        self.scenes
            .retain(|_, slot| !matches!(slot, SceneSlot::Building(_)));
        let scenes = &self.scenes;
        self.scene_order.retain(|page| scenes.contains_key(page));
        for worker in &self.scene_workers {
            worker.cancel();
        }
        self.scene_workers.retain(|w| !w.is_finished());
        crate::background::FileUsers {
            scenes: self.scene_workers.clone(),
            probe_released: self.unit_probe.as_ref().map(|p| p.released.clone()),
        }
    }

    pub(crate) fn unreadable_pages(&self) -> &[bool] {
        &self.unreadable
    }

    /// Whether this document settles every page's geometry whenever its mirror is opened
    /// ([`OpenMode::Complete`]).
    pub(crate) fn settles_at_open(&self) -> bool {
        self.open_mode == OpenMode::Complete
    }

    /// Records that one page's rendered content changed: bumps that page's revision (and the
    /// document counter) and drops the page's cached text and scene. Returns the page's new
    /// revision; the owner of the tile cache then calls
    /// `TileCache::remove_stale(render_id, page, new_revision)` so the old tiles give their
    /// memory back. No other page is affected.
    ///
    /// For a change confined to a rectangle (one annotation hidden or moved) prefer
    /// `TileCache::remove_covering` and leave the revision alone, so the page's other tiles
    /// stay cached too; call [`drop_page_caches`](Document::drop_page_caches) for the text.
    pub fn invalidate_page(&mut self, page: u32) -> u64 {
        self.revision += 1;
        self.drop_page_caches(page);
        match self.revisions.get_mut(page as usize) {
            Some(r) => {
                *r = self.revision;
                *r
            }
            None => 0,
        }
    }

    /// Drops a page's cached text and scene without touching its revision.
    pub fn drop_page_caches(&mut self, page: u32) {
        self.texts.remove(&page);
        self.text_order.retain(|p| *p != page);
        self.scenes.remove(&page);
        self.scene_order.retain(|p| *p != page);
    }

    // ---- save ------------------------------------------------------------------------------

    /// Saves `edits` into the file as an incremental update, with PDFium as the third
    /// verifier.
    ///
    /// pl-pdf writes `name.pdf.tmp` and verifies it with lopdf and hayro-syntax. Before the
    /// file is replaced, this opens the temporary file in the render backend and checks that
    ///
    /// * PDFium sees the same number of pages,
    /// * on every touched page (and a few others) PDFium counts the annotations pl-pdf
    ///   expects, and
    /// * a sample of untouched pages renders exactly as it did before the save.
    ///
    /// Any failure leaves the original file as it was. `also_touched` names pages whose
    /// rendering the edits change without adding or removing an annotation (a patched
    /// appearance, say); pages that gain or lose annotations are found from the edit set.
    ///
    /// The mirror is closed for the replace and reopened afterwards, so cached text and
    /// scenes are dropped and page revisions go up.
    ///
    /// This writes the caller's own edit set and knows nothing of the annotation store. If
    /// the store has unsaved changes the call is refused with [`DocError::UnsavedMarkups`]
    /// (save them with [`Document::save`] first); otherwise the store is read again from the
    /// new file afterwards.
    pub fn save_incremental(
        &mut self,
        edits: &EditSet,
        also_touched: &[u32],
        options: &SaveOptions,
    ) -> Result<SaveOutcome> {
        self.check_raw_save()?;
        let saved = self.save_blocking(
            crate::background::SaveTarget::InPlace,
            options,
            Some((edits.clone(), also_touched.to_vec())),
        )?;
        // An edit set is always written, so there is an outcome.
        saved.outcome.ok_or(DocError::NoFile)
    }

    // ---- text, search, bookmarks -----------------------------------------------------------

    /// A page's characters if they are cached. Never waits.
    pub fn cached_text(&self, page: u32) -> Option<Arc<PageText>> {
        self.texts.get(&page).cloned()
    }

    /// Stores a page's text from a finished `JobKind::PageText` job. An answer for a mirror
    /// that has since been reopened is ignored. Returns whether it was stored.
    pub fn accept_text(&mut self, render: DocId, page: u32, text: Arc<PageText>) -> bool {
        if render != self.render || page >= self.page_count() {
            return false;
        }
        self.store_text(page, text);
        true
    }

    fn store_text(&mut self, page: u32, text: Arc<PageText>) {
        if self.texts.insert(page, text).is_none() {
            if self.text_order.len() >= TEXT_CACHE_PAGES {
                if let Some(old) = self.text_order.pop_front() {
                    self.texts.remove(&old);
                }
            }
            self.text_order.push_back(page);
        }
    }

    /// A page's characters in reading order, with boxes in page space. Cached. On a miss this
    /// waits for the backend.
    pub fn page_text(&mut self, page: u32) -> Result<Arc<PageText>> {
        self.check_page(page)?;
        if let Some(text) = self.texts.get(&page) {
            return Ok(text.clone());
        }
        let text = Arc::new(self.backend.page_text(self.render, page)?);
        self.store_text(page, text.clone());
        Ok(text)
    }

    /// The text selected by a drag from `a` to `b` on one page (page space): the characters in
    /// reading order from the one nearest `a` to the one nearest `b`, with their quads and
    /// string. `None` when the page has no text. Waits for the backend if the text is not
    /// cached; a user interface uses [`cached_text`](Document::cached_text) with
    /// [`select_between`] instead.
    pub fn select_text(&mut self, page: u32, a: Point, b: Point) -> Result<Option<TextSelection>> {
        let text = self.page_text(page)?;
        Ok(select_between(&text, a, b))
    }

    /// The text a drag from `a` to `b` covers on one page, as the geometry of a text markup
    /// (see [`crate::markup_between`]): give the result's
    /// [`annotation`](crate::MarkupSelection::annotation) to [`Document::apply`]. `None` when
    /// the page has no text. Waits for the backend if the text is not cached; a user
    /// interface uses [`cached_text`](Document::cached_text) with [`crate::markup_between`].
    pub fn select_markup(
        &mut self,
        page: u32,
        a: Point,
        b: Point,
    ) -> Result<Option<crate::selection::MarkupSelection>> {
        let text = self.page_text(page)?;
        Ok(crate::selection::markup_between(&text, a, b))
    }

    /// A search that has not started. Drive it with [`step_search`](Document::step_search)
    /// (which waits for the backend), or feed it page text as it arrives from the scheduler
    /// with [`DocSearch::feed`] (which does not).
    pub fn start_search(&self, query: &str, options: SearchOptions) -> DocSearch {
        DocSearch::new(query, options, self.page_count())
    }

    /// Searches up to `max_pages` more pages. Returns `true` when the search is complete.
    /// Waits for the backend once per page.
    pub fn step_search(&self, search: &mut DocSearch, max_pages: u32) -> Result<bool> {
        Ok(search.step(self.backend.as_ref(), self.render, max_pages)?)
    }

    /// Searches the whole document now. Waits for the backend once per page.
    pub fn search(&self, query: &str, options: SearchOptions) -> Result<DocSearch> {
        let mut search = self.start_search(query, options);
        search.run(self.backend.as_ref(), self.render)?;
        Ok(search)
    }

    /// The bookmark tree if it has been fetched. Never waits.
    pub fn cached_bookmarks(&self) -> Option<Arc<BookmarkTree>> {
        self.bookmarks.clone()
    }

    /// Stores the bookmark tree from a finished `JobKind::Outline` job. An answer for a mirror
    /// that has since been reopened is ignored.
    pub fn accept_outline(&mut self, render: DocId, items: &[OutlineItem]) -> bool {
        if render != self.render {
            return false;
        }
        self.bookmarks = Some(Arc::new(BookmarkTree::from_outline(items)));
        true
    }

    /// The bookmark tree. Cached. On a miss this waits for the backend.
    pub fn bookmarks(&mut self) -> Result<Arc<BookmarkTree>> {
        if let Some(tree) = &self.bookmarks {
            return Ok(tree.clone());
        }
        let items = self.backend.outline(self.render)?;
        let tree = Arc::new(BookmarkTree::from_outline(&items));
        self.bookmarks = Some(tree.clone());
        Ok(tree)
    }

    /// The annotations PDFium sees on a page. Waits for the backend.
    pub fn annotations(&self, page: u32) -> Result<Vec<AnnotInfo>> {
        self.check_page(page)?;
        Ok(self.backend.annotations(self.render, page)?)
    }

    // ---- content-snap scenes ---------------------------------------------------------------

    /// The page's content-snap scene. The first call starts a build on a worker thread and
    /// returns `Pending`; later calls return the result once it is there. Never blocks.
    pub fn scene(&mut self, page: u32) -> SceneState {
        if page >= self.page_count() {
            return SceneState::Unavailable(NoSnapReason::PageOutOfRange);
        }
        if self.is_saving() && !self.scenes.contains_key(&page) {
            // A build would map the file the save is about to replace: ask again afterwards.
            return SceneState::Pending;
        }
        if !self.scenes.contains_key(&page) {
            let slot = match self.scene_source() {
                Ok(source) => {
                    let job = spawn_scene_build(source, page as usize, self.scene_limits);
                    // The worker maps the file until its thread exits, which a timed-out job
                    // does not wait for: keep its handle so a save can.
                    self.scene_workers.retain(|w| !w.is_finished());
                    self.scene_workers.push(job.worker());
                    SceneSlot::Building(job)
                }
                Err(_) => SceneSlot::Unavailable(NoSnapReason::WorkerFailed),
            };
            self.scenes.insert(page, slot);
        }
        self.touch_scene(page);
        let slot = self.scenes.get_mut(&page).expect("present");
        if let SceneSlot::Building(job) = slot {
            match job.poll() {
                None => return SceneState::Pending,
                Some(build) => *slot = finished(build),
            }
        }
        let state = match slot {
            SceneSlot::Building(_) => SceneState::Pending,
            SceneSlot::Ready(scene) => SceneState::Ready(scene.clone()),
            SceneSlot::Unavailable(reason) => SceneState::Unavailable(*reason),
        };
        self.trim_scenes(page);
        state
    }

    /// As [`scene`](Document::scene), but waits for a running build (at most the time cap plus
    /// a short grace period). For tests and the command line; a UI should poll instead.
    pub fn scene_wait(&mut self, page: u32) -> SceneState {
        if let SceneState::Pending = self.scene(page) {
            if let Some(SceneSlot::Building(job)) = self.scenes.remove(&page) {
                self.scenes.insert(page, finished(job.wait()));
            }
        }
        self.scene(page)
    }

    /// Changes the caps for scene builds started from now on.
    pub fn set_scene_limits(&mut self, limits: SceneLimits) {
        self.scene_limits = limits;
    }

    fn scene_source(&self) -> Result<SceneSource> {
        match &self.origin {
            Origin::Memory { bytes, .. } => Ok(SceneSource::new(bytes.clone())),
            Origin::File(path) => Ok(SceneSource::new(Arc::new(map_file(path)?))),
        }
    }

    fn touch_scene(&mut self, page: u32) {
        self.scene_order.retain(|p| *p != page);
        self.scene_order.push_back(page);
    }

    /// Drops the least recently used finished scenes beyond the cache size, never `keep`.
    fn trim_scenes(&mut self, keep: u32) {
        let mut ready = self
            .scenes
            .values()
            .filter(|s| matches!(s, SceneSlot::Ready(_)))
            .count();
        let mut i = 0;
        while ready > SCENE_CACHE_PAGES && i < self.scene_order.len() {
            let page = self.scene_order[i];
            if page != keep && matches!(self.scenes.get(&page), Some(SceneSlot::Ready(_))) {
                self.scenes.remove(&page);
                self.scene_order.remove(i);
                ready -= 1;
            } else {
                i += 1;
            }
        }
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        // A save in flight is waited for: it owns the display until it is done, and a file
        // half-way through "verify, then replace" is better finished than abandoned.
        // Its result is taken in like any other, so the journal of saved work goes with it.
        self.settle_save();
        if !self.suspended {
            let _ = self.backend.close(self.render);
        }
    }
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("id", &self.id)
            .field("render", &self.render)
            .field("title", &self.title())
            .field("pages", &self.pages.len())
            .field("revision", &self.revision)
            .finish()
    }
}

fn finished(build: pl_scene::SceneBuild) -> SceneSlot {
    match build {
        pl_scene::SceneBuild::Ready(scene) => SceneSlot::Ready(Arc::new(scene)),
        pl_scene::SceneBuild::NoContentSnap(reason) => SceneSlot::Unavailable(reason),
    }
}

/// Maps a file read-only. The map keeps the file open, so it must not outlive the work that
/// needs it: scene builds hold one only while their worker thread runs (see
/// [`Document::file_users`]), and the `/UserUnit` probe drops its own as soon as it has read
/// the page tree.
fn map_file(path: &Path) -> Result<memmap2::Mmap> {
    let file = std::fs::File::open(path).map_err(|e| DocError::io("open", path, e))?;
    // SAFETY: the map is read-only and private to this process. Like every reader of a file
    // another program might rewrite, it relies on the file not being truncated while mapped;
    // on Windows a mapped file cannot be truncated or replaced at all.
    unsafe { memmap2::Mmap::map(&file) }.map_err(|e| DocError::io("map", path, e))
}

/// Finds every page's `/UserUnit` without the render backend. See [`UserUnitPolicy::Scan`].
/// `released` is set as soon as this no longer maps the file.
fn probe_user_units(
    origin: &Origin,
    password: Option<&str>,
    released: &AtomicBool,
) -> ProbeOutcome {
    let hayro_failed_but_key_present = match origin {
        Origin::Memory { bytes, .. } => {
            released.store(true, Ordering::Release);
            match pl_pdf::probe_user_units(bytes.clone(), password) {
                Ok(units) => return Ok(Some(units)),
                Err(_) => pl_pdf::mentions_user_unit(bytes),
            }
        }
        Origin::File(path) => {
            let map = Arc::new(map_file(path).map_err(|e| e.to_string())?);
            let probed = pl_pdf::probe_user_units(map.clone(), password);
            let mentioned = probed.is_err() && pl_pdf::mentions_user_unit(&map[..]);
            drop(map);
            released.store(true, Ordering::Release);
            match probed {
                Ok(units) => return Ok(Some(units)),
                Err(_) => mentioned,
            }
        }
    };
    if !hayro_failed_but_key_present {
        return Ok(None);
    }
    // hayro-syntax cannot read this file but the key is there in the clear: the one case
    // that is worth a lopdf parse.
    let pages = match origin {
        Origin::Memory { bytes, .. } => {
            PdfDocument::from_bytes(bytes.to_vec(), password).and_then(|d| d.pages())
        }
        Origin::File(path) => match password {
            Some(pw) => PdfFile::open_with_password(path, pw),
            None => PdfFile::open(path),
        }
        .and_then(|mut file| file.document().and_then(|d| d.pages())),
    };
    pages
        .map(|pages| Some(pages.iter().map(|p| p.user_unit).collect()))
        .map_err(|e| e.to_string())
}

/// Up to `n` items spread over `items`: the first, the last and evenly between.
pub(crate) fn sample(items: &[u32], n: usize) -> Vec<u32> {
    if items.len() <= n {
        return items.to_vec();
    }
    let mut out: Vec<u32> = (0..n)
        .map(|k| items[k * (items.len() - 1) / (n - 1).max(1)])
        .collect();
    out.dedup();
    out
}

pub(crate) fn bitmap_hash(bitmap: &Bitmap) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&bitmap.width.to_le_bytes());
    eat(&bitmap.height.to_le_bytes());
    eat(&bitmap.pixels);
    h
}

/// The PDFium half of save verification, run on the temporary file before it replaces the
/// original. The temporary document is closed again whatever the outcome: its mapping would
/// otherwise make the replace fail.
pub(crate) fn pdfium_check(
    backend: &dyn RenderBackend,
    tmp: &Path,
    password: Option<&str>,
    expected: &Expected,
    count_pages: &[u32],
    renders_before: &[(u32, u64)],
) -> std::result::Result<(), VerifyError> {
    const PARSER: &str = "PDFium";
    let reparse = |e: pl_render::RenderError| VerifyError::Reparse {
        parser: PARSER,
        message: e.to_string(),
    };
    let info = backend
        .open(DocSource::file(tmp), password)
        .map_err(reparse)?;
    let result = (|| {
        if info.page_count as usize != expected.page_count {
            return Err(VerifyError::PageCount {
                parser: PARSER,
                expected: expected.page_count,
                found: info.page_count as usize,
            });
        }
        for page in count_pages {
            let want = expected.annotation_counts[*page as usize];
            let found = backend.annotations(info.doc, *page).map_err(reparse)?.len();
            if found != want {
                return Err(VerifyError::AnnotationCount {
                    parser: PARSER,
                    page_index: *page as usize,
                    expected: want,
                    found,
                });
            }
        }
        for (page, before) in renders_before {
            let after = backend
                .thumbnail(info.doc, *page, SAVE_RENDER_PX)
                .map_err(reparse)?;
            if bitmap_hash(&after) != *before {
                return Err(VerifyError::RenderChanged {
                    parser: PARSER,
                    page_index: *page as usize,
                });
            }
        }
        Ok(())
    })();
    let closed = backend.close(info.doc).map_err(reparse);
    result.and(closed)
}

struct PageSet {
    pages: Vec<PageGeometry>,
    exact: Vec<bool>,
}

struct Layout {
    pages: Vec<PageGeometry>,
    exact: Vec<bool>,
    unreadable: Vec<bool>,
}

/// Builds the page list from the sizes the backend reports. A page whose size cannot be read
/// gets the size of the page before it (US Letter if it is the first) and a warning; the
/// document stays open. With `old` (a reopen), a page whose size is unchanged keeps its real
/// geometry and every page keeps its `/UserUnit`.
fn layout_from_sizes(
    sizes: &[Option<[f64; 2]>],
    old: Option<&PageSet>,
    warnings: &mut Vec<String>,
) -> Layout {
    let old = old.filter(|o| o.pages.len() == sizes.len());
    let mut out = Layout {
        pages: Vec::with_capacity(sizes.len()),
        exact: Vec::with_capacity(sizes.len()),
        unreadable: Vec::with_capacity(sizes.len()),
    };
    let mut previous = PLACEHOLDER_PAGE;
    for (i, size) in sizes.iter().enumerate() {
        let known = old.map(|o| (&o.pages[i], o.exact[i]));
        match size {
            Some(size) => {
                previous = *size;
                let same_size = |g: &PageGeometry| {
                    let d = g.display_size();
                    (d.width - size[0]).abs() < 1e-3 && (d.height - size[1]).abs() < 1e-3
                };
                match known {
                    Some((geometry, true)) if same_size(geometry) => {
                        out.pages.push(*geometry);
                        out.exact.push(true);
                    }
                    _ => {
                        let mut info = PageInfo::provisional(*size);
                        if let Some((geometry, _)) = known {
                            info.user_unit = geometry.user_unit();
                        }
                        out.pages.push(info.geometry());
                        out.exact.push(false);
                    }
                }
                out.unreadable.push(false);
            }
            None => {
                let message = format!(
                    "page {} could not be read; it is shown as a blank {} x {} placeholder",
                    i + 1,
                    previous[0],
                    previous[1]
                );
                if !warnings.contains(&message) {
                    warnings.push(message);
                }
                out.pages.push(PageInfo::provisional(previous).geometry());
                out.exact.push(false);
                out.unreadable.push(true);
            }
        }
    }
    out
}

/// The open documents and the render backend they share.
pub struct Session {
    backend: Arc<dyn RenderBackend>,
    options: SessionOptions,
    docs: Vec<Document>,
    next_id: u32,
    journal_dir: Option<PathBuf>,
}

impl Session {
    /// A session with the default options: [`OpenMode::Interactive`].
    pub fn new(backend: Arc<dyn RenderBackend>) -> Session {
        Session::with_options(backend, SessionOptions::default())
    }

    pub fn with_options(backend: Arc<dyn RenderBackend>, options: SessionOptions) -> Session {
        Session {
            backend,
            options,
            docs: Vec::new(),
            next_id: 1,
            journal_dir: None,
        }
    }

    /// Where documents opened from now on keep their crash journals (see [`crate::journal`]).
    /// `None`, the default, switches journaling off. The application passes
    /// [`crate::default_journal_dir`]; tests pass a temporary directory or nothing, so the
    /// real one is never touched by them.
    pub fn set_journal_dir(&mut self, dir: Option<PathBuf>) {
        self.journal_dir = dir;
    }

    pub fn journal_dir(&self) -> Option<&Path> {
        self.journal_dir.as_deref()
    }

    pub fn backend(&self) -> &Arc<dyn RenderBackend> {
        &self.backend
    }

    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// Opens a file. If the same file is already open in this session, its id is returned and
    /// nothing is opened twice. `password` of `None` tries the empty user password.
    ///
    /// In [`OpenMode::Interactive`] the cost is the backend's open plus one request for the
    /// page sizes: no page is loaded, nothing is parsed with lopdf and the file is not read
    /// into memory, whatever its size.
    pub fn open(&mut self, path: impl AsRef<Path>, password: Option<&str>) -> Result<DocumentId> {
        let path = path.as_ref();
        let path = std::path::absolute(path).map_err(|e| DocError::io("resolve", path, e))?;
        if let Some(id) = self.find_by_path(&path) {
            return Ok(id);
        }
        if !path.is_file() {
            let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
            return Err(DocError::io("open", &path, missing));
        }
        self.open_origin(Origin::File(path), password)
    }

    /// Opens a document held in memory. `name` is what [`Document::title`] reports.
    pub fn open_bytes(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        password: Option<&str>,
    ) -> Result<DocumentId> {
        self.open_origin(
            Origin::Memory {
                name: name.to_owned(),
                bytes: Arc::new(bytes),
            },
            password,
        )
    }

    fn open_origin(&mut self, origin: Origin, password: Option<&str>) -> Result<DocumentId> {
        let source = match &origin {
            Origin::File(path) => DocSource::file(path.clone()),
            Origin::Memory { bytes, .. } => DocSource::Bytes(bytes.to_vec()),
        };
        let info = self.backend.open(source, password)?;
        let sizes = match self.backend.page_sizes(info.doc) {
            Ok(sizes) if sizes.len() == info.page_count as usize => sizes,
            Ok(_) => {
                let _ = self.backend.close(info.doc);
                return Err(pl_render::RenderError::UnexpectedReply.into());
            }
            Err(e) => {
                let _ = self.backend.close(info.doc);
                return Err(e.into());
            }
        };
        let mut warnings = Vec::new();
        let layout = layout_from_sizes(&sizes, None, &mut warnings);
        let id = DocumentId(self.next_id);
        self.next_id += 1;
        let page_count = layout.pages.len();
        // From here the document closes itself in the backend if it is dropped.
        let mut doc = Document {
            id,
            backend: self.backend.clone(),
            render: info.doc,
            suspended: false,
            origin,
            password: password.map(str::to_owned),
            open_mode: self.options.open,
            pages: layout.pages,
            exact: layout.exact,
            unreadable: layout.unreadable,
            geometry_epoch: 0,
            pdf: None,
            texts: HashMap::new(),
            text_order: VecDeque::new(),
            bookmarks: None,
            scenes: HashMap::new(),
            scene_order: VecDeque::new(),
            scene_limits: self.options.scene_limits,
            scene_workers: Vec::new(),
            unit_probe: None,
            revisions: vec![0; page_count],
            revision: 0,
            warnings,
            edit: EditSlot::new(self.journal_dir.clone()),
            layers: None,
            flight: None,
            next_ticket: 0,
        };
        match (self.options.user_unit, self.options.open) {
            (UserUnitPolicy::Never, _) => {}
            (UserUnitPolicy::Always, _) => {
                // The renderer has the file open and showing; a second parser that cannot read
                // it must not turn that into a failed open.
                if let Err(e) = doc.refresh_user_units() {
                    doc.warnings
                        .push(format!("/UserUnit could not be read: {e}"));
                }
            }
            (UserUnitPolicy::Scan, OpenMode::Complete) => {
                let released = AtomicBool::new(false);
                let outcome = probe_user_units(&doc.origin, doc.password.as_deref(), &released);
                doc.take_probe_outcome(outcome, true);
            }
            (UserUnitPolicy::Scan, OpenMode::Interactive) => {
                let (tx, rx) = channel();
                let released = Arc::new(AtomicBool::new(false));
                let worker_released = released.clone();
                let origin = doc.origin.clone();
                let password = doc.password.clone();
                let spawned = std::thread::Builder::new()
                    .name("pl-doc-units".into())
                    .spawn(move || {
                        let outcome =
                            probe_user_units(&origin, password.as_deref(), &worker_released);
                        worker_released.store(true, Ordering::Release);
                        let _ = tx.send(outcome);
                    });
                match spawned {
                    Ok(_) => doc.unit_probe = Some(UnitProbe { rx, released }),
                    Err(e) => doc
                        .warnings
                        .push(format!("/UserUnit could not be read: {e}")),
                }
            }
        }
        if self.options.open == OpenMode::Complete {
            doc.load_all_geometry();
        }
        self.docs.push(doc);
        Ok(id)
    }

    /// Closes a document and releases its render mirror.
    pub fn close(&mut self, id: DocumentId) -> Result<()> {
        let i = self
            .docs
            .iter()
            .position(|d| d.id == id)
            .ok_or(DocError::UnknownDocument(id))?;
        let mut doc = self.docs.remove(i);
        // A save in flight is finished first (see `Drop`).
        doc.settle_save();
        doc.release_journal_if_unmodified();
        Ok(())
    }

    /// [`Document::save_as`] for a document of this session, refusing a destination that is
    /// the file of another open document: that document would lose its file (and its crash
    /// journal) underneath it.
    pub fn save_as(
        &mut self,
        id: DocumentId,
        dest: impl AsRef<Path>,
    ) -> Result<crate::editing::MarkupSave> {
        let dest = dest.as_ref();
        if let Some(other) = self.find_by_path(dest) {
            if other != id {
                let path = std::path::absolute(dest).unwrap_or_else(|_| dest.to_path_buf());
                return Err(DocError::OpenElsewhere { path });
            }
        }
        self.document_mut(id)?.save_as(dest)
    }

    /// [`Document::start_save_as`] for a document of this session, with the same refusal as
    /// [`save_as`](Session::save_as).
    pub fn start_save_as(
        &mut self,
        id: DocumentId,
        dest: impl AsRef<Path>,
    ) -> Result<crate::background::SaveTicket> {
        let dest = dest.as_ref();
        if let Some(other) = self.find_by_path(dest) {
            if other != id {
                let path = std::path::absolute(dest).unwrap_or_else(|_| dest.to_path_buf());
                return Err(DocError::OpenElsewhere { path });
            }
        }
        self.document_mut(id)?.start_save_as(dest)
    }

    pub fn document(&self, id: DocumentId) -> Result<&Document> {
        self.docs
            .iter()
            .find(|d| d.id == id)
            .ok_or(DocError::UnknownDocument(id))
    }

    pub fn document_mut(&mut self, id: DocumentId) -> Result<&mut Document> {
        self.docs
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or(DocError::UnknownDocument(id))
    }

    /// The open documents in the order they were opened.
    pub fn documents(&self) -> impl Iterator<Item = &Document> {
        self.docs.iter()
    }

    /// As [`documents`](Session::documents), mutably.
    pub fn documents_mut(&mut self) -> impl Iterator<Item = &mut Document> {
        self.docs.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// The open document for a file, compared without regard to case or slash direction. A
    /// file that a Save As in flight is writing belongs to the document being saved: two
    /// saves to one destination would write the same temporary file and leave two documents
    /// tied to one file and one journal.
    pub fn find_by_path(&self, path: &Path) -> Option<DocumentId> {
        let key = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        let wanted = key(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        self.docs
            .iter()
            .find(|d| {
                d.path().is_some_and(|p| key(p) == wanted)
                    || d.saving_to().is_some_and(|p| key(p) == wanted)
            })
            .map(|d| d.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::synthetic_text;
    use pl_geom::{Rect, Rotation};
    use pl_render::{MockBackend, OutlineItem, Request};

    fn session(pages: usize) -> (Arc<MockBackend>, Session) {
        let mock = Arc::new(MockBackend::with_pages(&vec![
            PageGeometry::simple(
                612.0, 792.0
            );
            pages
        ]));
        let session = Session::with_options(mock.clone(), SessionOptions::complete());
        (mock, session)
    }

    fn count(mock: &MockBackend, pred: impl Fn(&Request) -> bool) -> usize {
        mock.calls().iter().filter(|r| pred(r)).count()
    }

    #[test]
    fn open_reads_geometry_and_close_releases_the_mirror() {
        let (mock, mut s) = session(3);
        let a = s
            .open_bytes("a.pdf", b"%PDF-1.4 fake".to_vec(), None)
            .unwrap();
        let b = s
            .open_bytes("b.pdf", b"%PDF-1.4 fake".to_vec(), None)
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(s.len(), 2);
        let doc = s.document(a).unwrap();
        assert_eq!(doc.page_count(), 3);
        assert_eq!(doc.title(), "a.pdf");
        assert!(doc.path().is_none());
        assert_eq!(
            doc.page_geometry(2).unwrap(),
            &PageGeometry::simple(612.0, 792.0)
        );
        assert!(matches!(
            doc.page_geometry(3),
            Err(DocError::PageOutOfRange { page: 3, count: 3 })
        ));
        let render = doc.render_id();
        s.close(a).unwrap();
        assert!(mock.calls().contains(&Request::Close { doc: render }));
        assert!(matches!(s.close(a), Err(DocError::UnknownDocument(_))));
        assert!(s.document(a).is_err());
        assert_eq!(s.documents().map(Document::id).collect::<Vec<_>>(), [b]);
    }

    /// Pages that differ from their stand-in: rotated, cropped, with an offset origin.
    fn odd_pages() -> Vec<PageGeometry> {
        vec![
            PageGeometry::simple(612.0, 792.0),
            PageGeometry::new(
                Rect::new(100.0, 100.0, 400.0, 500.0),
                None,
                Rotation::R90,
                1.0,
            ),
            PageGeometry::new(
                Rect::new(0.0, 0.0, 600.0, 800.0),
                Some(Rect::new(100.0, 100.0, 300.0, 300.0)),
                Rotation::R180,
                1.0,
            ),
        ]
    }

    #[test]
    fn an_interactive_open_loads_no_page_and_asks_for_sizes_once() {
        let pages = odd_pages();
        let mock = Arc::new(MockBackend::with_pages(&pages));
        let mut s = Session::new(mock.clone());
        assert_eq!(s.options().open, OpenMode::Interactive);
        let id = s.open_bytes("a", b"%PDF-1.4 fake".to_vec(), None).unwrap();
        let doc = s.document_mut(id).unwrap();

        // The open: one Open, one PageSizes, and nothing that loads a page.
        let calls = mock.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(matches!(calls[0], Request::Open(_)));
        assert!(matches!(calls[1], Request::PageSizes { .. }));
        assert_eq!(mock.page_loads(doc.render_id()).unwrap(), 0);

        // Every page lays out at its real displayed size straight away...
        assert_eq!(doc.page_count(), 3);
        for (i, real) in pages.iter().enumerate() {
            assert_eq!(
                doc.pages()[i].display_size(),
                real.display_size(),
                "page {i}"
            );
            assert!(!doc.geometry_is_exact(i as u32));
            // ... but its page space is not offered until it is really known.
            assert!(matches!(
                doc.page_geometry(i as u32),
                Err(DocError::GeometryPending { .. })
            ));
        }
        assert_eq!(doc.geometry_epoch(), 0);

        // Geometry arriving from a scheduler job: stored without a backend call.
        let before = mock.calls().len();
        let render = doc.render_id();
        assert!(doc.accept_page_info(render, 1, PageInfo::from_geometry(&pages[1])));
        assert_eq!(doc.page_geometry(1).unwrap(), &pages[1]);
        assert_eq!(doc.geometry_epoch(), 1);
        // A plain page's stand-in already equals its geometry: exact now, layout unchanged.
        assert!(!doc.accept_page_info(render, 0, PageInfo::from_geometry(&pages[0])));
        assert!(doc.geometry_is_exact(0));
        assert_eq!(doc.geometry_epoch(), 1);
        // An answer for another mirror, or another page count, is ignored.
        assert!(!doc.accept_page_info(DocId(9999), 2, PageInfo::from_geometry(&pages[2])));
        assert!(!doc.accept_page_info(render, 7, PageInfo::from_geometry(&pages[2])));
        assert!(!doc.geometry_is_exact(2));
        assert_eq!(mock.calls().len(), before);

        // The waiting path loads exactly the page asked for, once.
        assert_eq!(doc.load_geometry(2).unwrap(), &pages[2]);
        doc.load_geometry(2).unwrap();
        doc.load_geometry(1).unwrap();
        assert_eq!(count(&mock, |r| matches!(r, Request::PageInfo { .. })), 1);
        assert!(doc.wait_background(Duration::from_secs(20)));
        assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    }

    #[test]
    fn dropping_the_session_closes_every_document() {
        let (mock, mut s) = session(1);
        s.open_bytes("a", Vec::new(), None).unwrap();
        s.open_bytes("b", Vec::new(), None).unwrap();
        drop(s);
        assert_eq!(count(&mock, |r| matches!(r, Request::Close { .. })), 2);
    }

    #[test]
    fn a_missing_file_is_an_error_and_nothing_is_opened() {
        let (mock, mut s) = session(1);
        let tmp = tempfile::tempdir().unwrap();
        let err = s.open(tmp.path().join("nope.pdf"), None).unwrap_err();
        assert!(matches!(err, DocError::Io { op: "open", .. }), "{err}");
        assert!(s.is_empty());
        assert!(mock.calls().is_empty());
    }

    #[test]
    fn the_same_file_is_opened_once() {
        let (mock, mut s) = session(2);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Plan.pdf");
        std::fs::write(&path, b"%PDF-1.4 fake").unwrap();
        let a = s.open(&path, None).unwrap();
        let upper = PathBuf::from(path.to_string_lossy().to_uppercase());
        let again = s.open(&upper, None).unwrap();
        assert_eq!(a, again);
        assert_eq!(s.len(), 1);
        assert_eq!(s.find_by_path(&path), Some(a));
        assert_eq!(count(&mock, |r| matches!(r, Request::Open(_))), 1);
        let doc = s.document(a).unwrap();
        assert_eq!(doc.title(), "Plan.pdf");
        assert!(doc.path().unwrap().is_absolute());
        // The fake file has no /UserUnit: lopdf was never asked.
        assert!(!doc.pdf_is_loaded());
        assert!(doc.warnings().is_empty());
    }

    #[test]
    fn page_text_is_fetched_once_and_selection_uses_it() {
        let (mock, mut s) = session(2);
        mock.set_page_text(synthetic_text(1, &[("Door D-12", [50.0, 500.0], 10.0)]));
        let id = s.open_bytes("a", Vec::new(), None).unwrap();
        let doc = s.document_mut(id).unwrap();
        assert!(doc.cached_text(1).is_none());
        let sel = doc
            .select_text(1, Point::new(76.0, 503.0), Point::new(93.0, 503.0))
            .unwrap()
            .unwrap();
        assert_eq!(sel.text, "D-12");
        assert_eq!(sel.page, 1);
        assert_eq!(doc.page_text(1).unwrap().text(), "Door D-12");
        assert_eq!(doc.cached_text(1).unwrap().text(), "Door D-12");
        assert!(doc
            .select_text(0, Point::ZERO, Point::ZERO)
            .unwrap()
            .is_none());
        assert!(doc.page_text(2).is_err());
        let fetches =
            |mock: &MockBackend| count(mock, |r| matches!(r, Request::PageText { page: 1, .. }));
        assert_eq!(fetches(&mock), 1);
        // Invalidating the page drops the cached text and bumps that page's revision only.
        let before = doc.revision();
        let new = doc.invalidate_page(1);
        assert_eq!(doc.revision(), before + 1);
        assert_eq!(doc.page_revision(1), new);
        assert_eq!(doc.page_revision(0), 0);
        assert!(doc.cached_text(1).is_none());
        doc.page_text(1).unwrap();
        assert_eq!(fetches(&mock), 2);
    }

    #[test]
    fn text_and_bookmarks_from_scheduler_jobs_are_cached_without_a_backend_call() {
        let (mock, mut s) = session(3);
        let id = s.open_bytes("a", Vec::new(), None).unwrap();
        let doc = s.document_mut(id).unwrap();
        let render = doc.render_id();
        let before = mock.calls().len();

        let text = Arc::new(synthetic_text(2, &[("south wall", [10.0, 100.0], 10.0)]));
        assert!(doc.accept_text(render, 2, text.clone()));
        assert!(Arc::ptr_eq(&doc.cached_text(2).unwrap(), &text));
        assert_eq!(doc.page_text(2).unwrap().text(), "south wall");
        // Stale or out-of-range answers are dropped.
        assert!(!doc.accept_text(DocId(424242), 0, text.clone()));
        assert!(!doc.accept_text(render, 9, text.clone()));
        assert!(doc.cached_text(0).is_none());

        assert!(doc.cached_bookmarks().is_none());
        let outline = [OutlineItem {
            title: "Plans".into(),
            level: 0,
            page: Some(0),
            x: None,
            y: None,
            zoom: None,
        }];
        assert!(!doc.accept_outline(DocId(424242), &outline));
        assert!(doc.cached_bookmarks().is_none());
        assert!(doc.accept_outline(render, &outline));
        assert_eq!(doc.cached_bookmarks().unwrap().len(), 1);
        assert_eq!(doc.bookmarks().unwrap().len(), 1);
        assert_eq!(mock.calls().len(), before, "nothing went to the backend");
    }

    #[test]
    fn the_text_cache_is_bounded() {
        let (_, mut s) = session(TEXT_CACHE_PAGES + 10);
        let id = s.open_bytes("a", Vec::new(), None).unwrap();
        let doc = s.document_mut(id).unwrap();
        for page in 0..doc.page_count() {
            doc.page_text(page).unwrap();
        }
        assert_eq!(doc.texts.len(), TEXT_CACHE_PAGES);
        assert!(!doc.texts.contains_key(&0));
        assert!(doc.texts.contains_key(&(TEXT_CACHE_PAGES as u32 + 9)));
        // Accepting the same page twice does not grow the bookkeeping.
        let render = doc.render_id();
        let text = Arc::new(synthetic_text(5, &[("x", [0.0, 0.0], 10.0)]));
        doc.accept_text(render, 5, text.clone());
        doc.accept_text(render, 5, text);
        assert_eq!(doc.texts.len(), TEXT_CACHE_PAGES);
        assert_eq!(doc.text_order.len(), TEXT_CACHE_PAGES);
    }

    #[test]
    fn search_and_bookmarks_go_through_the_backend() {
        let (mock, mut s) = session(3);
        mock.set_page_text(synthetic_text(0, &[("north wall", [10.0, 100.0], 10.0)]));
        mock.set_page_text(synthetic_text(2, &[("south wall", [10.0, 100.0], 10.0)]));
        mock.set_outline(vec![
            OutlineItem {
                title: "Plans".into(),
                level: 0,
                page: Some(0),
                x: None,
                y: None,
                zoom: None,
            },
            OutlineItem {
                title: "Level 1".into(),
                level: 1,
                page: Some(2),
                x: Some(0.0),
                y: Some(792.0),
                zoom: None,
            },
        ]);
        let id = s.open_bytes("a", Vec::new(), None).unwrap();
        let doc = s.document_mut(id).unwrap();
        let mut found = doc.search("wall", SearchOptions::default()).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found.next_hit().unwrap().page, 0);
        assert_eq!(found.next_hit().unwrap().page, 2);

        let mut stepped = doc.start_search("south", SearchOptions::default());
        assert!(!doc.step_search(&mut stepped, 2).unwrap());
        assert!(stepped.is_empty());
        assert!(doc.step_search(&mut stepped, 2).unwrap());
        assert_eq!(stepped.len(), 1);

        let tree = doc.bookmarks().unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree.roots[0].children[0].title, "Level 1");
        doc.bookmarks().unwrap();
        assert_eq!(
            count(&mock, |r| matches!(r, Request::Outline { .. })),
            1,
            "the tree is cached"
        );
    }

    #[test]
    fn invalidating_one_page_leaves_every_other_page_revision_alone() {
        let (_, mut s) = session(4);
        let id = s.open_bytes("a", Vec::new(), None).unwrap();
        let doc = s.document_mut(id).unwrap();
        assert_eq!(
            (0..4).map(|p| doc.page_revision(p)).collect::<Vec<_>>(),
            [0, 0, 0, 0]
        );
        let first = doc.invalidate_page(2);
        assert!(first > 0);
        assert_eq!(
            (0..4).map(|p| doc.page_revision(p)).collect::<Vec<_>>(),
            [0, 0, first, 0]
        );
        let second = doc.invalidate_page(2);
        let other = doc.invalidate_page(0);
        assert!(second > first && other > second);
        assert_eq!(
            (0..4).map(|p| doc.page_revision(p)).collect::<Vec<_>>(),
            [other, 0, second, 0]
        );
        // Out of range: nothing to bump, no panic.
        assert_eq!(doc.invalidate_page(99), 0);
        assert_eq!(doc.page_revision(99), 0);
        // Dropping caches only does not move a revision.
        doc.drop_page_caches(1);
        assert_eq!(doc.page_revision(1), 0);
    }

    #[test]
    fn suspend_and_reopen_swap_the_mirror_and_bump_the_revision() {
        let (mock, mut s) = session(2);
        let id = s.open_bytes("a", b"abc".to_vec(), None).unwrap();
        let doc = s.document_mut(id).unwrap();
        let first = doc.render_id();
        doc.suspend_render().unwrap();
        doc.suspend_render().unwrap();
        assert!(doc.annotations(0).is_err(), "the mirror is closed");
        let epoch = doc.geometry_epoch();
        doc.reopen_render(b"tail".to_vec()).unwrap();
        assert_ne!(doc.render_id(), first);
        assert_eq!(doc.revision(), 1);
        assert_eq!(doc.page_revision(0), 1);
        assert_eq!(doc.page_revision(1), 1);
        assert!(doc.geometry_epoch() > epoch);
        assert_eq!(doc.page_count(), 2);
        assert!(doc.annotations(0).is_ok());
        // Pages whose size did not change keep their geometry: nothing is loaded again.
        assert!(doc.geometry_is_exact(0) && doc.geometry_is_exact(1));
        // The appended bytes follow the original ones.
        let last_open = mock
            .calls()
            .iter()
            .rev()
            .find_map(|r| match r {
                Request::Open(o) => Some(o.source.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(last_open, DocSource::Bytes(b"abctail".to_vec()));
        // Reopening without suspending closes the old mirror.
        let second = doc.render_id();
        doc.reopen_render(Vec::new()).unwrap();
        assert!(mock.calls().contains(&Request::Close { doc: second }));
        assert_eq!(count(&mock, |r| matches!(r, Request::Close { .. })), 2);
        // Two opens' worth of geometry reads at most: the reopens asked for sizes only.
        assert_eq!(count(&mock, |r| matches!(r, Request::PageInfo { .. })), 2);
        assert_eq!(count(&mock, |r| matches!(r, Request::PageSizes { .. })), 3);
    }

    #[test]
    fn scenes_degrade_without_content_for_bytes_that_are_not_a_pdf() {
        let (_, mut s) = session(1);
        let id = s
            .open_bytes("a", b"not a pdf at all".to_vec(), None)
            .unwrap();
        let doc = s.document_mut(id).unwrap();
        assert!(matches!(
            doc.scene(5),
            SceneState::Unavailable(NoSnapReason::PageOutOfRange)
        ));
        let state = doc.scene_wait(0);
        assert!(
            matches!(state, SceneState::Unavailable(_)),
            "expected a typed failure, got {state:?}"
        );
        assert!(state.scene().is_none());
        // A view comes straight from the page list.
        let view = doc.new_view(Size::new(800.0, 600.0), 1.0);
        assert_eq!(view.page_count(), 1);
    }

    #[test]
    fn page_lists_from_sizes_use_placeholders_and_keep_what_is_known() {
        let mut warnings = Vec::new();
        let sizes = [None, Some([200.0, 300.0]), None, Some([400.0, 100.0])];
        let fresh = layout_from_sizes(&sizes, None, &mut warnings);
        // No earlier page: US Letter. Later: the page before it.
        assert_eq!(fresh.pages[0].display_size(), Size::new(612.0, 792.0));
        assert_eq!(fresh.pages[2].display_size(), Size::new(200.0, 300.0));
        assert_eq!(fresh.unreadable, [true, false, true, false]);
        assert_eq!(fresh.exact, [false; 4]);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("page 1") && warnings[1].contains("page 3"));

        // A reopen: page 2 was exact and has the same size (rotated box 300 x 200 shown as
        // 200 x 300), so it stays; page 4 changed size and goes back to a stand-in, keeping
        // its user unit.
        let rotated = PageGeometry::new(
            Rect::new(10.0, 10.0, 310.0, 210.0),
            None,
            Rotation::R90,
            1.0,
        );
        let scaled = PageGeometry::new(Rect::new(0.0, 0.0, 50.0, 50.0), None, Rotation::R0, 2.0);
        let old = PageSet {
            pages: vec![fresh.pages[0], rotated, fresh.pages[2], scaled],
            exact: vec![false, true, false, true],
        };
        let again = layout_from_sizes(&sizes, Some(&old), &mut warnings);
        assert_eq!(again.pages[1], rotated);
        assert_eq!(again.exact, [false, true, false, false]);
        assert_eq!(again.pages[3].display_size(), Size::new(400.0, 100.0));
        assert_eq!(again.pages[3].user_unit(), 2.0);
        assert_eq!(warnings.len(), 2, "the same warning is not repeated");
        // Another page count: nothing is carried over.
        let other = layout_from_sizes(&sizes[..2], Some(&old), &mut warnings);
        assert_eq!(other.exact, [false, false]);
    }

    #[test]
    fn samples_spread_over_the_pages() {
        assert_eq!(sample(&[], 3), Vec::<u32>::new());
        assert_eq!(sample(&[4, 9], 3), [4, 9]);
        assert_eq!(sample(&[0, 1, 2, 3, 4, 5, 6], 3), [0, 3, 6]);
        assert_eq!(sample(&[2, 3, 5, 8], 1), [2]);
    }
}
