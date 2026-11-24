//! Markup editing: the active tool, the bridge from pointer and key events to pl-tools and
//! from its intents to pl-doc commands, and the editing commands themselves (undo, save,
//! clipboard, arrange, presets, snapshot).
//!
//! Nothing here draws; [`crate::overlay`] paints what this module leaves in the views, and
//! [`crate::panels`] is the tool bar, the properties panel and the dialogs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{
    AnnotId, AnnotKind, Annotation, FreeText, Origin, PageViewports, PdfDate, Quad, RgbaImage,
    StampSpec, Style,
};
use pl_doc::pl_draw::DrawList;
use pl_doc::pl_geom::{PageGeometry, Point, Rect, Vec2};
use pl_doc::pl_render::DocId;
use pl_doc::pl_scene::SnapModes;
use pl_doc::{
    Clipboard, Command as DocCommand, Document, DocumentId, OverlayItem, PastePlacement,
    PendingJournal, SceneState, TileInvalidation, ViewState,
};
use pl_tools::{
    Cursor, Intent, Modifiers, Pointer, PresetLibrary, TextPreset, ToolInput, ToolKind, ToolPreset,
    ToolState,
};

use crate::app::{display_name, AppState, ViewId};
use crate::commands::Arrange;
use crate::icons::Icon;
use crate::jobs::AfterSave;
use crate::platform::{FakeClipboard, SystemClipboard};
use crate::textedit::{NotePopup, TextEditor};

/// The file the tool presets are kept in, under the app-data directory.
pub const PRESETS_FILE_NAME: &str = "presets.json";
/// The folder crash journals are kept in, under the app-data directory.
pub const JOURNAL_DIR_NAME: &str = "journal";
/// How near the pointer must be to a snap point, in screen pixels.
pub const SNAP_PX: f64 = 9.0;
/// How far a paste lands from what was copied, in page units, per paste.
pub const PASTE_STEP: f64 = 12.0;
/// How long after the last change to a tool's style the presets file is written, in seconds:
/// one write when a slider is let go or the typing pauses, not one per frame.
pub const PRESET_WRITE_DELAY: f64 = 0.4;
/// How many frames a tile invalidation is applied for (see [`AppState::apply_invalidations`]).
const INVALIDATION_FRAMES: u8 = 3;

/// The tools of the tool bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Select,
    Pan,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Polyline,
    Polygon,
    Cloud,
    Pen,
    Highlighter,
    Note,
    /// Text in a box with a border.
    TextBox,
    /// Bare text typed straight onto the page.
    Typewriter,
    /// A text box with a leader pointing at something.
    Callout,
    /// A rectangle with a cloud border, dragged out.
    CloudBox,
    /// A cloud with a callout pointing at it.
    CloudCallout,
    Stamp,
    /// A picture placed on the page.
    Image,
    /// Highlights the text dragged across.
    TextHighlight,
    TextUnderline,
    TextStrikeOut,
    TextSquiggly,
    /// Removes the pen strokes it crosses.
    Eraser,
    /// Sets a page's or a region's scale from two points a known distance apart.
    Calibrate,
    /// Marks a region of the page that has a scale of its own.
    Viewport,
    Length,
    Polylength,
    Perimeter,
    Area,
    /// Cuts a hole out of an area or volume.
    Cutout,
    Volume,
    Angle,
    Radius,
    Diameter,
    Count,
    /// Draws a named region (a room, a zone) that markups are counted to.
    Space,
}

impl Tool {
    pub const ALL: [Tool; 37] = [
        Tool::Select,
        Tool::Pan,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Arrow,
        Tool::Polyline,
        Tool::Polygon,
        Tool::Cloud,
        Tool::CloudBox,
        Tool::Pen,
        Tool::Highlighter,
        Tool::Eraser,
        Tool::Note,
        Tool::TextBox,
        Tool::Typewriter,
        Tool::Callout,
        Tool::CloudCallout,
        Tool::Stamp,
        Tool::Image,
        Tool::TextHighlight,
        Tool::TextUnderline,
        Tool::TextStrikeOut,
        Tool::TextSquiggly,
        Tool::Calibrate,
        Tool::Viewport,
        Tool::Length,
        Tool::Polylength,
        Tool::Perimeter,
        Tool::Area,
        Tool::Cutout,
        Tool::Volume,
        Tool::Angle,
        Tool::Radius,
        Tool::Diameter,
        Tool::Count,
        Tool::Space,
    ];

    /// Where a group of tools begins on the tool bar (indices into [`Tool::ALL`]). The last
    /// group is the Measure group.
    pub const GROUP_STARTS: [usize; 5] = [2, 10, 14, 20, 24];

    /// The measurement tools, as the Measure group of the tool bar shows them.
    pub const MEASURE: [Tool; 13] = [
        Tool::Calibrate,
        Tool::Viewport,
        Tool::Length,
        Tool::Polylength,
        Tool::Perimeter,
        Tool::Area,
        Tool::Cutout,
        Tool::Volume,
        Tool::Angle,
        Tool::Radius,
        Tool::Diameter,
        Tool::Count,
        Tool::Space,
    ];

    /// Whether the tool is one of the Measure group.
    pub fn is_measure(self) -> bool {
        Tool::MEASURE.contains(&self)
    }

    /// Whether the tool writes a value worked out through a scale: it is refused on a page
    /// (or region) with no scale, rather than writing a value at the wrong scale.
    pub fn needs_scale(self) -> bool {
        matches!(
            self,
            Tool::Length
                | Tool::Polylength
                | Tool::Perimeter
                | Tool::Area
                | Tool::Volume
                | Tool::Radius
                | Tool::Diameter
        )
    }

    /// The pl-tools state machine behind the tool; `None` for Pan, which only moves the view.
    pub fn kind(self) -> Option<ToolKind> {
        Some(match self {
            Tool::Pan => return None,
            Tool::Calibrate => ToolKind::Calibrate,
            Tool::Viewport => ToolKind::Viewport,
            Tool::Length => ToolKind::Length,
            Tool::Polylength => ToolKind::Polylength,
            Tool::Perimeter => ToolKind::Perimeter,
            Tool::Area => ToolKind::Area,
            Tool::Cutout => ToolKind::Cutout,
            Tool::Volume => ToolKind::Volume,
            Tool::Angle => ToolKind::Angle,
            Tool::Radius => ToolKind::Radius,
            Tool::Diameter => ToolKind::Diameter,
            Tool::Count => ToolKind::Count,
            // A space is drawn as a polygon and named once it exists.
            Tool::Space => ToolKind::Polygon,
            Tool::Select => ToolKind::Select,
            Tool::Rectangle => ToolKind::Rectangle,
            Tool::Ellipse => ToolKind::Ellipse,
            Tool::Line => ToolKind::Line,
            Tool::Arrow => ToolKind::Arrow,
            Tool::Polyline => ToolKind::Polyline,
            Tool::Polygon => ToolKind::Polygon,
            Tool::Cloud => ToolKind::Cloud,
            Tool::Pen => ToolKind::Pen,
            Tool::Highlighter => ToolKind::Highlighter,
            Tool::Note => ToolKind::Note,
            Tool::TextBox => ToolKind::TextBox,
            Tool::Typewriter => ToolKind::Typewriter,
            Tool::Callout => ToolKind::Callout,
            Tool::CloudBox => ToolKind::CloudBox,
            Tool::CloudCallout => ToolKind::CloudCallout,
            Tool::Stamp => ToolKind::Stamp,
            Tool::Image => ToolKind::Image,
            Tool::TextHighlight => ToolKind::TextHighlight,
            Tool::TextUnderline => ToolKind::TextUnderline,
            Tool::TextStrikeOut => ToolKind::TextStrikeOut,
            Tool::TextSquiggly => ToolKind::TextSquiggly,
            Tool::Eraser => ToolKind::Eraser,
        })
    }

    /// The tool that draws markups in a style of its own; `None` for Select, Pan and the
    /// Eraser, and for the tools that draw nothing of their own (Calibrate, Viewport, Cutout,
    /// and Space, whose regions have one fixed look).
    pub fn drawing(self) -> Option<ToolKind> {
        if matches!(
            self,
            Tool::Calibrate | Tool::Viewport | Tool::Cutout | Tool::Space
        ) {
            return None;
        }
        self.kind()
            .filter(|k| !matches!(k, ToolKind::Select | ToolKind::Eraser))
    }

    /// The tools that mark up the page's own text: they need the text of the pages in view.
    pub fn marks_text(self) -> bool {
        self.kind().is_some_and(ToolKind::is_text_markup)
    }

