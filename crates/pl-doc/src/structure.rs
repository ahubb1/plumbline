//! Structural edits of a [`Document`] (milestone M8): page operations, bookmarks, links and
//! flatten, as undoable, journaled commands ([`StructCommand`], applied with
//! [`Document::apply_structure`]).
//!
//! # The working copy
//!
//! pl-pdf's structure operations are pure: each takes a parsed document and gives a new one.
//! The first structural command of a document therefore starts a *working copy*: the new
//! document, serialised to bytes in memory (exactly as a full save would write it) and parsed
//! again. From then on
//!
//! * the render mirror shows the working copy (it is opened from those bytes), and so do the
//!   content-snap scenes and the layer list;
//! * the annotation store is read from the working copy, so annotation commands after a page
//!   operation work on the new pages;
//! * [`Document::is_modified`] is true, and the next save is the full rewrite
//!   (`PdfFile::save_restructured`: written to `name.pdf.tmp`, verified by lopdf, hayro-syntax
//!   and PDFium, then atomically replaced). The file itself is not touched before that save.
//!
//! # Unsaved markup changes
//!
//! A structural command first *commits* the annotation store's unsaved changes into the
//! structure (the store's save plan is applied to the document in memory) and then performs
//! the operation, so a markup that was drawn before a page was moved moves with its page. This
//! is the safe choice of the two: nothing the user did is lost or left behind on a page that
//! no longer exists, and the commit is undone together with the operation.
//!
//! # Undo
//!
//! A structural step is undone by restoring a snapshot taken before it: the working copy's
//! bytes (none when the document was still its file), the annotation store with its own undo
//! history, what the mirror had hidden or moved, the layer choices and every page's
//! `/UserUnit`. Restoring reopens the mirror on the restored bytes, so undo and redo give back
//! exactly the same pages and annotations. Annotation steps and structural steps undo in the
//! order they were done: after a structural step the store starts with an empty history, so
//! [`Document::undo`] takes back annotation steps first and the structural step when the
//! store has none left.
//!
//! Snapshots cost memory: each holds one copy of the document's bytes. Their total is held to
//! a budget ([`DEFAULT_STRUCTURE_UNDO_BUDGET`], [`Document::set_structure_undo_budget`]); when
//! a new step goes over it the oldest steps are forgotten first (the newest undo step is
//! always kept), and [`Document::structure_steps_dropped`] counts them. The annotation-store
//! copies in the snapshots are not counted against the budget. A save makes the saved state
//! the start of history: every structural step is forgotten (structural edits are undoable
//! until saved).
//!
//! While a working copy exists the document holds the file's bytes up to three more times:
//! the working bytes, the parsed working document and PDFium's own copy of the bytes.
//!
//! # Page-indexed state
//!
//! After a structural step the mirror is new ([`Document::render_id`] changes), every page has
//! a new revision, and cached text, bookmarks and content-snap scenes are dropped. Page scales
//! and viewports travel with their pages (they are read again from each page's `/VP`); the
//! selection keeps the annotations that still exist; the layer choices are kept by layer.
//! [`Document::take_structure_change`] hands a user interface the [`PageMap`] from the old
//! page numbers to the new ones, for its views, thumbnails and markups-list caches.
//!
//! # Signatures
//!
//! Every structural save is a full rewrite, which invalidates digital signatures. A command on
//! a signed document (or one that brings a signed page in) is refused with
//! `DocError::Pdf(PdfError::SignaturesWouldBreak { fields })` and changes nothing, unless it is
//! given [`SignaturePolicy::Invalidate`]: a user interface shows the fields
//! ([`DocError::signatures_to_confirm`]), asks, and applies the command again. The agreement
//! is remembered with the working copy and used by its save.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use pl_annot::{AnnotId, AnnotSet};
use pl_geom::{Rect, Rotation};
use pl_pdf::{
    BlankPage, DestView, Destination, Dictionary, FlatOutlineItem, FlattenOptions, FlattenReport,
    FlattenTarget, LinkAction, LinkDest, LinkSpec, Object, ObjectId, Outline, OutlineItem,
    OutlineTarget, PageOpReport, PdfDocument, PdfError, RemoteDest, ReplaceOptions, ResolvedDest,
    SaveOptions, SaveReport, SignaturePolicy, StringFormat,
};
use pl_render::DocId;

use crate::command::{EditError, Editor};
use crate::editing::{MirrorSlot, MovedSlot};
use crate::error::{DocError, Result};
use crate::journal::{FileStamp, Record};
use crate::layers::LayerState;
use crate::scales::ScaleStore;
use crate::session::{open_mirror, pdfium_check, Document, NewMirror, Origin};

/// The default memory budget of a document's structural undo snapshots: 256 MiB of document
/// bytes.
pub const DEFAULT_STRUCTURE_UNDO_BUDGET: usize = 256 << 20;

// ---- command data ------------------------------------------------------------------------------

/// How a destination shows its page; [`DestView`] in a form the journal can store.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ViewSpec {
    Xyz {
        left: Option<f64>,
        top: Option<f64>,
        zoom: Option<f64>,
    },
    Fit,
    FitH {
        top: Option<f64>,
    },
    FitV {
        left: Option<f64>,
    },
    FitR {
        left: f64,
        bottom: f64,
        right: f64,
        top: f64,
    },
    FitB,
    FitBH {
        top: Option<f64>,
    },
    FitBV {
        left: Option<f64>,
    },
}

impl ViewSpec {
    pub fn to_view(self) -> DestView {
        match self {
            ViewSpec::Xyz { left, top, zoom } => DestView::Xyz { left, top, zoom },
            ViewSpec::Fit => DestView::Fit,
            ViewSpec::FitH { top } => DestView::FitH { top },
            ViewSpec::FitV { left } => DestView::FitV { left },
            ViewSpec::FitR {
                left,
                bottom,
                right,
                top,
            } => DestView::FitR {
                left,
                bottom,
                right,
                top,
            },
            ViewSpec::FitB => DestView::FitB,
            ViewSpec::FitBH { top } => DestView::FitBH { top },
            ViewSpec::FitBV { left } => DestView::FitBV { left },
        }
    }
}

impl From<DestView> for ViewSpec {
    fn from(v: DestView) -> ViewSpec {
        match v {
            DestView::Xyz { left, top, zoom } => ViewSpec::Xyz { left, top, zoom },
            DestView::Fit => ViewSpec::Fit,
            DestView::FitH { top } => ViewSpec::FitH { top },
            DestView::FitV { left } => ViewSpec::FitV { left },
            DestView::FitR {
                left,
                bottom,
                right,
                top,
            } => ViewSpec::FitR {
                left,
                bottom,
                right,
                top,
            },
            DestView::FitB => ViewSpec::FitB,
            DestView::FitBH { top } => ViewSpec::FitBH { top },
            DestView::FitBV { left } => ViewSpec::FitBV { left },
        }
    }
}

/// A place in this document: a zero-based page and a view (page user space).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DestSpec {
    pub page: u32,
    pub view: ViewSpec,
}

impl DestSpec {
    /// The whole page.
    pub fn page(page: u32) -> DestSpec {
        DestSpec {
            page,
            view: ViewSpec::Fit,
        }
    }

