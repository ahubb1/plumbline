//! [`AppState`]: everything the window knows, with no window attached to it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui_tiles::{Container, Linear, LinearDir, Tile, TileId, Tree};
use pl_doc::pl_geom::{Point, Rect, Rotation, Size, Unit, Vec2};
use pl_doc::pl_render::{
    DocId, JobKind, JobOutput, JobResult, PageText, RenderBackend, RenderError, Scheduler,
    SearchOptions, StreamId, TileCache, DEFAULT_CACHE_BUDGET,
};
use pl_doc::{
    quads_bounds, select_all, select_line_at, select_word_at, DocError, DocSearch, Document,
    DocumentId, FitMode, PageHit, RecentFiles, Session, TextSelection, ViewState,
};

use crate::commands::{self, Command};
use crate::jobs::{AfterSave, Jobs};
use crate::list::ListState;
use crate::markup::{MarkupState, SaveHow, Tool, JOURNAL_DIR_NAME};
use crate::overlay::OverlayTiles;
use crate::platform::FileDialogs;
use crate::queries::Queries;
use crate::textures::TextureStore;

/// How many pages ahead of a running search have their text requested at once.
const SEARCH_WINDOW: u32 = 8;

/// A text action waiting for the page's text to arrive from the backend.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PendingText {
    SelectAll { page: u32 },
    Word { page: u32, point: Point },
    Line { page: u32, point: Point },
}

impl PendingText {
    fn page(&self) -> u32 {
        match *self {
            PendingText::SelectAll { page }
            | PendingText::Word { page, .. }
            | PendingText::Line { page, .. } => page,
        }
    }

    fn apply(&self, text: &PageText) -> Option<TextSelection> {
        match *self {
            PendingText::SelectAll { .. } => select_all(text),
            PendingText::Word { point, .. } => select_word_at(text, point),
            PendingText::Line { point, .. } => select_line_at(text, point),
        }
    }
}

/// A move to a place given in page coordinates, waiting for the page's real geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PendingNav {
    /// A bookmark or link destination.
    Destination {
        page: u32,
        x: Option<f64>,
        y: Option<f64>,
    },
    /// The current search hit.
    Hit,
}

/// Identifies one view (one tab) onto a document. Several views can show the same document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewId(pub u32);

/// Settings the app is started with.
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// Where the recent-files list lives. `None` keeps no list.
    pub data_dir: Option<PathBuf>,
    /// The tile cache's budget in bytes.
    pub cache_bytes: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            data_dir: None,
            cache_bytes: DEFAULT_CACHE_BUDGET,
        }
    }
}

/// The units the status bar reports in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LengthUnit {
    Inches,
    Millimetres,
    Points,
}

impl LengthUnit {
    pub const ALL: [LengthUnit; 3] = [
        LengthUnit::Inches,
        LengthUnit::Millimetres,
        LengthUnit::Points,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LengthUnit::Inches => "Inches",
            LengthUnit::Millimetres => "Millimetres",
            LengthUnit::Points => "Points",
        }
    }

    fn unit(self) -> Unit {
        match self {
            LengthUnit::Inches => Unit::Inch,
            LengthUnit::Millimetres => Unit::Millimetre,
            LengthUnit::Points => Unit::Point,
        }
    }

    /// Formats a length given in PDF points.
    pub fn format(self, points: f64) -> String {
        let value = pl_doc::pl_geom::convert(points, Unit::Point, self.unit());
        match self {
            LengthUnit::Inches => format!("{value:.2}"),
            LengthUnit::Millimetres | LengthUnit::Points => format!("{value:.1}"),
        }
    }

    pub fn suffix(self) -> &'static str {
        self.unit().abbreviation()
    }
}

/// Which list the side panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidePage {
    Thumbnails,
    Bookmarks,
    Layers,
}

impl AppState {
    /// True when the keyboard is the page's: no widget has keyboard focus, or a page canvas
    /// has it. Nothing is being typed into, and no button, slider, checkbox or list was
    /// reached with Tab. Only then do the single keys (tool letters, Enter, Delete, the
    /// arrows, Space for panning) act on the page; otherwise they are the focused widget's.
    /// Clicking the page or pressing Escape gives the focus up again.
    pub(crate) fn keyboard_free(&self, ctx: &egui::Context) -> bool {
        if ctx.text_edit_focused() {
            return false;
        }
        match ctx.memory(|m| m.focused()) {
            None => true,
            Some(id) => self.views.values().any(|dv| dv.canvas_id == Some(id)),
        }
    }
}

/// What the status bar says, as plain strings (also what the tests read).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusInfo {
    /// For example `Page 3 of 24`.
    pub page: String,
    /// The current page's size as shown, for example `8.50 x 11.00 in`.
    pub size: String,
    /// The cursor's position on the page, measured from the top-left corner of the page as
    /// shown. `None` when the pointer is not over a view.
    pub cursor: Option<String>,
    pub zoom: String,
    /// The active tool's name, for example `Rectangle`.
    pub tool: String,
    /// What the active tool does with the pointer and keys.
    pub hint: String,
    /// The scale at the pointer (or on the page in view), such as `1 in = 10 ft`, with the
    /// viewport's name when a viewport's scale applies there; `Scale not set` where there is
    /// none. Empty until the document's markups (and with them its scales) have been read.
    pub scale: String,
}

/// One view onto a document: a tab.
pub struct DocView {
    pub id: ViewId,
    pub doc: DocumentId,
    pub title: String,
    pub view: ViewState,
    pub selection: Option<TextSelection>,
    pub search: Option<DocSearch>,
    /// The page point under the pointer, when the pointer is over this view.
    pub cursor: Option<PageHit>,
    /// Where the page canvas was on screen in the last frame.
    pub rect: egui::Rect,
    /// The canvas widget's id, once it has been shown: keyboard focus on it is focus on the
    /// page.
    pub(crate) canvas_id: Option<egui::Id>,
    pub(crate) stream: StreamId,
    /// The document's [`Document::geometry_epoch`] this view's page list was taken at.
    pub(crate) geometry_epoch: u64,
    pub(crate) pending_text: Option<PendingText>,
    pub(crate) pending_nav: Option<PendingNav>,
    /// The first page a running search has not asked the text of yet, and the render document
    /// those requests went to.
    search_sent: u32,
    search_render: Option<DocId>,
    pub(crate) drag_anchor: Option<(u32, Point)>,
    /// The page last navigated to and the scroll that navigation produced. While the scroll is
    /// unchanged that page is "the current page", even where another page fills more of the
    /// viewport (short pages near the end of a document).
    page_pin: Option<(u32, Vec2)>,
    pub(crate) requested: Option<RequestSignature>,
    /// The markup tool's state in this view: the tool, its style and its gesture in progress.
    pub(crate) tool: pl_tools::ToolState,
    /// What the gesture in progress looks like (page space), and the page it is on.
    pub(crate) preview: pl_doc::pl_draw::DrawList,
    pub(crate) preview_page: Option<u32>,
    /// Goes up whenever `preview` changes.
    pub(crate) preview_serial: u64,
    /// The primary button went down on this canvas for the markup tool and is still down.
    pub(crate) pointer_down: bool,
    /// The primary button went down on text with the Select tool: a text selection.
    pub(crate) text_press: bool,
    /// The last press was the markup tool's (so a double-click is too).
    pub(crate) press_markup: bool,
    /// The cursor the tool last asked for.
    pub(crate) tool_cursor: Option<pl_tools::Cursor>,
    /// The snap point under the pointer: its page and the point.
    pub(crate) snap_mark: Option<(u32, Point)>,
    /// What the snap point is (an endpoint, a midpoint...): the marker is drawn for it.
    pub(crate) snap_kind: Option<pl_doc::SnapKind>,
    /// What the measurement in progress amounts to so far, for the read-out at the pointer.
    pub(crate) readout: Option<pl_tools::Readout>,
    /// A region being dragged out for "Copy snapshot of region", in screen coordinates.
    pub(crate) region: Option<(egui::Pos2, egui::Pos2)>,
}