    /// The tool's name: its accessible name and the first words of its tooltip.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Pan => "Pan",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Arrow => "Arrow",
            Tool::Polyline => "Polyline",
            Tool::Polygon => "Polygon",
            Tool::Cloud => "Cloud",
            Tool::Pen => "Pen",
            Tool::Highlighter => "Highlighter",
            Tool::Note => "Note",
            Tool::TextBox => "Text Box",
            Tool::Typewriter => "Typewriter",
            Tool::Callout => "Callout",
            Tool::CloudBox => "Cloud Box",
            Tool::CloudCallout => "Cloud Callout",
            Tool::Stamp => "Stamp",
            Tool::Image => "Image",
            Tool::TextHighlight => "Highlight Text",
            Tool::TextUnderline => "Underline Text",
            Tool::TextStrikeOut => "Strikeout Text",
            Tool::TextSquiggly => "Squiggly Underline",
            Tool::Eraser => "Eraser",
            Tool::Calibrate => "Calibrate",
            Tool::Viewport => "Viewport",
            Tool::Length => "Length",
            Tool::Polylength => "Polylength",
            Tool::Perimeter => "Perimeter",
            Tool::Area => "Area",
            Tool::Cutout => "Cutout",
            Tool::Volume => "Volume",
            Tool::Angle => "Angle",
            Tool::Radius => "Radius",
            Tool::Diameter => "Diameter",
            Tool::Count => "Count",
            Tool::Space => "Space",
        }
    }

    /// The modifier keys held with [`Tool::key`]: none for the markup tools, Shift for the
    /// Measure group (whose letters are those of the markup tools).
    pub fn modifiers(self) -> egui::Modifiers {
        if self.is_measure() {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        }
    }

    /// The key and modifiers that pick the tool.
    pub fn shortcut(self) -> egui::KeyboardShortcut {
        egui::KeyboardShortcut::new(self.modifiers(), self.key())
    }

    /// The key that picks the tool (with [`Tool::modifiers`] held).
    pub fn key(self) -> egui::Key {
        use egui::Key;
        match self {
            Tool::Select => Key::V,
            Tool::Pan => Key::H,
            Tool::Rectangle => Key::R,
            Tool::Ellipse => Key::E,
            Tool::Line => Key::L,
            Tool::Arrow => Key::A,
            Tool::Polyline => Key::Y,
            Tool::Polygon => Key::G,
            Tool::Cloud => Key::C,
            Tool::Pen => Key::P,
            Tool::Highlighter => Key::M,
            Tool::Note => Key::N,
            // These are the app's own letters: F for text typed free of a box, O for a
            // callOut, Q for a cloud box (the shape of the letter), K for a cloud callout.
            Tool::TextBox => Key::T,
            Tool::Typewriter => Key::F,
            Tool::Callout => Key::O,
            Tool::CloudBox => Key::Q,
            Tool::CloudCallout => Key::K,
            Tool::Stamp => Key::B,
            Tool::Image => Key::I,
            Tool::TextHighlight => Key::J,
            Tool::TextUnderline => Key::U,
            Tool::TextStrikeOut => Key::W,
            Tool::TextSquiggly => Key::Z,
            Tool::Eraser => Key::X,
            // The Measure group, with Shift: K for a calibration, W for a window of the
            // sheet, R for peRimeter, U for cUtout, G for anGle, D for raDius, M for
            // diaMeter, O for a rOom.
            Tool::Calibrate => Key::K,
            Tool::Viewport => Key::W,
            Tool::Length => Key::L,
            Tool::Polylength => Key::P,
            Tool::Perimeter => Key::R,
            Tool::Area => Key::A,
            Tool::Cutout => Key::U,
            Tool::Volume => Key::V,
            Tool::Angle => Key::G,
            Tool::Radius => Key::D,
            Tool::Diameter => Key::M,
            Tool::Count => Key::C,
            Tool::Space => Key::O,
        }
    }

    /// What the status bar says while the tool is active.
    pub fn hint(self) -> &'static str {
        match self {
            Tool::Select => {
                "Click a markup to select it, drag it to move it, drag a handle to reshape it. \
                 Drag across text to select text."
            }
            Tool::Pan => "Drag to move the page.",
            Tool::Rectangle | Tool::Ellipse | Tool::CloudBox => {
                "Drag to draw. Shift keeps it square, Ctrl draws from the centre."
            }
            Tool::Line | Tool::Arrow => "Drag to draw. Shift keeps to 45 degree steps.",
            Tool::Polyline => {
                "Click each point. Double-click or Enter finishes, Backspace takes a point back."
            }
            Tool::Polygon | Tool::Cloud => {
                "Click each corner. Double-click, Enter or the first point closes the shape."
            }
            Tool::Pen | Tool::Highlighter => "Drag to draw freehand. Shift draws a straight line.",
            Tool::Note => "Click where the note goes, then type its text under Properties.",
            Tool::TextBox => {
                "Drag the box, or click for a standard one, then type. Ctrl+Enter finishes, \
                 Esc cancels."
            }
            Tool::Typewriter => {
                "Click where the text starts, then type. Ctrl+Enter finishes, Esc cancels."
            }
            Tool::Callout => {
                "Click what it points at, optionally click a bend, then drag the text box and type."
            }
            Tool::CloudCallout => "Drag the cloud, then drag the text box and type.",
            Tool::Stamp => {
                "Click to place the stamp, or drag to size it. Pick the stamp under Properties."
            }
            Tool::Image => "Click to place the picture, or drag to size it. Shift frees its shape.",
            Tool::TextHighlight
            | Tool::TextUnderline
            | Tool::TextStrikeOut
            | Tool::TextSquiggly => "Drag across the page's text to mark it.",
            Tool::Eraser => "Drag across pen strokes to remove them.",
            Tool::Calibrate => {
                "Click two points a known distance apart (drag a region first to calibrate only \
                 that region), then enter the distance."
            }
            Tool::Viewport => "Drag the region that has a scale of its own, then give its scale.",
            Tool::Length => "Click both ends, or drag. Shift keeps to angle steps.",
            Tool::Polylength => {
                "Click each point. Double-click or Enter finishes, Backspace takes a point back."
            }
            Tool::Perimeter | Tool::Area | Tool::Volume => {
                "Click each corner (the first point closes the outline), or drag a rectangle."
            }
            Tool::Cutout => "Click inside an area, then click the corners of the hole.",
            Tool::Angle => "Click the end of one arm, the vertex, then the end of the other arm.",
            Tool::Radius | Tool::Diameter => "Click the centre, then a point on the circle.",
            Tool::Count => "Click each item to count. Alt-click removes one, Enter ends the count.",
            Tool::Space => "Click each corner of the room or zone; it is named when it closes.",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Tool::Select => Icon::Pointer,
            Tool::Pan => Icon::Pan,
            Tool::Rectangle => Icon::Rectangle,
            Tool::Ellipse => Icon::Ellipse,
            Tool::Line => Icon::Line,
            Tool::Arrow => Icon::Arrow,
            Tool::Polyline => Icon::Polyline,
            Tool::Polygon => Icon::Polygon,
            Tool::Cloud => Icon::Cloud,
            Tool::Pen => Icon::Pen,
            Tool::Highlighter => Icon::Highlighter,
            Tool::Note => Icon::Note,
            Tool::TextBox => Icon::TextBox,
            Tool::Typewriter => Icon::Typewriter,
            Tool::Callout => Icon::Callout,
            Tool::CloudBox => Icon::CloudBox,
            Tool::CloudCallout => Icon::CloudCallout,
            Tool::Stamp => Icon::Stamp,
            Tool::Image => Icon::Picture,
            Tool::TextHighlight => Icon::TextHighlight,
            Tool::TextUnderline => Icon::TextUnderline,
            Tool::TextStrikeOut => Icon::TextStrikeOut,
            Tool::TextSquiggly => Icon::TextSquiggly,
            Tool::Eraser => Icon::Eraser,
            Tool::Calibrate => Icon::Calibrate,
            Tool::Viewport => Icon::Viewport,
            Tool::Length => Icon::MeasureLength,
            Tool::Polylength => Icon::Polylength,
            Tool::Perimeter => Icon::Perimeter,
            Tool::Area => Icon::MeasureArea,
            Tool::Cutout => Icon::Cutout,
            Tool::Volume => Icon::Volume,
            Tool::Angle => Icon::Angle,
            Tool::Radius => Icon::Radius,
            Tool::Diameter => Icon::Diameter,
            Tool::Count => Icon::Count,
            Tool::Space => Icon::Space,
        }
    }

    /// The tooltip: the name and the key.
    pub fn tooltip(self) -> String {
        if self.is_measure() {
            format!("{} (Shift+{})", self.label(), self.key().name())
        } else {
            format!("{} ({})", self.label(), self.key().name())
        }
    }
}

// How a save is to be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveHow {
    /// Into the document's own file, as an incremental update.
    InPlace,
    /// To a file the user names.
    As,
    /// A full rewrite of the document's own file.
    Optimised,
}

/// What a close prompt was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClosePrompt {
    pub view: ViewId,
    /// The whole window is closing: go on to the next modified document afterwards.
    pub quitting: bool,
}

/// The answer to a close prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// Text boxes of the properties panel: what the user is typing, and which markup it is for.
#[derive(Default)]
pub(crate) struct PropertyFields {
    pub author: String,
    pub subject: String,
    pub comment: String,
    pub stroke_hex: String,
    pub fill_hex: String,
    pub text_hex: String,
    pub preset_name: String,
    /// Bumped whenever a text box or slider starts a new interaction, so that one interaction
    /// is one undo step and the next is another.
    pub session: u64,
    pub focus_comment: bool,
}

/// Each page's overlay items, with the edit revision they were computed at.
pub(crate) type OverlayCache = HashMap<(DocumentId, u32), (u64, Arc<Vec<OverlayItem>>)>;

/// Everything about markup editing that is not tied to one view.
pub(crate) struct MarkupState {
    pub tool: Tool,
    pub presets: PresetLibrary,
    pub presets_path: Option<PathBuf>,
    /// The presets differ from the file; when they last changed (egui time, seconds).
    pub presets_dirty: Option<f64>,
    pub snap: bool,
    pub author: String,
    pub clipboard: Option<Clipboard>,
    pub paste_serial: u32,
    pub system_clipboard: Box<dyn SystemClipboard>,
    /// The next drag on a canvas picks a region to copy as an image.
    pub snapshot_pick: bool,
    /// The size of the image last copied.
    pub last_snapshot: Option<(u32, u32)>,
    pub properties_open: bool,
    pub fields: PropertyFields,
    pub close_prompt: Option<ClosePrompt>,
    pub recovery: Option<DocumentId>,
    /// The user has answered for every modified document; the window may close.
    pub quit_confirmed: bool,
    pub overlay_cache: OverlayCache,
    /// Documents whose layers could not be read; they are not asked again.
    pub layers_failed: Vec<DocumentId>,
    /// The picture the Image tool places.
    pub image: Option<Arc<RgbaImage>>,
    /// The in-place text editor, while a text box is being typed into.
    pub text_editor: Option<TextEditor>,
    /// The note window, while a note's text, status and replies are shown.
    pub note: Option<NotePopup>,
    /// Counts the eraser's drags: each is one undo step.
    pub erase_drag: u64,
    selection_marks: Option<Arc<SelectionMarks>>,
    invalidations: Vec<(TileInvalidation, u8)>,
}