    /// The page with `left`, `top` (page user space) at the top left of the window.
    pub fn at(page: u32, left: f64, top: f64) -> DestSpec {
        DestSpec {
            page,
            view: ViewSpec::Xyz {
                left: Some(left),
                top: Some(top),
                zoom: None,
            },
        }
    }

    fn destination(self, page_count: usize) -> Result<Destination> {
        if self.page as usize >= page_count {
            return Err(DocError::PageOutOfRange {
                page: self.page,
                count: page_count as u32,
            });
        }
        Ok(Destination::new(self.page as usize, self.view.to_view()))
    }
}

/// Where a bookmark leads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BookmarkTarget {
    /// Nowhere: a heading.
    None,
    Page(DestSpec),
    /// A named destination of this document.
    Named(String),
    /// A web address (the viewer opens nothing by itself; see `pl_pdf::links`).
    Uri(String),
}

/// A new bookmark.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookmarkSpec {
    pub title: String,
    pub target: BookmarkTarget,
    pub color: Option<[f64; 3]>,
    pub bold: bool,
    pub italic: bool,
    /// Whether it shows its children.
    pub open: bool,
}

impl BookmarkSpec {
    /// A plain bookmark to a whole page.
    pub fn to_page(title: impl Into<String>, page: u32) -> BookmarkSpec {
        BookmarkSpec {
            title: title.into(),
            target: BookmarkTarget::Page(DestSpec::page(page)),
            color: None,
            bold: false,
            italic: false,
            open: false,
        }
    }
}

/// What clicking a link does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// A place in this document.
    Page(DestSpec),
    /// A named destination of this document.
    Named(String),
    /// A web address.
    Uri(String),
    /// A page (zero-based; `None` for the first) of another PDF file.
    File { file: String, page: Option<u32> },
    /// A viewer action: `NextPage`, `PrevPage`, `FirstPage`, `LastPage`.
    Viewer(String),
}

/// A new or edited link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkData {
    /// `[x0, y0, x1, y1]`, unrotated page user space.
    pub rect: [f64; 4],
    /// Optional quadrilaterals, eight numbers each.
    pub quads: Vec<[f64; 8]>,
    pub target: LinkTarget,
    /// 0 for an invisible link.
    pub border_width: f64,
    pub color: Option<[f64; 3]>,
}

impl LinkData {
    pub fn new(rect: Rect, target: LinkTarget) -> LinkData {
        LinkData {
            rect: [rect.x0, rect.y0, rect.x1, rect.y1],
            quads: Vec::new(),
            target,
            border_width: 0.0,
            color: None,
        }
    }

    fn spec(&self, page_count: usize) -> Result<LinkSpec> {
        let action = match &self.target {
            LinkTarget::Page(d) => LinkAction::GoTo(LinkDest::Page(d.destination(page_count)?)),
            LinkTarget::Named(n) => LinkAction::GoTo(LinkDest::Named(n.as_bytes().to_vec())),
            LinkTarget::Uri(u) => LinkAction::Uri(u.clone()),
            LinkTarget::File { file, page } => LinkAction::GoToR {
                file: file.clone(),
                dest: page.map(|p| RemoteDest::Page {
                    page_index: p as usize,
                    view: DestView::Fit,
                }),
                new_window: None,
            },
            LinkTarget::Viewer(n) => LinkAction::Named(n.clone()),
        };
        let [x0, y0, x1, y1] = self.rect;
        let rect = Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
        if !(rect.width() > 0.0 && rect.height() > 0.0) || !rect.is_finite() {
            return Err(
                PdfError::InvalidOperation("a link needs a rectangle with an area".into()).into(),
            );
        }
        Ok(LinkSpec {
            rect,
            quad_points: self.quads.clone(),
            action,
            border_width: self.border_width,
            color: self.color,
        })
    }
}

/// Where inserted pages come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageSource {
    /// This document as it stands (duplicating pages).
    This,
    /// Another PDF file (opened with the empty password). It is read when the command runs,
    /// and again when a crash journal is replayed.
    File(PathBuf),
}

/// Which annotations a flatten burns in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlattenScope {
    All,
    /// Every qualifying annotation on these pages (zero-based).
    Pages(Vec<u32>),
    /// These annotations (by id, as the store names them).
    Annotations(Vec<String>),
}

/// A structural command: one undo step that changes the document's structure. Page numbers
/// are zero-based and refer to the document as it is when the command runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StructCommand {
    /// `count` blank pages at `at` (0..=page count). With `media_box` they have that size and
    /// `rotate`; without, the boxes and rotation of the page now at `at` (or the last page).
    InsertBlank {
        at: u32,
        count: u32,
        media_box: Option<[f64; 4]>,
        rotate: i32,
    },
    /// Copies of `pages` of `source`, in that order, at `at`.
    InsertPages {
        at: u32,
        source: PageSource,
        pages: Vec<u32>,
    },
    /// Every page of `source` at `at` (a merge); with `import_outline` its bookmarks come too.
    InsertDocument {
        at: u32,
        source: PageSource,
        import_outline: bool,
    },
    DeletePages {
        pages: Vec<u32>,
    },
    /// Page `i` of the result is the current page `order[i]`.
    ReorderPages {
        order: Vec<u32>,
    },
    /// `pages`, in their order, moved so the first lands at `to` of the result.
    MovePages {
        pages: Vec<u32>,
        to: u32,
    },
    /// Clockwise, a multiple of 90 (negative for anticlockwise).
    RotatePages {
        pages: Vec<u32>,
        degrees: i32,
    },
    /// The pages `at..at + pages.len()` replaced by copies of `pages` of `source`.
    ReplacePages {
        at: u32,
        source: PageSource,
        pages: Vec<u32>,
        keep_markups: bool,
    },
    /// A bookmark inserted as child `index` of the item at `parent` (empty: the top level).
    AddBookmark {
        parent: Vec<usize>,
        index: usize,
        bookmark: BookmarkSpec,
    },
    RenameBookmark {
        path: Vec<usize>,
        title: String,
    },
    /// The item at `from` (with its children) moved to child `index` of `parent`.
    MoveBookmark {
        from: Vec<usize>,
        parent: Vec<usize>,
        index: usize,
    },
    DeleteBookmark {
        path: Vec<usize>,
    },
    SetBookmarkTarget {
        path: Vec<usize>,
        target: BookmarkTarget,
    },
    AddLink {
        page: u32,
        link: LinkData,
    },
    /// The link annotation `id` (as the store names it) changed to `link`.
    EditLink {
        id: String,
        link: LinkData,
    },
    DeleteLink {
        id: String,
    },
    /// Burn annotation appearances into the page content. `print` flattens what prints rather
    /// than what shows; widgets only with `include_form_fields`.
    Flatten {
        scope: FlattenScope,
        print: bool,
        include_form_fields: bool,
    },
}

