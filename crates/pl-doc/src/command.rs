//! Editing commands, their exact inverses, and the undo history ([`Editor`]).
//!
//! A [`Command`] is plain serialisable data that says what to change. Executing one through
//! [`Editor::execute`] applies it to the [`AnnotSet`] and records how to take it back
//! exactly: an annotation that is removed and put back returns to the same place in the
//! stacking order and keeps its tie to the file, so an undone edit of a markup that came from
//! the file leaves nothing for a save to write.
//!
//! Nothing here knows about files, renderers or journals; [`crate::Document`] adds those.

use std::collections::{HashMap, HashSet};

use pl_annot::{AnnotId, AnnotKind, AnnotSet, Annotation, Removed, Viewport, ViewportError};
use pl_geom::Rect;
use serde::{Deserialize, Serialize};

use crate::annot_data::{as_data, PdfValue};
use crate::measure_data::{measurement_is_finite, PageScalesData, ScaleSetting};
use crate::scales::{page_data, DocSettings, ScaleStore, TakeoffRefresh};

/// Why a command was refused. A refused command changes nothing.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    #[error("there is no annotation {0}")]
    NotFound(AnnotId),
    #[error("an annotation with the id {0} already exists")]
    DuplicateId(AnnotId),
    #[error("annotation {id} is on page {page}, but the document has {count} pages")]
    PageOutOfRange {
        id: AnnotId,
        page: usize,
        count: usize,
    },
    #[error("annotation {0} is of a kind that can be moved and deleted but not created")]
    Unmodelled(AnnotId),
    #[error("annotation {0} is written inside the page's annotation list and cannot be changed")]
    NotEditable(AnnotId),
    #[error("annotation {0} has a coordinate or style value that is not a finite number")]
    NotFinite(AnnotId),
    #[error("annotation {0} cannot change its kind")]
    KindChange(AnnotId),
    #[error("annotation {0} cannot change page; delete it and add it on the other page")]
    PageChange(AnnotId),
    /// The change is not one this annotation can take: text a text box cannot show (the
    /// reason names the character), a reply to something that cannot be replied to, and the
    /// like.
    #[error("annotation {id}: {reason}")]
    Unsupported { id: AnnotId, reason: String },
    /// A scale or viewport command names a page the document does not have.
    #[error("page {} is out of range: the document has {count} pages", page + 1)]
    NoPage { page: usize, count: usize },
    /// A scale that cannot be used (an unknown unit, a side that is not a positive number).
    #[error("{0}")]
    Scale(String),
    #[error("{0}")]
    Viewport(ViewportError),
    #[error("page {} has no viewport named {name:?}", page + 1)]
    NoViewport { page: usize, name: String },
    /// Takeoff definitions that do not fit the document ([`DocSettings::validate`]).
    #[error("{0}")]
    Settings(String),
    /// A measurement command was given an annotation that is not a measurement of the kind
    /// it needs.
    #[error("annotation {id} is not {what}")]
    NotMeasurement { id: AnnotId, what: &'static str },
    /// A column definition that cannot be made (a name in use, a choice without options).
    #[error(transparent)]
    Column(#[from] pl_takeoff::ColumnError),
}

mod ids {
    use pl_annot::AnnotId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[AnnotId], s: S) -> Result<S::Ok, S::Error> {
        v.iter()
            .map(AnnotId::as_str)
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<AnnotId>, D::Error> {
        Ok(Vec::<String>::deserialize(d)?
            .into_iter()
            .map(AnnotId)
            .collect())
    }
}

/// One change to a document's annotations.
///
/// Annotations inside a command are data only: their tie to a file, if they have one, is
/// ignored. Every command has an exact inverse, which [`Editor::undo`] applies and
/// [`Editor::inverse_of_last`] states as a command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Adds new annotations, each on top of its page's stack.
    AddAnnotations(#[serde(with = "as_data::list")] Vec<Annotation>),
    /// Gives existing annotations (found by id) new field values. The kind and the page
    /// cannot change.
    ModifyAnnotations(#[serde(with = "as_data::list")] Vec<Annotation>),
    /// Removes annotations, each with its popup and replies.
    DeleteAnnotations(#[serde(with = "ids")] Vec<AnnotId>),
    /// Restacks one page: `order` lists annotations from bottom to top; annotations of the
    /// page it leaves out stay beneath them in their present order.
    Reorder {
        page: usize,
        #[serde(with = "ids")]
        order: Vec<AnnotId>,
    },
    /// Puts annotations at exact positions in the document's stacking order (ascending). This
    /// is the inverse of a delete.
    Restore(#[serde(with = "as_data::indexed")] Vec<(usize, Annotation)>),
    /// Several commands as one undo step, applied in order.
    Batch(Vec<Command>),
    /// Sets (or, with `None`, clears) a page's own scale. Every measurement on the page is
    /// put on the scale that now applies where it is, in the same undo step.
    SetPageScale {
        page: usize,
        scale: Option<ScaleSetting>,
    },
    /// [`Command::SetPageScale`] for every page from `first` to `last` (both included, from 0).
    ApplyScaleToPages {
        first: usize,
        last: usize,
        scale: Option<ScaleSetting>,
    },
    /// Adds a viewport on top of the page's others. `bbox` is `[x0, y0, x1, y1]`. The
    /// measurements on the page are put on the scale that now applies where each is.
    AddViewport {
        page: usize,
        name: String,
        bbox: [f64; 4],
        scale: Option<ScaleSetting>,
    },
    /// Gives a viewport a new box and scale.
    EditViewport {
        page: usize,
        name: String,
        bbox: [f64; 4],
        scale: Option<ScaleSetting>,
    },
    RemoveViewport {
        page: usize,
        name: String,
    },
    /// Puts a page's scales exactly as given (the inverse of every scale command). The
    /// measurements on the page are not touched.
    SetScales {
        page: usize,
        scales: PageScalesData,
    },
    /// Replaces the document settings: the default scale and the legend, space and sequence
    /// definitions. A change of the default scale puts the measurements it applies to on it.
    SetDocSettings(Box<DocSettings>),
    /// Puts the document settings exactly as given, touching no measurement (the inverse of
    /// [`Command::SetDocSettings`]).
    RestoreDocSettings(Box<DocSettings>),
    /// Replaces the entries of Bluebeam Revu's custom column definitions (the catalog's
    /// `/BSIAnnotColumns`), exactly as given. The values in the annotations are not touched
    /// (see [`crate::markup_ops::add_bluebeam_column`], which pads them in the same step).
    SetBluebeamColumns(Vec<PdfValue>),
}

impl Command {
    pub fn add(annotation: Annotation) -> Command {
        Command::AddAnnotations(vec![annotation])
    }

    pub fn modify(annotation: Annotation) -> Command {
        Command::ModifyAnnotations(vec![annotation])
    }

    pub fn delete(id: AnnotId) -> Command {
        Command::DeleteAnnotations(vec![id])
    }

    /// A short name for menus: "Undo Move", say, is the caller's wording; this is the kind.
    pub fn name(&self) -> &'static str {
        match self {
            Command::AddAnnotations(_) => "add",
            Command::ModifyAnnotations(_) => "modify",
            Command::DeleteAnnotations(_) => "delete",
            Command::Reorder { .. } => "reorder",
            Command::Restore(_) => "restore",
            Command::Batch(_) => "batch",
            Command::SetPageScale { .. } => "set page scale",
            Command::ApplyScaleToPages { .. } => "apply scale to pages",
            Command::AddViewport { .. } => "add viewport",
            Command::EditViewport { .. } => "edit viewport",
            Command::RemoveViewport { .. } => "remove viewport",
            Command::SetScales { .. } => "set scales",
            Command::SetDocSettings(_) => "set document settings",
            Command::RestoreDocSettings(_) => "restore document settings",
            Command::SetBluebeamColumns(_) => "set Bluebeam columns",
        }
    }
}

/// A drag key ([`Editor::execute_merged`]) with this bit set makes one undo step of *every*
/// command given with it in a row, whatever each does and to which annotations: the way an
/// eraser drag, which changes some strokes and deletes others as it goes, is taken back with
/// one undo. A key without the bit merges only consecutive modifications of the same
/// annotations.
pub const GROUP_KEY: u64 = 1 << 63;

/// From this many annotations on, an add or a delete is done in one pass over the set instead
/// of one annotation at a time (which costs a pass each: a large paste or import would take
/// minutes). Both ways give the same document.
const BULK: usize = 32;

fn empty_removed() -> Removed {
    Removed {
        items: Vec::new(),
        unlinked_parent: None,
    }
}

/// One primitive change, holding what both directions need.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// Applied: the annotation is in the set and `out` is empty. Not applied: `out` holds it.
    Add { id: AnnotId, out: Removed },
    /// Applied: `out` holds what was removed. Not applied: `out` is empty.
    Delete { id: AnnotId, out: Removed },
    /// Many annotations added by one command, put into the set in one pass ([`BULK`]).
    /// `ids` are in the order the command gave them; `out` is as for `Add`.
    AddMany { ids: Vec<AnnotId>, out: Removed },
    /// Many annotations deleted by one command, taken out of the set in one pass.
    DeleteMany { ids: Vec<AnnotId>, out: Removed },
    Replace {
        id: AnnotId,
        before: Box<Annotation>,
        after: Box<Annotation>,
    },
    Order {
        page: usize,
        before: Vec<AnnotId>,
        after: Vec<AnnotId>,
    },
    /// A page's scales, as plain data ([`ScaleStore::set_page_data`]).
    Scales {
        page: usize,
        before: Box<PageScalesData>,
        after: Box<PageScalesData>,
    },
    Settings {
        before: Box<DocSettings>,
        after: Box<DocSettings>,
    },
    /// The entries of `/BSIAnnotColumns`.
    Bluebeam {
        before: Vec<pl_pdf::Object>,
        after: Vec<pl_pdf::Object>,
    },
}

/// Copies every field of the model from `from`, leaving the tie to the file alone.
fn assign(to: &mut Annotation, from: &Annotation) {
    to.id = from.id.clone();
    to.page = from.page;
    to.kind = from.kind.clone();
    to.geometry = from.geometry.clone();
    to.style = from.style.clone();
    to.meta = from.meta.clone();
    to.icon = from.icon.clone();
    to.open = from.open;
    to.popup = from.popup.clone();
    to.parent = from.parent.clone();
    to.link = from.link.clone();
    to.app_data = from.app_data.clone();
    to.free_text = from.free_text.clone();
    to.stamp = from.stamp.clone();
    to.measure = from.measure.clone();
    to.column_data = from.column_data.clone();
}

/// `a` as it goes into the set by an add or a modification: a measurement's text is its
/// value, worked out afresh from its geometry and scale (which is also what a save writes as
/// its `/Contents` and draws in its appearance). A restore puts back exactly what it is given.
fn normalised(a: &Annotation) -> Annotation {
    let mut b = a.detached();
    b.sync_measurement();
    b
}

/// True when two measure dictionaries state the same scale: the same metres per page unit
/// along each axis (whatever units and precision they show values in).
fn same_scale(a: &pl_annot::MeasureSpec, b: &pl_annot::MeasureSpec) -> bool {
    let close = |x: f64, y: f64| (x - y).abs() <= 1e-9 * x.abs().max(y.abs());
    match (a.metres_per_unit(), b.metres_per_unit()) {
        (Some(ma), Some(mb)) => {
            close(a.x_factor() * ma, b.x_factor() * mb)
                && close(a.y_factor() * ma, b.y_factor() * mb)
        }
        _ => a == b,
    }
}

/// The annotations a restack concerns: those of `to` from the lowest position whose occupant
/// changed, upwards. Everything beneath that position is where it was, under the same things.
fn restacked<'a>(from: &[AnnotId], to: &'a [AnnotId]) -> &'a [AnnotId] {
    let first = from
        .iter()
        .zip(to)
        .position(|(a, b)| a != b)
        .unwrap_or(from.len().min(to.len()));
    &to[first..]
}