impl MarkupState {
    pub(crate) fn new(data_dir: Option<&Path>) -> MarkupState {
        let presets_path = data_dir.map(|d| d.join(PRESETS_FILE_NAME));
        let presets = presets_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|json| serde_json::from_str::<PresetLibrary>(&json).ok())
            .unwrap_or_default();
        let author = std::env::var("USERNAME")
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Author".to_owned());
        MarkupState {
            tool: Tool::Select,
            presets,
            presets_path,
            presets_dirty: None,
            snap: false,
            author,
            clipboard: None,
            paste_serial: 0,
            // Nothing touches the real clipboard until the window installs it.
            system_clipboard: Box::new(FakeClipboard::default()),
            snapshot_pick: false,
            last_snapshot: None,
            properties_open: false,
            fields: PropertyFields::default(),
            close_prompt: None,
            recovery: None,
            quit_confirmed: false,
            overlay_cache: HashMap::new(),
            layers_failed: Vec::new(),
            image: None,
            text_editor: None,
            note: None,
            erase_drag: 0,
            selection_marks: None,
            invalidations: Vec::new(),
        }
    }

    /// Writes the presets to the app-data directory. Returns what went wrong, if anything.
    pub(crate) fn store_presets(&self) -> Result<(), String> {
        let Some(path) = &self.presets_path else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(&self.presets).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// What the canvas marks of the selection, worked out once for each selection and state
    /// of the document, however many frames show it and however much is selected.
    pub(crate) fn selection_marks(
        &mut self,
        doc: &Document,
        view: &ViewState,
    ) -> Arc<SelectionMarks> {
        let key = (doc.id(), doc.selection_serial(), doc.edit_revision());
        if let Some(marks) = &self.selection_marks {
            if marks.key == key {
                return marks.clone();
            }
        }
        let selection = doc.selection();
        let mut on_page: HashMap<u32, Vec<AnnotId>> = HashMap::new();
        for id in selection {
            if let Some(a) = doc.annotation(id) {
                on_page.entry(a.page as u32).or_default().push(id.clone());
            }
        }
        let first_page = selection
            .first()
            .and_then(|id| doc.annotation(id))
            .map(|a| a.page as u32);
        let tool_view = ToolView::new(doc, view, &self.author, first_page.unwrap_or(0));
        let group = first_page
            .filter(|_| selection.len() > 1)
            .and_then(|page| pl_tools::group_bounds(&tool_view, page as usize));
        let handles = match first_page {
            Some(_) => pl_tools::selection_handles(&tool_view),
            None => Vec::new(),
        };
        let marks = Arc::new(SelectionMarks {
            key,
            on_page,
            first_page,
            group,
            handles,
        });
        self.selection_marks = Some(marks.clone());
        marks
    }

    /// A page's overlay items, computed once per edit revision.
    pub(crate) fn overlay(&mut self, doc: &Document, page: u32) -> Arc<Vec<OverlayItem>> {
        let revision = doc.edit_revision();
        let key = (doc.id(), page);
        if let Some((at, items)) = self.overlay_cache.get(&key) {
            if *at == revision {
                return items.clone();
            }
        }
        if self.overlay_cache.len() > 256 {
            self.overlay_cache.clear();
        }
        let items = Arc::new(doc.overlay(page));
        self.overlay_cache.insert(key, (revision, items.clone()));
        items
    }
}

/// What the canvas draws for the selection ([`MarkupState::selection_marks`]).
pub(crate) struct SelectionMarks {
    /// The document, its selection and its state this was worked out for.
    key: (DocumentId, u64, u64),
    /// The selected markups of each page that has any.
    pub on_page: HashMap<u32, Vec<AnnotId>>,
    /// The page of the first selected markup: the one the handles are shown on.
    pub first_page: Option<u32>,
    /// The box around several selected markups of that page.
    pub group: Option<Rect>,
    /// The handles: one markup's own, or those of the box around several.
    pub handles: Vec<pl_doc::pl_annot::Handle>,
}

/// What pl-tools sees of a document through one view.
pub(crate) struct ToolView<'a> {
    pub doc: &'a Document,
    pub view: &'a ViewState,
    pub author: &'a str,
    /// The page whose scale `pixel_size` reports.
    pub pixel_page: u32,
    /// `pixel_page`'s scales with the document's default scale standing in for a page scale
    /// it does not have, so a tool measures there as pl-doc would.
    defaulted: Option<PageViewports>,
}

impl<'a> ToolView<'a> {
    pub(crate) fn new(
        doc: &'a Document,
        view: &'a ViewState,
        author: &'a str,
        pixel_page: u32,
    ) -> ToolView<'a> {
        let defaulted = doc.editor().and_then(|e| {
            let store = e.scales();
            let vps = store.page(pixel_page as usize)?;
            if vps.page_scale.is_some() {
                return None;
            }
            let setting = store.settings().default_scale.as_ref()?;
            let spec = setting.spec(store.user_unit(pixel_page as usize)).ok()?;
            let mut vps = vps.clone();
            vps.page_scale = Some(spec);
            Some(vps)
        });
        ToolView {
            doc,
            view,
            author,
            pixel_page,
            defaulted,
        }
    }
}

impl pl_tools::DocView for ToolView<'_> {
    fn viewports(&self, page: usize) -> Option<&PageViewports> {
        if page == self.pixel_page as usize {
            if let Some(vps) = &self.defaulted {
                return Some(vps);
            }
        }
        self.doc.page_scales(u32::try_from(page).ok()?)
    }

    fn page(&self, page: usize) -> Option<PageGeometry> {
        let page = u32::try_from(page).ok()?;
        // Page coordinates mean what the file means by them only once the page's real
        // geometry is known.
        if !self.doc.geometry_is_exact(page) {
            return None;
        }
        self.view.page_geometry(page).copied()
    }

    fn annotations(&self, page: usize) -> Vec<&Annotation> {
        self.doc
            .editor()
            .map(|e| e.set().on_page(page).collect())
            .unwrap_or_default()
    }

    fn annotation(&self, id: &AnnotId) -> Option<&Annotation> {
        self.doc.annotation(id)
    }

    fn selection(&self) -> &[AnnotId] {
        self.doc.selection()
    }

    fn pixel_size(&self) -> f64 {
        pixel_size(self.view, self.pixel_page)
    }

    fn origin(&self) -> Origin {
        Origin::now(self.author)
    }

    fn text_quads(&self, page: usize, from: Point, to: Point) -> Vec<Quad> {
        // The page's text as the scheduler last delivered it: a drag over a page whose text
        // has not arrived yet marks nothing (the canvas asks for it while such a tool is
        // active).
        let Ok(page) = u32::try_from(page) else {
            return Vec::new();
        };
        if !self.doc.geometry_is_exact(page) {
            return Vec::new();
        }
        self.doc
            .cached_text(page)
            .and_then(|text| pl_doc::markup_between(&text, from, to))
            .map(|selection| selection.quads)
            .unwrap_or_default()
    }
}

/// False for a new measurement whose value depends on a scale (every kind but Angle and Count)
/// when no scale of the document applies to all of its points; true for everything else.
fn scale_covers(doc: &Document, a: &Annotation) -> bool {
    use pl_doc::pl_annot::MeasureKind;
    let Some(m) = a.measurement() else {
        return true;
    };
    if matches!(m.kind, MeasureKind::Angle | MeasureKind::Count) {
        return true;
    }
    let Some(editor) = doc.editor() else {
        return true;
    };
    let points = pl_doc::measure_ops::geometry_points(&a.geometry);
    editor.scales().resolve(a.page, &points).is_some()
}

/// The text of the page a text markup lies over: the characters whose middles are inside
/// its quads, from the first to the last in reading order, line breaks as spaces. `None` when
/// the page's text is not cached or nothing is under the markup.
fn text_under(doc: &Document, markup: &Annotation) -> Option<String> {
    let pl_doc::pl_annot::Geometry::Quads(quads) = &markup.geometry else {
        return None;
    };
    let text = doc.cached_text(u32::try_from(markup.page).ok()?)?;
    let boxes: Vec<Rect> = quads
        .iter()
        .map(|q| {
            let xs = q.iter().map(|p| p.x);
            let ys = q.iter().map(|p| p.y);
            Rect::new(
                xs.clone().fold(f64::INFINITY, f64::min),
                ys.clone().fold(f64::INFINITY, f64::min),
                xs.fold(f64::NEG_INFINITY, f64::max),
                ys.fold(f64::NEG_INFINITY, f64::max),
            )
        })
        .collect();
    let inside = |c: &pl_doc::pl_render::TextChar| {
        let (x, y) = ((c.bbox[0] + c.bbox[2]) / 2.0, (c.bbox[1] + c.bbox[3]) / 2.0);
        c.has_box()
            && boxes
                .iter()
                .any(|b| x >= b.x0 && x <= b.x1 && y >= b.y0 && y <= b.y1)
    };
    let first = text.chars.iter().position(inside)?;
    let last = text.chars.iter().rposition(inside)?;
    let mut out = String::new();
    for c in &text.chars[first..=last] {
        match c.ch {
            '\r' | '\n' => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            ch => out.push(ch),
        }
    }
    let out = out.trim().to_owned();
    (!out.is_empty()).then_some(out)
}

/// The size of one screen pixel on `page`, in that page's units.
pub(crate) fn pixel_size(view: &ViewState, page: u32) -> f64 {
    view.view_transform(page)
        .zip(view.page_geometry(page))
        .map(|(transform, geometry)| 1.0 / transform.logical_px_per_unit(geometry))
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(1.0)
}

pub(crate) fn tool_modifiers(m: egui::Modifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        ctrl: m.ctrl || m.command,
        alt: m.alt,
    }
}

/// A style edit from the properties panel.
pub(crate) type StyleEdit<'a> = &'a dyn Fn(&AnnotKind, &mut Style);

impl AppState {
    // ---- reading -----------------------------------------------------------------------------

    pub fn tool(&self) -> Tool {
        self.markup.tool
    }

    /// Makes `tool` the active tool. A gesture in progress is abandoned.
    pub fn set_tool(&mut self, tool: Tool) {
        if self.markup.tool == tool {
            return;
        }
        self.commit_text_edit();
        if tool == Tool::Image && self.markup.image.is_none() && !self.pick_image() {
            // No picture to place: the tool that was active stays.
            return;
        }
        self.markup.tool = tool;
        self.markup.snapshot_pick = false;
        // A question the previous tool asked has no one to answer it now.
        self.measure.request = None;
        for id in self.view_ids() {
            self.sync_tool(id);
        }
    }

    pub fn snap_enabled(&self) -> bool {
        self.markup.snap
    }

    pub fn set_snap(&mut self, on: bool) {
        self.markup.snap = on;
        if !on {
            for dv in self.views.values_mut() {
                dv.snap_mark = None;
            }
        }
    }

    /// The name new markups are signed with (`/T`).
    pub fn author(&self) -> &str {
        &self.markup.author
    }

    pub fn set_author(&mut self, author: &str) {
        self.markup.author = author.to_owned();
    }

    /// Replaces the system clipboard the app writes text and images to.
    pub fn set_clipboard(&mut self, clipboard: Box<dyn SystemClipboard>) {
        self.markup.system_clipboard = clipboard;
    }

    pub fn presets(&self) -> &PresetLibrary {
        &self.markup.presets
    }

    pub fn close_prompt_open(&self) -> bool {
        self.markup.close_prompt.is_some()
    }

    pub fn recovery_prompt_open(&self) -> bool {
        self.markup.recovery.is_some()
    }