impl StructCommand {
    /// A short name for an undo menu or a log.
    pub fn name(&self) -> &'static str {
        match self {
            StructCommand::InsertBlank { .. } => "insert blank page",
            StructCommand::InsertPages { .. } => "insert pages",
            StructCommand::InsertDocument { .. } => "insert document",
            StructCommand::DeletePages { .. } => "delete pages",
            StructCommand::ReorderPages { .. } => "reorder pages",
            StructCommand::MovePages { .. } => "move pages",
            StructCommand::RotatePages { .. } => "rotate pages",
            StructCommand::ReplacePages { .. } => "replace pages",
            StructCommand::AddBookmark { .. } => "add bookmark",
            StructCommand::RenameBookmark { .. } => "rename bookmark",
            StructCommand::MoveBookmark { .. } => "move bookmark",
            StructCommand::DeleteBookmark { .. } => "delete bookmark",
            StructCommand::SetBookmarkTarget { .. } => "set bookmark destination",
            StructCommand::AddLink { .. } => "add link",
            StructCommand::EditLink { .. } => "edit link",
            StructCommand::DeleteLink { .. } => "delete link",
            StructCommand::Flatten { .. } => "flatten",
        }
    }

    /// Whether the command can change which pages there are or their order.
    pub fn changes_pages(&self) -> bool {
        matches!(
            self,
            StructCommand::InsertBlank { .. }
                | StructCommand::InsertPages { .. }
                | StructCommand::InsertDocument { .. }
                | StructCommand::DeletePages { .. }
                | StructCommand::ReorderPages { .. }
                | StructCommand::MovePages { .. }
                | StructCommand::ReplacePages { .. }
        )
    }
}

// ---- results -----------------------------------------------------------------------------------

/// Where each page went: `old_to_new[i]` is the new index of the page that was at `i`, `None`
/// for a page that is gone. A page that was replaced maps to its replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageMap {
    pub old_to_new: Vec<Option<u32>>,
    pub new_page_count: u32,
}

impl PageMap {
    pub fn identity(pages: u32) -> PageMap {
        PageMap {
            old_to_new: (0..pages).map(Some).collect(),
            new_page_count: pages,
        }
    }

    /// Where the page that was at `old` is now.
    pub fn map(&self, old: u32) -> Option<u32> {
        self.old_to_new.get(old as usize).copied().flatten()
    }

    pub fn is_identity(&self) -> bool {
        self.old_to_new.len() == self.new_page_count as usize
            && self
                .old_to_new
                .iter()
                .enumerate()
                .all(|(i, n)| *n == Some(i as u32))
    }

    /// This map followed by `next` (a map of the pages this one leads to).
    pub fn then(&self, next: &PageMap) -> PageMap {
        PageMap {
            old_to_new: self
                .old_to_new
                .iter()
                .map(|n| n.and_then(|m| next.map(m)))
                .collect(),
            new_page_count: next.new_page_count,
        }
    }

    /// The map back: from the new pages to the old ones.
    pub fn inverse(&self) -> PageMap {
        let mut back = vec![None; self.new_page_count as usize];
        for (old, new) in self.old_to_new.iter().enumerate() {
            if let Some(new) = new {
                if let Some(slot) = back.get_mut(*new as usize) {
                    *slot = Some(old as u32);
                }
            }
        }
        PageMap {
            old_to_new: back,
            new_page_count: self.old_to_new.len() as u32,
        }
    }
}

/// What a structural change did to the display, for a user interface to follow
/// ([`Document::take_structure_change`]): remap its views and page-indexed caches by
/// `page_map` and drop every tile and thumbnail of `old_render`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureChange {
    pub page_map: PageMap,
    pub old_render: DocId,
    pub new_render: DocId,
}

/// pl-pdf's account of the operation.
#[derive(Clone, Debug, PartialEq)]
pub enum StructReport {
    Pages(Box<PageOpReport>),
    Flatten(FlattenReport),
    /// A bookmark or link edit.
    Edit,
}

/// What [`Document::apply_structure`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct StructureOutcome {
    pub command: &'static str,
    pub report: StructReport,
    pub page_map: PageMap,
    /// Signatures the next save will invalidate (the user agreed to it).
    pub signatures_invalidated: Vec<String>,
    /// Unsaved markup changes were written into the structure before the operation (see the
    /// module documentation); undo takes them back out with it.
    pub committed_markups: bool,
    /// Takeoff definitions (spaces, legends) that were on pages the operation removed, so
    /// they were dropped (`space "Room 1"`). The others follow their pages.
    pub settings_dropped: Vec<String>,
}

/// One page as its content, boxes and rotation make it: what tells pages apart after any
/// reordering. Annotations are not part of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageFingerprint {
    /// A hash of the page's decoded content streams.
    pub content: u64,
    pub media_box: Rect,
    pub crop_box: Option<Rect>,
    pub rotate: Rotation,
    pub user_unit: f64,
}

/// One link of a page, with what the store calls it and where it leads in this document.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkInfo {
    pub id: Option<AnnotId>,
    pub link: pl_pdf::Link,
    pub resolved: Option<ResolvedDest>,
}

// ---- state -------------------------------------------------------------------------------------

/// The document as the structural commands have made it (see the module documentation).
pub(crate) struct Working {
    pub(crate) bytes: Arc<Vec<u8>>,
    /// `bytes`, parsed.
    pub(crate) doc: PdfDocument,
    /// The user agreed that the save may invalidate signatures.
    pub(crate) accepted: bool,
}

/// Everything a structural step changes, as it was.
#[derive(Clone)]
struct Snapshot {
    /// The working copy's bytes and agreement; `None` for the file itself.
    working: Option<(Arc<Vec<u8>>, bool)>,
    editor: Editor,
    hidden: BTreeMap<AnnotId, MirrorSlot>,
    moved: BTreeMap<AnnotId, MovedSlot>,
    layers: Option<LayerState>,
    units: Vec<f64>,
}

impl Snapshot {
    fn bytes(&self) -> usize {
        self.working.as_ref().map_or(0, |(b, _)| b.len())
    }
}

struct Step {
    command: StructCommand,
    /// For an undo step the state before the command; for a redo step the state after it.
    state: Snapshot,
    /// The command's page map (old to new).
    map: PageMap,
}

pub(crate) struct StructState {
    pub(crate) working: Option<Working>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    budget: usize,
    dropped: usize,
    change: Option<StructureChange>,
}

impl Default for StructState {
    fn default() -> StructState {
        StructState {
            working: None,
            undo: Vec::new(),
            redo: Vec::new(),
            budget: DEFAULT_STRUCTURE_UNDO_BUDGET,
            dropped: 0,
            change: None,
        }
    }
}

impl StructState {
    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// A save made the file the truth: no working copy, no structural history.
    pub(crate) fn clear_all(&mut self) {
        self.working = None;
        self.undo.clear();
        self.redo.clear();
    }

    fn held_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|s| s.state.bytes())
            .sum()
    }

    /// Forgets the oldest steps until the snapshots fit the budget; the newest undo step is
    /// always kept.
    fn enforce_budget(&mut self) {
        while self.held_bytes() > self.budget && !self.redo.is_empty() {
            self.redo.remove(0);
            self.dropped += 1;
        }
        while self.held_bytes() > self.budget && self.undo.len() > 1 {
            self.undo.remove(0);
            self.dropped += 1;
        }
    }
}