fn page_ids(set: &AnnotSet, page: usize) -> Vec<AnnotId> {
    set.on_page(page).map(|a| a.id.clone()).collect()
}

impl Step {
    fn apply(&mut self, set: &mut AnnotSet, scales: &mut ScaleStore, touched: &mut Vec<AnnotId>) {
        match self {
            Step::Add { out, .. } => {
                touched.extend(out.ids().cloned());
                touched.extend(out.unlinked_parent.clone());
                set.restore(std::mem::replace(out, empty_removed()));
            }
            Step::Delete { id, out } => {
                *out = set.remove(id);
                touched.extend(out.ids().cloned());
                touched.extend(out.unlinked_parent.clone());
            }
            Step::AddMany { ids, out } => {
                touched.extend(ids.iter().cloned());
                touched.extend(out.unlinked_parent.clone());
                set.restore(std::mem::replace(out, empty_removed()));
            }
            Step::DeleteMany { ids, out } => {
                *out = set.remove_many(ids);
                touched.extend(out.ids().cloned());
                touched.extend(out.unlinked_parent.clone());
            }
            Step::Replace { id, after, .. } => {
                if let Some(a) = set.get_mut(id) {
                    assign(a, after);
                }
                touched.push(id.clone());
            }
            Step::Order {
                page,
                before,
                after,
            } => {
                set.set_order(*page, after);
                touched.extend(restacked(before, after).iter().cloned());
            }
            Step::Scales { page, after, .. } => scales.set_page_data(*page, after),
            Step::Settings { after, .. } => scales.set_settings((**after).clone()),
            Step::Bluebeam { after, .. } => scales.set_bluebeam_entries(after.clone()),
        }
    }

    fn unapply(&mut self, set: &mut AnnotSet, scales: &mut ScaleStore, touched: &mut Vec<AnnotId>) {
        match self {
            Step::Add { id, out } => {
                *out = set.remove(id);
                touched.extend(out.ids().cloned());
                touched.extend(out.unlinked_parent.clone());
            }
            Step::Delete { out, .. } | Step::DeleteMany { out, .. } => {
                touched.extend(out.ids().cloned());
                touched.extend(out.unlinked_parent.clone());
                set.restore(std::mem::replace(out, empty_removed()));
            }
            Step::AddMany { ids, out } => {
                *out = set.remove_many(ids);
                touched.extend(out.ids().cloned());
                touched.extend(out.unlinked_parent.clone());
            }
            Step::Replace { id, before, .. } => {
                if let Some(a) = set.get_mut(id) {
                    assign(a, before);
                }
                touched.push(id.clone());
            }
            Step::Order {
                page,
                before,
                after,
            } => {
                set.set_order(*page, before);
                touched.extend(restacked(after, before).iter().cloned());
            }
            Step::Scales { page, before, .. } => scales.set_page_data(*page, before),
            Step::Settings { before, .. } => scales.set_settings((**before).clone()),
            Step::Bluebeam { before, .. } => scales.set_bluebeam_entries(before.clone()),
        }
    }

    /// Notes in `out` the pages the step changes something on, `set` being in the state
    /// the step left it in.
    fn note_pages(&self, set: &AnnotSet, out: &mut TakeoffRefresh) {
        let mut of = |id: &AnnotId| {
            if let Some(a) = set.get(id) {
                out.pages.insert(a.page);
            }
        };
        match self {
            Step::Add { id, out: held } | Step::Delete { id, out: held } => {
                of(id);
                out.pages.extend(held.items.iter().map(|(_, a)| a.page));
            }
            Step::AddMany { ids, out: held } | Step::DeleteMany { ids, out: held } => {
                ids.iter().for_each(&mut of);
                out.pages.extend(held.items.iter().map(|(_, a)| a.page));
            }
            Step::Replace { before, .. } => {
                out.pages.insert(before.page);
            }
            Step::Order { page, .. } | Step::Scales { page, .. } => {
                out.pages.insert(*page);
            }
            Step::Settings { .. } | Step::Bluebeam { .. } => out.definitions = true,
        }
    }

    /// The command that undoes this step, which must be applied.
    fn inverse(&self) -> Command {
        match self {
            Step::Add { id, .. } => Command::DeleteAnnotations(vec![id.clone()]),
            Step::AddMany { ids, .. } => Command::DeleteAnnotations(ids.clone()),
            Step::Delete { out, .. } | Step::DeleteMany { out, .. } => {
                Command::Restore(out.items.iter().map(|(i, a)| (*i, a.detached())).collect())
            }
            Step::Replace { before, .. } => Command::ModifyAnnotations(vec![(**before).clone()]),
            Step::Order { page, before, .. } => Command::Reorder {
                page: *page,
                order: before.clone(),
            },
            Step::Scales { page, before, .. } => Command::SetScales {
                page: *page,
                scales: (**before).clone(),
            },
            Step::Settings { before, .. } => Command::RestoreDocSettings(before.clone()),
            Step::Bluebeam { before, .. } => {
                Command::SetBluebeamColumns(before.iter().map(PdfValue::from_object).collect())
            }
        }
    }

    fn held_mut(&mut self) -> Option<&mut Removed> {
        match self {
            Step::Add { out, .. }
            | Step::Delete { out, .. }
            | Step::AddMany { out, .. }
            | Step::DeleteMany { out, .. } => Some(out),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    serial: u64,
    command: Command,
    steps: Vec<Step>,
    merge: Option<u64>,
    /// Whether the command is in the current journal (see [`crate::journal`]).
    journaled: bool,
}

/// What a change did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    /// False when the command asked for nothing that differs from the present state; nothing
    /// was recorded then.
    pub changed: bool,
    /// True when the command was folded into the previous undo step (a continued drag).
    pub merged: bool,
    /// Every annotation that was added, removed, altered or restacked, without repeats.
    pub touched: Vec<AnnotId>,
}

/// What [`Editor::undo`] or [`Editor::redo`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Replayed {
    pub touched: Vec<AnnotId>,
    /// The change that was made, as a command: the inverse of the undone command, or the
    /// redone command itself.
    pub command: Command,
    /// Whether the undone or redone step is one the current journal recorded.
    pub(crate) journaled: bool,
}

/// The order of the selected annotations is the order they were selected in.
pub type Selection = Vec<AnnotId>;

/// A document's annotations with their undo history, selection and unsaved-changes state.
#[derive(Debug, Clone)]
pub struct Editor {
    set: AnnotSet,
    /// The pages' scales and viewports and the document settings.
    scales: ScaleStore,
    /// What changed since [`Editor::take_takeoff_refresh`] was last called.
    refresh: TakeoffRefresh,
    page_count: usize,
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    serial: u64,
    /// The serial of the state beneath the oldest undo step.
    floor: u64,
    /// The serial of the state that is on disk; `None` when no state in reach is.
    saved: Option<u64>,
    selection: Selection,
    /// The selection again, for asking whether one annotation is in it.
    selected: HashSet<AnnotId>,
    /// Goes up whenever the selection changes.
    selection_serial: u64,
    revision: u64,
}

fn dedup(ids: Vec<AnnotId>) -> Vec<AnnotId> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

impl Editor {
    /// An editor over `set` for a document of `page_count` pages. The set is taken as the
    /// state on disk.
    pub fn new(set: AnnotSet, page_count: usize) -> Editor {
        Editor::with_scales(set, ScaleStore::empty(page_count), page_count)
    }

    /// As [`Editor::new`], with the document's scales and settings as the file holds them.
    pub fn with_scales(set: AnnotSet, mut scales: ScaleStore, page_count: usize) -> Editor {
        if scales.page_count() != page_count {
            scales.resize(page_count);
        }
        Editor {
            set,
            scales,
            refresh: TakeoffRefresh::default(),
            page_count,
            undo: Vec::new(),
            redo: Vec::new(),
            serial: 0,
            floor: 0,
            saved: Some(0),
            selection: Vec::new(),
            selected: HashSet::new(),
            selection_serial: 0,
            revision: 0,
        }
    }