    pub fn properties_panel_open(&self) -> bool {
        self.markup.properties_open
    }

    /// The document a view shows.
    pub fn document_of(&self, view: ViewId) -> Option<&Document> {
        let dv = self.views.get(&view)?;
        self.session.document(dv.doc).ok()
    }

    /// The active view's document.
    pub fn active_document(&self) -> Option<&Document> {
        self.active.and_then(|id| self.document_of(id))
    }

    pub(crate) fn active_doc_id(&self) -> Option<DocumentId> {
        self.active_view().map(|v| v.doc)
    }

    /// The annotations of the active document, bottom of each page's stack first; empty
    /// before anything has needed them.
    pub fn markups(&self) -> Vec<Annotation> {
        self.active_document()
            .and_then(Document::editor)
            .map(|e| e.model())
            .unwrap_or_default()
    }

    /// The selected markups of the active document.
    pub fn selected_markups(&self) -> Vec<&Annotation> {
        let Some(doc) = self.active_document() else {
            return Vec::new();
        };
        doc.selection()
            .iter()
            .filter_map(|id| doc.annotation(id))
            .collect()
    }

    /// How many markups of the active document are selected. The menus and panels ask this
    /// every frame, so it does not look the markups up.
    pub fn selected_count(&self) -> usize {
        self.active_document().map_or(0, |d| d.selection().len())
    }

    /// The first selected markup of the active document: the one whose settings are shown.
    pub fn first_selected(&self) -> Option<&Annotation> {
        let doc = self.active_document()?;
        doc.selection().first().and_then(|id| doc.annotation(id))
    }

    /// Whether the document a view shows has unsaved markup changes.
    pub fn is_modified(&self, view: ViewId) -> bool {
        self.document_of(view).is_some_and(Document::is_modified)
    }

    /// A view's tab title: the document's name, with a mark when it has unsaved changes.
    pub fn view_title(&self, view: ViewId) -> String {
        let Some(dv) = self.views.get(&view) else {
            return String::new();
        };
        if self.is_modified(view) {
            format!("{} *", dv.title)
        } else {
            dv.title.clone()
        }
    }

    /// The preview of the gesture in progress in a view (page space), with its page.
    pub fn preview(&self, view: ViewId) -> Option<(u32, &DrawList)> {
        let dv = self.views.get(&view)?;
        dv.preview_page.map(|page| (page, &dv.preview))
    }

    /// The snap point shown in a view: the page and the point snapped to.
    pub fn snap_marker(&self, view: ViewId) -> Option<(u32, Point)> {
        self.views.get(&view).and_then(|dv| dv.snap_mark)
    }

    /// The size of the image last copied by "Copy snapshot".
    pub fn last_snapshot(&self) -> Option<(u32, u32)> {
        self.markup.last_snapshot
    }

    // ---- tools -------------------------------------------------------------------------------

    /// Brings a view's tool state in line with the active tool and its current style.
    pub(crate) fn sync_tool(&mut self, id: ViewId) {
        let kind = self.markup.tool.kind().unwrap_or(ToolKind::Select);
        let preset = self.markup.presets.current(kind);
        let image = self
            .markup
            .image
            .clone()
            .filter(|_| kind == ToolKind::Image);
        let Some(dv) = self.views.get_mut(&id) else {
            return;
        };
        let same_image = match (dv.tool.image(), &image) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if dv.tool.tool() == kind && *dv.tool.preset() == preset && same_image {
            return;
        }
        let changed_tool = dv.tool.tool() != kind;
        let state = std::mem::replace(&mut dv.tool, ToolState::for_tool(kind));
        dv.tool = state.with_preset(preset);
        if let Some(image) = image {
            let state = std::mem::replace(&mut dv.tool, ToolState::for_tool(kind));
            dv.tool = state.with_image(image);
        }
        if changed_tool {
            dv.pointer_down = false;
            dv.tool_cursor = None;
            dv.snap_mark = None;
            if !dv.preview.is_empty() {
                dv.preview = DrawList::new();
                dv.preview_serial += 1;
            }
            dv.preview_page = None;
        }
    }

    /// Where a view-space position is for a tool: the page (the gesture's own page while one
    /// is in progress), the page point, and the snap point if snapping is on and there is one.
    pub(crate) fn pointer_at(
        &mut self,
        id: ViewId,
        local: Point,
        modifiers: egui::Modifiers,
    ) -> Option<Pointer> {
        let snap_on = self.markup.snap;
        let dv = self.views.get_mut(&id)?;
        let page = match dv.tool.page() {
            Some(page) => page as u32,
            None => dv.view.hit_test_nearest(local)?.page,
        };
        let transform = dv.view.view_transform(page)?;
        let geometry = dv.view.page_geometry(page)?;
        let point = transform.view_to_page(geometry) * local;
        let mut pointer =
            Pointer::new(page as usize, point).with_modifiers(tool_modifiers(modifiers));
        dv.snap_mark = None;
        dv.snap_kind = None;
        let freehand = matches!(dv.tool.tool(), ToolKind::Pen | ToolKind::Highlighter);
        let measuring = dv.tool.tool().is_measure() || self.markup.tool == Tool::Space;
        let toggles = self.measure.snap;
        if snap_on && !freehand && !modifiers.alt {
            let radius = SNAP_PX * pixel_size(&dv.view, page);
            let kinds = dv.tool.snap_kinds();
            if let Ok(doc) = self.session.document_mut(dv.doc) {
                if doc.geometry_is_exact(page) && measuring {
                    // A measurement snaps to the points of markups and measurements first,
                    // then to the drawing, as far as the tool and the toggles allow.
                    let options = pl_doc::SnapOptions {
                        radius,
                        content: SnapModes {
                            endpoint: toggles.endpoint && kinds.endpoint,
                            intersection: toggles.intersection && kinds.intersection,
                            midpoint: toggles.midpoint && kinds.midpoint,
                            nearest: toggles.nearest && kinds.nearest,
                        },
                        measurements: toggles.markups,
                        markups: toggles.markups,
                        // A Count click near a marker is a new item, not the same one again.
                        counts: dv.tool.tool() != ToolKind::Count,
                        exclude: None,
                    };
                    match doc.snap(page, point, &options) {
                        pl_doc::SnapOutcome::Snapped(target) => {
                            pointer = pointer.with_snap(target.point);
                            dv.snap_mark = Some((page, target.point));
                            dv.snap_kind = Some(target.kind);
                        }
                        pl_doc::SnapOutcome::Pending => self.request_repaint_soon(),
                        pl_doc::SnapOutcome::Nothing => {}
                    }
                } else if doc.geometry_is_exact(page) {
                    match doc.scene(page) {
                        SceneState::Ready(scene) => {
                            let modes = SnapModes {
                                endpoint: toggles.endpoint,
                                intersection: toggles.intersection,
                                midpoint: toggles.midpoint,
                                nearest: false,
                            };
                            if let Some(hit) = scene.snap(point, radius, modes) {
                                pointer = pointer.with_snap(hit.point);
                                dv.snap_mark = Some((page, hit.point));
                                dv.snap_kind = Some(match hit.kind {
                                    pl_doc::pl_scene::SnapKind::Endpoint => {
                                        pl_doc::SnapKind::Endpoint
                                    }
                                    pl_doc::pl_scene::SnapKind::Intersection => {
                                        pl_doc::SnapKind::Intersection
                                    }
                                    pl_doc::pl_scene::SnapKind::Midpoint => {
                                        pl_doc::SnapKind::Midpoint
                                    }
                                    pl_doc::pl_scene::SnapKind::Nearest => {
                                        pl_doc::SnapKind::Nearest
                                    }
                                });
                            }
                        }
                        SceneState::Pending => self.request_repaint_soon(),
                        SceneState::Unavailable(_) => {}
                    }
                }
            }
        }
        Some(pointer)
    }

    /// Whether a press at `pointer` with the Select tool lands on a markup or one of the
    /// selection's handles (as opposed to text or empty paper).
    pub(crate) fn press_hits_markup(&self, id: ViewId, pointer: &Pointer) -> bool {
        let Some(dv) = self.views.get(&id) else {
            return false;
        };
        let Ok(doc) = self.session.document(dv.doc) else {
            return false;
        };
        let Some(editor) = doc.editor() else {
            return false;
        };
        let px = pixel_size(&dv.view, pointer.page as u32);
        if editor
            .set()
            .hit_test(pointer.page, pointer.point, pl_tools::HIT_PX * px)
            .is_some()
        {
            return true;
        }
        // A handle of the one selected markup (they reach beyond the markup itself).
        match doc.selection() {
            [only] => doc.annotation(only).is_some_and(|a| {
                a.page == pointer.page
                    && !a.meta.flags.is_locked()
                    && (pl_tools::grab_handle(a, pointer.point, px).is_some()
                        // A measurement's own grips: its dimension line's offset, its value,
                        // the corners of its cutouts.
                        || pl_tools::measure_handles(a).iter().any(|h| {
                            (h.at - pointer.point).hypot() <= pl_tools::HIT_PX * px * 1.5
                        }))
            }),
            _ => false,
        }
    }