// ---- pure helpers ------------------------------------------------------------------------------

fn fnv(mut h: u64, data: &[u8]) -> u64 {
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn content_hash(doc: &PdfDocument, page: ObjectId) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let Some(Object::Dictionary(dict)) = doc.object(page) else {
        return h;
    };
    let mut parts: Vec<&Object> = Vec::new();
    match dict.get(b"Contents").ok().and_then(|o| doc.resolve(o)) {
        Some(Object::Array(items)) => parts.extend(items.iter()),
        Some(other) => parts.push(other),
        None => {}
    }
    for part in parts {
        if let Some(Object::Stream(s)) = doc.resolve(part) {
            let data = if s.dict.has(b"Filter") {
                s.decompressed_content()
                    .unwrap_or_else(|_| s.content.clone())
            } else {
                s.content.clone()
            };
            h = fnv(h, &data);
            h = fnv(h, b"\n");
        }
    }
    h
}

/// Every page's fingerprint.
pub fn fingerprints(doc: &PdfDocument) -> Result<Vec<PageFingerprint>> {
    let pages = doc.pages()?;
    Ok(pages
        .iter()
        .map(|p| PageFingerprint {
            content: content_hash(doc, p.id),
            media_box: p.media_box,
            crop_box: p.crop_box,
            rotate: p.rotate,
            user_unit: p.user_unit,
        })
        .collect())
}

fn usize_pages(pages: &[u32]) -> Vec<usize> {
    pages.iter().map(|p| *p as usize).collect()
}

fn invalid(message: impl Into<String>) -> DocError {
    PdfError::InvalidOperation(message.into()).into()
}

fn read_source(this: &PdfDocument, source: &PageSource) -> Result<PdfDocument> {
    match source {
        PageSource::This => Ok(this.clone()),
        PageSource::File(path) => {
            let bytes = std::fs::read(path).map_err(|e| DocError::io("read", path, e))?;
            Ok(PdfDocument::from_bytes(bytes, None)?)
        }
    }
}

fn outline_target(target: &BookmarkTarget, page_count: usize) -> Result<OutlineTarget> {
    Ok(match target {
        BookmarkTarget::None => OutlineTarget::None,
        BookmarkTarget::Page(d) => OutlineTarget::Page {
            dest: d.destination(page_count)?,
            as_action: false,
        },
        BookmarkTarget::Named(n) => OutlineTarget::Named {
            name: n.as_bytes().to_vec(),
            as_action: false,
        },
        BookmarkTarget::Uri(u) => {
            let mut action = Dictionary::new();
            action.set("S", Object::Name(b"URI".to_vec()));
            action.set(
                "URI",
                Object::String(u.as_bytes().to_vec(), StringFormat::Literal),
            );
            OutlineTarget::Action(Object::Dictionary(action))
        }
    })
}

/// The annotation the store calls `id` in `doc`: its page and object.
fn annotation_object(doc: &PdfDocument, id: &str) -> Result<(usize, ObjectId, Vec<ObjectId>)> {
    let set = AnnotSet::load(doc);
    let annot_id = AnnotId(id.to_string());
    let a = set
        .get(&annot_id)
        .ok_or_else(|| DocError::Edit(EditError::NotFound(annot_id.clone())))?;
    let base = a
        .baseline()
        .ok_or_else(|| DocError::Edit(EditError::NotFound(annot_id.clone())))?;
    let object = base.object.ok_or_else(|| {
        invalid(format!(
            "annotation {id} is written inside its page and cannot be changed"
        ))
    })?;
    let members = base.members.iter().filter_map(|m| m.object).collect();
    Ok((base.page, object, members))
}

fn is_link(doc: &PdfDocument, object: ObjectId) -> bool {
    matches!(
        doc.object(object),
        Some(Object::Dictionary(d)) if d.get(b"Subtype").ok().and_then(|o| o.as_name().ok()) == Some(b"Link".as_slice())
    )
}

/// Performs `command` on `doc` (pure).
fn perform(doc: &PdfDocument, command: &StructCommand) -> Result<(PdfDocument, StructReport)> {
    let pages =
        |r: pl_pdf::Restructure, report: PageOpReport| -> Result<(PdfDocument, StructReport)> {
            Ok((r.finish()?, StructReport::Pages(Box::new(report))))
        };
    let count = doc.page_count();
    match command {
        StructCommand::InsertBlank {
            at,
            count: n,
            media_box,
            rotate,
        } => {
            if *n == 0 {
                return Err(invalid("there are no pages to insert"));
            }
            let size = match media_box {
                Some([x0, y0, x1, y1]) => BlankPage::Size {
                    media_box: Rect::new(*x0, *y0, *x1, *y1),
                    rotate: *rotate,
                },
                None => BlankPage::LikeNeighbour,
            };
            let mut r = doc.restructure()?;
            let mut report = PageOpReport::default();
            for k in 0..*n {
                report.absorb(r.insert_blank((*at + k) as usize, size)?);
            }
            pages(r, report)
        }
        StructCommand::InsertPages {
            at,
            source,
            pages: p,
        } => {
            let src = read_source(doc, source)?;
            let mut r = doc.restructure()?;
            let report = r.insert_from(*at as usize, &src, &usize_pages(p))?;
            pages(r, report)
        }
        StructCommand::InsertDocument {
            at,
            source,
            import_outline,
        } => {
            let src = read_source(doc, source)?;
            let mut r = doc.restructure()?;
            let report = r.insert_document(*at as usize, &src, *import_outline)?;
            pages(r, report)
        }
        StructCommand::DeletePages { pages: p } => {
            let mut r = doc.restructure()?;
            let report = r.delete(&usize_pages(p))?;
            pages(r, report)
        }
        StructCommand::ReorderPages { order } => {
            let mut r = doc.restructure()?;
            let report = r.reorder(&usize_pages(order))?;
            pages(r, report)
        }
        StructCommand::MovePages { pages: p, to } => {
            let mut r = doc.restructure()?;
            let report = r.move_pages(&usize_pages(p), *to as usize)?;
            pages(r, report)
        }
        StructCommand::RotatePages { pages: p, degrees } => {
            let mut r = doc.restructure()?;
            let report = r.rotate(&usize_pages(p), *degrees)?;
            pages(r, report)
        }
        StructCommand::ReplacePages {
            at,
            source,
            pages: p,
            keep_markups,
        } => {
            let src = read_source(doc, source)?;
            let mut r = doc.restructure()?;
            let report = r.replace(
                *at as usize,
                &src,
                &usize_pages(p),
                ReplaceOptions {
                    keep_markups: *keep_markups,
                },
            )?;
            pages(r, report)
        }
        StructCommand::AddBookmark {
            parent,
            index,
            bookmark,
        } => {
            let mut outline = doc.outline();
            let mut item = OutlineItem::new(
                bookmark.title.clone(),
                outline_target(&bookmark.target, count)?,
            );
            item.color = bookmark.color;
            item.bold = bookmark.bold;
            item.italic = bookmark.italic;
            item.open = bookmark.open;
            outline.insert(parent, *index, item)?;
            Ok((doc.with_outline(&outline)?, StructReport::Edit))
        }
        StructCommand::RenameBookmark { path, title } => {
            let mut outline = doc.outline();
            outline.rename(path, title.clone())?;
            Ok((doc.with_outline(&outline)?, StructReport::Edit))
        }
        StructCommand::MoveBookmark {
            from,
            parent,
            index,
        } => {
            let mut outline = doc.outline();
            outline.move_item(from, parent, *index)?;
            Ok((doc.with_outline(&outline)?, StructReport::Edit))
        }
        StructCommand::DeleteBookmark { path } => {
            let mut outline = doc.outline();
            outline.remove(path)?;
            Ok((doc.with_outline(&outline)?, StructReport::Edit))
        }
        StructCommand::SetBookmarkTarget { path, target } => {
            let mut outline = doc.outline();
            outline.set_target(path, outline_target(target, count)?)?;
            Ok((doc.with_outline(&outline)?, StructReport::Edit))
        }
        StructCommand::AddLink { page, link } => {
            if *page as usize >= count {
                return Err(DocError::PageOutOfRange {
                    page: *page,
                    count: count as u32,
                });
            }
            let mut edits = doc.edit();
            doc.add_link(&mut edits, *page as usize, &link.spec(count)?)?;
            Ok((doc.with_edits(&edits)?, StructReport::Edit))
        }
        StructCommand::EditLink { id, link } => {
            let (_, object, _) = annotation_object(doc, id)?;
            if !is_link(doc, object) {
                return Err(invalid(format!("annotation {id} is not a link")));
            }
            let mut edits = doc.edit();
            edits.modify_annotation(object, doc.link_patch(&link.spec(count)?)?);
            Ok((doc.with_edits(&edits)?, StructReport::Edit))
        }
        StructCommand::DeleteLink { id } => {
            let (page, object, _) = annotation_object(doc, id)?;
            if !is_link(doc, object) {
                return Err(invalid(format!("annotation {id} is not a link")));
            }
            let mut edits = doc.edit();
            edits.delete_annotation(page, object);
            Ok((doc.with_edits(&edits)?, StructReport::Edit))
        }
        StructCommand::Flatten {
            scope,
            print,
            include_form_fields,
        } => {
            let mut options = FlattenOptions {
                pages: None,
                ids: None,
                target: if *print {
                    FlattenTarget::Print
                } else {
                    FlattenTarget::Screen
                },
                include_form_fields: *include_form_fields,
            };
            match scope {
                FlattenScope::All => {}
                FlattenScope::Pages(p) => {
                    if let Some(bad) = p.iter().find(|p| **p as usize >= count) {
                        return Err(DocError::PageOutOfRange {
                            page: *bad,
                            count: count as u32,
                        });
                    }
                    options.pages = Some(p.iter().map(|p| *p as usize).collect());
                }
                FlattenScope::Annotations(ids) => {
                    let mut objects = BTreeSet::new();
                    for id in ids {
                        let (_, object, members) = annotation_object(doc, id)?;
                        objects.insert(object);
                        objects.extend(members);
                    }
                    options.ids = Some(objects);
                }
            }
            let (flat, report) = doc.flatten(&options)?;
            Ok((flat, StructReport::Flatten(report)))
        }
    }
}