    pub fn set(&self) -> &AnnotSet {
        &self.set
    }

    /// Every page's scales and viewports, and the document settings.
    pub fn scales(&self) -> &ScaleStore {
        &self.scales
    }

    /// What has changed (pages, takeoff definitions) since this was last called: the
    /// refresh hook of legends, spaces and sequences (see [`TakeoffRefresh`]).
    pub fn take_takeoff_refresh(&mut self) -> TakeoffRefresh {
        std::mem::take(&mut self.refresh)
    }

    fn note_refresh(&mut self, steps: &[Step]) {
        for step in steps {
            step.note_pages(&self.set, &mut self.refresh);
        }
    }

    /// True when a save would write something: an annotation, a page's scales or the
    /// document settings differ from the file.
    pub fn has_unsaved(&self) -> bool {
        self.set.is_dirty() || self.scales.is_dirty()
    }

    /// Replaces the annotations with the same annotations rebased on a saved file (a save
    /// that ran on a copy of the set). The history and the selection are untouched.
    pub(crate) fn replace_set(&mut self, set: AnnotSet) {
        self.set = set;
        self.prune_selection();
    }

    /// Replaces the scales with the same scales read back from a saved file.
    pub(crate) fn replace_scales(&mut self, mut scales: ScaleStore) {
        if scales.page_count() != self.page_count {
            scales.resize(self.page_count);
        }
        self.scales = scales;
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub(crate) fn set_page_count(&mut self, page_count: usize) {
        self.page_count = page_count;
        self.scales.resize(page_count);
    }

    /// The annotations as plain values in stacking order, without their ties to the file:
    /// what two states are compared by.
    pub fn model(&self) -> Vec<Annotation> {
        self.set.iter().map(Annotation::detached).collect()
    }

    /// Goes up with every change to the annotations (commands, undo, redo, save).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// The command the next undo takes back.
    pub fn undo_command(&self) -> Option<&Command> {
        self.undo.last().map(|e| &e.command)
    }

    /// The command the next redo applies again.
    pub fn redo_command(&self) -> Option<&Command> {
        self.redo.last().map(|e| &e.command)
    }

    /// The exact inverse of the most recent command, as a command.
    pub fn inverse_of_last(&self) -> Option<Command> {
        self.undo.last().map(inverse_of)
    }

    fn state_serial(&self) -> u64 {
        self.undo.last().map_or(self.floor, |e| e.serial)
    }

    /// Whether the annotations differ from what was last saved (or loaded), as far as the
    /// history can tell: undoing back to the saved state makes this false again.
    pub fn is_modified(&self) -> bool {
        Some(self.state_serial()) != self.saved
    }

    // ---- selection ---------------------------------------------------------------------------

    pub fn selection(&self) -> &[AnnotId] {
        &self.selection
    }

    /// Whether an annotation is selected. The answer does not take longer for a large
    /// selection.
    pub fn is_selected(&self, id: &AnnotId) -> bool {
        self.selected.contains(id)
    }

    /// A number that changes whenever the selection does: a user interface keys what it
    /// derives from the selection by it instead of working it out every frame.
    pub fn selection_serial(&self) -> u64 {
        self.selection_serial
    }

    /// Called after `selection` was replaced or lost members.
    fn selection_replaced(&mut self) {
        self.selected = self.selection.iter().cloned().collect();
        self.selection_serial += 1;
    }

    /// Replaces the selection. Ids that are not in the document, and repeats, are dropped.
    pub fn select(&mut self, ids: impl IntoIterator<Item = AnnotId>) {
        let set = &self.set;
        let new = dedup(ids.into_iter().filter(|id| set.contains(id)).collect());
        if new != self.selection {
            self.selection = new;
            self.selection_replaced();
        }
    }

    /// Adds an annotation to the selection (it becomes the last selected).
    pub fn select_also(&mut self, id: AnnotId) {
        if !self.set.contains(&id) {
            return;
        }
        if !self.selected.insert(id.clone()) {
            self.selection.retain(|s| *s != id);
        }
        self.selection.push(id);
        self.selection_serial += 1;
    }

    /// Adds the annotation to the selection, or removes it if it is selected. Returns whether
    /// it is selected afterwards.
    pub fn toggle_selected(&mut self, id: &AnnotId) -> bool {
        if self.selected.remove(id) {
            self.selection.retain(|s| s != id);
            self.selection_serial += 1;
            false
        } else {
            self.select_also(id.clone());
            self.is_selected(id)
        }
    }

    pub fn clear_selection(&mut self) {
        if !self.selection.is_empty() {
            self.selection.clear();
            self.selected.clear();
            self.selection_serial += 1;
        }
    }

    /// Selects every annotation of a page that can be hit (no popups, nothing hidden).
    pub fn select_page(&mut self, page: usize) {
        self.selection = self
            .set
            .on_page(page)
            .filter(|a| a.kind != AnnotKind::Popup && !a.meta.flags.is_hidden())
            .map(|a| a.id.clone())
            .collect();
        self.selection_replaced();
    }

    fn prune_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let set = &self.set;
        let before = self.selection.len();
        self.selection.retain(|id| set.contains(id));
        if self.selection.len() != before {
            self.selection_replaced();
        }
    }

    // ---- commands ----------------------------------------------------------------------------

    /// Applies a command and records it as one undo step. A refused command changes nothing.
    pub fn execute(&mut self, command: Command) -> Result<Outcome, EditError> {
        self.run(command, None)
    }

    /// As [`execute`](Editor::execute) for one step of a drag. Consecutive
    /// `ModifyAnnotations` commands with the same `drag` key over the same annotations become
    /// a single undo step that goes from the state before the first to the state after the
    /// last.
    pub fn execute_merged(&mut self, command: Command, drag: u64) -> Result<Outcome, EditError> {
        self.run(command, Some(drag))
    }

    fn run(&mut self, command: Command, merge: Option<u64>) -> Result<Outcome, EditError> {
        if let (Some(key), Command::ModifyAnnotations(items)) = (merge, &command) {
            if self.can_merge(key, items) {
                for a in items {
                    self.check_modify(a)?;
                }
                let top = self.undo.last_mut().expect("can_merge saw it");
                let mut touched = Vec::new();
                for (step, a) in top.steps.iter_mut().zip(items) {
                    if let Step::Replace { after, .. } = step {
                        **after = normalised(a);
                    }
                    step.apply(&mut self.set, &mut self.scales, &mut touched);
                }
                for step in &top.steps {
                    step.note_pages(&self.set, &mut self.refresh);
                }
                self.serial += 1;
                top.serial = self.serial;
                top.command = command;
                // Nothing can be redoable above a step that has just changed (can_merge
                // already refuses a step that an undo, redo or save has been past).
                self.redo.clear();
                self.revision += 1;
                return Ok(Outcome {
                    changed: true,
                    merged: true,
                    touched: dedup(touched),
                });
            }
        }
        let mut steps = Vec::new();
        let mut touched = Vec::new();
        let mut gone = HashSet::new();
        if let Err(e) = self.build(&command, &mut steps, &mut touched, &mut gone) {
            let mut scratch = Vec::new();
            for step in steps.iter_mut().rev() {
                step.unapply(&mut self.set, &mut self.scales, &mut scratch);
            }
            return Err(e);
        }
        if steps.is_empty() {
            return Ok(Outcome::default());
        }
        self.note_refresh(&steps);
        // A grouping key: the command joins the step on top when that step has the same key
        // (which an undo, a redo and a save all clear).
        let joins = merge.is_some_and(|key| {
            key & GROUP_KEY != 0 && self.undo.last().is_some_and(|top| top.merge == Some(key))
        });
        if joins {
            self.serial += 1;
            let top = self.undo.last_mut().expect("joins saw it");
            top.steps.extend(steps);
            top.serial = self.serial;
            let mut list = match std::mem::replace(&mut top.command, Command::Batch(Vec::new())) {
                Command::Batch(list) => list,
                single => vec![single],
            };
            list.push(command);
            top.command = Command::Batch(list);
            self.redo.clear();
            self.revision += 1;
            self.prune_selection();
            return Ok(Outcome {
                changed: true,
                merged: true,
                touched: dedup(touched),
            });
        }
        self.serial += 1;
        self.undo.push(Entry {
            serial: self.serial,
            command,
            steps,
            merge,
            journaled: true,
        });
        self.redo.clear();
        self.revision += 1;
        self.prune_selection();
        Ok(Outcome {
            changed: true,
            merged: false,
            touched: dedup(touched),
        })
    }

    /// Applies a command without recording an undo step, leaving the history as it is. This
    /// is for replaying a journal; afterwards the document counts as modified.
    pub(crate) fn apply_raw(&mut self, command: &Command) -> Result<Outcome, EditError> {
        let mut steps = Vec::new();
        let mut touched = Vec::new();
        let mut gone = HashSet::new();
        if let Err(e) = self.build(command, &mut steps, &mut touched, &mut gone) {
            let mut scratch = Vec::new();
            for step in steps.iter_mut().rev() {
                step.unapply(&mut self.set, &mut self.scales, &mut scratch);
            }
            return Err(e);
        }
        self.note_refresh(&steps);
        if !steps.is_empty() {
            self.saved = None;
            self.revision += 1;
            self.prune_selection();
        }
        Ok(Outcome {
            changed: !steps.is_empty(),
            merged: false,
            touched: dedup(touched),
        })
    }

    fn can_merge(&self, key: u64, items: &[Annotation]) -> bool {
        let Some(top) = self.undo.last() else {
            return false;
        };
        top.merge == Some(key)
            && top.steps.len() == items.len()
            && top
                .steps
                .iter()
                .zip(items)
                .all(|(step, a)| matches!(step, Step::Replace { id, .. } if *id == a.id))
    }