/// What a view last asked the scheduler for; a new request goes out only when this changes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RequestSignature {
    pub render_doc: u64,
    /// Page, its content revision, and the tiles of it that are visible.
    pub pages: Vec<(u32, u64, pl_doc::pl_geom::TileRange)>,
}

impl DocView {
    /// The page the view is on (zero-based).
    pub fn shown_page(&self) -> u32 {
        match self.page_pin {
            Some((page, scroll)) if scroll == self.view.scroll() => page,
            _ => self.view.current_page().unwrap_or(0),
        }
    }

    pub fn go_to_page(&mut self, page: u32) {
        let count = self.view.page_count();
        if count == 0 {
            return;
        }
        let page = page.min(count - 1);
        self.view.go_to_page(page);
        self.page_pin = Some((page, self.view.scroll()));
    }

    pub fn next_page(&mut self) {
        self.go_to_page(self.shown_page().saturating_add(1));
    }

    pub fn previous_page(&mut self) {
        self.go_to_page(self.shown_page().saturating_sub(1));
    }

    /// Goes to a bookmark or link destination. The position is in page coordinates, so the
    /// page's real geometry is needed: until it has arrived (`exact` is false) the view goes
    /// to the top of the page, and to the exact place once it has.
    pub fn go_to_destination(&mut self, page: u32, x: Option<f64>, y: Option<f64>, exact: bool) {
        let count = self.view.page_count();
        if count == 0 {
            return;
        }
        let page = page.min(count - 1);
        if exact {
            self.view.go_to_destination(page, x, y);
            self.page_pin = Some((page, self.view.scroll()));
            self.pending_nav = None;
        } else {
            self.go_to_page(page);
            self.pending_nav = Some(PendingNav::Destination { page, x, y });
        }
    }

    /// Scrolls the current search hit into view. A hit's quads are in page coordinates: on a
    /// page whose real geometry has not arrived yet the view goes to the page, and to the hit
    /// itself once it has.
    pub(crate) fn reveal_current_hit(&mut self, doc: &Document) {
        let Some(hit) = self.search.as_ref().and_then(|s| s.current()).cloned() else {
            return;
        };
        if !doc.geometry_is_exact(hit.page) {
            self.go_to_page(hit.page);
            self.pending_nav = Some(PendingNav::Hit);
            return;
        }
        self.pending_nav = None;
        match quads_bounds(&hit.quads) {
            Some(b) => self
                .view
                .reveal_rect(hit.page, Rect::new(b[0], b[1], b[2], b[3]), 48.0),
            None => self.go_to_page(hit.page),
        }
    }

    /// Takes the document's page list again if it has changed, and finishes a move that was
    /// waiting for a page's real geometry.
    fn follow_geometry(&mut self, doc: &Document) {
        if self.geometry_epoch != doc.geometry_epoch() {
            self.geometry_epoch = doc.geometry_epoch();
            self.view.set_pages(doc.pages());
            self.requested = None;
        }
        match self.pending_nav {
            Some(PendingNav::Destination { page, x, y }) if doc.geometry_is_exact(page) => {
                self.go_to_destination(page, x, y, true);
            }
            Some(PendingNav::Hit) => self.reveal_current_hit(doc),
            _ => {}
        }
    }

    /// `(current hit number, hit count)`, one-based; `None` without a search.
    pub fn search_position(&self) -> Option<(usize, usize)> {
        let search = self.search.as_ref()?;
        Some((search.current_index().map_or(0, |i| i + 1), search.len()))
    }
}

/// A password being asked for.
pub(crate) struct PasswordPrompt {
    pub path: PathBuf,
    pub text: String,
    pub wrong: bool,
    pub focus: bool,
}

/// State of the window furniture (as opposed to the documents).
pub(crate) struct Chrome {
    pub side_open: bool,
    pub side_page: SidePage,
    pub page_box: String,
    pub zoom_box: String,
    pub find_open: bool,
    /// The text field that had the keyboard when the last frame ended.
    pub text_focus: Option<egui::Id>,
    /// The text field Escape was pressed in, for the frame it is pressed in.
    pub escape_field: Option<egui::Id>,
    pub find_text: String,
    pub find_options: SearchOptions,
    pub focus_find: bool,
    pub focus_page_box: bool,
    pub unit: LengthUnit,
    pub theme: egui::ThemePreference,
    pub status: Option<String>,
    pub password: Option<PasswordPrompt>,
    pub window_title: String,
}

/// The whole application: open documents, their views, the render pipeline and the window
/// furniture. Nothing here needs a window; [`AppState::ui`] draws one frame into any `egui::Ui`.
pub struct AppState {
    pub(crate) session: Session,
    pub(crate) scheduler: Scheduler,
    results: Receiver<JobResult>,
    pub(crate) cache: Arc<TileCache>,
    repaint: Arc<Mutex<Option<egui::Context>>>,
    pub(crate) tree: Tree<ViewId>,
    pub(crate) views: HashMap<ViewId, DocView>,
    pub(crate) active: Option<ViewId>,
    pub(crate) shown_this_frame: Vec<ViewId>,
    next_view: u32,
    pub(crate) textures: TextureStore,
    pub(crate) queries: Queries,
    pub(crate) recent: Option<RecentFiles>,
    pub(crate) dialogs: Box<dyn FileDialogs>,
    pub(crate) chrome: Chrome,
    pub(crate) markup: MarkupState,
    /// Scales, measurement dialogs, snapping kinds.
    pub(crate) measure: crate::measure::MeasureState,
    /// Saves and loads on worker threads.
    pub(crate) jobs: Jobs,
    /// The markups list.
    pub(crate) list: ListState,
    pub(crate) overlays: OverlayTiles,
    last_copied: Option<String>,
    /// What the platform's paste event carried, until the Paste command takes it.
    pasted_text: Option<String>,
    pub(crate) frame: u64,
    /// A frame is being drawn: nothing may wait for a worker thread now.
    pub(crate) in_frame: bool,
    failed_tiles: u64,
}