    /// Gives one input to a view's tool and carries out what it asks for.
    pub(crate) fn tool_event(&mut self, id: ViewId, input: ToolInput) {
        if self.markup.tool.kind().is_none() {
            return;
        }
        self.sync_tool(id);
        let Some(doc_id) = self.views.get(&id).map(|v| v.doc) else {
            return;
        };
        let (loaded, saving) = self
            .session
            .document(doc_id)
            .map_or((false, false), |d| (d.annots_loaded(), d.is_saving()));
        if saving {
            // The document is read-only until its save has finished.
            return;
        }
        if !loaded {
            // The annotations are read in the background; hovering does not ask for them.
            if matches!(input, ToolInput::PointerMove(_)) || !self.ensure_annots(doc_id) {
                return;
            }
        }
        if self.measure.request.is_some() {
            // A measurement tool is waiting for its dialog.
            return;
        }
        // A value at the wrong scale is worse than none: where there is no scale, the tools
        // that write a scaled value ask for one instead of starting.
        if let ToolInput::PointerDown(p) = &input {
            let busy = self.views.get(&id).is_some_and(|dv| dv.tool.is_busy());
            if self.markup.tool.needs_scale()
                && !busy
                && !self.scale_known(doc_id, p.page, p.effective())
            {
                if let Some(dv) = self.views.get_mut(&id) {
                    dv.pointer_down = false;
                }
                self.prompt_for_scale(p.page as u32);
                return;
            }
        }
        let Some(dv) = self.views.get_mut(&id) else {
            return;
        };
        let Ok(doc) = self.session.document(doc_id) else {
            return;
        };
        let pixel_page = match &input {
            ToolInput::PointerDown(p)
            | ToolInput::PointerMove(p)
            | ToolInput::PointerUp(p)
            | ToolInput::DoubleClick(p) => p.page as u32,
            ToolInput::Key { .. } => dv
                .tool
                .page()
                .map(|p| p as u32)
                .or_else(|| {
                    doc.selection()
                        .first()
                        .and_then(|id| doc.annotation(id))
                        .map(|a| a.page as u32)
                })
                .unwrap_or_else(|| dv.shown_page()),
        };
        let view = ToolView::new(doc, &dv.view, &self.markup.author, pixel_page);
        let kind = dv.tool.tool();
        let before_page = dv.tool.page();
        let state = std::mem::replace(&mut dv.tool, ToolState::for_tool(kind));
        let (state, intents, preview) = pl_tools::step(state, &input, &view);
        dv.tool = state;
        dv.readout = dv.tool.readout(&view);
        if preview != dv.preview {
            dv.preview = preview;
            dv.preview_serial += 1;
        }
        dv.preview_page = if dv.preview.is_empty() {
            None
        } else {
            dv.tool
                .page()
                .or(before_page)
                .map(|p| p as u32)
                .or(dv.preview_page)
                .or(Some(pixel_page))
        };
        // The eraser works as it goes, a command for every move that touches a stroke. One
        // drag is still one thing to undo: its commands share a grouping key.
        let group = (kind == ToolKind::Eraser).then(|| {
            if matches!(input, ToolInput::PointerDown(_)) {
                self.markup.erase_drag += 1;
            }
            pl_doc::GROUP_KEY | self.markup.erase_drag
        });
        self.apply_intents_grouped(id, intents, group);
    }

    /// Gives a measurement tool the answer to what it asked ([`Intent::RequestMeasure`]) and
    /// carries out what it does with it.
    pub(crate) fn answer_measure(&mut self, id: ViewId, input: pl_tools::MeasureInput) {
        let Some(doc_id) = self.views.get(&id).map(|v| v.doc) else {
            return;
        };
        let Some(dv) = self.views.get_mut(&id) else {
            return;
        };
        let Ok(doc) = self.session.document(doc_id) else {
            return;
        };
        let page = dv.tool.page().map_or_else(|| dv.shown_page(), |p| p as u32);
        let view = ToolView::new(doc, &dv.view, &self.markup.author, page);
        let kind = dv.tool.tool();
        let state = std::mem::replace(&mut dv.tool, ToolState::for_tool(kind));
        let (state, intents, preview) = pl_tools::step_measure(state, &input, &view);
        dv.tool = state;
        dv.readout = dv.tool.readout(&view);
        if preview != dv.preview {
            dv.preview = preview;
            dv.preview_serial += 1;
        }
        dv.preview_page = if dv.preview.is_empty() {
            None
        } else {
            Some(page)
        };
        self.apply_intents(id, intents);
    }

    /// Carries out a tool's intents: the changes as one undo step, then the selection.
    pub(crate) fn apply_intents(&mut self, id: ViewId, intents: Vec<Intent>) {
        self.apply_intents_grouped(id, intents, None);
    }

    /// As [`apply_intents`](AppState::apply_intents); with `group`, the changes join the undo
    /// step of the changes made with the same key just before them.
    fn apply_intents_grouped(&mut self, id: ViewId, intents: Vec<Intent>, group: Option<u64>) {
        let Some(doc_id) = self.views.get(&id).map(|v| v.doc) else {
            return;
        };
        let mut commands: Vec<DocCommand> = Vec::new();
        let mut select: Option<Vec<AnnotId>> = None;
        let mut cursor = None;
        let mut text_edit = None;
        let mut scale_changes = Vec::new();
        let mut request = None;
        {
            let Ok(doc) = self.session.document(doc_id) else {
                return;
            };
            let now = PdfDate::now();
            for intent in intents {
                match intent {
                    Intent::Create(mut annotation) => {
                        // A markup of the page's own text says which text, as its comment.
                        if annotation.kind.is_text_markup() && annotation.meta.contents.is_none() {
                            annotation.meta.contents = text_under(doc, &annotation);
                        }
                        // A new measurement whose value hangs on a scale is made only where
                        // one applies to all its points (the first click was checked; the
                        // last may be outside the viewport it started in).
                        if !scale_covers(doc, &annotation) {
                            request = Some(pl_tools::MeasureRequest::NoScale {
                                page: annotation.page,
                            });
                            continue;
                        }
                        if let Some(DocCommand::AddAnnotations(list)) = commands.last_mut() {
                            list.push(*annotation);
                        } else {
                            commands.push(DocCommand::AddAnnotations(vec![*annotation]));
                        }
                    }
                    Intent::Modify { id, change } => {
                        let Some(mut changed) = doc.annotation(&id).cloned() else {
                            continue;
                        };
                        if !change.apply(&mut changed) {
                            continue;
                        }
                        changed.touch(now);
                        if let Some(DocCommand::ModifyAnnotations(list)) = commands.last_mut() {
                            list.push(changed);
                        } else {
                            commands.push(DocCommand::ModifyAnnotations(vec![changed]));
                        }
                    }
                    Intent::Delete(ids) => commands.push(DocCommand::DeleteAnnotations(ids)),
                    Intent::Reorder { page, order } => {
                        commands.push(DocCommand::Reorder { page, order });
                    }
                    Intent::Select(ids) => select = Some(ids),
                    Intent::SetCursor(c) => cursor = Some(c),
                    Intent::RequestTextEdit(id) => text_edit = Some(id),
                    Intent::SetScale(change) => scale_changes.push(change),
                    Intent::RequestMeasure(r) => request = Some(r),
                }
            }
        }
        // A new space gets its name, a new count its number.
        for command in &mut commands {
            if let DocCommand::AddAnnotations(list) = command {
                for a in list.iter_mut() {
                    self.dress_new(doc_id, a);
                }
            }
        }
        for change in scale_changes {
            if let Some(command) = self.scale_command(doc_id, &change) {
                commands.push(command);
            }
        }
        if let Some(r) = request {
            self.open_request(id, r);
        }
        if !commands.is_empty() {
            let command = if commands.len() == 1 {
                commands.remove(0)
            } else {
                DocCommand::Batch(commands)
            };
            self.exec(doc_id, command, group);
        }
        if let Some(ids) = select {
            if let Ok(doc) = self.session.document_mut(doc_id) {
                let _ = doc.select(ids);
            }
        }
        if let Some(target) = text_edit {
            self.request_text_edit(id, doc_id, target);
        }
        if let Some(dv) = self.views.get_mut(&id) {
            if cursor.is_some() {
                dv.tool_cursor = cursor;
            }
        }
    }

    /// Runs a document command. With `drag`, consecutive commands with the same key are one
    /// undo step. Returns whether anything changed; a refused command goes to the status bar.
    pub(crate) fn exec(&mut self, doc: DocumentId, command: DocCommand, drag: Option<u64>) -> bool {
        // A document with legends has them brought up to date by every change, in the same
        // undo step: the change is made with a grouping key the legends' update joins.
        let legends = self.has_legends(doc);
        let drag = if legends {
            Some(self.measure.merge_key(drag))
        } else {
            drag
        };
        let Ok(d) = self.session.document_mut(doc) else {
            return false;
        };
        let result = match drag {
            None => d.apply(command),
            Some(key) => d.apply_drag(command, key),
        };
        match result {
            Ok(outcome) => {
                if let (true, Some(key)) = (legends && outcome.changed, drag) {
                    self.refresh_legends(doc, key);
                }
                self.after_edit(doc);
                outcome.changed
            }
            Err(pl_doc::DocError::Saving) => {
                self.chrome.status = Some(
                    "The document is being saved; it can be changed again in a moment.".into(),
                );
                false
            }
            Err(e) => {
                self.chrome.status = Some(format!("That change could not be made: {e}"));
                false
            }
        }
    }

    /// Adds markups to the active document as one undo step (an import, a script, a test).
    /// Returns whether they were added.
    pub fn add_markups(&mut self, markups: Vec<Annotation>) -> bool {
        let Some(doc_id) = self.active_doc_id() else {
            return false;
        };
        if markups.is_empty() || !self.ensure_annots(doc_id) {
            return false;
        }
        self.exec(doc_id, DocCommand::AddAnnotations(markups), None)
    }

    /// After any change to a document's annotations: stale tiles go, and a frame is asked for.
    pub(crate) fn after_edit(&mut self, doc: DocumentId) {
        if let Ok(d) = self.session.document_mut(doc) {
            for invalidation in d.take_invalidations() {
                self.markup
                    .invalidations
                    .push((invalidation, INVALIDATION_FRAMES));
            }
        }
        self.apply_invalidations();
        self.request_repaint_soon();
    }

    /// Removes the tiles that show an annotation where it no longer is (it was hidden in the
    /// render mirror because it is now painted from its draw list). Each region is applied
    /// again on the next frames: a tile that was being rendered when the annotation was hidden
    /// reaches the cache a moment after the first removal.
    pub(crate) fn apply_invalidations(&mut self) {
        if self.markup.invalidations.is_empty() {
            return;
        }
        let mut touched: Vec<DocumentId> = Vec::new();
        for (invalidation, frames) in &mut self.markup.invalidations {
            *frames = frames.saturating_sub(1);
            let Some(doc) = self
                .session
                .documents()
                .find(|d| d.render_id() == invalidation.render)
            else {
                *frames = 0;
                continue;
            };
            let Ok(geometry) = doc.page_geometry(invalidation.page) else {
                continue;
            };
            self.cache.remove_covering(
                invalidation.render,
                invalidation.page,
                geometry,
                invalidation.rect,
            );
            self.textures
                .forget_page(invalidation.render, invalidation.page);
            touched.push(doc.id());
        }
        self.markup.invalidations.retain(|(_, frames)| *frames > 0);
        for dv in self.views.values_mut() {
            if touched.contains(&dv.doc) {
                dv.requested = None;
            }
        }
        self.request_repaint_soon();
    }