    /// The annotation to put into the set for `a`. An annotation of the file that was deleted
    /// and not yet saved away comes back as itself, with its tie to the file, so that a save
    /// changes it in place instead of deleting one object and adding another.
    fn materialise(&self, a: &Annotation) -> Annotation {
        match self.set.removed().iter().find(|r| r.id == a.id) {
            Some(deleted) => {
                let mut back = deleted.clone();
                assign(&mut back, a);
                back
            }
            None => a.detached(),
        }
    }

    /// Whether [`Command::AddAnnotations`] would take `a` as the document is now.
    pub fn check_addable(&self, a: &Annotation) -> Result<(), EditError> {
        self.check_new(a)
    }

    fn check_new(&self, a: &Annotation) -> Result<(), EditError> {
        let pending = self.set.removed().iter().find(|r| r.id == a.id);
        self.check_new_against(a, pending)
    }

    /// `pending` is the annotation of the file with this id that was deleted and not yet
    /// saved away, if there is one.
    fn check_new_against(
        &self,
        a: &Annotation,
        pending: Option<&Annotation>,
    ) -> Result<(), EditError> {
        if self.set.contains(&a.id) || pending.is_some_and(|r| r.kind != a.kind) {
            return Err(EditError::DuplicateId(a.id.clone()));
        }
        if a.page >= self.page_count {
            return Err(EditError::PageOutOfRange {
                id: a.id.clone(),
                page: a.page,
                count: self.page_count,
            });
        }
        if !a.is_modelled() && pending.is_none() {
            return Err(EditError::Unmodelled(a.id.clone()));
        }
        check_finite(a)
    }

    fn check_modify(&self, a: &Annotation) -> Result<(), EditError> {
        let old = self
            .set
            .get(&a.id)
            .ok_or_else(|| EditError::NotFound(a.id.clone()))?;
        if old.kind != a.kind {
            return Err(EditError::KindChange(a.id.clone()));
        }
        if old.page != a.page {
            return Err(EditError::PageChange(a.id.clone()));
        }
        check_editable(old)?;
        check_finite(a)
    }

    /// Where a new annotation of `page` goes: after the last annotation of that page or of
    /// any page before it, so the set stays in page order (the order a reload gives).
    fn top_of_page(&self, page: usize) -> usize {
        self.set.top_of_page(page)
    }

    /// `AddAnnotations` for many annotations: the same checks and the same resulting order as
    /// adding them one after the other, in one pass over the set.
    fn add_many(
        &mut self,
        items: &[Annotation],
        steps: &mut Vec<Step>,
        touched: &mut Vec<AnnotId>,
    ) -> Result<(), EditError> {
        let pending: HashMap<&AnnotId, &Annotation> = {
            let mut map = HashMap::new();
            for r in self.set.removed() {
                map.entry(&r.id).or_insert(r);
            }
            map
        };
        let mut coming: HashSet<&AnnotId> = HashSet::with_capacity(items.len());
        let mut new: Vec<Annotation> = Vec::with_capacity(items.len());
        for a in items {
            let was = pending.get(&a.id).copied();
            if !coming.insert(&a.id) {
                return Err(EditError::DuplicateId(a.id.clone()));
            }
            self.check_new_against(a, was)?;
            let a = normalised(a);
            new.push(match was {
                Some(deleted) => {
                    let mut back = deleted.clone();
                    assign(&mut back, &a);
                    back
                }
                None => a,
            });
        }
        // One at a time, each goes before the first annotation of a later page. For the set
        // as it is, that position is found from the pages at which the highest page so far
        // goes up; the new ones that share a position then stand in page order, and within a
        // page in the order they were given.
        let mut rises: Vec<(usize, usize)> = Vec::new();
        for (at, a) in self.set.iter().enumerate() {
            if rises.last().is_none_or(|(page, _)| a.page > *page) {
                rises.push((a.page, at));
            }
        }
        let len = self.set.len();
        let before = |page: usize| {
            let k = rises.partition_point(|(p, _)| *p <= page);
            rises.get(k).map_or(len, |(_, at)| *at)
        };
        let mut placed: Vec<(usize, Annotation)> =
            new.into_iter().map(|a| (before(a.page), a)).collect();
        placed.sort_by_key(|(at, a)| (*at, a.page));
        for (shift, (at, _)) in placed.iter_mut().enumerate() {
            *at += shift;
        }
        let mut step = Step::AddMany {
            ids: items.iter().map(|a| a.id.clone()).collect(),
            out: Removed {
                items: placed,
                unlinked_parent: None,
            },
        };
        step.apply(&mut self.set, &mut self.scales, touched);
        steps.push(step);
        Ok(())
    }

    fn check_page(&self, page: usize) -> Result<(), EditError> {
        if page < self.page_count {
            Ok(())
        } else {
            Err(EditError::NoPage {
                page,
                count: self.page_count,
            })
        }
    }

    /// The measure dictionary of `scale` on `page`.
    fn spec_on(
        &self,
        scale: Option<&ScaleSetting>,
        page: usize,
    ) -> Result<Option<pl_annot::MeasureSpec>, EditError> {
        scale
            .map(|s| s.spec(self.scales.user_unit(page)))
            .transpose()
            .map_err(EditError::Scale)
    }

    /// Puts a page's scales as `after` says, then every measurement of the page on the scale
    /// that applies where it is: all in the steps of the command being built.
    fn change_scales(
        &mut self,
        page: usize,
        after: PageScalesData,
        steps: &mut Vec<Step>,
        touched: &mut Vec<AnnotId>,
    ) {
        let before = self.scales.page_data(page);
        if before == after {
            return;
        }
        let followers = self.store_followers(page);
        let mut step = Step::Scales {
            page,
            before: Box::new(before),
            after: Box::new(after),
        };
        step.apply(&mut self.set, &mut self.scales, touched);
        steps.push(step);
        self.rescale_page(page, &followers, steps, touched);
    }

    /// The measurements of `page` whose measure dictionary is the very one the
    /// scales give where they are. They follow a change of that scale in full; the others were
    /// given units or a precision of their own (a tool preset's), which a change that leaves
    /// the value of their scale alone does not undo.
    fn store_followers(&self, page: usize) -> HashSet<AnnotId> {
        self.set
            .on_page(page)
            .filter(|a| {
                a.measurement().is_some_and(|m| {
                    m.spec.is_some()
                        && self
                            .scales
                            .resolve(page, &crate::measure_ops::geometry_points(&a.geometry))
                            .is_some_and(|(r, s)| r == m.scale && Some(&s) == m.spec.as_ref())
                })
            })
            .map(|a| a.id.clone())
            .collect()
    }

    /// `a` put on the scale that applies where it is now (the topmost viewport holding all its
    /// points, else its page's scale, else the document's default); `None` when that changes
    /// nothing, or no scale applies there (it then keeps the one it has). A count's value does
    /// not depend on it, but it carries it like every other kind (Revu's `/BBMeasure`).
    ///
    /// With `follow` the measure dictionary becomes the scale's own. Without it, a measurement
    /// whose scale is the same in value keeps its dictionary (its units and precision) and
    /// only takes the new scale's name.
    fn rescaled(&self, a: &Annotation, follow: bool) -> Option<Annotation> {
        let m = a.measurement()?;
        let points = crate::measure_ops::geometry_points(&a.geometry);
        let (scale, spec) = self.scales.resolve(a.page, &points)?;
        let keep = !follow && m.spec.as_ref().is_some_and(|s| same_scale(s, &spec));
        let mut after = a.clone();
        if keep {
            if m.scale == scale {
                return None;
            }
            if let Some(m) = after.measure.as_mut() {
                m.scale = scale;
            }
            after.sync_measurement();
        } else {
            after.apply_scale(scale, spec);
        }
        (after != *a).then_some(after)
    }

    /// Puts every measurement of `page` (counts included) on the scale that
    /// applies where it is. A measurement in a place with no scale keeps the one it has; one
    /// not in `followers` keeps its own presentation while its scale's value is unchanged.
    fn rescale_page(
        &mut self,
        page: usize,
        followers: &HashSet<AnnotId>,
        steps: &mut Vec<Step>,
        touched: &mut Vec<AnnotId>,
    ) {
        let measured: Vec<Annotation> = self
            .set
            .on_page(page)
            .filter(|a| a.measurement().is_some())
            .map(Annotation::detached)
            .collect();
        for before in measured {
            let follow = followers.contains(&before.id);
            let Some(after) = self.rescaled(&before, follow) else {
                continue;
            };
            {
                let mut step = Step::Replace {
                    id: before.id.clone(),
                    before: Box::new(before),
                    after: Box::new(after),
                };
                step.apply(&mut self.set, &mut self.scales, touched);
                steps.push(step);
            }
        }
    }

    /// The scales of `page` with one viewport changed by `change`.
    fn with_viewports(
        &self,
        page: usize,
        change: impl FnOnce(&mut pl_annot::PageViewports) -> Result<(), EditError>,
    ) -> Result<PageScalesData, EditError> {
        self.check_page(page)?;
        let mut vps = self
            .scales
            .page(page)
            .cloned()
            .unwrap_or_else(|| pl_annot::PageViewports::new(page, Rect::ZERO));
        change(&mut vps)?;
        Ok(page_data(&vps))
    }