fn page_map(before: &PdfDocument, after: &PdfDocument, command: &StructCommand) -> PageMap {
    let position: HashMap<ObjectId, u32> = after
        .page_ids()
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i as u32))
        .collect();
    let mut old_to_new: Vec<Option<u32>> = before
        .page_ids()
        .iter()
        .map(|id| position.get(id).copied())
        .collect();
    if let StructCommand::ReplacePages { at, pages, .. } = command {
        for k in 0..pages.len() {
            let i = *at as usize + k;
            if i < old_to_new.len() {
                old_to_new[i] = Some(i as u32);
            }
        }
    }
    PageMap {
        old_to_new,
        new_page_count: after.page_count() as u32,
    }
}

/// `doc` with its takeoff definitions (spaces, legends: stored by page index in the catalog's
/// private data) moved with their pages; and the definitions dropped with removed pages.
fn remap_settings(doc: PdfDocument, map: &PageMap) -> Result<(PdfDocument, Vec<String>)> {
    if map.is_identity() {
        return Ok((doc, Vec::new()));
    }
    let mut store = ScaleStore::load(&doc);
    let (settings, dropped) = store
        .settings()
        .remap_pages(|p| map.map(p as u32).map(|n| n as usize));
    if settings == *store.settings() {
        return Ok((doc, dropped));
    }
    store.set_settings(settings);
    let mut edits = doc.edit();
    store.plan(&doc, &mut edits)?;
    if edits.is_empty() {
        return Ok((doc, dropped));
    }
    Ok((doc.with_edits(&edits)?, dropped))
}

/// Refuses `command` when the file's security settings forbid it: page and bookmark changes
/// need the assemble or modify permission, link edits and flattening the modify permission.
fn check_permissions(doc: &PdfDocument, command: &StructCommand) -> Result<()> {
    let Some(enc) = doc.encryption() else {
        return Ok(());
    };
    let (allowed, what) = match command {
        StructCommand::AddLink { .. }
        | StructCommand::EditLink { .. }
        | StructCommand::DeleteLink { .. } => (enc.can_modify, "change its links"),
        StructCommand::Flatten { .. } => (enc.can_modify, "be flattened"),
        StructCommand::AddBookmark { .. }
        | StructCommand::RenameBookmark { .. }
        | StructCommand::MoveBookmark { .. }
        | StructCommand::DeleteBookmark { .. }
        | StructCommand::SetBookmarkTarget { .. } => {
            (enc.can_modify || enc.can_assemble, "change its bookmarks")
        }
        _ => (enc.can_modify || enc.can_assemble, "change its pages"),
    };
    if allowed {
        Ok(())
    } else {
        Err(DocError::NotPermitted { what })
    }
}

fn union(into: &mut Vec<String>, more: Vec<String>) {
    for f in more {
        if !into.contains(&f) {
            into.push(f);
        }
    }
}

// ---- the document's side -----------------------------------------------------------------------

/// A state ready to be made the document's: everything that can fail is done.
struct Prepared {
    working: Option<Working>,
    editor: Editor,
    hidden: BTreeMap<AnnotId, MirrorSlot>,
    moved: BTreeMap<AnnotId, MovedSlot>,
    layers: Option<LayerState>,
    units: Vec<f64>,
    mirror: NewMirror,
}

impl Document {
    /// The document as the structural commands see it: the working copy, or the file (parsed
    /// now if it is not; released again afterwards). Unsaved annotation changes are not in it.
    pub(crate) fn structure_base(&mut self) -> Result<PdfDocument> {
        if let Some(w) = &self.structure.working {
            return Ok(w.doc.clone());
        }
        match &self.origin {
            Origin::Memory { bytes, .. } => Ok(PdfDocument::from_bytes(
                bytes.to_vec(),
                self.password.as_deref(),
            )?),
            Origin::File(path) => {
                let path = path.clone();
                if let Some(then) = self.edit.state.as_ref().and_then(|s| s.stamp) {
                    let now =
                        FileStamp::of_file(&path).map_err(|e| DocError::io("read", &path, e))?;
                    if (now.len, now.ends_hash) != (then.len, then.ends_hash) {
                        return Err(PdfError::ChangedOnDisk { path }.into());
                    }
                }
                let was_loaded = self.pdf_is_loaded();
                let file = self.pdf()?;
                let doc = file.document().cloned();
                if !was_loaded {
                    file.unload();
                }
                Ok(doc?)
            }
        }
    }