const TREE_ID: &str = "plumbline-views";

impl AppState {
    /// Builds the app around a render backend. Nothing is opened and nothing is drawn yet.
    pub fn new(
        backend: Arc<dyn RenderBackend>,
        config: AppConfig,
        dialogs: Box<dyn FileDialogs>,
    ) -> AppState {
        let cache = Arc::new(TileCache::new(config.cache_bytes));
        let repaint: Arc<Mutex<Option<egui::Context>>> = Arc::default();
        let wake = repaint.clone();
        let notify: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            if let Ok(slot) = wake.lock() {
                if let Some(ctx) = slot.as_ref() {
                    ctx.request_repaint();
                }
            }
        });
        let (scheduler, results) =
            Scheduler::with_options(backend.clone(), cache.clone(), 1, Some(notify));
        let mut session = Session::new(backend);
        // Unsaved markups are journaled beside the other app data; with no data directory
        // (most tests) nothing is journaled.
        session.set_journal_dir(config.data_dir.as_ref().map(|d| d.join(JOURNAL_DIR_NAME)));
        AppState {
            session,
            scheduler,
            results,
            cache,
            repaint,
            tree: Tree::empty(egui::Id::unique(TREE_ID)),
            views: HashMap::new(),
            active: None,
            shown_this_frame: Vec::new(),
            next_view: 1,
            textures: TextureStore::default(),
            queries: Queries::default(),
            markup: MarkupState::new(config.data_dir.as_deref()),
            measure: crate::measure::MeasureState::new(),
            jobs: Jobs::default(),
            list: ListState::new(config.data_dir.as_deref()),
            overlays: OverlayTiles::default(),
            recent: config.data_dir.map(RecentFiles::load),
            dialogs,
            chrome: Chrome {
                side_open: true,
                side_page: SidePage::Thumbnails,
                page_box: String::new(),
                zoom_box: String::new(),
                find_open: false,
                text_focus: None,
                escape_field: None,
                find_text: String::new(),
                find_options: SearchOptions::default(),
                focus_find: false,
                focus_page_box: false,
                unit: LengthUnit::Inches,
                theme: egui::ThemePreference::System,
                status: None,
                password: None,
                window_title: String::new(),
            },
            last_copied: None,
            pasted_text: None,
            frame: 0,
            in_frame: false,
            failed_tiles: 0,
        }
    }

    // ---- reading the state -----------------------------------------------------------------

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The view that commands apply to.
    pub fn active(&self) -> Option<ViewId> {
        self.active
    }

    pub fn view(&self, id: ViewId) -> Option<&DocView> {
        self.views.get(&id)
    }

    pub fn view_mut(&mut self, id: ViewId) -> Option<&mut DocView> {
        self.views.get_mut(&id)
    }

    pub fn active_view(&self) -> Option<&DocView> {
        self.active.and_then(|id| self.views.get(&id))
    }

    pub fn active_view_mut(&mut self) -> Option<&mut DocView> {
        self.active.and_then(|id| self.views.get_mut(&id))
    }

    /// Every view, in the order they were created.
    pub fn view_ids(&self) -> Vec<ViewId> {
        let mut ids: Vec<ViewId> = self.views.keys().copied().collect();
        ids.sort();
        ids
    }

    /// The text last put on the clipboard by this app.
    pub fn last_copied(&self) -> Option<&str> {
        self.last_copied.as_deref()
    }

    /// The message in the status bar, if any (the last error or notice).
    pub fn status(&self) -> Option<&str> {
        self.chrome.status.as_deref()
    }

    pub fn recent(&self) -> Option<&RecentFiles> {
        self.recent.as_ref()
    }

    pub fn tile_cache(&self) -> &Arc<TileCache> {
        &self.cache
    }

    pub fn textures(&self) -> &TextureStore {
        &self.textures
    }

    pub fn find_bar_open(&self) -> bool {
        self.chrome.find_open
    }

    pub fn side_panel_open(&self) -> bool {
        self.chrome.side_open
    }

    pub fn set_side_page(&mut self, page: SidePage) {
        self.chrome.side_page = page;
    }

    pub fn set_theme(&mut self, theme: egui::ThemePreference) {
        self.chrome.theme = theme;
    }

    pub fn set_unit(&mut self, unit: LengthUnit) {
        self.chrome.unit = unit;
    }

    /// Whether the user is being asked for a document password.
    pub fn password_prompt_open(&self) -> bool {
        self.chrome.password.is_some()
    }

    /// Waits until every queued job has finished and every document's background work is
    /// done. For tests and shutdown; the window itself never waits. The results are taken up
    /// by the next frame.
    pub fn wait_idle(&mut self, timeout: Duration) -> bool {
        // Saves and loads on worker threads are waited for and taken in.
        self.drain_jobs();
        let mut done = true;
        for doc in self.session.documents_mut() {
            done &= doc.wait_background(timeout);
        }
        // With snapping on, the content scenes of the pages on screen are part of "idle".
        if self.markup.snap {
            for dv in self.views.values() {
                if let Ok(doc) = self.session.document_mut(dv.doc) {
                    for page in dv.view.visible_pages() {
                        let _ = doc.scene_wait(page);
                    }
                }
            }
        }
        done & self.scheduler.wait_idle(timeout)
    }

    /// The egui context the app draws into, once a frame has been drawn.
    pub(crate) fn repaint_context(&self) -> Option<egui::Context> {
        self.repaint.lock().ok().and_then(|slot| slot.clone())
    }

    /// Asks for another frame shortly (from inside or outside a frame).
    pub(crate) fn request_repaint_soon(&self) {
        if let Ok(slot) = self.repaint.lock() {
            if let Some(ctx) = slot.as_ref() {
                ctx.request_repaint();
            }
        }
    }

    /// Whether there are copied markups to paste.
    pub fn can_paste(&self) -> bool {
        self.markup.clipboard.is_some()
    }

    /// Requests for text, geometry or the outline that have not been answered yet.
    pub fn pending_queries(&self) -> usize {
        self.queries.pending()
    }

    /// What the status bar shows for the active view.
    pub fn status_info(&self) -> StatusInfo {
        let Some(dv) = self.active_view() else {
            return StatusInfo::default();
        };
        let unit = self.chrome.unit;
        let page = dv.shown_page();
        let tool = self.markup.tool;
        let mut info = StatusInfo {
            page: format!("Page {} of {}", page + 1, dv.view.page_count()),
            zoom: format!("{:.0}%", dv.view.zoom() * 100.0),
            tool: tool.label().to_owned(),
            hint: if self.markup.snapshot_pick {
                "Drag across the part of the view to copy as an image. Esc cancels.".to_owned()
            } else {
                tool.hint().to_owned()
            },
            ..StatusInfo::default()
        };
        if let Some(geometry) = dv.view.page_geometry(page) {
            let size = geometry.display_size_points();
            info.size = format!(
                "{} x {} {}",
                unit.format(size.width),
                unit.format(size.height),
                unit.suffix()
            );
        }
        if let Some(hit) = dv.cursor {
            if let Some(geometry) = dv.view.page_geometry(hit.page) {
                let shown = geometry.page_to_display() * hit.point;
                let k = geometry.user_unit();
                info.cursor = Some(format!(
                    "x {}  y {} {}",
                    unit.format(shown.x * k),
                    unit.format(shown.y * k),
                    unit.suffix()
                ));
            }
        }
        info.scale = self.scale_text(dv);
        info
    }

    // ---- opening and closing ---------------------------------------------------------------

    /// Opens a file in a new tab, or shows the tab it is already open in. Failures go to the
    /// status bar; a file that needs a password brings up the password prompt.
    pub fn open_path(&mut self, path: &Path) -> Option<ViewId> {
        self.open_with_password(path, None)
    }

    pub(crate) fn open_with_password(
        &mut self,
        path: &Path,
        password: Option<&str>,
    ) -> Option<ViewId> {
        match self.session.open(path, password) {
            Ok(doc) => {
                self.chrome.password = None;
                self.chrome.status = None;
                let existing = self
                    .views
                    .values()
                    .filter(|v| v.doc == doc)
                    .map(|v| v.id)
                    .min();
                let id = match existing {
                    Some(id) => {
                        self.activate(id);
                        id
                    }
                    None => {
                        let id = self.add_view(doc, None);
                        self.check_recovery(doc);
                        id
                    }
                };
                self.note_opened(doc, id);
                Some(id)
            }
            Err(DocError::Render(RenderError::PasswordRequired)) => {
                self.chrome.password = Some(PasswordPrompt {
                    path: path.to_path_buf(),
                    text: String::new(),
                    wrong: password.is_some(),
                    focus: true,
                });
                None
            }
            Err(e) => {
                self.chrome.status = Some(format!("Could not open {}: {e}", display_name(path)));
                if let Some(recent) = &mut self.recent {
                    if !path.exists() && recent.remove(path) {
                        let _ = recent.save();
                    }
                }
                None
            }
        }
    }

    /// Opens a document held in memory (it has no file, so it is not added to the recent list).
    pub fn open_bytes(&mut self, name: &str, bytes: Vec<u8>) -> Result<ViewId, DocError> {
        let doc = self.session.open_bytes(name, bytes, None)?;
        Ok(self.add_view(doc, None))
    }

    /// Records a freshly opened file in the recent list and returns to the page it was last on.
    fn note_opened(&mut self, doc: DocumentId, view: ViewId) {
        let Some(path) = self
            .session
            .document(doc)
            .ok()
            .and_then(|d| d.path().map(Path::to_path_buf))
        else {
            return;
        };
        let Some(recent) = &mut self.recent else {
            return;
        };
        let last_page = recent.get(&path).map(|e| e.last_page);
        recent.touch(&path);
        let _ = recent.save();
        if let (Some(page), Some(dv)) = (last_page, self.views.get_mut(&view)) {
            if page > 0 && dv.shown_page() == 0 {
                dv.go_to_page(page);
            }
        }
    }

    /// Adds a view of an open document as a new tab; `like` copies another view's position.
    fn add_view(&mut self, doc: DocumentId, like: Option<ViewId>) -> ViewId {
        let id = ViewId(self.next_view);
        self.next_view += 1;
        let (title, mut view, geometry_epoch) = match self.session.document(doc) {
            Ok(d) => (
                d.title(),
                d.new_view(Size::new(800.0, 600.0), 1.0),
                d.geometry_epoch(),
            ),
            Err(_) => (
                String::new(),
                ViewState::new(&[], Size::new(800.0, 600.0), 1.0),
                0,
            ),
        };
        match like.and_then(|v| self.views.get(&v)) {
            Some(source) => view = source.view.clone(),
            None => view.set_fit(FitMode::Width),
        }
        self.views.insert(
            id,
            DocView {
                id,
                doc,
                title,
                view,
                selection: None,
                search: None,
                cursor: None,
                rect: egui::Rect::NOTHING,
                canvas_id: None,
                // Stream 0 is the thumbnail stream, u32::MAX the query stream.
                stream: StreamId(id.0),
                geometry_epoch,
                pending_text: None,
                pending_nav: None,
                search_sent: 0,
                search_render: None,
                drag_anchor: None,
                page_pin: None,
                requested: None,
                tool: pl_tools::ToolState::for_tool(pl_tools::ToolKind::Select),
                preview: pl_doc::pl_draw::DrawList::new(),
                preview_page: None,
                preview_serial: 0,
                pointer_down: false,
                text_press: false,
                press_markup: false,
                tool_cursor: None,
                snap_mark: None,
                snap_kind: None,
                readout: None,
                region: None,
            },
        );
        self.sync_tool(id);
        if like.is_none() {
            self.insert_tab(id);
        }
        id
    }

    fn tabs_of(&self, view: ViewId) -> Option<TileId> {
        let pane = self.tree.tiles.find_pane(&view)?;
        let parent = self.tree.tiles.parent_of(pane)?;
        matches!(
            self.tree.tiles.get(parent),
            Some(Tile::Container(Container::Tabs(_)))
        )
        .then_some(parent)
    }

    fn insert_tab(&mut self, id: ViewId) {
        let host = self.active.and_then(|a| self.tabs_of(a)).or_else(|| {
            self.tree
                .tiles
                .iter()
                .find(|(_, tile)| matches!(tile, Tile::Container(Container::Tabs(_))))
                .map(|(tile_id, _)| *tile_id)
        });
        let pane = self.tree.tiles.insert_pane(id);
        match host {
            Some(host) => {
                if let Some(Tile::Container(Container::Tabs(tabs))) = self.tree.tiles.get_mut(host)
                {
                    tabs.add_child(pane);
                    tabs.set_active(pane);
                }
            }
            None => {
                let tabs = self.tree.tiles.insert_tab_tile(vec![pane]);
                self.tree.root = Some(match self.tree.root {
                    Some(old) => self.tree.tiles.insert_horizontal_tile(vec![old, tabs]),
                    None => tabs,
                });
            }
        }
        self.active = Some(id);
    }

    /// Makes a view the active one and brings its tab to the front.
    pub fn activate(&mut self, id: ViewId) {
        if !self.views.contains_key(&id) {
            return;
        }
        self.active = Some(id);
        self.tree
            .make_active(|_, tile| matches!(tile, Tile::Pane(pane) if *pane == id));
    }

    /// Opens a second view of the active view's document beside it (`side`) or below it.
    pub fn split_active(&mut self, side: bool) -> Option<ViewId> {
        let source = self.active?;
        let doc = self.views.get(&source)?.doc;
        let new_view = self.add_view(doc, Some(source));
        let pane = self.tree.tiles.insert_pane(new_view);
        let new_tabs = self.tree.tiles.insert_tab_tile(vec![pane]);
        let dir = if side {
            LinearDir::Horizontal
        } else {
            LinearDir::Vertical
        };
        match self.tabs_of(source) {
            Some(tabs) => {
                let parent = self.tree.tiles.parent_of(tabs);
                let linear = self.tree.tiles.insert_container(Linear::new_binary(
                    dir,
                    [tabs, new_tabs],
                    0.5,
                ));
                match parent {
                    Some(parent) => {
                        if let Some(Tile::Container(container)) = self.tree.tiles.get_mut(parent) {
                            let _ = container.replace_child(tabs, linear);
                        }
                    }
                    None => self.tree.root = Some(linear),
                }
            }
            None => {
                self.tree.root = Some(match self.tree.root {
                    Some(old) => self.tree.tiles.insert_container(Linear::new_binary(
                        dir,
                        [old, new_tabs],
                        0.5,
                    )),
                    None => new_tabs,
                });
            }
        }
        self.active = Some(new_view);
        Some(new_view)
    }

    /// Closes a view's tab. The document closes with its last view.
    pub fn close_view(&mut self, id: ViewId) {
        if let Some(pane) = self.tree.tiles.find_pane(&id) {
            self.tree.tiles.remove(pane);
        }
        self.collect_closed();
    }

    /// Drops the views whose tabs are gone and the documents nothing shows any more.
    pub(crate) fn collect_closed(&mut self) {
        let alive: HashSet<ViewId> = self
            .tree
            .tiles
            .tiles()
            .filter_map(|tile| match tile {
                Tile::Pane(pane) => Some(*pane),
                Tile::Container(_) => None,
            })
            .collect();
        let gone: Vec<ViewId> = self
            .views
            .keys()
            .copied()
            .filter(|id| !alive.contains(id))
            .collect();
        for id in gone {
            let Some(dv) = self.views.remove(&id) else {
                continue;
            };
            self.scheduler.advance(dv.stream);
            let last_view = !self.views.values().any(|v| v.doc == dv.doc);
            if !last_view {
                continue;
            }
            if let Ok(doc) = self.session.document(dv.doc) {
                let render = doc.render_id();
                if let (Some(recent), Some(path)) = (&mut self.recent, doc.path()) {
                    if recent.set_last_page(path, dv.shown_page()) {
                        let _ = recent.save();
                    }
                }
                self.forget_render(render);
            }
            self.markup
                .overlay_cache
                .retain(|(doc, _), _| *doc != dv.doc);
            if self.markup.recovery == Some(dv.doc) {
                self.markup.recovery = None;
            }
            self.markup.layers_failed.retain(|d| *d != dv.doc);
            if self.markup.note.as_ref().is_some_and(|n| n.doc == dv.doc) {
                self.markup.note = None;
            }
            if self
                .markup
                .text_editor
                .as_ref()
                .is_some_and(|e| e.doc == dv.doc)
            {
                self.markup.text_editor = None;
            }
            self.jobs.forget(dv.doc);
            let _ = self.session.close(dv.doc);
        }
        if self.views.is_empty() {
            self.tree = Tree::empty(egui::Id::unique(TREE_ID));
            self.active = None;
        } else if self.active.is_none_or(|a| !self.views.contains_key(&a)) {
            self.active = self.view_ids().first().copied();
        }
    }

    /// Records that the rendered content of one page changed (an annotation on it was edited,
    /// say). Only that page is affected: its content revision goes up, its old tiles and
    /// textures are dropped at once, and it is rendered again the next time it is on screen.
    /// Every other page of the document keeps its cached tiles.
    pub fn invalidate_page(&mut self, doc: DocumentId, page: u32) {
        let Ok(d) = self.session.document_mut(doc) else {
            return;
        };
        if page >= d.page_count() {
            return;
        }
        let render = d.render_id();
        let revision = d.invalidate_page(page);
        self.cache.remove_stale(render, page, revision);
        self.textures.forget_stale(render, page, revision);
    }

    /// Records where each document was left. Call when the window closes.
    pub fn shutdown(&mut self) {
        self.commit_text_edit();
        self.flush_presets();
        self.flush_list();
        let Some(recent) = &mut self.recent else {
            return;
        };
        for dv in self.views.values() {
            if let Some(path) = self.session.document(dv.doc).ok().and_then(|d| d.path()) {
                recent.set_last_page(path, dv.shown_page());
            }
        }
        let _ = recent.save();
    }

    // ---- search ----------------------------------------------------------------------------

    /// Sets the find text, as typing in the find bar does.
    pub fn set_find_text(&mut self, text: &str) {
        self.chrome.find_text = text.to_owned();
    }

    /// Finds the next (or previous) match of the find text in the active view. A new or
    /// changed query starts a search, which runs as page text arrives from the scheduler: the
    /// window does not wait for any of it.
    pub fn find(&mut self, forward: bool) {
        let query = self.chrome.find_text.trim().to_owned();
        let options = self.chrome.find_options;
        let Some(id) = self.active else {
            return;
        };
        let Some(dv) = self.views.get_mut(&id) else {
            return;
        };
        if query.is_empty() {
            dv.search = None;
            return;
        }
        let same = dv.search.as_ref().is_some_and(|s| {
            s.query() == query
                && s.options().match_case == options.match_case
                && s.options().whole_word == options.whole_word
        });
        let Ok(doc) = self.session.document(dv.doc) else {
            return;
        };
        if !same {
            dv.search = Some(doc.start_search(&query, options));
            dv.search_sent = 0;
            dv.search_render = None;
            return;
        }
        if let Some(search) = dv.search.as_mut().filter(|s| s.is_finished()) {
            if forward {
                search.next_hit();
            } else {
                search.previous_hit();
            }
            dv.reveal_current_hit(doc);
        }
    }

    /// Keeps running searches supplied with page text: pages whose text is cached are searched
    /// at once, the next few others are requested from the scheduler. Nothing here waits for
    /// the backend. Returns whether any search is still running.
    fn pump_searches(&mut self) -> bool {
        let mut running = false;
        for dv in self.views.values_mut() {
            if dv.search.as_ref().is_none_or(|s| s.is_finished()) {
                continue;
            }
            let Ok(doc) = self.session.document(dv.doc) else {
                dv.search = None;
                continue;
            };
            if doc.is_saving() {
                // The display belongs to the save until it has finished.
                running = true;
                continue;
            }
            let render = doc.render_id();
            let from = dv.shown_page();
            let Some(search) = dv.search.as_mut() else {
                continue;
            };
            // Requests that went to a mirror since reopened, or whose answers are all in
            // without the search having moved, are sent again from where the search stands.
            if dv.search_render != Some(render) || !self.queries.text_pending_for(render) {
                dv.search_render = Some(render);
                dv.search_sent = search.next_page();
            }
            dv.search_sent = dv.search_sent.max(search.next_page());
            while dv.search_sent < doc.page_count()
                && dv.search_sent < search.next_page().saturating_add(SEARCH_WINDOW)
            {
                let page = dv.search_sent;
                dv.search_sent += 1;
                if !search.wants(page) {
                    continue;
                }
                if let Some(text) = doc.cached_text(page) {
                    search.feed(page, Some(&text));
                } else if !self.queries.want_search_text(&self.scheduler, render, page) {
                    // The page's text cannot be read: it has no hits.
                    search.feed(page, None);
                }
                // Hits are in page coordinates: have the page's real geometry by the time
                // one of them is shown or gone to.
                if !doc.geometry_is_exact(page) {
                    self.queries.want_search_info(&self.scheduler, render, page);
                }
            }
            if search.is_finished() {
                search.select_first_from(from);
                dv.reveal_current_hit(doc);
            } else {
                running = true;
            }
        }
        running
    }

    /// Hands a page's text (or the news that it could not be read) to the document's cache,
    /// to the searches waiting for it and to a text action waiting for it.
    fn text_arrived(&mut self, render: DocId, page: u32, text: Option<Arc<PageText>>) {
        let Some(doc) = self
            .session
            .documents_mut()
            .find(|d| d.render_id() == render)
        else {
            return;
        };
        if let Some(text) = &text {
            doc.accept_text(render, page, text.clone());
        }
        let doc_id = doc.id();
        for dv in self.views.values_mut().filter(|v| v.doc == doc_id) {
            if let Some(search) = dv.search.as_mut().filter(|s| !s.is_finished()) {
                if search.wants(page) && search.feed(page, text.as_deref()) {
                    let from = dv.shown_page();
                    if let Some(search) = dv.search.as_mut() {
                        search.select_first_from(from);
                    }
                    dv.reveal_current_hit(doc);
                }
            }
            if dv.pending_text.is_some_and(|p| p.page() == page) {
                let action = dv.pending_text.take().expect("checked");
                if let Some(text) = &text {
                    dv.selection = action.apply(text);
                }
            }
        }
    }

    /// Picks up what the documents' background work has finished and lets every view follow
    /// a page list that changed. Returns whether any background work is still running.
    fn sync_documents(&mut self) -> bool {
        let mut pending = false;
        for doc in self.session.documents_mut() {
            doc.poll_background();
            pending |= doc.background_pending();
        }
        for dv in self.views.values_mut() {
            if let Ok(doc) = self.session.document(dv.doc) {
                dv.follow_geometry(doc);
            }
        }
        pending
    }

    // ---- commands --------------------------------------------------------------------------

    /// Carries out a command.
    pub fn run(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::Open => {
                for path in self.dialogs.pick_pdfs() {
                    self.open_path(&path);
                }
            }
            Command::OpenPath(path) => {
                self.open_path(&path);
            }
            Command::ClearRecent => {
                if let Some(recent) = &mut self.recent {
                    recent.clear();
                    let _ = recent.save();
                }
            }
            Command::CloseTab => {
                if let Some(id) = self.active {
                    self.request_close(id);
                }
            }
            Command::Quit => self.request_quit(ctx),
            Command::Undo => {
                self.undo();
            }
            Command::Redo => {
                self.redo();
            }
            // A save runs on a worker thread; the frame that starts it does not wait for it.
            Command::Save => {
                self.start_save(SaveHow::InPlace);
            }
            Command::SaveAs => {
                self.start_save(SaveHow::As);
            }
            Command::SaveOptimised => {
                self.start_save(SaveHow::Optimised);
            }
            Command::ToggleMarkupsList => {
                let open = !self.list.open;
                self.set_markups_list_open(open);
            }
            Command::ExportMarkups(kind) => {
                self.export_markups(kind);
            }
            Command::ImportXfdf => {
                self.import_xfdf();
            }
            Command::PickImage => {
                if self.pick_image() {
                    self.set_tool(Tool::Image);
                }
            }
            Command::ScalePanel => self.open_scale_panel(None),
            Command::InsertLegend(quantity) => {
                self.insert_legend(quantity);
            }
            Command::TakeoffLayout => self.use_takeoff_layout(),
            Command::FormulaColumn => self.open_formula_dialog(),
            Command::Cut => {
                self.copy_markups(ctx, true);
            }
            Command::Paste => {
                let text = self.pasted_text.take();
                self.paste_markups(text.as_deref());
            }
            Command::Duplicate => {
                self.duplicate_selection();
            }
            Command::Delete => {
                self.delete_selection();
            }
            Command::SelectAllMarkups => self.select_all_markups(),
            Command::Tool(tool) => {
                self.commit_text_edit();
                self.cancel_gesture();
                self.chrome.status = None;
                self.set_tool(tool);
            }
            Command::ToggleSnap => {
                let on = !self.markup.snap;
                self.set_snap(on);
            }
            Command::TogglePropertiesPanel => {
                self.markup.properties_open = !self.markup.properties_open;
            }
            Command::Arrange(how) => self.arrange(how),
            Command::CopySnapshot => {
                let canvas = crate::theme::Palette::of(&ctx.global_style().visuals).canvas;
                self.copy_snapshot(None, canvas);
            }
            Command::CopySnapshotRegion => {
                self.cancel_gesture();
                self.markup.snapshot_pick = true;
                self.chrome.status = None;
            }
            Command::ToggleSidePanel => self.chrome.side_open = !self.chrome.side_open,
            Command::SplitSide => {
                self.split_active(true);
            }
            Command::SplitStack => {
                self.split_active(false);
            }
            Command::Find => {
                self.chrome.find_open = true;
                self.chrome.focus_find = true;
            }
            Command::FindNext => self.find(true),
            Command::FindPrevious => self.find(false),
            Command::CloseFind => self.close_find(),
            Command::FocusPageBox => self.chrome.focus_page_box = true,
            Command::Dismiss => {
                let has_markups = self.selected_count() > 0;
                let has_text = self.active_view().is_some_and(|v| v.selection.is_some());
                if self.cancel_text_edit() || self.list_dismiss() {
                    // What was being typed is thrown away; nothing else changes.
                } else if self.escape_leaves_field(ctx) {
                    // Escape in a text field that has nothing of its own to cancel (the
                    // list's Search, a filter box, a field of a dialog or of the properties)
                    // leaves the field and does nothing else: no window closes behind it,
                    // the selection and the tool stay.
                } else if self.markup.snapshot_pick {
                    self.markup.snapshot_pick = false;
                    for dv in self.views.values_mut() {
                        dv.region = None;
                    }
                } else if self.cancel_gesture() {
                    // The gesture is gone; nothing else changes.
                } else if self.markup.note.is_some() {
                    // The note window closes before anything behind it changes.
                    self.markup.note = None;
                } else if self.chrome.find_open {
                    self.close_find();
                } else if has_markups {
                    if let Some(doc) = self
                        .active_view()
                        .map(|v| v.doc)
                        .and_then(|d| self.session.document_mut(d).ok())
                    {
                        doc.clear_selection();
                    }
                } else if has_text {
                    if let Some(dv) = self.active_view_mut() {
                        dv.selection = None;
                    }
                } else {
                    self.set_tool(Tool::Select);
                }
            }
            Command::Copy => {
                if self.copy_markups(ctx, false) > 0 {
                    return;
                }
                if let Some(text) = self
                    .active_view()
                    .and_then(|v| v.selection.as_ref())
                    .map(|s| s.text.clone())
                    .filter(|t| !t.is_empty())
                {
                    ctx.copy_text(text.clone());
                    // egui's own clipboard support is not compiled in; this reaches the system.
                    let _ = self.markup.system_clipboard.set_text(&text);
                    self.last_copied = Some(text);
                }
            }
            Command::SelectAll => self.select_all_on_page(),
            other => self.run_on_view(other),
        }
    }

    /// Starts saving the active document on a worker thread.
    fn start_save(&mut self, how: SaveHow) {
        if let Some(id) = self.active {
            self.begin_save(id, how, AfterSave::Stay);
        }
    }

    fn close_find(&mut self) {
        self.chrome.find_open = false;
        for dv in self.views.values_mut() {
            dv.search = None;
        }
    }

    fn select_all_on_page(&mut self) {
        let Some(id) = self.active else {
            return;
        };
        let Some(dv) = self.views.get_mut(&id) else {
            return;
        };
        let page = dv.shown_page();
        let Ok(doc) = self.session.document(dv.doc) else {
            return;
        };
        match doc.cached_text(page) {
            Some(text) => dv.selection = select_all(&text),
            None => {
                // The text is fetched by the scheduler; the selection appears when it is in.
                dv.selection = None;
                if self
                    .queries
                    .want_text(&self.scheduler, doc.render_id(), page)
                {
                    dv.pending_text = Some(PendingText::SelectAll { page });
                }
            }
        }
    }

    /// The commands that only move the active view.
    fn run_on_view(&mut self, command: Command) {
        let Some(dv) = self.active_view_mut() else {
            return;
        };
        match command {
            Command::NextPage => dv.next_page(),
            Command::PreviousPage => dv.previous_page(),
            Command::FirstPage => dv.go_to_page(0),
            Command::LastPage => dv.go_to_page(u32::MAX),
            Command::ScrollLines(lines) => dv.view.scroll_by(Vec2::new(0.0, 60.0 * lines as f64)),
            Command::ScrollPages(pages) => {
                let step = dv.view.viewport().height * 0.9 * pages as f64;
                dv.view.scroll_by(Vec2::new(0.0, step));
            }
            Command::ZoomIn => dv.view.zoom_in(),
            Command::ZoomOut => dv.view.zoom_out(),
            Command::FitWidth => dv.view.set_fit(FitMode::Width),
            Command::FitPage => dv.view.set_fit(FitMode::Page),
            Command::ActualSize => dv.view.set_zoom(1.0),
            Command::RotateRight => dv.view.rotate_view(Rotation::R90),
            Command::RotateLeft => dv.view.rotate_view(Rotation::R270),
            _ => {}
        }
    }

    // ---- one frame -------------------------------------------------------------------------

    /// Draws one frame of the whole interface and handles its input.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.frame += 1;
        self.in_frame = true;
        if let Ok(mut slot) = self.repaint.lock() {
            if slot.is_none() {
                *slot = Some(ctx.clone());
            }
        }
        ctx.set_theme(self.chrome.theme);
        self.receive_results(&ctx);
        self.apply_invalidations();
        // The window's own close button: documents with unsaved markups are asked about first.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.markup.quit_confirmed
            && self.view_ids().into_iter().any(|id| self.is_modified(id))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_quit(&ctx);
        }
        if self.sync_documents() {
            // A document's background work is picked up by a frame; ask for one soon.
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        // Saves and loads on worker threads are taken in when they have finished. This
        // never waits. A read of the annotations is short and something is usually about to
        // need it, so frames keep coming until it is in; a save is looked at again shortly.
        let (loading, saving) = self.poll_jobs();
        if loading {
            // (A millisecond for the worker, so that frames with nothing else to do do not
            // spin while it reads.)
            std::thread::sleep(Duration::from_millis(1));
            ctx.request_repaint();
        } else if saving {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        self.receive_dropped_files(&ctx);
        self.settle_presets(&ctx);
        self.settle_list(&ctx);
        self.handle_shortcuts(&ctx);
        // A running search is moved on by the results it is waiting for, each of which asks
        // for a repaint when it arrives. Only when it is being fed from cached text alone is
        // there no result to wait for, and the next frame is asked for here.
        if self.pump_searches() && self.queries.pending() == 0 {
            ctx.request_repaint();
        }

        self.shown_this_frame.clear();
        crate::chrome::show(self, ui);
        crate::chrome::password_prompt(self, &ctx);
        crate::textedit::show_note(self, &ctx);
        crate::panels::recovery_prompt(self, &ctx);
        crate::panels::close_prompt(self, &ctx);
        crate::measure::scale_panel(self, &ctx);
        crate::measure::request_dialog(self, &ctx);
        crate::list::formula_dialog(self, &ctx);
        crate::chrome::drop_hint(&ctx);

        self.collect_closed();
        self.textures.end_frame(self.frame);
        self.overlays.end_frame(self.frame);

        let title = match self.active {
            Some(id) => format!("{} - Plumbline", self.view_title(id)),
            None => "Plumbline".to_owned(),
        };
        if title != self.chrome.window_title {
            self.chrome.window_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
        self.in_frame = false;
    }

    fn receive_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.results.try_recv() {
            let error = result.outcome.as_ref().err().map(|e| e.to_string());
            self.queries.done(&result.job.kind, error);
            match (result.job.kind, result.outcome) {
                (JobKind::Thumbnail { doc, page, .. }, Ok(JobOutput::Bitmap(bitmap))) => {
                    self.textures.set_thumb(ctx, doc, page, &bitmap);
                }
                (JobKind::Tile { .. }, Err(e)) => {
                    self.failed_tiles += 1;
                    if self.failed_tiles == 1 {
                        self.chrome.status = Some(format!("A page area could not be drawn: {e}"));
                    }
                }
                (JobKind::PageText { doc, page }, Ok(JobOutput::Text(text))) => {
                    self.text_arrived(doc, page, Some(text));
                }
                (JobKind::PageText { doc, page }, Err(_)) => self.text_arrived(doc, page, None),
                (JobKind::PageInfo { doc, page }, Ok(JobOutput::PageInfo(info))) => {
                    if let Some(d) = self.session.documents_mut().find(|d| d.render_id() == doc) {
                        d.accept_page_info(doc, page, info);
                    }
                }
                (JobKind::PageInfo { doc, page }, Err(e)) => {
                    if let Some(d) = self.session.documents_mut().find(|d| d.render_id() == doc) {
                        d.note_unreadable(doc, page, &e.to_string());
                    }
                }
                (JobKind::Outline { doc }, Ok(JobOutput::Outline(items))) => {
                    if let Some(d) = self.session.documents_mut().find(|d| d.render_id() == doc) {
                        d.accept_outline(doc, &items);
                    }
                }
                _ => {}
            }
        }
    }

    fn receive_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        });
        for path in dropped {
            let is_pdf = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
            if is_pdf {
                self.open_path(&path);
            } else {
                self.chrome.status = Some(format!("{} is not a PDF file", display_name(&path)));
            }
        }
    }

    /// Keys the active tool takes before the shortcuts see them: Escape while a gesture is in
    /// progress, Enter, Delete and Backspace with a gesture or a selection, and the arrow keys
    /// (a nudge) while markups are selected with the Select tool.
    fn tool_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        const KEYS: [(Key, pl_tools::Key); 8] = [
            (Key::Escape, pl_tools::Key::Escape),
            (Key::Enter, pl_tools::Key::Enter),
            (Key::Delete, pl_tools::Key::Delete),
            (Key::Backspace, pl_tools::Key::Backspace),
            (Key::ArrowLeft, pl_tools::Key::Left),
            (Key::ArrowRight, pl_tools::Key::Right),
            (Key::ArrowUp, pl_tools::Key::Up),
            (Key::ArrowDown, pl_tools::Key::Down),
        ];
        let pressed: Vec<(Key, pl_tools::Key, Modifiers)> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if !modifiers.alt => KEYS
                        .iter()
                        .find(|(k, _)| k == key)
                        .map(|(k, t)| (*k, *t, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, tool_key, modifiers) in pressed {
            if self.tool_key(tool_key, modifiers) {
                ctx.input_mut(|input| {
                    input.events.retain(|event| {
                        !matches!(
                            event,
                            egui::Event::Key { key: k, pressed: true, .. } if *k == key
                        )
                    });
                });
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.chrome.password.is_some()
            || self.markup.close_prompt.is_some()
            || self.markup.recovery.is_some()
        {
            return;
        }
        let typing = ctx.text_edit_focused();
        // Which text field has the keyboard, and which had it a frame ago (egui has already
        // taken it away again if Escape was pressed).
        let field = ctx.memory(|m| m.focused()).filter(|_| typing);
        let before = std::mem::replace(&mut self.chrome.text_focus, field);
        self.chrome.escape_field = field.or(before);
        // A focused widget of any kind has the plain keys (see `keyboard_free`).
        let owned = !self.keyboard_free(ctx);
        if !owned {
            self.tool_keys(ctx);
        }
        let mut pending = Vec::new();
        let mut pasted = None;
        ctx.input_mut(|input| {
            for binding in commands::bindings() {
                // While typing, every "plain" binding is the text field's (Ctrl+A and Ctrl+D
                // among them). Another focused control has only the bare keys: a letter,
                // Enter, the arrows.
                let m = binding.shortcut.modifiers;
                let bare = !(m.ctrl || m.command || m.mac_cmd || m.alt);
                if binding.plain && (typing || (owned && bare)) {
                    continue;
                }
                // An exact binding fires with its own modifiers and no others (the tool
                // letters bare, the Measure tools with Shift).
                if binding.exact && !same_modifiers(input.modifiers, binding.shortcut.modifiers) {
                    continue;
                }
                if input.consume_shortcut(&binding.shortcut) {
                    pending.push(binding.command);
                }
            }
            if typing {
                return;
            }
            // The platform turns Ctrl+C, Ctrl+X and Ctrl+V into events rather than key presses.
            for event in &input.events {
                match event {
                    egui::Event::Copy => pending.push(Command::Copy),
                    egui::Event::Cut => pending.push(Command::Cut),
                    egui::Event::Paste(text) => {
                        pasted = Some(text.clone());
                        pending.push(Command::Paste);
                    }
                    _ => {}
                }
            }
        });
        if pasted.is_some() {
            self.pasted_text = pasted;
        }
        pending.dedup();
        for command in pending {
            self.run(ctx, command);
        }
    }
}

impl AppState {
    /// Escape was pressed: when a text field has the keyboard, and it is not one whose own
    /// window Escape closes (the find bar, the note window), the field gives the keyboard up.
    /// Returns whether that is all Escape does this time.
    fn escape_leaves_field(&mut self, ctx: &egui::Context) -> bool {
        // egui takes the keyboard from the focused widget when Escape is pressed, before
        // anything of ours runs: the field is the one that had it when the frame began.
        let Some(field) = self.chrome.escape_field.take() else {
            return false;
        };
        if crate::chrome::is_find_box(field) || crate::textedit::is_note_field(field) {
            return false;
        }
        ctx.memory_mut(|m| m.surrender_focus(field));
        true
    }
}

/// Whether the same modifier keys are held (Ctrl and Command count as one).
fn same_modifiers(a: egui::Modifiers, b: egui::Modifiers) -> bool {
    a.alt == b.alt && a.shift == b.shift && (a.ctrl || a.command) == (b.ctrl || b.command)
}

/// A file's name for messages.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ScriptedDialogs;
    use pl_doc::pl_geom::PageGeometry;
    use pl_doc::pl_render::MockBackend;

    fn app(pages: usize) -> AppState {
        let mock = Arc::new(MockBackend::with_pages(&vec![
            PageGeometry::simple(
                612.0, 792.0
            );
            pages
        ]));
        AppState::new(
            mock,
            AppConfig::default(),
            Box::new(ScriptedDialogs::default()),
        )
    }

    #[test]
    fn length_units_format_points() {
        assert_eq!(LengthUnit::Inches.format(612.0), "8.50");
        assert_eq!(LengthUnit::Millimetres.format(72.0), "25.4");
        assert_eq!(LengthUnit::Points.format(72.0), "72.0");
        assert_eq!(LengthUnit::Inches.suffix(), "in");
        assert_eq!(LengthUnit::Millimetres.suffix(), "mm");
    }

    #[test]
    fn the_page_box_names_the_page_navigated_to() {
        let mut state = app(8);
        let id = state.open_bytes("a.pdf", Vec::new()).unwrap();
        let dv = state.view_mut(id).unwrap();
        // A tall viewport: the last pages cannot reach its top, so "most visible" alone
        // would never report them.
        dv.view
            .set_viewport(pl_doc::pl_geom::Size::new(900.0, 3000.0));
        dv.view.set_zoom(1.0);
        dv.go_to_page(7);
        assert_eq!(dv.shown_page(), 7);
        dv.previous_page();
        assert_eq!(dv.shown_page(), 6);
        dv.next_page();
        dv.next_page();
        assert_eq!(dv.shown_page(), 7, "clamped at the last page");
        // Scrolling by hand hands the answer back to the view.
        dv.view.set_scroll(Vec2::ZERO);
        assert_eq!(dv.shown_page(), 0);
    }

    #[test]
    fn views_and_documents_are_collected_when_tabs_close() {
        let mut state = app(2);
        let a = state.open_bytes("a.pdf", Vec::new()).unwrap();
        let b = state.split_active(true).unwrap();
        assert_eq!(state.session().len(), 1);
        assert_eq!(state.view(a).unwrap().doc, state.view(b).unwrap().doc);
        state.close_view(a);
        assert_eq!(state.session().len(), 1);
        assert_eq!(state.active(), Some(b));
        state.close_view(b);
        assert!(state.session().is_empty());
        assert_eq!(state.active(), None);
        assert!(state.tree.root.is_none());
    }

    #[test]
    fn status_is_empty_without_a_document() {
        assert_eq!(app(1).status_info(), StatusInfo::default());
    }
}