    fn build(
        &mut self,
        command: &Command,
        steps: &mut Vec<Step>,
        touched: &mut Vec<AnnotId>,
        gone: &mut HashSet<AnnotId>,
    ) -> Result<(), EditError> {
        fn push(
            mut step: Step,
            steps: &mut Vec<Step>,
            set: &mut AnnotSet,
            scales: &mut ScaleStore,
            touched: &mut Vec<AnnotId>,
        ) {
            step.apply(set, scales, touched);
            steps.push(step);
        }
        let bbox_rect = |b: &[f64; 4]| -> Result<Rect, EditError> {
            if b.iter().all(|v| v.is_finite()) {
                Ok(Rect::new(b[0], b[1], b[2], b[3]))
            } else {
                Err(EditError::Viewport(ViewportError::BadBox))
            }
        };
        match command {
            // A popup is linked to its markup one annotation at a time; anything else in
            // bulk when there is a lot of it.
            Command::AddAnnotations(items)
                if items.len() >= BULK && items.iter().all(|a| a.kind != AnnotKind::Popup) =>
            {
                self.add_many(items, steps, touched)?;
            }
            Command::DeleteAnnotations(ids)
                if ids.len() >= BULK
                    && ids
                        .iter()
                        .all(|id| self.set.get(id).is_none_or(|a| a.kind != AnnotKind::Popup)) =>
            {
                let mut wanted = Vec::with_capacity(ids.len());
                for id in ids {
                    let Some(a) = self.set.get(id) else {
                        if gone.contains(id) {
                            continue;
                        }
                        return Err(EditError::NotFound(id.clone()));
                    };
                    check_editable(a)?;
                    wanted.push(id.clone());
                }
                if !wanted.is_empty() {
                    let mut step = Step::DeleteMany {
                        ids: wanted,
                        out: empty_removed(),
                    };
                    step.apply(&mut self.set, &mut self.scales, touched);
                    if let Step::DeleteMany { out, .. } = &step {
                        gone.extend(out.ids().cloned());
                    }
                    steps.push(step);
                }
            }
            Command::AddAnnotations(items) => {
                for a in items {
                    self.check_new(a)?;
                    let index = self.top_of_page(a.page);
                    // A measurement made, pasted or dropped here takes the scale that applies
                    // here, not the one it was made or copied at.
                    let a = normalised(a);
                    let a = self.rescaled(&a, false).unwrap_or(a);
                    let step = Step::Add {
                        id: a.id.clone(),
                        out: Removed {
                            items: vec![(index, self.materialise(&a))],
                            unlinked_parent: None,
                        },
                    };
                    push(step, steps, &mut self.set, &mut self.scales, touched);
                }
            }
            Command::Restore(items) => {
                let mut sorted: Vec<&(usize, Annotation)> = items.iter().collect();
                sorted.sort_by_key(|(i, _)| *i);
                let mut popups = Vec::new();
                for (index, a) in sorted {
                    self.check_new(a)?;
                    if a.kind == AnnotKind::Popup {
                        popups.push(a);
                    }
                    let step = Step::Add {
                        id: a.id.clone(),
                        out: Removed {
                            items: vec![(*index, self.materialise(a))],
                            unlinked_parent: None,
                        },
                    };
                    push(step, steps, &mut self.set, &mut self.scales, touched);
                }
                // A popup that comes back alone is linked to its markup again.
                for popup in popups {
                    let Some(parent) = popup.parent.as_ref().and_then(|p| self.set.get(p)) else {
                        continue;
                    };
                    if parent.popup.is_none() {
                        let before = parent.detached();
                        let mut after = before.clone();
                        after.popup = Some(popup.id.clone());
                        let step = Step::Replace {
                            id: before.id.clone(),
                            before: Box::new(before),
                            after: Box::new(after),
                        };
                        push(step, steps, &mut self.set, &mut self.scales, touched);
                    }
                }
            }
            Command::ModifyAnnotations(items) => {
                for a in items {
                    self.check_modify(a)?;
                    let before = self.set.get(&a.id).expect("checked").detached();
                    // A change that leaves the measurement, its shape and its text alone (a
                    // column value, the author, a flag) takes the text as it is: a foreign
                    // measurement whose stored text the model would word differently is not
                    // rewritten for it.
                    let measure_untouched = a.measure == before.measure
                        && a.geometry == before.geometry
                        && a.page == before.page
                        && a.meta.contents == before.meta.contents;
                    let mut after = if measure_untouched {
                        a.detached()
                    } else {
                        normalised(a)
                    };
                    // A measurement moved or reshaped takes the scale of where it now is.
                    if after.page != before.page || after.geometry != before.geometry {
                        if let Some(rescaled) = self.rescaled(&after, false) {
                            after = rescaled;
                        }
                    }
                    if before == after {
                        continue;
                    }
                    let step = Step::Replace {
                        id: a.id.clone(),
                        before: Box::new(before),
                        after: Box::new(after),
                    };
                    push(step, steps, &mut self.set, &mut self.scales, touched);
                }
            }
            Command::DeleteAnnotations(ids) => {
                for id in ids {
                    let Some(a) = self.set.get(id) else {
                        if gone.contains(id) {
                            // Its markup went earlier in this command and took it along.
                            continue;
                        }
                        return Err(EditError::NotFound(id.clone()));
                    };
                    check_editable(a)?;
                    let mut step = Step::Delete {
                        id: id.clone(),
                        out: empty_removed(),
                    };
                    step.apply(&mut self.set, &mut self.scales, touched);
                    if let Step::Delete { out, .. } = &step {
                        gone.extend(out.ids().cloned());
                    }
                    steps.push(step);
                }
            }
            Command::Reorder { page, order } => {
                let before = page_ids(&self.set, *page);
                let mut probe = self.set.clone();
                probe.set_order(*page, order);
                let after = page_ids(&probe, *page);
                if before != after {
                    let step = Step::Order {
                        page: *page,
                        before,
                        after,
                    };
                    push(step, steps, &mut self.set, &mut self.scales, touched);
                }
            }
            Command::Batch(commands) => {
                for c in commands {
                    self.build(c, steps, touched, gone)?;
                }
            }
            Command::SetPageScale { page, scale } => {
                self.check_page(*page)?;
                let spec = self.spec_on(scale.as_ref(), *page)?;
                let mut data = self.scales.page_data(*page);
                data.page_scale = spec.as_ref().map(Into::into);
                self.change_scales(*page, data, steps, touched);
            }
            Command::ApplyScaleToPages { first, last, scale } => {
                self.check_page(*last)?;
                if first > last {
                    return Err(EditError::Scale(format!(
                        "the page range {}-{} runs backwards",
                        first + 1,
                        last + 1
                    )));
                }
                for page in *first..=*last {
                    let spec = self.spec_on(scale.as_ref(), page)?;
                    let mut data = self.scales.page_data(page);
                    data.page_scale = spec.as_ref().map(Into::into);
                    self.change_scales(page, data, steps, touched);
                }
            }
            Command::AddViewport {
                page,
                name,
                bbox,
                scale,
            } => {
                self.check_page(*page)?;
                let spec = self.spec_on(scale.as_ref(), *page)?;
                let rect = bbox_rect(bbox)?;
                let data = self.with_viewports(*page, |vps| {
                    vps.add(Viewport::new(name.clone(), rect, spec))
                        .map_err(EditError::Viewport)
                })?;
                self.change_scales(*page, data, steps, touched);
            }
            Command::EditViewport {
                page,
                name,
                bbox,
                scale,
            } => {
                self.check_page(*page)?;
                let spec = self.spec_on(scale.as_ref(), *page)?;
                let rect = bbox_rect(bbox)?;
                let data = self.with_viewports(*page, |vps| {
                    match vps.edit(name, rect, spec).map_err(EditError::Viewport)? {
                        true => Ok(()),
                        false => Err(EditError::NoViewport {
                            page: *page,
                            name: name.clone(),
                        }),
                    }
                })?;
                self.change_scales(*page, data, steps, touched);
            }
            Command::RemoveViewport { page, name } => {
                let data = self.with_viewports(*page, |vps| match vps.remove(name) {
                    Some(_) => Ok(()),
                    None => Err(EditError::NoViewport {
                        page: *page,
                        name: name.clone(),
                    }),
                })?;
                self.change_scales(*page, data, steps, touched);
            }
            Command::SetScales { page, scales } => {
                self.check_page(*page)?;
                // Exactly as given: a viewport the file has with an empty box comes back too.
                for v in &scales.viewports {
                    bbox_rect(&v.bbox)?;
                }
                let before = self.scales.page_data(*page);
                if before != *scales {
                    let step = Step::Scales {
                        page: *page,
                        before: Box::new(before),
                        after: Box::new(scales.clone()),
                    };
                    push(step, steps, &mut self.set, &mut self.scales, touched);
                }
            }
            Command::SetBluebeamColumns(entries) => {
                let after: Vec<pl_pdf::Object> = entries.iter().map(PdfValue::to_object).collect();
                let before = self.scales.bluebeam().entries.clone();
                if before != after {
                    let step = Step::Bluebeam { before, after };
                    push(step, steps, &mut self.set, &mut self.scales, touched);
                }
            }
            Command::SetDocSettings(settings) | Command::RestoreDocSettings(settings) => {
                settings
                    .validate(self.page_count)
                    .map_err(EditError::Settings)?;
                let before = self.scales.settings().clone();
                if before != **settings {
                    let rescale = matches!(command, Command::SetDocSettings(_))
                        && before.default_scale != settings.default_scale;
                    let followers: Vec<HashSet<AnnotId>> = if rescale {
                        (0..self.page_count)
                            .map(|page| self.store_followers(page))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let step = Step::Settings {
                        before: Box::new(before),
                        after: settings.clone(),
                    };
                    push(step, steps, &mut self.set, &mut self.scales, touched);
                    for (page, followers) in followers.iter().enumerate() {
                        self.rescale_page(page, followers, steps, touched);
                    }
                }
            }
        }
        Ok(())
    }

    /// Takes back the most recent command. `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Option<Replayed> {
        let mut entry = self.undo.pop()?;
        let command = inverse_of(&entry);
        let mut touched = Vec::new();
        for step in entry.steps.iter_mut().rev() {
            step.unapply(&mut self.set, &mut self.scales, &mut touched);
        }
        self.note_refresh(&entry.steps);
        let journaled = entry.journaled;
        // A drag does not continue across an undo: neither into the step that was taken back
        // nor into the one that is now on top.
        entry.merge = None;
        if let Some(top) = self.undo.last_mut() {
            top.merge = None;
        }
        self.redo.push(entry);
        self.revision += 1;
        self.prune_selection();
        Some(Replayed {
            touched: dedup(touched),
            command,
            journaled,
        })
    }