    /// Gives a key to the active view's tool. Returns whether the tool had a use for it.
    pub(crate) fn tool_key(&mut self, key: pl_tools::Key, modifiers: egui::Modifiers) -> bool {
        let Some(id) = self.active else {
            return false;
        };
        if self.markup.tool.kind().is_none() {
            return false;
        }
        let Some(dv) = self.views.get(&id) else {
            return false;
        };
        let busy = dv.tool.is_busy();
        let selected = self
            .session
            .document(dv.doc)
            .is_ok_and(|d| !d.selection().is_empty());
        // A shape that was just drawn stays selected while its tool stays active. The drawing
        // tools have no use for these keys between gestures, so they act on the selection as
        // they do with the Select tool.
        // (Count is the exception for Enter: the count being made is the selected one, and
        // Enter ends it.)
        let count_ends = self.markup.tool == Tool::Count && key == pl_tools::Key::Enter;
        if !busy && selected && self.markup.tool != Tool::Select && !count_ends {
            match key {
                pl_tools::Key::Delete | pl_tools::Key::Backspace => {
                    self.delete_selection();
                    return true;
                }
                pl_tools::Key::Enter => {
                    let one = self
                        .session
                        .document(dv.doc)
                        .is_ok_and(|d| d.selection().len() == 1);
                    if one {
                        self.markup.properties_open = true;
                        self.markup.fields.focus_comment = true;
                    }
                    return true;
                }
                _ => {}
            }
        }
        let wanted = match key {
            pl_tools::Key::Escape => busy,
            pl_tools::Key::Enter | pl_tools::Key::Backspace | pl_tools::Key::Delete => {
                busy || selected
            }
            _ => !busy && selected && self.markup.tool == Tool::Select,
        };
        if !wanted {
            return false;
        }
        self.tool_event(
            id,
            ToolInput::Key {
                key,
                modifiers: tool_modifiers(modifiers),
            },
        );
        true
    }

    /// Abandons the gesture in progress in the active view. Returns whether there was one.
    pub(crate) fn cancel_gesture(&mut self) -> bool {
        let Some(id) = self.active else {
            return false;
        };
        let busy = self.views.get(&id).is_some_and(|dv| dv.tool.is_busy());
        if self
            .markup
            .text_editor
            .as_ref()
            .is_some_and(|e| e.view == id && e.is_draft())
        {
            // The text being typed for a markup that does not exist yet goes with its gesture.
            self.markup.text_editor = None;
        }
        if busy {
            self.tool_event(id, ToolInput::key(pl_tools::Key::Escape));
            if let Some(dv) = self.views.get_mut(&id) {
                // Whatever the tool made of the key, the gesture is over.
                let state = std::mem::replace(&mut dv.tool, ToolState::for_tool(ToolKind::Select));
                dv.tool = state.cancelled();
                dv.pointer_down = false;
                if !dv.preview.is_empty() {
                    dv.preview = DrawList::new();
                    dv.preview_serial += 1;
                }
                dv.preview_page = None;
            }
        }
        busy
    }

    // ---- editing commands --------------------------------------------------------------------

    pub fn can_undo(&self) -> bool {
        self.active_document().is_some_and(Document::can_undo)
    }

    pub fn can_redo(&self) -> bool {
        self.active_document().is_some_and(Document::can_redo)
    }

    pub fn undo(&mut self) -> bool {
        self.undo_redo(true)
    }

    pub fn redo(&mut self) -> bool {
        self.undo_redo(false)
    }