    /// `base` with the store's unsaved changes applied; and whether there were any.
    fn committed(&self, base: PdfDocument) -> Result<(PdfDocument, bool)> {
        let Some(state) = self.edit.state.as_ref() else {
            return Ok((base, false));
        };
        if !state.editor.has_unsaved() {
            return Ok((base, false));
        }
        let mut plan = state.editor.set().plan_save(&base)?;
        state.editor.scales().plan(&base, &mut plan.edits)?;
        if plan.edits.is_empty() {
            return Ok((base, false));
        }
        Ok((base.with_edits(&plan.edits)?, true))
    }

    /// The document as it would be saved now, unsaved markups included.
    pub(crate) fn committed_document(&mut self) -> Result<PdfDocument> {
        self.load_annots()?;
        let base = self.structure_base()?;
        Ok(self.committed(base)?.0)
    }

    fn signatures_accepted(&self) -> bool {
        self.structure.working.as_ref().is_some_and(|w| w.accepted)
    }

    /// The signatures a structural save of this document would invalidate (empty for an
    /// unsigned document). A user interface asks before the first structural command.
    pub fn structure_signatures(&mut self) -> Result<Vec<String>> {
        Ok(self.structure_base()?.signatures().descriptions())
    }

    /// Opens a mirror on `working` (or the file) with the layer choices of `layers`.
    fn open_display(
        &mut self,
        working: Option<&Working>,
        layers: Option<&LayerState>,
    ) -> Result<NewMirror> {
        let off = layers.filter(|l| !l.at_default()).map(LayerState::off);
        let tail = match (off, working) {
            (None, _) => Vec::new(),
            (Some(off), Some(w)) => w.doc.layer_override_tail(&off)?,
            (Some(off), None) => {
                let doc = match &self.origin {
                    Origin::Memory { bytes, .. } => {
                        PdfDocument::from_bytes(bytes.to_vec(), self.password.as_deref())?
                    }
                    Origin::File(_) => {
                        let was_loaded = self.pdf_is_loaded();
                        let file = self.pdf()?;
                        let doc = file.document().cloned();
                        if !was_loaded {
                            file.unload();
                        }
                        doc?
                    }
                };
                doc.layer_override_tail(&off)?
            }
        };
        let origin = match working {
            Some(w) => Origin::Memory {
                name: self.title(),
                bytes: w.bytes.clone(),
            },
            None => self.origin.clone(),
        };
        open_mirror(
            self.backend.as_ref(),
            &origin,
            self.password.as_deref(),
            tail,
        )
    }

    /// The origin the display is opened on: the working copy's bytes, or the file.
    pub(crate) fn display_origin(&self) -> Origin {
        match &self.structure.working {
            Some(w) => Origin::Memory {
                name: self.title(),
                bytes: w.bytes.clone(),
            },
            None => self.origin.clone(),
        }
    }

    fn capture(&self) -> Snapshot {
        let state = self.edit.state.as_ref().expect("the store is loaded");
        Snapshot {
            working: self
                .structure
                .working
                .as_ref()
                .map(|w| (w.bytes.clone(), w.accepted)),
            editor: state.editor.clone(),
            hidden: state.hidden.clone(),
            moved: state.moved.clone(),
            layers: self.layers.clone(),
            units: self.pages().iter().map(|p| p.user_unit()).collect(),
        }
    }

    fn prepare_snapshot(&mut self, snap: &Snapshot) -> Result<Prepared> {
        let working = match &snap.working {
            Some((bytes, accepted)) => Some(Working {
                doc: PdfDocument::from_bytes(bytes.to_vec(), self.password.as_deref())?,
                bytes: bytes.clone(),
                accepted: *accepted,
            }),
            None => None,
        };
        let mirror = self.open_display(working.as_ref(), snap.layers.as_ref())?;
        Ok(Prepared {
            working,
            editor: snap.editor.clone(),
            hidden: snap.hidden.clone(),
            moved: snap.moved.clone(),
            layers: snap.layers.clone(),
            units: snap.units.clone(),
            mirror,
        })
    }

    /// Makes a prepared state the document's. Nothing here can fail; display calls that do
    /// become warnings.
    fn install_prepared(&mut self, prepared: Prepared, map: PageMap) {
        let old_total = self.edit_revision();
        let state = self.edit.state.as_mut().expect("the store is loaded");
        state.editor = prepared.editor;
        state.hidden = prepared.hidden;
        state.moved = prepared.moved;
        state.invalid.clear();
        self.rebase_edit_revision(old_total);
        self.layers = prepared.layers;
        self.structure.working = prepared.working;
        let old_render = self.render;
        self.adopt_fresh_mirror(prepared.mirror, &prepared.units);
        self.structure.change = Some(StructureChange {
            page_map: map,
            old_render,
            new_render: self.render,
        });
    }

    /// Applies a structural command as one undo step (see the module documentation). A refused
    /// command changes nothing. `signatures` must be [`SignaturePolicy::Invalidate`] for a
    /// signed document once the user has agreed; with `Refuse` such a command fails with
    /// `PdfError::SignaturesWouldBreak`.
    pub fn apply_structure(
        &mut self,
        command: StructCommand,
        signatures: SignaturePolicy,
    ) -> Result<StructureOutcome> {
        self.refuse_while_saving()?;
        self.load_annots()?;
        // A source file is named by its absolute path, so a journal replay finds it whatever
        // the working directory is then.
        let mut command = command;
        if let StructCommand::InsertPages { source, .. }
        | StructCommand::InsertDocument { source, .. }
        | StructCommand::ReplacePages { source, .. } = &mut command
        {
            if let PageSource::File(path) = source {
                *path =
                    std::path::absolute(&*path).map_err(|e| DocError::io("resolve", path, e))?;
            }
        }
        let outcome = self.run_structure(&command, signatures)?;
        let invalidate = self.signatures_accepted();
        self.journal(&Record::Structure {
            command,
            invalidate_signatures: invalidate,
        });
        Ok(outcome)
    }