    /// Applies again the command most recently undone. `None` when there is nothing to redo.
    pub fn redo(&mut self) -> Option<Replayed> {
        let mut entry = self.redo.pop()?;
        let mut touched = Vec::new();
        for step in entry.steps.iter_mut() {
            step.apply(&mut self.set, &mut self.scales, &mut touched);
        }
        self.note_refresh(&entry.steps);
        let journaled = entry.journaled;
        let command = entry.command.clone();
        entry.merge = None;
        self.undo.push(entry);
        self.revision += 1;
        self.prune_selection();
        Some(Replayed {
            touched: dedup(touched),
            command,
            journaled,
        })
    }

    /// Records that the present state is the one on disk, after the set has been rebased on
    /// the saved file.
    ///
    /// Annotations the history holds outside the set (deleted ones an undo would bring back,
    /// added ones a redo would) are no longer in the file, so they lose their tie to it and
    /// come back as new annotations. A deleted annotation of a kind that cannot be created
    /// (a foreign FreeText, say) cannot come back at all: the undo steps up to and including
    /// its deletion are dropped.
    pub(crate) fn mark_saved(&mut self) {
        for entry in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            entry.journaled = false;
            // A drag does not continue across a save: the step before it is not in the new
            // journal, so a replay could not fold into it either.
            entry.merge = None;
            for step in &mut entry.steps {
                if let Some(out) = step.held_mut() {
                    for (_, a) in &mut out.items {
                        *a = a.detached();
                    }
                }
            }
        }
        let holds_unmodelled = |entry: &Entry| {
            entry.steps.iter().any(|s| match s {
                Step::Add { out, .. }
                | Step::Delete { out, .. }
                | Step::AddMany { out, .. }
                | Step::DeleteMany { out, .. } => out.items.iter().any(|(_, a)| !a.is_modelled()),
                _ => false,
            })
        };
        if let Some(last) = self.undo.iter().rposition(holds_unmodelled) {
            self.floor = self.undo[last].serial;
            self.undo.drain(..=last);
        }
        if let Some(first) = self.redo.iter().position(holds_unmodelled) {
            // The redo stack is applied from its end; everything that would be redone after
            // the blocked entry goes with it.
            self.redo.drain(..=first);
        }
        self.saved = Some(self.state_serial());
        self.revision += 1;
        self.prune_selection();
    }

    /// After a journal replay: when the annotations are exactly what the file holds and
    /// nothing says which state is the saved one, the present state is. (A replay of only
    /// history-less records, such as an undo and redo of steps from before the last save,
    /// ends on the file's own state without the history knowing.)
    pub(crate) fn settle_if_clean(&mut self) {
        if self.saved.is_none() && !self.has_unsaved() {
            self.saved = Some(self.state_serial());
        }
    }

    /// Forgets what could be redone: a structural step is a new change (see
    /// [`crate::structure`]).
    pub(crate) fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// Forgets the history (not the annotations).
    pub fn clear_history(&mut self) {
        let state = self.state_serial();
        self.undo.clear();
        self.redo.clear();
        self.floor = state;
    }
}

fn inverse_of(entry: &Entry) -> Command {
    let mut inverses: Vec<Command> = entry.steps.iter().rev().map(Step::inverse).collect();
    if inverses.len() == 1 {
        inverses.pop().expect("one")
    } else {
        Command::Batch(inverses)
    }
}

fn check_editable(a: &Annotation) -> Result<(), EditError> {
    match a.baseline() {
        Some(base) if base.object.is_none() => Err(EditError::NotEditable(a.id.clone())),
        _ => Ok(()),
    }
}

/// Text a text box or a stamp cannot show is refused here, by name, instead of failing the
/// save later (or being dropped).
fn check_drawable(a: &Annotation) -> Result<(), EditError> {
    if a.free_text.is_none() && a.stamp.is_none() {
        return Ok(());
    }
    a.validate().map_err(|e| EditError::Unsupported {
        id: a.id.clone(),
        reason: e.to_string(),
    })
}