    fn undo_redo(&mut self, undo: bool) -> bool {
        self.commit_text_edit();
        self.cancel_gesture();
        let Some(doc_id) = self.active_doc_id() else {
            return false;
        };
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return false;
        };
        if !doc.annots_loaded() {
            return false;
        }
        let result = if undo { doc.undo() } else { doc.redo() };
        match result {
            Ok(done) => {
                self.after_edit(doc_id);
                done
            }
            Err(e) => {
                self.chrome.status = Some(format!("That could not be undone: {e}"));
                false
            }
        }
    }

    /// The selected markups of a document that may be removed, and how many are locked (and
    /// so may not).
    pub(crate) fn removable_selection(&self, doc_id: DocumentId) -> (Vec<AnnotId>, usize) {
        let Ok(doc) = self.session.document(doc_id) else {
            return (Vec::new(), 0);
        };
        let (locked, free): (Vec<AnnotId>, Vec<AnnotId>) = doc
            .selection()
            .iter()
            .cloned()
            .partition(|id| doc.annotation(id).is_some_and(|a| a.meta.flags.is_locked()));
        (free, locked.len())
    }

    pub(crate) fn report_locked(&mut self, locked: usize) {
        if locked > 0 {
            self.chrome.status = Some(format!("{locked} locked markup(s) were left"));
        }
    }

    /// Deletes the selected markups of the active document. Locked markups stay.
    pub fn delete_selection(&mut self) -> bool {
        let Some(doc_id) = self.active_doc_id() else {
            return false;
        };
        let (ids, locked) = self.removable_selection(doc_id);
        self.report_locked(locked);
        if ids.is_empty() {
            return false;
        }
        self.exec(doc_id, DocCommand::DeleteAnnotations(ids), None)
    }

    /// Copies the selected markups. Returns how many were copied.
    pub fn copy_markups(&mut self, ctx: &egui::Context, cut: bool) -> usize {
        let Some(doc_id) = self.active_doc_id() else {
            return 0;
        };
        // A cut takes only what may be removed: locked markups stay, uncopied.
        let (ids, locked) = if cut {
            self.removable_selection(doc_id)
        } else {
            match self.session.document(doc_id) {
                Ok(doc) => (doc.selection().to_vec(), 0),
                Err(_) => return 0,
            }
        };
        self.report_locked(locked);
        if ids.is_empty() {
            return 0;
        }
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return 0;
        };
        let result = if cut { doc.cut(&ids) } else { doc.copy(&ids) };
        match result {
            Ok(clipboard) => {
                let count = clipboard.len();
                if clipboard.skipped > 0 {
                    let fate = if cut {
                        "stay where they are"
                    } else {
                        "were not copied"
                    };
                    let also = if locked > 0 {
                        format!("; {locked} locked markup(s) were left")
                    } else {
                        String::new()
                    };
                    self.chrome.status = Some(format!(
                        "{} markup(s) of a kind that cannot be copied yet {fate}{also}",
                        clipboard.skipped
                    ));
                }
                if count > 0 {
                    // egui's clipboard carries the markups as text, so a paste event brings
                    // them back (also into another Plumbline window).
                    ctx.copy_text(clipboard.to_json());
                    self.markup.clipboard = Some(clipboard);
                    self.markup.paste_serial = 0;
                }
                if cut {
                    self.after_edit(doc_id);
                }
                count
            }
            Err(e) => {
                self.chrome.status = Some(format!("The markups could not be copied: {e}"));
                0
            }
        }
    }

    /// Pastes the copied markups onto the page the active view is on, a step away from where
    /// they were copied. `text` is what a platform paste event carried, if it was one.
    pub fn paste_markups(&mut self, text: Option<&str>) -> usize {
        if let Some(clipboard) = text.and_then(|t| Clipboard::from_json(t).ok()) {
            if !clipboard.is_empty() && self.markup.clipboard.as_ref() != Some(&clipboard) {
                self.markup.clipboard = Some(clipboard);
                self.markup.paste_serial = 0;
            }
        }
        let Some(clipboard) = self.markup.clipboard.clone() else {
            return 0;
        };
        self.markup.paste_serial += 1;
        let steps = self.markup.paste_serial as f64;
        self.paste_clipboard(&clipboard, steps)
    }

    /// Copies the selection and pastes it beside itself, without touching the clipboard.
    pub fn duplicate_selection(&mut self) -> usize {
        let Some(doc_id) = self.active_doc_id() else {
            return 0;
        };
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return 0;
        };
        let ids = doc.selection().to_vec();
        if ids.is_empty() {
            return 0;
        }
        match doc.copy(&ids) {
            Ok(clipboard) => self.paste_clipboard(&clipboard, 1.0),
            Err(_) => 0,
        }
    }

    fn paste_clipboard(&mut self, clipboard: &Clipboard, steps: f64) -> usize {
        let Some(id) = self.active else {
            return 0;
        };
        let Some(dv) = self.views.get(&id) else {
            return 0;
        };
        let doc_id = dv.doc;
        let page = dv.shown_page();
        let rotation = dv
            .view
            .page_geometry(page)
            .map(|g| g.rotate())
            .unwrap_or(pl_doc::pl_geom::Rotation::R0);
        // Down and to the right as the page is shown.
        let by = pl_tools::to_page(rotation, Vec2::new(PASTE_STEP * steps, PASTE_STEP * steps));
        // That is where the copies go if it is on the page. On a page with another box (a
        // smaller sheet, an offset CropBox), or after many pastes, it may not be: then they
        // go to the middle of what the view shows of the page, kept inside the page.
        let mut placement = PastePlacement::Offset(by);
        if let (Some(bounds), Some(geometry)) = (clipboard.bounds(), dv.view.page_geometry(page)) {
            let visible = geometry.crop_box();
            let moved = Rect::new(
                bounds.x0 + by.x,
                bounds.y0 + by.y,
                bounds.x1 + by.x,
                bounds.y1 + by.y,
            );
            let inside = moved.x0 >= visible.x0
                && moved.y0 >= visible.y0
                && moved.x1 <= visible.x1
                && moved.y1 <= visible.y1;
            if !inside {
                let viewport = dv.view.viewport();
                let centre = Point::new(viewport.width / 2.0, viewport.height / 2.0);
                let wanted = dv
                    .view
                    .view_transform(page)
                    .map(|t| t.view_to_page(geometry) * centre)
                    .filter(|p| p.x.is_finite() && p.y.is_finite())
                    .unwrap_or_else(|| visible.center());
                let fit = |v: f64, lo: f64, hi: f64, size: f64| {
                    if size >= hi - lo {
                        (lo + hi) / 2.0
                    } else {
                        v.clamp(lo + size / 2.0, hi - size / 2.0)
                    }
                };
                placement = PastePlacement::CenteredAt(Point::new(
                    fit(wanted.x, visible.x0, visible.x1, bounds.width()),
                    fit(wanted.y, visible.y0, visible.y1, bounds.height()),
                ));
            }
        }
        let origin = Origin::now(self.markup.author.as_str());
        if !self.ensure_annots(doc_id) {
            return 0;
        }
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return 0;
        };
        match doc.paste(clipboard, page, placement, &origin) {
            Ok(ids) => {
                self.after_edit(doc_id);
                self.set_tool(Tool::Select);
                ids.len()
            }
            Err(e) => {
                self.chrome.status = Some(format!("The markups could not be pasted: {e}"));
                0
            }
        }
    }

    /// Selects these markups of the active document (a script, a test); ids that are not in
    /// it are dropped.
    pub fn select_markups(&mut self, ids: Vec<AnnotId>) {
        let Some(doc_id) = self.active_doc_id() else {
            return;
        };
        if !self.ensure_annots(doc_id) {
            return;
        }
        if let Ok(doc) = self.session.document_mut(doc_id) {
            let _ = doc.select(ids);
        }
    }

    /// Selects every markup on the page the active view is on.
    pub fn select_all_markups(&mut self) {
        let Some(id) = self.active else {
            return;
        };
        let Some(dv) = self.views.get(&id) else {
            return;
        };
        let (doc_id, page) = (dv.doc, dv.shown_page());
        if !self.ensure_annots(doc_id) {
            return;
        }
        if let Ok(doc) = self.session.document_mut(doc_id) {
            let _ = doc.select_page(page);
        }
        self.set_tool(Tool::Select);
    }

    /// Lines up, spaces out or restacks the selected markups, as one undo step.
    pub fn arrange(&mut self, how: Arrange) {
        let Some(id) = self.active else {
            return;
        };
        let Some(dv) = self.views.get(&id) else {
            return;
        };
        let Ok(doc) = self.session.document(dv.doc) else {
            return;
        };
        let selected: Vec<&Annotation> = doc
            .selection()
            .iter()
            .filter_map(|id| doc.annotation(id))
            .collect();
        let Some(page) = selected.first().map(|a| a.page) else {
            return;
        };
        let view = ToolView::new(doc, &dv.view, &self.markup.author, page as u32);
        let rotation = pl_tools::DocView::rotation(&view, page);
        let on_page: Vec<&Annotation> = selected
            .iter()
            .copied()
            .filter(|a| a.page == page)
            .collect();
        let intents = match how {
            Arrange::Align(align) => pl_tools::align(&on_page, align, rotation),
            Arrange::Distribute(axis) => pl_tools::distribute(&on_page, axis, rotation),
            Arrange::BringToFront => {
                pl_tools::reorder_intent(&view, page, pl_tools::bring_to_front)
                    .into_iter()
                    .collect()
            }
            Arrange::BringForward => pl_tools::reorder_intent(&view, page, pl_tools::bring_forward)
                .into_iter()
                .collect(),
            Arrange::SendBackward => pl_tools::reorder_intent(&view, page, pl_tools::send_backward)
                .into_iter()
                .collect(),
            Arrange::SendToBack => pl_tools::reorder_intent(&view, page, pl_tools::send_to_back)
                .into_iter()
                .collect(),
        };
        // An arrange command never changes what is selected.
        let intents = intents
            .into_iter()
            .filter(|i| !matches!(i, Intent::Select(_) | Intent::SetCursor(_)))
            .collect();
        self.apply_intents(id, intents);
    }

    // ---- properties and presets --------------------------------------------------------------

    /// The tool whose style the properties panel edits when nothing is selected.
    pub(crate) fn style_tool(&self) -> Option<ToolKind> {
        self.markup.tool.drawing()
    }

    /// Changes the style of the selected markups (one undo step; with `drag`, one step for
    /// the whole interaction), or, with nothing selected, the active tool's current style.
    pub(crate) fn edit_style(&mut self, drag: Option<u64>, edit: StyleEdit<'_>) {
        let Some(doc_id) = self.active_doc_id() else {
            return;
        };
        let changed: Vec<Annotation> = match self.session.document(doc_id) {
            Ok(doc) => {
                let now = PdfDate::now();
                doc.selection()
                    .iter()
                    .filter_map(|id| doc.annotation(id))
                    .filter(|a| !a.meta.flags.is_locked())
                    .filter_map(|a| {
                        let mut changed = a.clone();
                        // A text box from another program is taken up as ours first.
                        if a.kind == AnnotKind::FreeText {
                            changed.make_editable();
                        }
                        if !changed.is_modelled() {
                            return None;
                        }
                        edit(&a.kind, &mut changed.style);
                        if changed.style == a.style {
                            return None;
                        }
                        changed.touch(now);
                        Some(changed)
                    })
                    .collect()
            }
            Err(_) => return,
        };
        let has_selection = self
            .session
            .document(doc_id)
            .is_ok_and(|d| !d.selection().is_empty());
        if has_selection {
            if !changed.is_empty() {
                self.exec(doc_id, DocCommand::ModifyAnnotations(changed), drag);
            }
            return;
        }
        if let Some(tool) = self.style_tool() {
            let mut preset = self.markup.presets.current(tool);
            let kind = tool.annot_kind().unwrap_or(AnnotKind::Square);
            edit(&kind, &mut preset.style);
            preset.name.clear();
            self.set_current_preset(preset);
        }
    }

    /// Changes the text settings (font, size, colour, alignment) of the selected text boxes
    /// or, with nothing selected, of the active text tool.
    pub(crate) fn edit_free_text(&mut self, drag: Option<u64>, edit: &dyn Fn(&mut FreeText)) {
        let Some(doc_id) = self.active_doc_id() else {
            return;
        };
        let (changed, has_selection): (Vec<Annotation>, bool) = match self.session.document(doc_id)
        {
            Ok(doc) => {
                let now = PdfDate::now();
                let changed = doc
                    .selection()
                    .iter()
                    .filter_map(|id| doc.annotation(id))
                    .filter(|a| a.kind == AnnotKind::FreeText && !a.meta.flags.is_locked())
                    .filter_map(|a| {
                        let mut changed = a.clone();
                        changed.make_editable();
                        edit(changed.free_text.as_mut()?);
                        if changed.free_text == a.free_text || changed.validate().is_err() {
                            return None;
                        }
                        changed.touch(now);
                        Some(changed)
                    })
                    .collect();
                (changed, !doc.selection().is_empty())
            }
            Err(_) => return,
        };
        if has_selection {
            if !changed.is_empty() {
                self.exec(doc_id, DocCommand::ModifyAnnotations(changed), drag);
            }
            return;
        }
        if let Some(tool) = self.style_tool().filter(|t| t.is_text()) {
            let mut preset = self.markup.presets.current(tool);
            let mut text = FreeText::default();
            preset.text.apply_to(&mut text);
            edit(&mut text);
            preset.text = TextPreset {
                font: text.font,
                size: text.size,
                color: text.color,
                align: text.align,
                v_align: text.v_align,
                line_spacing: text.line_spacing,
                padding: text.padding,
            };
            preset.name.clear();
            self.set_current_preset(preset);
        }
    }

    /// The stamp the Stamp tool places.
    pub fn stamp(&self) -> Option<StampSpec> {
        self.markup.presets.current(ToolKind::Stamp).stamp
    }

    /// Makes `spec` the stamp the Stamp tool places.
    pub fn set_stamp(&mut self, spec: StampSpec) {
        let mut preset = self.markup.presets.current(ToolKind::Stamp);
        preset.stamp = Some(spec);
        preset.name.clear();
        self.set_current_preset(preset);
    }

    /// Whether a picture has been chosen for the Image tool.
    pub fn has_image(&self) -> bool {
        self.markup.image.is_some()
    }

    /// Asks for a picture file and makes it what the Image tool places. Returns whether a
    /// picture was taken; a file that cannot be read goes to the status bar.
    pub fn pick_image(&mut self) -> bool {
        let Some(path) = self.dialogs.pick_image() else {
            return false;
        };
        match pl_doc::import_image_file(&path) {
            Ok(imported) => {
                self.chrome.status = Some(if imported.downscaled {
                    format!(
                        "{} was scaled down to {} x {} pixels",
                        display_name(&path),
                        imported.image.width(),
                        imported.image.height()
                    )
                } else {
                    format!("Click or drag on the page to place {}", display_name(&path))
                });
                self.markup.image = Some(imported.image);
                for id in self.view_ids() {
                    self.sync_tool(id);
                }
                true
            }
            Err(e) => {
                self.chrome.status = Some(format!(
                    "{} could not be used as a picture: {e}",
                    display_name(&path)
                ));
                false
            }
        }
    }

    /// Changes the author, subject or comment of the selected markups.
    pub(crate) fn edit_meta(&mut self, drag: Option<u64>, edit: &dyn Fn(&mut Annotation)) {
        let Some(doc_id) = self.active_doc_id() else {
            return;
        };
        let changed: Vec<Annotation> = match self.session.document(doc_id) {
            Ok(doc) => {
                let now = PdfDate::now();
                doc.selection()
                    .iter()
                    .filter_map(|id| doc.annotation(id))
                    .filter(|a| !a.meta.flags.is_locked())
                    .filter_map(|a| {
                        let mut changed = a.clone();
                        edit(&mut changed);
                        if changed.meta == a.meta {
                            return None;
                        }
                        changed.touch(now);
                        Some(changed)
                    })
                    .collect()
            }
            Err(_) => return,
        };
        if !changed.is_empty() {
            self.exec(doc_id, DocCommand::ModifyAnnotations(changed), drag);
        }
    }

    fn presets_changed(&mut self) {
        self.flush_presets_now();
        for id in self.view_ids() {
            self.sync_tool(id);
        }
    }

    fn flush_presets_now(&mut self) {
        self.markup.presets_dirty = None;
        if let Err(e) = self.markup.store_presets() {
            self.chrome.status = Some(format!("The presets could not be saved: {e}"));
        }
    }

    /// Writes the presets file if a tool's style changed since it was last written. Called
    /// when the window closes; a frame does it by itself once the change has settled.
    pub fn flush_presets(&mut self) {
        if self.markup.presets_dirty.is_some() {
            self.flush_presets_now();
        }
    }

    /// Whether a tool's style has changed and the presets file has not been written yet.
    pub fn presets_unsaved(&self) -> bool {
        self.markup.presets_dirty.is_some()
    }

    /// Once per frame: writes the presets when the last style change is
    /// [`PRESET_WRITE_DELAY`] old and no button is held (a slider is not mid-drag).
    pub(crate) fn settle_presets(&mut self, ctx: &egui::Context) {
        let Some(since) = self.markup.presets_dirty else {
            return;
        };
        let (now, held) = ctx.input(|i| (i.time, i.pointer.any_down()));
        // A change made outside a frame is stamped with the time of the first frame after it.
        let since = if since.is_finite() { since } else { now };
        self.markup.presets_dirty = Some(since);
        let age = now - since;
        if age >= PRESET_WRITE_DELAY && !held {
            self.flush_presets_now();
        } else {
            let wait = (PRESET_WRITE_DELAY - age).clamp(0.05, PRESET_WRITE_DELAY);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }

    /// Makes `preset` its tool's current style. The presets file follows once the change has
    /// settled (see [`AppState::settle_presets`]): dragging a slider is many of these.
    pub(crate) fn set_current_preset(&mut self, preset: ToolPreset) {
        self.markup.presets.set_current(preset);
        self.markup.presets_dirty = Some(f64::NAN);
        for id in self.view_ids() {
            self.sync_tool(id);
        }
        self.request_repaint_soon();
    }

    /// Saves the active tool's current style under a name. Returns whether it was saved (a
    /// name is needed, and a tool that draws).
    pub fn save_preset(&mut self, name: &str) -> bool {
        let Some(tool) = self.style_tool() else {
            return false;
        };
        let name = name.trim();
        if name.is_empty() {
            self.chrome.status = Some("Give the preset a name first.".to_owned());
            return false;
        }
        let mut preset = self.markup.presets.current(tool);
        preset.name = name.to_owned();
        let saved = self.markup.presets.save(preset.clone());
        if saved {
            self.markup.presets.set_current(preset);
            self.presets_changed();
        }
        saved
    }

    /// Makes a saved preset the active tool's style.
    pub fn use_preset(&mut self, name: &str) -> bool {
        let Some(tool) = self.style_tool() else {
            return false;
        };
        let found = self.markup.presets.activate(tool, name).is_some();
        if found {
            self.presets_changed();
        }
        found
    }

    pub fn remove_preset(&mut self, name: &str) -> bool {
        let Some(tool) = self.style_tool() else {
            return false;
        };
        let removed = self.markup.presets.remove(tool, name);
        if removed {
            let mut current = self.markup.presets.current(tool);
            if current.name == name {
                current.name.clear();
                self.markup.presets.set_current(current);
            }
            self.presets_changed();
        }
        removed
    }

    // ---- save and close ----------------------------------------------------------------------

    /// Saves the active document and waits for the save. Returns whether it is saved now.
    /// (The Save commands of the window start the same save and do not wait for it.)
    pub fn save_active(&mut self, how: SaveHow) -> bool {
        match self.active {
            Some(id) => self.save_view(id, how),
            None => false,
        }
    }

    /// Starts saving the active document on a worker thread. Returns whether a save was
    /// started; the status bar says how it went once it has finished.
    pub fn start_save_active(&mut self, how: SaveHow) -> bool {
        match self.active {
            Some(id) => self.begin_save(id, how, AfterSave::Stay),
            None => false,
        }
    }

    /// The layers of the active document as (name, shown), in the file's order; empty when
    /// there are none or they have not been read.
    pub fn layers(&self) -> Vec<(String, bool)> {
        self.active_document()
            // Listed once the Layers page (or a toggle) has asked for them.
            .filter(|d| self.jobs.layers_asked.contains(&d.id()))
            .and_then(Document::cached_layers)
            .map(|layers| layers.iter().map(|l| (l.name.clone(), l.visible)).collect())
            .unwrap_or_default()
    }

    /// Shows or hides the layer at `index` of the active document's layer list, or with
    /// `None` puts every layer back as the file has it. Only the display changes: the file is
    /// not touched and the document is not marked as modified. Returns whether anything
    /// changed.
    pub fn set_layer_visible(&mut self, index: Option<usize>, visible: bool) -> bool {
        self.commit_text_edit();
        self.cancel_gesture();
        let Some(doc_id) = self.active_doc_id() else {
            return false;
        };
        if !self.ensure_layers(doc_id) {
            return false;
        }
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return false;
        };
        let old_render = doc.render_id();
        let result = match index {
            None => doc.reset_layers(),
            Some(index) => match doc.cached_layers().and_then(|l| l.get(index)).map(|l| l.id) {
                Some(layer) => doc.set_layer_visible(layer, visible),
                None => return false,
            },
        };
        let new_render = doc.render_id();
        if new_render != old_render {
            // The pages are drawn again from a new render document.
            for dv in self.views.values_mut().filter(|v| v.doc == doc_id) {
                self.scheduler.advance(dv.stream);
                dv.requested = None;
                dv.snap_mark = None;
            }
            self.forget_render(old_render);
        }
        self.request_repaint_soon();
        match result {
            Ok(changed) => changed,
            Err(e) => {
                self.chrome.status = Some(format!("The layer could not be switched: {e}"));
                false
            }
        }
    }

    /// Drops what was cached for a render document that no longer exists.
    pub(crate) fn forget_render(&mut self, render: DocId) {
        self.cache.remove_doc(render);
        self.textures.forget_doc(render);
        self.queries.forget_doc(render);
        self.markup
            .invalidations
            .retain(|(invalidation, _)| invalidation.render != render);
    }

    /// Closes a view's tab. If that would close a document with unsaved changes, the user is
    /// asked first (see [`AppState::answer_close_prompt`]) and nothing closes yet.
    pub fn request_close(&mut self, id: ViewId) {
        self.request_close_inner(id, false);
    }

    fn request_close_inner(&mut self, id: ViewId, quitting: bool) {
        let Some(dv) = self.views.get(&id) else {
            return;
        };
        let last_view = !self.views.values().any(|v| v.doc == dv.doc && v.id != id);
        if last_view && self.doc_is_saving(dv.doc) {
            // Closing would have to wait for the save; the user closes it when it is done.
            self.chrome.status = Some(format!("{} is still being saved", dv.title));
            return;
        }
        self.commit_text_edit();
        if last_view && self.is_modified(id) {
            self.activate(id);
            self.markup.close_prompt = Some(ClosePrompt { view: id, quitting });
        } else {
            self.close_view(id);
        }
    }

    /// Carries out the user's answer to "save changes before closing?".
    pub fn answer_close_prompt(&mut self, ctx: &egui::Context, choice: CloseChoice) {
        let Some(prompt) = self.markup.close_prompt.take() else {
            return;
        };
        let doc_id = self.views.get(&prompt.view).map(|v| v.doc);
        match choice {
            CloseChoice::Cancel => return,
            CloseChoice::Save => {
                // The tab closes (and a quit goes on) when the save has succeeded; if it does
                // not start or fails, the reason is in the status bar and nothing closes.
                self.begin_save(
                    prompt.view,
                    SaveHow::InPlace,
                    AfterSave::Close {
                        quitting: prompt.quitting,
                    },
                );
                return;
            }
            CloseChoice::Discard => {
                if let Some(doc) = doc_id.and_then(|d| self.session.document_mut(d).ok()) {
                    doc.discard_changes();
                }
                self.close_view(prompt.view);
            }
        }
        if prompt.quitting {
            self.request_quit(ctx);
        }
    }

    /// Closes the window, asking about each document with unsaved changes first.
    pub fn request_quit(&mut self, ctx: &egui::Context) {
        let modified = self.view_ids().into_iter().find(|id| self.is_modified(*id));
        match modified {
            Some(id) => {
                // Every other view of that document goes without a question.
                let doc = self.views.get(&id).map(|v| v.doc);
                let others: Vec<ViewId> = self
                    .views
                    .values()
                    .filter(|v| Some(v.doc) == doc && v.id != id)
                    .map(|v| v.id)
                    .collect();
                for other in others {
                    self.close_view(other);
                }
                self.request_close_inner(id, true);
            }
            None => {
                self.markup.quit_confirmed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// After a file was opened: offers to recover unsaved markups from a crash journal.
    pub(crate) fn check_recovery(&mut self, doc: DocumentId) {
        let Ok(d) = self.session.document(doc) else {
            return;
        };
        match d.pending_journal() {
            PendingJournal::Recoverable => self.markup.recovery = Some(doc),
            PendingJournal::Stale => {
                self.chrome.status = Some(format!(
                    "{} has changed since its unsaved markups were recorded; they were set aside",
                    d.title()
                ));
            }
            PendingJournal::None => {}
        }
    }

    /// Carries out the answer to the recovery prompt: bring the unsaved markups back, or
    /// throw their record away.
    pub fn answer_recovery_prompt(&mut self, recover: bool) {
        let Some(doc_id) = self.markup.recovery.take() else {
            return;
        };
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return;
        };
        if recover {
            // The journal is replayed by the load, on a worker thread.
            self.start_annots_load(doc_id, true);
        } else {
            doc.discard_changes();
            self.start_annots_load(doc_id, false);
        }
    }

    // ---- snapshot ----------------------------------------------------------------------------

    /// Copies what the active view shows to the clipboard as an image: the whole view, or the
    /// part of it inside `region` (screen coordinates). Returns the image's size.
    pub fn copy_snapshot(
        &mut self,
        region: Option<egui::Rect>,
        canvas: egui::Color32,
    ) -> Option<(u32, u32)> {
        let id = self.active?;
        let dv = self.views.get(&id)?;
        let doc = self.session.document(dv.doc).ok()?;
        let region = match region {
            Some(r) => r.intersect(dv.rect),
            None => dv.rect,
        };
        if region.width() < 2.0 || region.height() < 2.0 {
            self.chrome.status = Some("That region is too small to copy.".to_owned());
            return None;
        }
        let image = crate::overlay::compose_view(dv, doc, &self.cache, region, canvas);
        let (width, height, pixels) = match image {
            Ok(image) => image,
            Err(e) => {
                self.chrome.status = Some(format!("The snapshot could not be made: {e}"));
                return None;
            }
        };
        match self
            .markup
            .system_clipboard
            .set_image(width, height, &pixels)
        {
            Ok(()) => {
                self.markup.last_snapshot = Some((width, height));
                self.chrome.status = Some(format!(
                    "Copied a {width} x {height} pixel image to the clipboard"
                ));
                Some((width, height))
            }
            Err(e) => {
                self.chrome.status = Some(format!("The clipboard refused the image: {e}"));
                None
            }
        }
    }
}

/// The egui cursor for a tool's cursor.
pub(crate) fn cursor_icon(cursor: Cursor) -> Option<egui::CursorIcon> {
    Some(match cursor {
        Cursor::Default => return None,
        Cursor::Crosshair => egui::CursorIcon::Crosshair,
        Cursor::Move => egui::CursorIcon::Move,
        Cursor::ResizeHorizontal => egui::CursorIcon::ResizeHorizontal,
        Cursor::ResizeVertical => egui::CursorIcon::ResizeVertical,
        Cursor::ResizeNwSe => egui::CursorIcon::ResizeNwSe,
        Cursor::ResizeNeSw => egui::CursorIcon::ResizeNeSw,
    })
}