    pub(crate) fn run_structure(
        &mut self,
        command: &StructCommand,
        policy: SignaturePolicy,
    ) -> Result<StructureOutcome> {
        let base = self.structure_base()?;
        check_permissions(&base, command)?;
        let accepted = self.signatures_accepted() || policy == SignaturePolicy::Invalidate;
        let mut fields = base.signatures().descriptions();
        let (committed, committed_markups) = self.committed(base)?;
        let (result, report) = perform(&committed, command)?;
        union(&mut fields, result.signatures().descriptions());
        if let StructReport::Pages(r) = &report {
            union(&mut fields, r.signatures_invalidated.clone());
        }
        if !fields.is_empty() && !accepted {
            return Err(PdfError::SignaturesWouldBreak { fields }.into());
        }
        let map = page_map(&committed, &result, command);
        drop(committed);
        let (result, settings_dropped) = remap_settings(result, &map)?;
        let bytes = result.to_bytes()?;
        drop(result);
        let parsed = PdfDocument::from_bytes(bytes.clone(), self.password.as_deref())?;
        let page_count = parsed.page_count();
        let units: Vec<f64> = parsed.pages()?.iter().map(|p| p.user_unit).collect();
        let set = AnnotSet::load(&parsed);
        let scales = ScaleStore::load(&parsed);
        let layers = self
            .layers
            .as_ref()
            .map(|old| old.remapped(parsed.layers()));
        let working = Working {
            bytes: Arc::new(bytes),
            doc: parsed,
            accepted,
        };
        let mirror = self.open_display(Some(&working), layers.as_ref())?;

        // From here nothing fails. The step is recorded with the state it leaves.
        {
            let state = self.edit.state.as_mut().expect("the store is loaded");
            state.editor.clear_redo();
        }
        let before = self.capture();
        let selection = self.selection().to_vec();
        let mut editor = Editor::with_scales(set, scales, page_count);
        editor.select(selection);
        self.structure.redo.clear();
        self.structure.undo.push(Step {
            command: command.clone(),
            state: before,
            map: map.clone(),
        });
        self.structure.enforce_budget();
        self.install_prepared(
            Prepared {
                working: Some(working),
                editor,
                hidden: BTreeMap::new(),
                moved: BTreeMap::new(),
                layers,
                units,
                mirror,
            },
            map.clone(),
        );
        Ok(StructureOutcome {
            command: command.name(),
            report,
            page_map: map,
            signatures_invalidated: if accepted { fields } else { Vec::new() },
            committed_markups,
            settings_dropped,
        })
    }

    /// Takes back the most recent structural step.
    pub(crate) fn undo_structure(&mut self) -> Result<bool> {
        let Some(step) = self.structure.undo.pop() else {
            return Ok(false);
        };
        let prepared = match self.prepare_snapshot(&step.state) {
            Ok(p) => p,
            Err(e) => {
                self.structure.undo.push(step);
                return Err(e);
            }
        };
        let after = self.capture();
        self.install_prepared(prepared, step.map.inverse());
        self.structure.redo.push(Step {
            command: step.command,
            state: after,
            map: step.map,
        });
        self.structure.enforce_budget();
        Ok(true)
    }

    /// Applies again the structural step most recently undone.
    pub(crate) fn redo_structure(&mut self) -> Result<bool> {
        let Some(step) = self.structure.redo.pop() else {
            return Ok(false);
        };
        let prepared = match self.prepare_snapshot(&step.state) {
            Ok(p) => p,
            Err(e) => {
                self.structure.redo.push(step);
                return Err(e);
            }
        };
        let before = self.capture();
        self.install_prepared(prepared, step.map.clone());
        self.structure.undo.push(Step {
            command: step.command,
            state: before,
            map: step.map,
        });
        self.structure.enforce_budget();
        Ok(true)
    }

    /// Goes back to the file, forgetting the working copy and every structural step (a
    /// discard). The store is not touched here.
    pub(crate) fn drop_structure(&mut self) -> Result<()> {
        let had_working = self.structure.working.is_some();
        // The oldest snapshot is the file's own state unless the budget dropped that step.
        let file_units = self
            .structure
            .undo
            .first()
            .filter(|s| s.state.working.is_none())
            .map(|s| s.state.units.clone());
        if had_working {
            let mirror = self.open_display(None, None)?;
            let old_pages = self.page_count() as usize;
            self.structure.clear_all();
            self.layers = None;
            let old_render = self.render;
            match file_units {
                Some(units) => self.adopt_fresh_mirror(mirror, &units),
                None => {
                    self.adopt_fresh_mirror(mirror, &[]);
                    if let Err(e) = self.refresh_user_units() {
                        self.warnings
                            .push(format!("/UserUnit could not be read: {e}"));
                    }
                }
            }
            // Which page went where is not known: every page is new to the display.
            self.structure.change = Some(StructureChange {
                page_map: PageMap {
                    old_to_new: vec![None; old_pages],
                    new_page_count: self.page_count(),
                },
                old_render,
                new_render: self.render,
            });
        } else {
            self.structure.clear_all();
        }
        Ok(())
    }

    /// Where each page of the file (as last opened or saved) is in the document as its
    /// structure stands now: the identity without structural changes. `None` when that is no
    /// longer known (the oldest steps left the undo history).
    pub fn page_map_from_file(&self) -> Option<PageMap> {
        if self.structure.working.is_none() {
            return Some(PageMap::identity(self.page_count()));
        }
        let (first, rest) = self.structure.undo.split_first()?;
        if first.state.working.is_some() {
            return None;
        }
        Some(
            rest.iter()
                .fold(first.map.clone(), |map, step| map.then(&step.map)),
        )
    }

    /// Whether the document has structural changes a save would write (a working copy).
    pub fn has_structure_changes(&self) -> bool {
        self.structure.working.is_some()
    }

    /// Whether the next undo or redo is a structural step (for an undo menu's wording).
    pub fn next_undo_is_structural(&self) -> bool {
        !self.editor().is_some_and(Editor::can_undo) && self.structure.can_undo()
    }

    pub fn next_redo_is_structural(&self) -> bool {
        !self.editor().is_some_and(Editor::can_redo) && self.structure.can_redo()
    }