fn check_finite(a: &Annotation) -> Result<(), EditError> {
    let text_ok = a.free_text.as_ref().is_none_or(|t| {
        t.size.is_finite()
            && t.line_spacing.is_finite()
            && t.padding.is_finite()
            && t.callout.iter().all(|p| p.x.is_finite() && p.y.is_finite())
    });
    if !text_ok || !a.measure.as_ref().is_none_or(measurement_is_finite) {
        return Err(EditError::NotFinite(a.id.clone()));
    }
    check_drawable(a)?;
    let style = &a.style;
    let numbers_ok = style.width.is_finite()
        && style.opacity.is_finite()
        && style.cloud.is_none_or(f64::is_finite)
        && style
            .dash
            .as_ref()
            .is_none_or(|d| d.iter().all(|v| v.is_finite()));
    if a.geometry.is_finite() && numbers_ok {
        Ok(())
    } else {
        Err(EditError::NotFinite(a.id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_annot::{Geometry, Origin, PdfDate, Rgb};
    use pl_geom::{Point, Rect, Vec2};

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn square(page: usize, x: f64) -> Annotation {
        Annotation::square(page, Rect::new(x, 0.0, x + 50.0, 50.0), &origin())
    }

    fn moved(a: &Annotation, dx: f64) -> Annotation {
        let mut b = a.clone();
        b.translate(Vec2::new(dx, 0.0));
        b
    }

    #[test]
    fn add_modify_delete_undo_redo() {
        let mut ed = Editor::new(AnnotSet::new(), 3);
        assert!(!ed.is_modified() && !ed.can_undo());
        let a = square(1, 0.0);
        let out = ed.execute(Command::add(a.clone())).unwrap();
        assert!(out.changed && out.touched == [a.id.clone()]);
        assert!(ed.is_modified());
        let b = moved(&a, 10.0);
        ed.execute(Command::modify(b.clone())).unwrap();
        assert_eq!(ed.model(), vec![b.clone()]);
        ed.execute(Command::delete(a.id.clone())).unwrap();
        assert!(ed.model().is_empty());
        assert_eq!(ed.undo_len(), 3);

        ed.undo().unwrap();
        assert_eq!(ed.model(), vec![b.clone()]);
        ed.undo().unwrap();
        assert_eq!(ed.model(), vec![a.clone()]);
        ed.undo().unwrap();
        assert!(ed.model().is_empty());
        assert!(!ed.is_modified(), "back at the loaded state");
        assert!(ed.undo().is_none());
        ed.redo().unwrap();
        ed.redo().unwrap();
        ed.redo().unwrap();
        assert!(ed.model().is_empty() && ed.redo().is_none());
        assert!(ed.is_modified());
    }

    #[test]
    fn refused_commands_change_nothing() {
        let mut ed = Editor::new(AnnotSet::new(), 2);
        let a = square(0, 0.0);
        ed.execute(Command::add(a.clone())).unwrap();
        let before = ed.clone();
        let same = |ed: &Editor| ed.set == before.set && ed.undo == before.undo;

        let err = ed.execute(Command::add(a.clone())).unwrap_err();
        assert_eq!(err, EditError::DuplicateId(a.id.clone()));
        let far = square(5, 0.0);
        assert!(matches!(
            ed.execute(Command::add(far)),
            Err(EditError::PageOutOfRange {
                page: 5,
                count: 2,
                ..
            })
        ));
        let ghost = square(0, 0.0);
        assert_eq!(
            ed.execute(Command::modify(ghost.clone())),
            Err(EditError::NotFound(ghost.id.clone()))
        );
        assert_eq!(
            ed.execute(Command::delete(ghost.id.clone())),
            Err(EditError::NotFound(ghost.id.clone()))
        );
        let mut other_page = a.clone();
        other_page.page = 1;
        assert_eq!(
            ed.execute(Command::modify(other_page)),
            Err(EditError::PageChange(a.id.clone()))
        );
        let mut other_kind = a.clone();
        other_kind.kind = AnnotKind::Circle;
        assert_eq!(
            ed.execute(Command::modify(other_kind)),
            Err(EditError::KindChange(a.id.clone()))
        );
        let mut nan = a.clone();
        nan.geometry = Geometry::Rect(Rect::new(0.0, f64::NAN, 1.0, 1.0));
        assert_eq!(
            ed.execute(Command::modify(nan)),
            Err(EditError::NotFinite(a.id.clone()))
        );
        let free = Annotation::new(
            AnnotKind::FreeText,
            0,
            Geometry::Rect(Rect::new(0.0, 0.0, 9.0, 9.0)),
            &origin(),
        );
        assert_eq!(
            ed.execute(Command::add(free.clone())),
            Err(EditError::Unmodelled(free.id.clone()))
        );
        assert!(same(&ed));

        // A batch that fails half-way is rolled back whole.
        let c = square(1, 5.0);
        let batch = Command::Batch(vec![
            Command::add(c.clone()),
            Command::modify(moved(&a, 3.0)),
            Command::delete(a.id.clone()),
            Command::delete(ghost.id.clone()),
        ]);
        assert!(ed.execute(batch).is_err());
        assert!(same(&ed));

        // A command that asks for the present state records nothing.
        let out = ed.execute(Command::modify(a.clone())).unwrap();
        assert!(!out.changed);
        let out = ed
            .execute(Command::Reorder {
                page: 0,
                order: vec![a.id.clone()],
            })
            .unwrap();
        assert!(!out.changed);
        assert!(same(&ed));
    }

    #[test]
    fn new_annotations_keep_the_set_in_page_order() {
        let mut ed = Editor::new(AnnotSet::new(), 4);
        let (p2, p0, p1, p0b) = (
            square(2, 0.0),
            square(0, 0.0),
            square(1, 0.0),
            square(0, 9.0),
        );
        for a in [&p2, &p0, &p1, &p0b] {
            ed.execute(Command::add(a.clone())).unwrap();
        }
        let order: Vec<AnnotId> = ed.set().iter().map(|a| a.id.clone()).collect();
        assert_eq!(order, [p0.id, p0b.id, p1.id, p2.id]);
    }

    #[test]
    fn consecutive_drag_steps_merge_into_one_undo_step() {
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let a = square(0, 0.0);
        let b = square(0, 100.0);
        ed.execute(Command::AddAnnotations(vec![a.clone(), b.clone()]))
            .unwrap();
        let drag = |dx: f64| Command::ModifyAnnotations(vec![moved(&a, dx), moved(&b, dx)]);
        for (i, dx) in [1.0, 2.0, 5.0, 9.0].into_iter().enumerate() {
            let out = ed.execute_merged(drag(dx), 7).unwrap();
            assert_eq!(out.merged, i > 0);
        }
        assert_eq!(ed.undo_len(), 2);
        assert_eq!(ed.model(), [moved(&a, 9.0), moved(&b, 9.0)]);
        assert_eq!(ed.undo_command(), Some(&drag(9.0)));
        assert_eq!(
            ed.inverse_of_last(),
            Some(Command::Batch(vec![
                Command::modify(b.clone()),
                Command::modify(a.clone())
            ]))
        );
        // Another drag key, another set of annotations, or a plain command starts a new step.
        ed.execute_merged(drag(12.0), 8).unwrap();
        assert_eq!(ed.undo_len(), 3);
        ed.execute_merged(Command::modify(moved(&a, 20.0)), 8)
            .unwrap();
        assert_eq!(ed.undo_len(), 4);
        ed.execute(Command::modify(moved(&a, 21.0))).unwrap();
        ed.execute_merged(Command::modify(moved(&a, 22.0)), 8)
            .unwrap();
        assert_eq!(ed.undo_len(), 6);

        for _ in 0..4 {
            ed.undo();
        }
        assert_eq!(ed.model(), [moved(&a, 9.0), moved(&b, 9.0)]);
        ed.undo();
        assert_eq!(
            ed.model(),
            [a.clone(), b.clone()],
            "the whole drag is one step"
        );
        ed.redo();
        assert_eq!(ed.model(), [moved(&a, 9.0), moved(&b, 9.0)]);
    }

    #[test]
    fn commands_with_a_grouping_key_are_one_undo_step_whatever_they_do() {
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let (a, b, c) = (square(0, 0.0), square(0, 60.0), square(0, 120.0));
        ed.execute(Command::AddAnnotations(vec![
            a.clone(),
            b.clone(),
            c.clone(),
        ]))
        .unwrap();
        let start = ed.model();
        let key = GROUP_KEY | 7;
        // One drag of an eraser: a change, a deletion, another change, another deletion.
        let out = ed
            .execute_merged(Command::modify(moved(&a, 5.0)), key)
            .unwrap();
        assert!(out.changed && !out.merged);
        let out = ed
            .execute_merged(Command::delete(b.id.clone()), key)
            .unwrap();
        assert!(out.changed && out.merged && out.touched == [b.id.clone()]);
        ed.execute_merged(Command::modify(moved(&c, 9.0)), key)
            .unwrap();
        ed.execute_merged(Command::delete(a.id.clone()), key)
            .unwrap();
        assert_eq!(ed.undo_len(), 2, "the add and the whole drag");
        assert_eq!(ed.model(), [moved(&c, 9.0)]);
        // A command that changes nothing, or is refused, leaves the group as it is.
        assert!(
            !ed.execute_merged(Command::modify(moved(&c, 9.0)), key)
                .unwrap()
                .changed
        );
        assert!(ed
            .execute_merged(Command::delete(a.id.clone()), key)
            .is_err());
        assert_eq!(ed.undo_len(), 2);

        let inverse = ed.inverse_of_last().unwrap();
        let done = ed.undo_command().unwrap().clone();
        assert!(matches!(&done, Command::Batch(list) if list.len() == 4));
        ed.undo().unwrap();
        assert_eq!(ed.model(), start, "one undo takes the whole drag back");
        ed.redo().unwrap();
        assert_eq!(ed.model(), [moved(&c, 9.0)]);
        // The inverse and the recorded command, run as commands of their own, agree.
        ed.execute(inverse).unwrap();
        assert_eq!(ed.model(), start);
        ed.execute(done).unwrap();
        assert_eq!(ed.model(), [moved(&c, 9.0)]);
        ed.undo().unwrap();
        ed.undo().unwrap();
        ed.redo().unwrap();

        // The group does not continue across an undo and redo, another key or a save.
        let steps = ed.undo_len();
        ed.execute_merged(Command::modify(moved(&c, 10.0)), key)
            .unwrap();
        assert_eq!(ed.undo_len(), steps + 1, "a redone step is not joined");
        ed.execute_merged(Command::modify(moved(&c, 11.0)), GROUP_KEY | 8)
            .unwrap();
        assert_eq!(ed.undo_len(), steps + 2, "nor one with another key");
        ed.execute_merged(Command::modify(moved(&c, 11.5)), GROUP_KEY | 8)
            .unwrap();
        assert_eq!(ed.undo_len(), steps + 2);
        ed.mark_saved();
        ed.execute_merged(Command::modify(moved(&c, 12.0)), GROUP_KEY | 8)
            .unwrap();
        assert_eq!(ed.undo_len(), steps + 3, "nor one from before a save");
        assert!(ed.is_modified());
        // A plain drag key still merges only modifications of the same annotations.
        ed.execute_merged(Command::modify(moved(&c, 13.0)), 8)
            .unwrap();
        ed.execute_merged(Command::delete(c.id.clone()), 8).unwrap();
        assert_eq!(ed.undo_len(), steps + 5);
    }

    #[test]
    fn a_merge_after_the_saved_state_still_counts_as_modified() {
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let a = square(0, 0.0);
        ed.execute(Command::add(a.clone())).unwrap();
        ed.execute_merged(Command::modify(moved(&a, 1.0)), 1)
            .unwrap();
        ed.mark_saved();
        assert!(!ed.is_modified());
        ed.execute_merged(Command::modify(moved(&a, 2.0)), 1)
            .unwrap();
        assert!(ed.is_modified());
    }

    #[test]
    fn delete_takes_popup_and_replies_and_undo_restores_positions() {
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let o = origin();
        let bottom = square(0, 0.0);
        let mut note = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "n", &o);
        let popup = Annotation::popup_for(&mut note, Rect::new(30.0, 0.0, 130.0, 60.0), &o);
        let middle = square(0, 60.0);
        let mut reply = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "r", &o);
        reply.meta.reply_to = Some(note.id.clone());
        let top = square(0, 120.0);
        ed.execute(Command::AddAnnotations(vec![
            bottom.clone(),
            note.clone(),
            popup.clone(),
            middle.clone(),
            reply.clone(),
            top.clone(),
        ]))
        .unwrap();
        let full = ed.model();

        // Naming the popup as well is not an error: it went with its markup.
        let out = ed
            .execute(Command::DeleteAnnotations(vec![
                note.id.clone(),
                popup.id.clone(),
            ]))
            .unwrap();
        assert_eq!(out.touched.len(), 3);
        assert_eq!(ed.model(), [bottom.clone(), middle.clone(), top.clone()]);
        let inverse = ed.inverse_of_last().unwrap();
        ed.undo().unwrap();
        assert_eq!(ed.model(), full);

        // The inverse, run as a command of its own, does the same.
        ed.redo().unwrap();
        ed.execute(inverse).unwrap();
        assert_eq!(ed.model(), full);

        // The popup alone: its markup loses the link, and gets it back.
        ed.execute(Command::delete(popup.id.clone())).unwrap();
        assert_eq!(ed.set().get(&note.id).unwrap().popup, None);
        let inverse = ed.inverse_of_last().unwrap();
        ed.undo().unwrap();
        assert_eq!(ed.model(), full);
        ed.redo().unwrap();
        ed.execute(inverse).unwrap();
        assert_eq!(
            ed.model(),
            full,
            "a restored popup is linked to its markup again"
        );
    }

    #[test]
    fn reorder_and_its_inverse() {
        let mut ed = Editor::new(AnnotSet::new(), 2);
        let (a, b, c, x) = (
            square(0, 0.0),
            square(0, 1.0),
            square(0, 2.0),
            square(1, 0.0),
        );
        ed.execute(Command::AddAnnotations(vec![
            a.clone(),
            b.clone(),
            c.clone(),
            x.clone(),
        ]))
        .unwrap();
        ed.execute(Command::Reorder {
            page: 0,
            order: vec![a.id.clone()],
        })
        .unwrap();
        let ids = |ed: &Editor| page_ids(ed.set(), 0);
        assert_eq!(ids(&ed), [b.id.clone(), c.id.clone(), a.id.clone()]);
        assert_eq!(
            ed.inverse_of_last(),
            Some(Command::Reorder {
                page: 0,
                order: vec![a.id.clone(), b.id.clone(), c.id.clone()]
            })
        );
        ed.undo().unwrap();
        assert_eq!(ids(&ed), [a.id.clone(), b.id.clone(), c.id.clone()]);
        ed.redo().unwrap();
        assert_eq!(ids(&ed), [b.id.clone(), c.id.clone(), a.id.clone()]);
        assert_eq!(ed.set().position(&x.id), Some(3));
    }

    /// A small deterministic generator, so a failure names its seed.
    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % n
        }
    }

    /// The same commands run on two editors: one given them whole, one given every
    /// annotation and id as a command of its own (which is below [`BULK`]).
    #[test]
    fn many_at_once_give_the_document_one_at_a_time_gives() {
        for seed in 0..40u64 {
            let mut rng = Lcg(seed);
            let pages = 6;
            let mut bulk = Editor::new(AnnotSet::new(), pages);
            let mut single = Editor::new(AnnotSet::new(), pages);
            // A document that is not in page order to begin with, on some seeds.
            let start: Vec<Annotation> = (0..rng.below(30))
                .map(|i| square(rng.below(pages), i as f64))
                .collect();
            for a in &start {
                if seed % 3 == 0 {
                    // Restore puts each exactly where it says, page order or not.
                    let at = bulk.set().len();
                    let c = Command::Restore(vec![(at, a.clone())]);
                    bulk.execute(c.clone()).unwrap();
                    single.execute(c).unwrap();
                } else {
                    bulk.execute(Command::add(a.clone())).unwrap();
                    single.execute(Command::add(a.clone())).unwrap();
                }
            }
            assert_eq!(bulk.model(), single.model());

            let mut batch: Vec<Annotation> = (0..BULK + rng.below(60))
                .map(|i| square(rng.below(pages), 1000.0 + i as f64))
                .collect();
            // Replies travel with what they answer.
            for i in 0..5 {
                let parent = batch[rng.below(batch.len())].clone();
                let mut reply = Annotation::note(
                    parent.page,
                    Rect::new(0.0, 0.0, 18.0, 18.0),
                    &format!("r{i}"),
                    &origin(),
                );
                reply.meta.reply_to = Some(parent.id.clone());
                batch.push(reply);
            }
            let out = bulk
                .execute(Command::AddAnnotations(batch.clone()))
                .unwrap();
            let ids: Vec<AnnotId> = batch.iter().map(|a| a.id.clone()).collect();
            assert_eq!(out.touched, ids, "seed {seed}: touched in the order given");
            assert_eq!(bulk.undo_len(), start.len() + 1);
            for a in &batch {
                single.execute(Command::add(a.clone())).unwrap();
            }
            assert_eq!(bulk.model(), single.model(), "seed {seed}: after the add");
            assert_eq!(
                bulk.inverse_of_last(),
                Some(Command::DeleteAnnotations(ids.clone()))
            );

            // Undo and redo of the whole add.
            let with = bulk.model();
            bulk.undo().unwrap();
            assert_eq!(bulk.model().len(), start.len(), "seed {seed}");
            assert_eq!(
                bulk.model(),
                {
                    let mut probe = single.clone();
                    for _ in &batch {
                        probe.undo().unwrap();
                    }
                    probe.model()
                },
                "seed {seed}: after the undo"
            );
            bulk.redo().unwrap();
            assert_eq!(bulk.model(), with, "seed {seed}: after the redo");

            // A delete of many, some of them replies that go with their markup anyway.
            let mut doomed: Vec<AnnotId> = with
                .iter()
                .filter(|_| rng.below(3) > 0)
                .map(|a| a.id.clone())
                .collect();
            while doomed.len() < BULK {
                doomed.extend(with.iter().map(|a| a.id.clone()));
            }
            bulk.execute(Command::DeleteAnnotations(doomed.clone()))
                .unwrap();
            for id in &doomed {
                if single.set().contains(id) {
                    single.execute(Command::delete(id.clone())).unwrap();
                }
            }
            assert_eq!(
                bulk.model(),
                single.model(),
                "seed {seed}: after the delete"
            );
            let inverse = bulk.inverse_of_last().unwrap();
            bulk.undo().unwrap();
            assert_eq!(bulk.model(), with, "seed {seed}: the delete undone");
            bulk.redo().unwrap();
            bulk.execute(inverse).unwrap();
            assert_eq!(bulk.model(), with, "seed {seed}: the inverse as a command");
        }
    }

    #[test]
    fn a_refused_annotation_among_many_leaves_the_document_alone() {
        let mut ed = Editor::new(AnnotSet::new(), 2);
        let first = square(0, 0.0);
        ed.execute(Command::add(first.clone())).unwrap();
        let mut batch: Vec<Annotation> = (0..BULK + 8).map(|i| square(1, i as f64)).collect();
        // Twice in the batch.
        batch.push(batch[3].clone());
        let err = ed
            .execute(Command::AddAnnotations(batch.clone()))
            .unwrap_err();
        assert_eq!(err, EditError::DuplicateId(batch[3].id.clone()));
        batch.pop();
        // Already in the document; on a page there is not; not a number.
        for (bad, want) in [
            (first.clone(), EditError::DuplicateId(first.id.clone())),
            (
                square(2, 0.0),
                EditError::PageOutOfRange {
                    id: AnnotId::from(""),
                    page: 2,
                    count: 2,
                },
            ),
        ] {
            let mut with = batch.clone();
            with.insert(5, bad.clone());
            let err = ed.execute(Command::AddAnnotations(with)).unwrap_err();
            match (&err, &want) {
                (
                    EditError::PageOutOfRange { page, count, .. },
                    EditError::PageOutOfRange { .. },
                ) => {
                    assert_eq!((*page, *count), (2, 2));
                }
                _ => assert_eq!(err, want),
            }
        }
        assert_eq!(ed.model(), vec![first.clone()]);
        assert_eq!(ed.undo_len(), 1);
        // A delete that names something that is not there.
        ed.execute(Command::AddAnnotations(batch.clone())).unwrap();
        let mut ids: Vec<AnnotId> = batch.iter().map(|a| a.id.clone()).collect();
        ids.push(AnnotId::from("nope"));
        let err = ed.execute(Command::DeleteAnnotations(ids)).unwrap_err();
        assert_eq!(err, EditError::NotFound(AnnotId::from("nope")));
        assert_eq!(ed.model().len(), batch.len() + 1);
    }

    #[test]
    fn adding_and_deleting_twenty_thousand_is_not_quadratic() {
        // Onto the first page of a document whose other pages have markups already: every
        // one of them goes into the middle of the set.
        let mut ed = Editor::new(AnnotSet::new(), 3);
        for page in [0, 1, 2] {
            for i in 0..50 {
                ed.execute(Command::add(square(page, i as f64))).unwrap();
            }
        }
        let batch: Vec<Annotation> = (0..20_000).map(|i| square(0, i as f64)).collect();
        let ids: Vec<AnnotId> = batch.iter().map(|a| a.id.clone()).collect();
        let start = std::time::Instant::now();
        ed.execute(Command::AddAnnotations(batch)).unwrap();
        assert_eq!(ed.set().len(), 20_150);
        assert_eq!(ed.set().position(&ids[19_999]), Some(20_049));
        ed.select(ids.iter().cloned());
        assert_eq!(ed.selection().len(), 20_000);
        assert!(ids.iter().all(|id| ed.is_selected(id)));
        ed.undo().unwrap();
        assert_eq!(ed.set().len(), 150);
        assert!(ed.selection().is_empty());
        ed.redo().unwrap();
        ed.execute(Command::DeleteAnnotations(ids)).unwrap();
        assert_eq!(ed.set().len(), 150);
        ed.undo().unwrap();
        assert_eq!(ed.set().len(), 20_150);
        // One at a time each of these five passes took tens of seconds.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn selection_follows_the_document() {
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let (a, b) = (square(0, 0.0), square(0, 60.0));
        ed.execute(Command::AddAnnotations(vec![a.clone(), b.clone()]))
            .unwrap();
        ed.select([a.id.clone(), AnnotId::from("nope"), a.id.clone()]);
        assert_eq!(ed.selection(), vec![a.id.clone()]);
        assert!(ed.toggle_selected(&b.id));
        assert_eq!(ed.selection(), [a.id.clone(), b.id.clone()]);
        assert!(!ed.toggle_selected(&a.id));
        ed.select_also(a.id.clone());
        assert_eq!(ed.selection(), [b.id.clone(), a.id.clone()]);
        ed.execute(Command::delete(b.id.clone())).unwrap();
        assert_eq!(
            ed.selection(),
            vec![a.id.clone()],
            "a deleted annotation is deselected"
        );
        assert!(ed.is_selected(&a.id) && !ed.is_selected(&b.id));
        let serial = ed.selection_serial();
        ed.undo().unwrap();
        assert_eq!(
            ed.selection_serial(),
            serial,
            "an undo that selects nothing new"
        );
        ed.select_page(0);
        assert_eq!(ed.selection().len(), 2);
        assert!(ed.is_selected(&a.id) && ed.is_selected(&b.id));
        assert!(ed.selection_serial() > serial);
        ed.select_also(AnnotId::from("nope"));
        assert_eq!(ed.selection().len(), 2);
        ed.clear_selection();
        assert!(!ed.is_selected(&a.id));
        assert!(ed.selection().is_empty());
    }

    #[test]
    fn commands_survive_json() {
        let a = {
            let mut a = square(0, 0.0);
            a.style.fill = Some(Rgb::new(0.2, 0.4, 0.6));
            a
        };
        let line = Annotation::line(1, Point::new(0.1, 0.2), Point::new(9.9, 7.7), &origin());
        let commands = vec![
            Command::AddAnnotations(vec![a.clone(), line.clone()]),
            Command::ModifyAnnotations(vec![moved(&a, 1.0 / 3.0)]),
            Command::DeleteAnnotations(vec![a.id.clone(), line.id.clone()]),
            Command::Reorder {
                page: 1,
                order: vec![line.id.clone()],
            },
            Command::Restore(vec![(0, a.clone()), (4, line.clone())]),
            Command::Batch(vec![
                Command::add(a.clone()),
                Command::Batch(vec![Command::delete(a.id.clone())]),
            ]),
        ];
        for c in commands {
            let json = serde_json::to_string(&c).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(back, c, "{json}");
        }
    }

    #[test]
    fn history_survives_a_save_marker() {
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let (a, b) = (square(0, 0.0), square(0, 60.0));
        ed.execute(Command::add(a.clone())).unwrap();
        ed.execute(Command::add(b.clone())).unwrap();
        ed.execute(Command::delete(a.id.clone())).unwrap();
        ed.mark_saved();
        assert!(!ed.is_modified());
        ed.undo().unwrap();
        assert!(ed.is_modified());
        assert_eq!(ed.model(), [a.clone(), b.clone()]);
        ed.redo().unwrap();
        assert!(!ed.is_modified());
        ed.clear_history();
        assert!(!ed.can_undo() && !ed.can_redo() && !ed.is_modified());
    }
}