    /// The name of the structural step the next undo takes back, when it is one.
    pub fn structure_undo_name(&self) -> Option<&'static str> {
        self.next_undo_is_structural()
            .then(|| self.structure.undo.last().map(|s| s.command.name()))
            .flatten()
    }

    /// Changes the memory budget of the structural undo snapshots (see the module
    /// documentation). Takes effect at once.
    pub fn set_structure_undo_budget(&mut self, bytes: usize) {
        self.structure.budget = bytes;
        self.structure.enforce_budget();
    }

    pub fn structure_undo_budget(&self) -> usize {
        self.structure.budget
    }

    /// How many structural steps were forgotten to stay within the budget.
    pub fn structure_steps_dropped(&self) -> usize {
        self.structure.dropped
    }

    /// The bytes the structural undo snapshots hold now.
    pub fn structure_undo_bytes(&self) -> usize {
        self.structure.held_bytes()
    }

    /// The most recent structural change of the display (a command, its undo or its redo),
    /// once: a user interface calls this every frame and remaps what it keeps per page.
    pub fn take_structure_change(&mut self) -> Option<StructureChange> {
        self.structure.change.take()
    }

    // ---- reading the structure ---------------------------------------------------------------

    /// Every page's fingerprint, as the structure stands (unsaved markups do not count).
    pub fn page_fingerprints(&mut self) -> Result<Vec<PageFingerprint>> {
        fingerprints(&self.structure_base()?)
    }

    /// The bookmarks as pl-pdf reads them, from the structure as it stands.
    pub fn outline(&mut self) -> Result<Outline> {
        Ok(self.structure_base()?.outline())
    }

    /// The bookmarks in pre-order with their depth, path and resolved destination.
    pub fn outline_rows(&mut self) -> Result<Vec<FlatOutlineItem>> {
        let doc = self.structure_base()?;
        Ok(doc.outline().flatten_with(&doc))
    }

    /// Each page's label as a viewer shows it; a document without labels gives the page
    /// numbers ("1", "2", ...).
    pub fn page_labels(&mut self) -> Result<Vec<String>> {
        let doc = self.structure_base()?;
        Ok(match doc.page_labels() {
            Some(labels) => labels.iter().map(|l| l.text()).collect(),
            None => (1..=doc.page_count()).map(|n| n.to_string()).collect(),
        })
    }

    /// The links of a page as the structure stands, each with the id the annotation store gives
    /// it and where it leads in this document.
    pub fn page_links(&mut self, page: u32) -> Result<Vec<LinkInfo>> {
        let doc = self.structure_base()?;
        let links = doc.links(page as usize)?;
        let set = AnnotSet::load_pages(&doc, &[page as usize]);
        Ok(links
            .into_iter()
            .map(|link| {
                let id = set
                    .iter()
                    .find(|a| {
                        a.baseline()
                            .is_some_and(|b| b.object.is_some() && b.object == link.id)
                    })
                    .map(|a| a.id.clone());
                let resolved = doc.resolve_link_action(&link.action, page as usize);
                LinkInfo { id, link, resolved }
            })
            .collect())
    }

    // ---- new files from the document ------------------------------------------------------

    /// Writes `pages` (zero-based, in the order given) of the document as it stands, unsaved
    /// markups included, to a new file at `dest` (replacing it if it exists), verified before
    /// it appears. The document itself does not change. `dest` must not be this document's
    /// own file.
    pub fn extract_to(
        &mut self,
        pages: &[u32],
        dest: &Path,
        signatures: SignaturePolicy,
    ) -> Result<SaveReport> {
        self.refuse_while_saving()?;
        let dest = std::path::absolute(dest).map_err(|e| DocError::io("resolve", dest, e))?;
        if self
            .path()
            .is_some_and(|p| crate::journal::same_path(p, &dest))
        {
            return Err(invalid(
                "pages cannot be extracted over the document's own file",
            ));
        }
        let doc = self.committed_document()?;
        let (part, _) = doc.extract_pages(&usize_pages(pages))?;
        self.save_part(part, &dest, signatures)
    }

    /// Writes one new file per range of pages (`ranges[i]` to `dests[i]`), as
    /// [`extract_to`](Document::extract_to) does for one.
    pub fn split_to(
        &mut self,
        ranges: &[std::ops::Range<u32>],
        dests: &[PathBuf],
        signatures: SignaturePolicy,
    ) -> Result<Vec<SaveReport>> {
        if ranges.len() != dests.len() {
            return Err(invalid("one destination is needed per range"));
        }
        let mut out = Vec::with_capacity(ranges.len());
        for (range, dest) in ranges.iter().zip(dests) {
            let pages: Vec<u32> = range.clone().collect();
            out.push(self.extract_to(&pages, dest, signatures)?);
        }
        Ok(out)
    }

    fn save_part(
        &mut self,
        part: PdfDocument,
        dest: &Path,
        signatures: SignaturePolicy,
    ) -> Result<SaveReport> {
        let backend = self.backend.clone();
        let mut check = |tmp: &Path, expected: &pl_pdf::Expected| {
            pdfium_check(backend.as_ref(), tmp, None, expected, &[], &[])
        };
        Ok(part.save_new_checked(dest, &SaveOptions::default(), signatures, &mut check)?)
    }
}

impl DocError {
    /// The signature fields a refused structural command or save would invalidate: what a
    /// user interface asks the user to agree to before trying again with
    /// [`SignaturePolicy::Invalidate`].
    pub fn signatures_to_confirm(&self) -> Option<&[String]> {
        match self {
            DocError::Pdf(PdfError::SignaturesWouldBreak { fields }) => Some(fields),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_maps_invert() {
        let map = PageMap {
            old_to_new: vec![Some(2), None, Some(0), Some(1)],
            new_page_count: 4,
        };
        let back = map.inverse();
        assert_eq!(back.old_to_new, vec![Some(2), Some(3), Some(0), None]);
        assert_eq!(back.new_page_count, 4);
        assert_eq!(map.map(1), None);
        assert_eq!(map.map(3), Some(1));
        assert!(!map.is_identity());
        assert!(PageMap::identity(3).is_identity());
        assert_eq!(back.inverse(), map);
    }

    #[test]
    fn page_maps_compose() {
        // Delete page 1 of three, then move the first page to the end.
        let delete = PageMap {
            old_to_new: vec![Some(0), None, Some(1)],
            new_page_count: 2,
        };
        let swap = PageMap {
            old_to_new: vec![Some(1), Some(0)],
            new_page_count: 2,
        };
        let both = delete.then(&swap);
        assert_eq!(both.old_to_new, vec![Some(1), None, Some(0)]);
        assert_eq!(both.new_page_count, 2);
        assert!(PageMap::identity(3).then(&delete) == delete);
    }

    #[test]
    fn commands_go_through_json_unchanged() {
        let commands = vec![
            StructCommand::InsertBlank {
                at: 1,
                count: 2,
                media_box: Some([0.0, 0.0, 612.0, 792.0]),
                rotate: 90,
            },
            StructCommand::InsertPages {
                at: 0,
                source: PageSource::File(PathBuf::from("C:/x/y.pdf")),
                pages: vec![2, 0],
            },
            StructCommand::MovePages {
                pages: vec![3],
                to: 0,
            },
            StructCommand::AddBookmark {
                parent: vec![0],
                index: 1,
                bookmark: BookmarkSpec {
                    title: "Level 2 \u{2013} east".into(),
                    target: BookmarkTarget::Page(DestSpec::at(3, 0.25, 700.5)),
                    color: Some([1.0, 0.0, 0.5]),
                    bold: true,
                    italic: false,
                    open: true,
                },
            },
            StructCommand::AddLink {
                page: 0,
                link: LinkData::new(
                    Rect::new(10.0, 10.0, 50.0, 30.0),
                    LinkTarget::File {
                        file: "other.pdf".into(),
                        page: Some(4),
                    },
                ),
            },
            StructCommand::Flatten {
                scope: FlattenScope::Annotations(vec!["a".into()]),
                print: true,
                include_form_fields: false,
            },
        ];
        for c in commands {
            let json = serde_json::to_string(&c).unwrap();
            let back: StructCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(back, c, "{json}");
        }
    }

    #[test]
    fn a_link_needs_an_area_and_an_existing_page() {
        let flat = LinkData::new(
            Rect::new(10.0, 10.0, 10.0, 30.0),
            LinkTarget::Uri("u".into()),
        );
        assert!(flat.spec(3).is_err());
        let far = LinkData::new(
            Rect::new(0.0, 0.0, 5.0, 5.0),
            LinkTarget::Page(DestSpec::page(3)),
        );
        assert!(matches!(
            far.spec(3),
            Err(DocError::PageOutOfRange { page: 3, count: 3 })
        ));
        assert!(far.spec(4).is_ok());
    }
}
