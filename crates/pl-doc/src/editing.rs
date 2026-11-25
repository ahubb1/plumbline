//! Editing a [`Document`]: the annotation store, commands with undo, what the user interface
//! paints, save, the crash journal and the clipboard.
//!
//! # The store
//!
//! A document's annotations are read with lopdf the first time they are needed
//! ([`Document::load_annots`], which every editing call makes for itself). Until then the
//! document costs what a viewer costs. The store is an [`Editor`]: the [`AnnotSet`], the undo
//! history and the selection.
//!
//! # Who draws what
//!
//! An annotation that is in the file is drawn by PDFium, from its own appearance, as part of
//! the page tiles. That stays so until it is first edited. Then:
//!
//! * it is hidden in the render mirror (the file is not touched),
//! * the tiles under its old rectangle are named in [`Document::take_invalidations`], and
//! * from then on it is in [`Document::overlay`], for the user interface to paint from its
//!   [`DrawList`].
//!
//! A new markup is only ever in the overlay; it never touches a tile. A deleted annotation is
//! hidden and in no overlay. The hidden set is kept and put back whenever the mirror is
//! reopened ([`Document::reopen_render`]: a layer toggle, a failed save). After a successful
//! save the new file is the truth: nothing is hidden and the overlay is empty.
//!
//! Annotations this version cannot draw (another application's text box or stamp, unknown
//! subtypes) are never hidden while they exist: PDFium keeps drawing them from their own
//! appearance. When one is moved, the mirror is given its new rectangle
//! (`SetAnnotationRect`), so it is seen at its new place before the save
//! ([`Document::moved_annotations`]); only if the mirror cannot be told does the overlay mark
//! the new place with a dashed outline instead.
//!
//! # Save
//!
//! [`Document::save`] writes an incremental update through the verified path
//! ([`Document::save_incremental`]: lopdf, hayro-syntax and PDFium check the temporary file
//! before it replaces the original, and a file changed on disk by another program is not
//! overwritten). [`Document::save_as`] writes the same update to another file and moves the
//! document there. [`Document::save_optimised`] rewrites the whole file as one revision,
//! dropping deleted objects. After any of them the store is rebased on what was written, so a
//! second save has nothing to write.
//!
//! Each of them runs on the calling thread. [`Document::start_save`] and its siblings run the
//! same save on a worker thread while the document stays readable (see [`crate::background`]);
//! [`Document::start_load_annots`] does the same for the first read of the annotations.
//!
//! # The markups list
//!
//! Text, review status, replies, checkmarks, locks, custom column values and popup windows are
//! ordinary commands built by [`crate::markup_ops`]; the methods of the same names here build
//! and apply them, so each is one undo step and is journaled.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pl_annot::{
    AnnotId, AnnotSet, Annotation, DrawList, Origin as Stamp, PdfDate, ReviewState, Rgb,
};
use pl_draw::{Dash, PathOp, Stroke};
use pl_geom::{Point, Rect};
use pl_pdf::{PdfDocument, SaveOptions};
use pl_render::{Bitmap, DocId, TileCache};

use crate::background::{MirrorFix, SaveTarget};
use crate::clipboard::{Clipboard, PastePlacement};
use crate::command::{Command, Editor, Outcome};
use crate::error::{DocError, Result};
use crate::journal::{journal_path, same_path, FileStamp, Journal, Record};
use crate::markup_ops;
use crate::scales::ScaleStore;
use crate::session::{Document, Origin, SaveOutcome};

/// How far beyond an annotation's rectangle tiles are invalidated, in page units: room for
/// anti-aliased edges.
const INVALIDATION_MARGIN: f64 = 2.0;

/// Where a hidden annotation sits in the render mirror.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MirrorSlot {
    pub page: u32,
    /// Index in the page's `/Annots` array.
    pub index: u32,
    /// The rectangle PDFium drew it in.
    pub rect: Rect,
    /// The file itself carries the Hidden flag for this annotation: the mirror never showed
    /// it, and un-hiding must not make it appear.
    pub hidden_in_file: bool,
}

/// Where an annotation PDFium draws has been moved to in the render mirror (a kind this
/// version cannot draw itself: its own appearance is shown at the new place).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovedSlot {
    pub page: u32,
    /// Index in the page's `/Annots` array.
    pub index: u32,
    /// The rectangle the file has for it.
    pub file: Rect,
    /// The rectangle the mirror draws it in now.
    pub shown: Rect,
}

/// Tiles that show stale content: every cached tile of `page` that covers any part of `rect`
/// (page space). Give these to `TileCache::remove_covering`, or let
/// [`Document::apply_invalidations`] do it. The page's revision does not change, so its other
/// tiles stay cached.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileInvalidation {
    pub render: DocId,
    pub page: u32,
    pub rect: Rect,
}

/// One annotation for the user interface to paint over the page tiles.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayItem {
    pub id: AnnotId,
    /// What to paint, in page space.
    pub list: DrawList,
    /// The list uses Multiply and must be composited against the page pixels (tiny-skia),
    /// not painted as plain shapes.
    pub multiply: bool,
    /// The list is a stand-in outline for an annotation this version cannot draw, not its
    /// appearance.
    pub placeholder: bool,
}

/// What a save of the annotation store did.
#[derive(Clone, Debug)]
pub struct MarkupSave {
    /// The file the document is now tied to.
    pub path: PathBuf,
    /// The verified write; `None` when nothing needed writing and the file was not touched.
    pub outcome: Option<SaveOutcome>,
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

impl MarkupSave {
    /// Whether the file was written.
    pub fn wrote(&self) -> bool {
        self.outcome.is_some()
    }
}

/// A journal found for a document that has not loaded its annotations yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingJournal {
    /// There is none.
    None,
    /// Unsaved changes from an earlier run can be recovered: loading the annotations replays
    /// them.
    Recoverable,
    /// There is a journal, but the file has changed since it was written; it will be set
    /// aside, not replayed.
    Stale,
}

pub(crate) struct EditState {
    pub(crate) editor: Editor,
    pub(crate) hidden: BTreeMap<AnnotId, MirrorSlot>,
    /// Annotations PDFium draws that the mirror shows at another place than the file's.
    pub(crate) moved: BTreeMap<AnnotId, MovedSlot>,
    pub(crate) invalid: Vec<TileInvalidation>,
    pub(crate) journal: Option<Journal>,
    pub(crate) journal_failed: bool,
    /// The file as it was when the store was loaded or last saved.
    pub(crate) stamp: Option<FileStamp>,
    pub(crate) recovered: usize,
}

impl EditState {
    /// What has to be done again in a fresh mirror of the same file: everything the store
    /// hides, and everything it has moved.
    pub(crate) fn mirror_fixes(&self) -> Vec<MirrorFix> {
        let mut fixes: Vec<MirrorFix> = self
            .moved
            .iter()
            .filter(|(id, _)| !self.hidden.contains_key(*id))
            .map(|(_, m)| MirrorFix {
                page: m.page,
                index: m.index,
                hide: false,
                rect: Some([m.shown.x0, m.shown.y0, m.shown.x1, m.shown.y1]),
            })
            .collect();
        fixes.extend(
            self.hidden
                .values()
                .filter(|slot| !slot.hidden_in_file)
                .map(|slot| MirrorFix {
                    page: slot.page,
                    index: slot.index,
                    hide: true,
                    rect: None,
                }),
        );
        // Pages of annotations the file itself hides still differ from nothing; they need no
        // fix but their page is a touched page all the same.
        fixes.extend(
            self.hidden
                .values()
                .filter(|slot| slot.hidden_in_file)
                .map(|slot| MirrorFix {
                    page: slot.page,
                    index: slot.index,
                    hide: false,
                    rect: None,
                }),
        );
        fixes
    }

    /// After a save wrote the file and a new mirror shows it: nothing is hidden or moved,
    /// and no tile of the old mirror is left to invalidate.
    pub(crate) fn clear_mirror_state(&mut self) {
        self.hidden.clear();
        self.moved.clear();
        self.invalid.clear();
    }

    /// The present state is the one on disk: the history says so and the journal goes.
    pub(crate) fn settle_saved(&mut self) {
        self.editor.mark_saved();
        if let Some(journal) = self.journal.take() {
            let _ = journal.discard();
        }
        self.journal_failed = false;
        self.recovered = 0;
    }
}

/// The editing side of a [`Document`]; empty until the annotations are first needed.
#[derive(Default)]
pub(crate) struct EditSlot {
    pub(crate) journal_dir: Option<PathBuf>,
    pub(crate) state: Option<Box<EditState>>,
    /// Tile regions still to be invalidated on behalf of a store that has been discarded.
    orphans: Vec<TileInvalidation>,
    /// Added to the editor's revision, so the sum never repeats across a discarded store.
    revision_base: u64,
}

impl EditSlot {
    pub(crate) fn new(journal_dir: Option<PathBuf>) -> EditSlot {
        EditSlot {
            journal_dir,
            ..EditSlot::default()
        }
    }
}

fn file_rect(a: &Annotation) -> Rect {
    a.original_dict()
        .and_then(|d| d.get(b"Rect").ok())
        .and_then(pl_pdf::rect_from_object)
        .unwrap_or_else(|| a.geometry.bounds())
}

fn placeholder_list(rect: Rect) -> DrawList {
    let stroke = Stroke::new(1.0, Rgb::new(0.35, 0.35, 0.35)).with_dash(Dash::even(4.0));
    DrawList::from_ops(vec![pl_draw::DrawOp::Path(PathOp::stroked(
        pl_draw::shapes::rect_path(rect),
        stroke,
    ))])
}

impl Document {
    // ---- the store ---------------------------------------------------------------------------

    /// Whether the annotations have been read (see [`load_annots`](Document::load_annots)).
    pub fn annots_loaded(&self) -> bool {
        self.edit.state.is_some()
    }

    /// Reads the document's annotations with lopdf, if that has not happened yet. This parses
    /// the file on the calling thread (see docs/measurements.md for what that costs on a
    /// large file); the parsed file is released again afterwards, the annotations stay.
    ///
    /// If a crash journal for this file exists and the file is unchanged since it was
    /// written, its commands are replayed: the document comes back as it was when the
    /// process died, undo history included ([`recovered_changes`](Document::recovered_changes)
    /// says how many records). A journal for another state of the file is renamed to
    /// `*.stale` and a warning is recorded.
    ///
    /// A document opened from memory whose bytes lopdf cannot read gets an empty store and a
    /// warning.
    pub fn load_annots(&mut self) -> Result<()> {
        if self.edit.state.is_some() {
            return Ok(());
        }
        // The file is being replaced while a save is in flight: it is not parsed beside it.
        self.refuse_while_saving()?;
        // A load on a worker thread is waited for, not repeated.
        self.settle_load()?;
        if self.edit.state.is_some() {
            return Ok(());
        }
        let mut units: Option<Vec<f64>> = None;
        let working = self.structure.working.as_ref().map(|w| {
            (
                AnnotSet::load(&w.doc),
                Some(ScaleStore::load(&w.doc)),
                match &self.origin {
                    Origin::File(path) => FileStamp::of_file(path).ok(),
                    Origin::Memory { .. } => None,
                },
            )
        });
        let (set, scales, stamp) = match (&self.origin, working) {
            // After a structural command the store describes the working copy.
            (_, Some(store)) => store,
            (Origin::File(path), None) => {
                let stamp = FileStamp::of_file(path).ok();
                let was_loaded = self.pdf_is_loaded();
                let file = self.pdf()?;
                let doc = file.document()?;
                let set = AnnotSet::load(doc);
                let scales = ScaleStore::load(doc);
                units = doc
                    .pages()
                    .ok()
                    .map(|pages| pages.iter().map(|p| p.user_unit).collect());
                if !was_loaded {
                    file.unload();
                }
                (set, Some(scales), stamp)
            }
            (Origin::Memory { bytes, .. }, None) => {
                match PdfDocument::from_bytes(bytes.to_vec(), self.password.as_deref()) {
                    Ok(doc) => (AnnotSet::load(&doc), Some(ScaleStore::load(&doc)), None),
                    Err(e) => {
                        self.warnings.push(format!(
                            "the annotations could not be read ({e}); starting with none"
                        ));
                        (AnnotSet::new(), None, None)
                    }
                }
            }
        };
        // lopdf knows every page's /UserUnit, which the open may not have been able to find.
        if let Some(units) = units {
            if let Err(e) = self.apply_user_units(&units, true) {
                self.warnings
                    .push(format!("/UserUnit could not be applied: {e}"));
            }
        }
        self.install_store(set, scales, stamp);
        self.recover_journal();
        Ok(())
    }

    /// Makes `set` the document's annotation store and `scales` its scales, as read from the
    /// file with `stamp`. Without `scales` (a file lopdf could not read) every page starts
    /// with no scale.
    pub(crate) fn install_store(
        &mut self,
        set: AnnotSet,
        scales: Option<ScaleStore>,
        stamp: Option<FileStamp>,
    ) {
        let page_count = self.page_count() as usize;
        let scales = scales.unwrap_or_else(|| {
            let pages: Vec<(Rect, f64)> = self
                .pages()
                .iter()
                .map(|g| (g.media_box(), g.user_unit()))
                .collect();
            ScaleStore::blank(&pages)
        });
        self.edit.state = Some(Box::new(EditState {
            editor: Editor::with_scales(set, scales, page_count),
            hidden: BTreeMap::new(),
            moved: BTreeMap::new(),
            invalid: Vec::new(),
            journal: None,
            journal_failed: false,
            stamp,
            recovered: 0,
        }));
    }

    /// Forgets the store without touching the display (the display already shows the file
    /// the store would be read from). The annotations are read afresh when next needed.
    pub(crate) fn drop_store(&mut self) {
        if let Some(mut state) = self.edit.state.take() {
            if let Some(journal) = state.journal.take() {
                let _ = journal.discard();
            }
            self.edit.revision_base += state.editor.revision() + 1;
        }
    }

    /// The annotation store, if it has been loaded. Never loads, never waits.
    pub fn editor(&self) -> Option<&Editor> {
        self.edit.state.as_ref().map(|s| &s.editor)
    }

    /// The annotation store, loading it first if need be.
    pub fn annots(&mut self) -> Result<&Editor> {
        self.load_annots()?;
        Ok(&self.state().editor)
    }

    fn state(&self) -> &EditState {
        self.edit.state.as_ref().expect("the store is loaded")
    }

    fn state_mut(&mut self) -> &mut EditState {
        self.edit.state.as_mut().expect("the store is loaded")
    }

    /// One annotation, if the store is loaded and has it.
    pub fn annotation(&self, id: &AnnotId) -> Option<&Annotation> {
        self.editor()?.set().get(id)
    }

    /// Goes up with every change to the annotations (commands, undo, redo, save, discard).
    /// A user interface caches
    /// [`overlay`](Document::overlay) by it.
    pub fn edit_revision(&self) -> u64 {
        self.edit.revision_base + self.editor().map_or(0, Editor::revision)
    }

    /// After the editor was swapped for another one (a structural step, its undo or redo, a
    /// structural save): keeps [`edit_revision`](Document::edit_revision) going up from
    /// `old_total`, whatever the new editor's own revision.
    pub(crate) fn rebase_edit_revision(&mut self, old_total: u64) {
        let own = self.editor().map_or(0, Editor::revision);
        self.edit.revision_base = (old_total + 1).saturating_sub(own);
    }

    /// Whether there are changes a save would write, as far as the history can tell: markup
    /// changes, or structural ones (see [`crate::structure`]).
    pub fn is_modified(&self) -> bool {
        self.structure.working.is_some() || self.editor().is_some_and(Editor::is_modified)
    }

    pub fn can_undo(&self) -> bool {
        self.editor().is_some_and(Editor::can_undo) || self.structure.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.editor().is_some_and(Editor::can_redo) || self.structure.can_redo()
    }

    // ---- commands ----------------------------------------------------------------------------

    /// Executes a command as one undo step: applies it to the store, appends it to the crash
    /// journal and updates the hidden set. A refused command changes nothing.
    pub fn apply(&mut self, command: Command) -> Result<Outcome> {
        self.apply_inner(command, None)
    }

    /// As [`apply`](Document::apply) for one step of a drag: consecutive `ModifyAnnotations`
    /// commands with the same `drag` key over the same annotations are one undo step.
    pub fn apply_drag(&mut self, command: Command, drag: u64) -> Result<Outcome> {
        self.apply_inner(command, Some(drag))
    }

    fn apply_inner(&mut self, command: Command, merge: Option<u64>) -> Result<Outcome> {
        self.refuse_while_saving()?;
        self.load_annots()?;
        // Serialised before the command is consumed, and only if there is a journal to write.
        let record = self.journaling().then(|| Record::Do {
            command: command.clone(),
            merge,
        });
        let editor = &mut self.state_mut().editor;
        let outcome = match merge {
            None => editor.execute(command),
            Some(key) => editor.execute_merged(command, key),
        }?;
        if outcome.changed {
            // A new change: a structural step that was undone cannot be redone after it.
            self.structure.clear_redo();
            if let Some(record) = record {
                self.journal(&record);
            }
            self.sync_mirror(&outcome.touched);
        }
        Ok(outcome)
    }

    /// Takes back the most recent command: the latest markup change, or, when the store has
    /// none left to undo, the latest structural step (see [`crate::structure`]). Returns
    /// whether there was one.
    pub fn undo(&mut self) -> Result<bool> {
        self.refuse_while_saving()?;
        self.load_annots()?;
        if !self.state().editor.can_undo() {
            if !self.undo_structure()? {
                return Ok(false);
            }
            self.journal(&Record::Undo);
            return Ok(true);
        }
        let Some(done) = self.state_mut().editor.undo() else {
            return Ok(false);
        };
        let record = if done.journaled {
            Record::Undo
        } else {
            Record::Raw {
                command: done.command,
            }
        };
        self.journal(&record);
        self.sync_mirror(&done.touched);
        Ok(true)
    }

    /// Applies again the command most recently undone. Returns whether there was one.
    pub fn redo(&mut self) -> Result<bool> {
        self.refuse_while_saving()?;
        self.load_annots()?;
        if !self.state().editor.can_redo() {
            if !self.redo_structure()? {
                return Ok(false);
            }
            self.journal(&Record::Redo);
            return Ok(true);
        }
        let Some(done) = self.state_mut().editor.redo() else {
            return Ok(false);
        };
        let record = if done.journaled {
            Record::Redo
        } else {
            Record::Raw {
                command: done.command,
            }
        };
        self.journal(&record);
        self.sync_mirror(&done.touched);
        Ok(true)
    }

    // ---- selection ---------------------------------------------------------------------------

    /// The selected annotations, in the order they were selected. Empty before the store is
    /// loaded.
    pub fn selection(&self) -> &[AnnotId] {
        self.editor().map_or(&[], |e| e.selection())
    }

    /// Whether an annotation is selected; as quick for a large selection as for a small one.
    pub fn is_selected(&self, id: &AnnotId) -> bool {
        self.editor().is_some_and(|e| e.is_selected(id))
    }

    /// A number that changes whenever the selection does ([`Editor::selection_serial`]).
    pub fn selection_serial(&self) -> u64 {
        self.editor().map_or(0, Editor::selection_serial)
    }

    /// Whether `annotation` could be added as it is: `Err` says why the editor would refuse
    /// it (its id is taken, its page does not exist, its text cannot be drawn, a number is
    /// not finite). An import leaves such an annotation out instead of failing as a whole.
    pub fn check_addable(&self, annotation: &Annotation) -> Result<()> {
        match self.editor() {
            Some(editor) => Ok(editor.check_addable(annotation)?),
            None => Ok(()),
        }
    }

    /// Replaces the selection (ids that are not in the document are dropped).
    pub fn select(&mut self, ids: impl IntoIterator<Item = AnnotId>) -> Result<()> {
        self.load_annots()?;
        self.state_mut().editor.select(ids);
        Ok(())
    }

    /// Adds one annotation to the selection.
    pub fn select_also(&mut self, id: AnnotId) -> Result<()> {
        self.load_annots()?;
        self.state_mut().editor.select_also(id);
        Ok(())
    }

    /// Selects the annotation if it is not selected, deselects it if it is. Returns whether
    /// it is selected afterwards.
    pub fn toggle_selected(&mut self, id: &AnnotId) -> Result<bool> {
        self.load_annots()?;
        Ok(self.state_mut().editor.toggle_selected(id))
    }

    /// Selects everything on a page that can be clicked.
    pub fn select_page(&mut self, page: u32) -> Result<()> {
        self.load_annots()?;
        self.state_mut().editor.select_page(page as usize);
        Ok(())
    }

    pub fn clear_selection(&mut self) {
        if let Some(state) = self.edit.state.as_mut() {
            state.editor.clear_selection();
        }
    }

    // ---- display -----------------------------------------------------------------------------

    /// The annotations of `page` that the user interface paints itself, bottom of the stack
    /// first: new markups and edited ones (see the module documentation). Annotations whose
    /// flags hide them are left out. Empty before the store is loaded.
    pub fn overlay(&self, page: u32) -> Vec<OverlayItem> {
        let Some(state) = self.edit.state.as_ref() else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for a in state.editor.set().on_page(page as usize) {
            if a.meta.flags.is_hidden() {
                continue;
            }
            let ours = a.baseline().is_none() || state.hidden.contains_key(&a.id);
            if ours {
                let list = a.draw_list();
                if !list.is_empty() {
                    items.push(OverlayItem {
                        id: a.id.clone(),
                        multiply: list.uses_blend(),
                        list,
                        placeholder: false,
                    });
                }
            } else if !a.is_modelled()
                && a.baseline().is_some_and(|b| b.model.geometry != a.geometry)
                && state
                    .moved
                    .get(&a.id)
                    .is_none_or(|m| m.shown != a.geometry.bounds())
            {
                // The mirror could not be told to draw it at its new place.
                items.push(OverlayItem {
                    id: a.id.clone(),
                    list: placeholder_list(a.geometry.bounds()),
                    multiply: false,
                    placeholder: true,
                });
            }
        }
        items
    }

    /// The annotations currently hidden in the render mirror, with their place in it.
    pub fn hidden_annotations(&self) -> Vec<(AnnotId, MirrorSlot)> {
        self.edit.state.as_ref().map_or_else(Vec::new, |s| {
            s.hidden
                .iter()
                .map(|(id, slot)| (id.clone(), *slot))
                .collect()
        })
    }

    /// The tile regions that became stale since the last call, oldest first.
    pub fn take_invalidations(&mut self) -> Vec<TileInvalidation> {
        let mut all = std::mem::take(&mut self.edit.orphans);
        if let Some(state) = self.edit.state.as_mut() {
            all.append(&mut state.invalid);
        }
        all
    }

    /// Removes the stale tiles ([`take_invalidations`](Document::take_invalidations)) from a
    /// tile cache. Returns how many tiles went. A region on a page whose real geometry is
    /// not known yet has no tiles of the right layout to remove and is skipped.
    pub fn apply_invalidations(&mut self, cache: &TileCache) -> usize {
        let mut removed = 0;
        for inv in self.take_invalidations() {
            if inv.render != self.render {
                // The mirror was reopened since; every tile of the old one is stale anyway.
                continue;
            }
            if let Ok(geometry) = self.page_geometry(inv.page) {
                removed += cache.remove_covering(inv.render, inv.page, geometry, inv.rect);
            }
        }
        removed
    }

    /// Renders a whole page as the user sees it: PDFium's render of the page with the mirror
    /// as it is (edited annotations hidden) and the overlay drawn on top with tiny-skia,
    /// Multiply included. `scale` is pixels per user-space unit. Waits for the backend.
    pub fn render_page_composite(&mut self, page: u32, scale: f64) -> Result<Bitmap> {
        let geometry = *self.load_geometry(page)?;
        let mut bitmap = self.backend.render_page(self.render, page, scale)?;
        let overlay = self.overlay(page);
        if overlay.is_empty() {
            return Ok(bitmap);
        }
        let draw = |e: pl_draw::DrawError| DocError::Draw(e.to_string());
        let transform = pl_draw::raster::page_transform(&geometry, scale);
        let mut canvas =
            pl_draw::raster::pixmap_from_rgba(&bitmap.pixels, bitmap.width, bitmap.height)
                .map_err(draw)?;
        for item in &overlay {
            pl_draw::raster::render(&item.list, transform, &mut canvas).map_err(draw)?;
        }
        bitmap.pixels = canvas.take_demultiplied();
        Ok(bitmap)
    }

    /// Brings the mirror in line with the store for the annotations a change touched: an
    /// annotation the user interface now paints (or that is deleted) is hidden, one that
    /// PDFium paints again is shown, and one PDFium paints that has been moved (a kind this
    /// version cannot draw) gets its new rectangle, so it is seen at its new place.
    fn sync_mirror(&mut self, touched: &[AnnotId]) {
        enum Change {
            Hide(MirrorSlot),
            Show(MirrorSlot),
            Move(MirrorSlot, Rect),
        }
        let mut changes: Vec<(AnnotId, Change)> = Vec::new();
        {
            let state = self.state();
            let set = state.editor.set();
            for id in touched {
                let (present, annotation) = match set.get(id) {
                    Some(a) => (true, Some(a)),
                    None => (false, set.removed().iter().find(|r| r.id == *id)),
                };
                // An annotation that is not in the file is not in the mirror either.
                let Some(base) = annotation.and_then(Annotation::baseline) else {
                    continue;
                };
                let a = annotation.expect("has a baseline");
                let hidden = state.hidden.get(id);
                let want_hidden = !present || a.is_modelled();
                let slot = || MirrorSlot {
                    page: base.page as u32,
                    index: base.slot as u32,
                    rect: file_rect(a),
                    hidden_in_file: base.model.meta.flags.has(pl_annot::Flags::HIDDEN),
                };
                match (want_hidden, hidden) {
                    (true, None) => {
                        changes.push((id.clone(), Change::Hide(slot())));
                        // A count is written as one annotation per marker: the further ones
                        // are hidden with it, under their own names.
                        for (k, m) in base.members.iter().enumerate() {
                            let name = m
                                .name
                                .clone()
                                .map_or_else(|| AnnotId(format!("{id}#{}", k + 1)), AnnotId);
                            if !state.hidden.contains_key(&name) {
                                changes.push((
                                    name,
                                    Change::Hide(MirrorSlot {
                                        page: m.page as u32,
                                        index: m.slot as u32,
                                        rect: m.rect,
                                        hidden_in_file: false,
                                    }),
                                ));
                            }
                        }
                    }
                    (false, Some(slot)) => changes.push((id.clone(), Change::Show(*slot))),
                    _ => {}
                }
                if !want_hidden {
                    // PDFium draws it: where?
                    let was = state.moved.get(id);
                    let shown = was.map_or_else(|| file_rect(a), |m| m.shown);
                    let wanted = if base.model.geometry == a.geometry {
                        file_rect(a)
                    } else {
                        a.geometry.bounds()
                    };
                    if wanted != shown && wanted.is_finite() {
                        let slot = match was {
                            Some(m) => MirrorSlot {
                                index: m.index,
                                ..slot()
                            },
                            None => slot(),
                        };
                        changes.push((id.clone(), Change::Move(slot, wanted)));
                    }
                }
            }
        }
        for (id, change) in changes {
            let render = self.render;
            let invalid = |rect: Rect, page: u32| TileInvalidation {
                render,
                page,
                rect: rect.inflate(INVALIDATION_MARGIN, INVALIDATION_MARGIN),
            };
            match change {
                Change::Hide(mut slot) => {
                    // A moved annotation keeps the index it was found at.
                    let moved = self.state().moved.get(&id).copied();
                    slot.index = match moved {
                        Some(m) => m.index,
                        None => self.mirror_index(&id, slot),
                    };
                    self.set_hidden_in_mirror(&id, slot, true);
                    let shown = moved.map_or(slot.rect, |m| m.shown);
                    let state = self.state_mut();
                    state.hidden.insert(id, slot);
                    state.invalid.push(invalid(shown, slot.page));
                }
                Change::Show(slot) => {
                    self.set_hidden_in_mirror(&id, slot, false);
                    let state = self.state_mut();
                    let shown = state.moved.get(&id).map_or(slot.rect, |m| m.shown);
                    state.hidden.remove(&id);
                    state.invalid.push(invalid(shown, slot.page));
                }
                Change::Move(mut slot, to) => {
                    let was = self.state().moved.get(&id).copied();
                    if was.is_none() {
                        slot.index = self.mirror_index(&id, slot);
                    }
                    if self.suspended {
                        // No display to move it in: the overlay marks the new place.
                        continue;
                    }
                    let rect = [to.x0, to.y0, to.x1, to.y1];
                    match self
                        .backend
                        .set_annotation_rect(render, slot.page, slot.index, rect)
                    {
                        Ok(()) => {
                            let from = was.map_or(slot.rect, |m| m.shown);
                            let state = self.state_mut();
                            if to == slot.rect {
                                state.moved.remove(&id);
                            } else {
                                state.moved.insert(
                                    id,
                                    MovedSlot {
                                        page: slot.page,
                                        index: slot.index,
                                        file: slot.rect,
                                        shown: to,
                                    },
                                );
                            }
                            state.invalid.push(invalid(from, slot.page));
                            state.invalid.push(invalid(to, slot.page));
                        }
                        Err(e) => self.warnings.push(format!(
                            "annotation {id} could not be moved in the page render: {e}"
                        )),
                    }
                }
            }
        }
    }

    fn set_hidden_in_mirror(&mut self, id: &AnnotId, slot: MirrorSlot, hide: bool) {
        // An annotation the file itself hides is hidden in the mirror already and stays so.
        if self.suspended || slot.hidden_in_file {
            return;
        }
        if let Err(e) = self
            .backend
            .set_annotation_hidden(self.render, slot.page, slot.index, hide)
        {
            self.warnings.push(format!(
                "annotation {id} could not be {} in the page render: {e}",
                if hide { "hidden" } else { "shown" }
            ));
        }
    }

    /// The annotation's index in the mirror's page. The file position is the index; when the
    /// mirror names its annotations and another position carries this id, that one is used.
    fn mirror_index(&self, id: &AnnotId, slot: MirrorSlot) -> u32 {
        if self.suspended {
            return slot.index;
        }
        let Ok(list) = self.backend.annotations(self.render, slot.page) else {
            return slot.index;
        };
        let named = |info: &pl_render::AnnotInfo| info.name.as_deref() == Some(id.as_str());
        if list.iter().any(|i| i.index == slot.index && named(i)) {
            return slot.index;
        }
        let mut matches = list.iter().filter(|i| named(i));
        match (matches.next(), matches.next()) {
            (Some(only), None) => only.index,
            _ => slot.index,
        }
    }

    /// Hides and moves again, in a freshly opened mirror, everything the store has hidden
    /// or moved.
    pub(crate) fn reapply_hidden(&mut self) {
        let Some(state) = self.edit.state.as_ref() else {
            return;
        };
        if self.suspended {
            return;
        }
        let mut failures = Vec::new();
        for fix in state.mirror_fixes() {
            let done = match fix.rect {
                Some(rect) => {
                    self.backend
                        .set_annotation_rect(self.render, fix.page, fix.index, rect)
                }
                None => Ok(()),
            }
            .and_then(|()| match fix.hide {
                true => self
                    .backend
                    .set_annotation_hidden(self.render, fix.page, fix.index, true),
                false => Ok(()),
            });
            if let Err(e) = done {
                failures.push(format!(
                    "an edited annotation on page {} could not be put back in the page render: {e}",
                    fix.page + 1
                ));
            }
        }
        self.warnings.extend(failures);
    }

    /// The annotations PDFium draws at another place than the file has them: kinds this
    /// version cannot draw itself that have been moved. With their place in the mirror.
    pub fn moved_annotations(&self) -> Vec<(AnnotId, MovedSlot)> {
        self.edit.state.as_ref().map_or_else(Vec::new, |s| {
            s.moved
                .iter()
                .map(|(id, slot)| (id.clone(), *slot))
                .collect()
        })
    }

    // ---- journal -----------------------------------------------------------------------------

    /// Where this document's crash journal is (whether or not it exists).
    pub(crate) fn journal_file_path(&self) -> Option<PathBuf> {
        self.journal_file().map(|(file, _)| file)
    }

    fn journal_file(&self) -> Option<(PathBuf, &Path)> {
        let dir = self.edit.journal_dir.as_deref()?;
        let path = self.path()?;
        Some((journal_path(dir, path), path))
    }

    fn journaling(&self) -> bool {
        self.journal_file().is_some()
            && self
                .edit
                .state
                .as_ref()
                .is_some_and(|s| !s.journal_failed && s.stamp.is_some())
    }

    /// Whether a crash journal waits for this document. Cheap: it reads the journal's header
    /// and the document's file stamp, nothing else. Once the annotations are loaded the
    /// answer is `None` (the journal has been dealt with).
    pub fn pending_journal(&self) -> PendingJournal {
        if self.edit.state.is_some() {
            return PendingJournal::None;
        }
        let Some((journal, path)) = self.journal_file() else {
            return PendingJournal::None;
        };
        if !journal.exists() {
            return PendingJournal::None;
        }
        match (Journal::peek(&journal), FileStamp::of_file(path)) {
            (Some((document, stamp)), Ok(now)) if same_path(&document, path) && stamp == now => {
                PendingJournal::Recoverable
            }
            _ => PendingJournal::Stale,
        }
    }

    /// How many journal records were replayed when the annotations were loaded: not zero
    /// means the document holds unsaved changes recovered from an earlier run.
    pub fn recovered_changes(&self) -> usize {
        self.edit.state.as_ref().map_or(0, |s| s.recovered)
    }

    /// Appends a record to the journal, starting the journal if this is the first. A journal
    /// that cannot be written becomes a warning, once; editing goes on without it.
    pub(crate) fn journal(&mut self, record: &Record) {
        if !self.journaling() {
            return;
        }
        let (_, path) = self.journal_file().expect("journaling");
        let path = path.to_path_buf();
        let dir = self.edit.journal_dir.clone().expect("journaling");
        let state = self.state_mut();
        let result = (|| {
            if state.journal.is_none() {
                let stamp = state.stamp.expect("journaling");
                state.journal = Some(Journal::create(&dir, &path, stamp)?);
            }
            state.journal.as_mut().expect("just set").append(record)
        })();
        if let Err(e) = result {
            state.journal_failed = true;
            state.journal = None;
            self.warnings.push(format!(
                "the crash journal could not be written ({e}); unsaved changes are not \
                 protected against a crash"
            ));
        }
    }

    /// Replays a matching journal onto the freshly loaded store, or sets a stale one aside.
    fn recover_journal(&mut self) {
        let contents = match self.journal_file_path() {
            Some(file) if file.exists() => Some(Journal::read(&file)),
            _ => None,
        };
        self.recover_journal_from(contents);
    }

    /// As [`recover_journal`](Document::recover_journal) with the journal already read
    /// (`None`: there is none).
    pub(crate) fn recover_journal_from(
        &mut self,
        contents: Option<std::io::Result<crate::journal::JournalContents>>,
    ) {
        let Some(contents) = contents else {
            return;
        };
        let Some((journal_file, path)) = self.journal_file() else {
            return;
        };
        let path = path.to_path_buf();
        let contents = match contents {
            Ok(contents) => contents,
            Err(e) => {
                self.warnings
                    .push(format!("the crash journal could not be read: {e}"));
                self.state_mut().journal_failed = true;
                return;
            }
        };
        let matches = match (&contents.header, self.state().stamp) {
            (Some((document, stamp)), Some(now)) => same_path(document, &path) && *stamp == now,
            _ => false,
        };
        if !matches {
            let mut aside = journal_file.clone().into_os_string();
            aside.push(".stale");
            let aside = PathBuf::from(aside);
            let _ = crate::journal::remove_if_present(&aside);
            let moved = std::fs::rename(&journal_file, &aside);
            self.warnings.push(match &moved {
                Ok(()) => format!(
                    "unsaved changes from an earlier run were not recovered because the file \
                     has changed since; the journal was kept as {}",
                    aside.display()
                ),
                Err(e) => format!(
                    "a crash journal for another state of this file could not be set aside: {e}"
                ),
            });
            if moved.is_err() {
                self.state_mut().journal_failed = true;
            }
            return;
        }

        let mut touched: Vec<AnnotId> = Vec::new();
        let mut valid_len = contents.header_end;
        let mut replayed = 0usize;
        let mut problem = None;
        for (record, end) in &contents.records {
            // A structural step (or its undo or redo) snapshots what the mirror hides: what
            // the records so far touched is shown in the mirror first, as it was live.
            let structural = match record {
                Record::Structure { .. } => true,
                Record::Undo => !self.state().editor.can_undo(),
                Record::Redo => !self.state().editor.can_redo(),
                _ => false,
            };
            if structural {
                touched.sort();
                touched.dedup();
                self.sync_mirror(&touched);
                touched.clear();
            }
            let done = match record {
                Record::Header { .. } => Err("a second header".to_string()),
                Record::Structure {
                    command,
                    invalidate_signatures,
                } => {
                    let policy = if *invalidate_signatures {
                        pl_pdf::SignaturePolicy::Invalidate
                    } else {
                        pl_pdf::SignaturePolicy::Refuse
                    };
                    self.run_structure(command, policy)
                        .map(|_| Vec::new())
                        .map_err(|e| e.to_string())
                }
                Record::Undo if structural => match self.undo_structure() {
                    Ok(true) => Ok(Vec::new()),
                    Ok(false) => Err("nothing to undo".to_string()),
                    Err(e) => Err(e.to_string()),
                },
                Record::Redo if structural => match self.redo_structure() {
                    Ok(true) => Ok(Vec::new()),
                    Ok(false) => Err("nothing to redo".to_string()),
                    Err(e) => Err(e.to_string()),
                },
                Record::Do { command, merge } => {
                    let editor = &mut self.state_mut().editor;
                    let done = match merge {
                        None => editor.execute(command.clone()),
                        Some(key) => editor.execute_merged(command.clone(), *key),
                    }
                    .map_err(|e| e.to_string());
                    if done.as_ref().is_ok_and(|o| o.changed) {
                        self.structure.clear_redo();
                    }
                    done.map(|o| o.touched)
                }
                Record::Raw { command } => self
                    .state_mut()
                    .editor
                    .apply_raw(command)
                    .map(|o| o.touched)
                    .map_err(|e| e.to_string()),
                Record::Undo => self
                    .state_mut()
                    .editor
                    .undo()
                    .map(|r| r.touched)
                    .ok_or_else(|| "nothing to undo".to_string()),
                Record::Redo => self
                    .state_mut()
                    .editor
                    .redo()
                    .map(|r| r.touched)
                    .ok_or_else(|| "nothing to redo".to_string()),
            };
            match done {
                Ok(ids) => {
                    touched.extend(ids);
                    valid_len = *end;
                    replayed += 1;
                }
                Err(e) => {
                    problem = Some(e);
                    break;
                }
            }
        }
        if self.structure.working.is_none() {
            self.state_mut().editor.settle_if_clean();
        }
        if let Some(e) = problem {
            self.warnings.push(format!(
                "the crash journal could only be replayed up to record {replayed} of {} ({e})",
                contents.records.len()
            ));
        } else if contents.damaged {
            self.warnings.push(
                "the last record of the crash journal was incomplete and was left out".into(),
            );
        }
        match Journal::resume(&journal_file, valid_len) {
            Ok(journal) => self.state_mut().journal = Some(journal),
            Err(e) => {
                self.state_mut().journal_failed = true;
                self.warnings
                    .push(format!("the crash journal could not be reopened: {e}"));
            }
        }
        self.state_mut().recovered = replayed;
        touched.sort();
        touched.dedup();
        self.sync_mirror(&touched);
    }

    /// Removes the crash journal of a document that is being closed with nothing unsaved in
    /// it (an edit that was undone again, say), so the next open does not offer to recover
    /// changes that do not exist. A journal that was never read (the annotations were not
    /// loaded in this run) is left for the next run, and so is that of a modified document.
    pub(crate) fn release_journal_if_unmodified(&mut self) {
        if self.edit.state.is_some() && !self.is_modified() {
            self.discard_journal_file();
        }
    }

    fn discard_journal_file(&mut self) {
        let file = self.journal_file().map(|(file, _)| file);
        if let Some(state) = self.edit.state.as_mut() {
            if let Some(journal) = state.journal.take() {
                let _ = journal.discard();
            }
        }
        if let Some(file) = file {
            let _ = crate::journal::remove_if_present(&file);
        }
    }

    /// Throws away every unsaved change: the store, its history and the crash journal. The
    /// document shows the file again; the annotations are read afresh when next needed. Call
    /// this when the user closes a document without saving, so its journal is not offered
    /// for recovery next time.
    pub fn discard_changes(&mut self) {
        // A save in flight is finished first: its file and its display are not to be touched
        // beside it. (If it succeeds there is nothing left to discard.)
        self.settle_save();
        // A load in flight would bring a store back: its result is not wanted any more.
        self.abandon_load();
        // Without loading the store: a journal from an earlier run is discarded unread.
        self.discard_journal_file();
        if self.structure.working.is_some() {
            // The display shows the working copy: it is reopened on the file, where nothing
            // is hidden or moved, so the store's mirror state is simply forgotten.
            if let Err(e) = self.drop_structure() {
                self.warnings.push(format!(
                    "the page display could not be reopened on the file: {e}"
                ));
            }
            if let Some(state) = self.edit.state.take() {
                self.edit.revision_base += state.editor.revision() + 1;
            }
            return;
        }
        // Structural steps that were all undone leave nothing to redo either.
        self.structure.clear_all();
        let Some(state) = self.edit.state.take() else {
            return;
        };
        let render = self.render;
        let mut stale = state.invalid;
        let inflate = |rect: Rect| rect.inflate(INVALIDATION_MARGIN, INVALIDATION_MARGIN);
        for (id, moved) in &state.moved {
            if !self.suspended {
                let file = [moved.file.x0, moved.file.y0, moved.file.x1, moved.file.y1];
                if let Err(e) =
                    self.backend
                        .set_annotation_rect(render, moved.page, moved.index, file)
                {
                    self.warnings.push(format!(
                        "annotation {id} could not be moved back in the page render: {e}"
                    ));
                }
            }
            for rect in [moved.file, moved.shown] {
                stale.push(TileInvalidation {
                    render,
                    page: moved.page,
                    rect: inflate(rect),
                });
            }
        }
        for (id, slot) in &state.hidden {
            if !self.suspended && !slot.hidden_in_file {
                if let Err(e) = self
                    .backend
                    .set_annotation_hidden(render, slot.page, slot.index, false)
                {
                    self.warnings.push(format!(
                        "annotation {id} could not be shown in the page render: {e}"
                    ));
                }
            }
            stale.push(TileInvalidation {
                render,
                page: slot.page,
                rect: inflate(slot.rect),
            });
        }
        // The regions still have to reach the tile cache.
        self.edit.orphans.append(&mut stale);
        self.edit.revision_base += state.editor.revision() + 1;
    }

    // ---- save --------------------------------------------------------------------------------

    /// Saves the annotation changes into the file as an incremental update (see the module
    /// documentation). When nothing differs from the file, nothing is written and the file is
    /// not touched. On failure the file is as it was and the document keeps its changes,
    /// history and journal.
    ///
    /// This runs on the calling thread; [`start_save`](Document::start_save) is the same
    /// save on a worker thread.
    pub fn save(&mut self) -> Result<MarkupSave> {
        self.save_with(&SaveOptions::default())
    }

    /// As [`save`](Document::save) with explicit pl-pdf options (tests use the fail points).
    pub fn save_with(&mut self, options: &SaveOptions) -> Result<MarkupSave> {
        self.save_blocking(SaveTarget::InPlace, options, None)
    }

    /// Rewrites the whole file as a single revision with the annotation changes applied:
    /// objects that are no longer used (deleted annotations, replaced appearances, earlier
    /// revisions) are dropped. Verified like [`save`](Document::save). This always writes.
    pub fn save_optimised(&mut self) -> Result<MarkupSave> {
        self.save_optimised_with(&SaveOptions::default())
    }

    pub fn save_optimised_with(&mut self, options: &SaveOptions) -> Result<MarkupSave> {
        self.save_blocking(SaveTarget::Optimised, options, None)
    }

    /// Saves to another file and ties the document to it: the original file is not modified,
    /// and from here on the document, its journal and later saves belong to `dest`. The new
    /// file is the original plus the annotation changes as an incremental update, verified
    /// before it appears under its name. An existing `dest` is replaced. Saving "as" the
    /// document's own file is [`save`](Document::save).
    ///
    /// A document opened from memory is written out first and then saved in place.
    pub fn save_as(&mut self, dest: impl AsRef<Path>) -> Result<MarkupSave> {
        self.save_as_with(dest, &SaveOptions::default())
    }

    pub fn save_as_with(
        &mut self,
        dest: impl AsRef<Path>,
        options: &SaveOptions,
    ) -> Result<MarkupSave> {
        self.save_blocking(SaveTarget::As(dest.as_ref().to_path_buf()), options, None)
    }

    /// Whether [`Document::save_incremental`] may run: it writes a caller's own edit set and
    /// knows nothing of the annotation store, so unsaved annotation changes must be saved (or
    /// discarded) first.
    pub(crate) fn check_raw_save(&self) -> Result<()> {
        if self.structure.working.is_some() {
            return Err(DocError::UnsavedStructure);
        }
        match self.editor() {
            Some(editor) if editor.has_unsaved() => Err(DocError::UnsavedMarkups),
            _ => Ok(()),
        }
    }

    // ---- the markups list --------------------------------------------------------------------

    /// Sets a markup's text ([`markup_ops::set_text`]) as one undo step.
    pub fn set_text(&mut self, id: &AnnotId, text: &str, when: PdfDate) -> Result<Outcome> {
        let command = markup_ops::set_text(self.annots()?.set(), id, text, when)?;
        self.apply(command)
    }

    /// Sets a markup's subject ([`markup_ops::set_subject`]). Nothing happens, and no undo
    /// step is made, when the subject is what it already was.
    pub fn set_subject(&mut self, id: &AnnotId, subject: &str, when: PdfDate) -> Result<Outcome> {
        let command = markup_ops::set_subject(self.annots()?.set(), id, subject, when)?;
        self.apply(command)
    }

    /// Records a review status for a markup, set by `origin.author`
    /// ([`markup_ops::set_status`]). Returns the id of the status annotation.
    pub fn set_status(
        &mut self,
        id: &AnnotId,
        state: ReviewState,
        origin: &Stamp,
    ) -> Result<AnnotId> {
        let (command, new) = markup_ops::set_status(self.annots()?.set(), id, state, origin)?;
        self.apply(command)?;
        Ok(new)
    }

    /// Records a status of any model for a markup: a review state, or a custom status set's
    /// state under the set's own `/StateModel` ([`markup_ops::set_status_named`]). Returns the
    /// id of the status annotation.
    pub fn set_status_named(
        &mut self,
        id: &AnnotId,
        status: &pl_annot::Status,
        origin: &Stamp,
    ) -> Result<AnnotId> {
        let (command, new) =
            markup_ops::set_status_named(self.annots()?.set(), id, status, origin)?;
        self.apply(command)?;
        Ok(new)
    }

    /// A markup's latest status of any model but the checkmark's; `None` when it has none or
    /// the annotations are not loaded.
    pub fn status(&self, id: &AnnotId) -> Option<pl_annot::Status> {
        self.editor()?.set().status(id)
    }

    /// The status sets a markup's status can be chosen from: the review states first, then
    /// each custom set the document's status annotations use (Bluebeam Revu's custom
    /// statuses), with the states met under it.
    pub fn status_sets(&self) -> Vec<pl_annot::StatusSet> {
        let mut sets = vec![pl_annot::StatusSet::review()];
        if let Some(e) = self.editor() {
            sets.extend(e.set().status_sets());
        }
        sets
    }

    /// The columns the document itself defines in Bluebeam Revu's `/BSIAnnotColumns`, in
    /// Revu's display order, each bound to its position in `/BSIColumnData`
    /// ([`pl_takeoff::bluebeam`]). Empty before the annotations are loaded.
    pub fn bluebeam_columns(&self) -> Vec<pl_takeoff::CustomColumn> {
        self.editor()
            .map(|e| e.scales().bluebeam().columns())
            .unwrap_or_default()
    }

    /// Whether the document has Bluebeam custom columns (`/BSIAnnotColumns`).
    pub fn has_bluebeam_columns(&self) -> bool {
        self.editor()
            .is_some_and(|e| e.scales().bluebeam().is_present())
    }

    /// Adds a column to the document's Bluebeam custom columns and pads every markup's
    /// `/BSIColumnData` ([`markup_ops::add_bluebeam_column`]), as one undo step. Returns the
    /// new column.
    pub fn add_bluebeam_column(
        &mut self,
        title: &str,
        kind: pl_takeoff::ColumnType,
        options: &[&str],
        multiline: bool,
    ) -> Result<pl_takeoff::CustomColumn> {
        let editor = self.annots()?;
        let (command, column) = markup_ops::add_bluebeam_column(
            editor.set(),
            editor.scales().bluebeam(),
            title,
            kind,
            options,
            multiline,
        )
        .map_err(crate::command::EditError::from)?;
        self.apply(command)?;
        Ok(column)
    }

    /// The review status of a markup; `None` state before the annotations are loaded.
    pub fn review_state(&self, id: &AnnotId) -> ReviewState {
        self.editor()
            .map_or(ReviewState::None, |e| e.set().review_state(id))
    }

    /// Adds a reply to a markup or to another reply ([`markup_ops::add_reply`]). Returns the
    /// reply's id.
    pub fn add_reply(&mut self, id: &AnnotId, text: &str, origin: &Stamp) -> Result<AnnotId> {
        let (command, new) = markup_ops::add_reply(self.annots()?.set(), id, text, origin)?;
        self.apply(command)?;
        Ok(new)
    }

    /// Deletes a reply or a status annotation ([`markup_ops::delete_reply`]).
    pub fn delete_reply(&mut self, id: &AnnotId) -> Result<Outcome> {
        let command = markup_ops::delete_reply(self.annots()?.set(), id)?;
        self.apply(command)
    }

    /// Ticks a markup off or clears the tick, on behalf of `origin.author`
    /// ([`markup_ops::set_checked`]).
    pub fn set_checked(&mut self, id: &AnnotId, checked: bool, origin: &Stamp) -> Result<Outcome> {
        let command = markup_ops::set_checked(self.annots()?.set(), id, checked, origin)?;
        self.apply(command)
    }

    /// Flips a markup's checkmark. Returns whether it is ticked afterwards.
    pub fn toggle_checked(&mut self, id: &AnnotId, origin: &Stamp) -> Result<bool> {
        let command = markup_ops::toggle_checked(self.annots()?.set(), id, origin)?;
        self.apply(command)?;
        Ok(self.is_checked(id))
    }

    /// Whether a markup is ticked off; false before the annotations are loaded.
    pub fn is_checked(&self, id: &AnnotId) -> bool {
        self.editor()
            .is_some_and(|e| markup_ops::is_checked(e.set(), id))
    }

    /// Sets or clears a markup's Locked flag ([`markup_ops::set_locked`]).
    pub fn set_locked(&mut self, id: &AnnotId, locked: bool, when: PdfDate) -> Result<Outcome> {
        let command = markup_ops::set_locked(self.annots()?.set(), id, locked, when)?;
        self.apply(command)
    }

    /// Flips a markup's Locked flag. Returns whether it is locked afterwards.
    pub fn toggle_locked(&mut self, id: &AnnotId, when: PdfDate) -> Result<bool> {
        let command = markup_ops::toggle_locked(self.annots()?.set(), id, when)?;
        self.apply(command)?;
        Ok(self.annotation(id).is_some_and(markup_ops::is_locked))
    }

    /// Sets a markup's value in a custom column; an empty value clears it
    /// ([`markup_ops::set_column`]).
    pub fn set_column(
        &mut self,
        id: &AnnotId,
        column: &pl_takeoff::CustomColumn,
        value: pl_takeoff::Value,
        when: PdfDate,
    ) -> Result<Outcome> {
        let command = markup_ops::set_column(self.annots()?.set(), id, column, value, when)?;
        self.apply(command)
    }

    /// As [`set_column`](Document::set_column) with the value as a user typed it.
    pub fn set_column_text(
        &mut self,
        id: &AnnotId,
        column: &pl_takeoff::CustomColumn,
        text: &str,
        when: PdfDate,
    ) -> Result<Outcome> {
        let command = markup_ops::set_column_text(self.annots()?.set(), id, column, text, when)?;
        self.apply(command)
    }

    /// Gives a markup a popup window when it has none ([`markup_ops::ensure_popup`]) and
    /// returns the popup's id. The popup is kept inside the page's visible box; a page whose
    /// geometry has not arrived yet is loaded for it (which waits for the backend).
    pub fn ensure_popup(&mut self, id: &AnnotId, origin: &Stamp) -> Result<AnnotId> {
        let page = self
            .annots()?
            .set()
            .get(id)
            .ok_or_else(|| crate::command::EditError::NotFound(id.clone()))?
            .page as u32;
        let page_box = self.load_geometry(page)?.crop_box();
        let (command, popup) =
            markup_ops::ensure_popup(self.annots()?.set(), id, page_box, origin)?;
        if let Some(command) = command {
            self.apply(command)?;
        }
        Ok(popup)
    }

    /// Opens or closes a markup's popup window ([`markup_ops::set_popup_open`]).
    pub fn set_popup_open(&mut self, id: &AnnotId, open: bool, when: PdfDate) -> Result<Outcome> {
        let command = markup_ops::set_popup_open(self.annots()?.set(), id, open, when)?;
        self.apply(command)
    }

    /// Moves or resizes a markup's popup window ([`markup_ops::set_popup_rect`]).
    pub fn set_popup_rect(&mut self, id: &AnnotId, rect: Rect, when: PdfDate) -> Result<Outcome> {
        let command = markup_ops::set_popup_rect(self.annots()?.set(), id, rect, when)?;
        self.apply(command)
    }

    // ---- clipboard ---------------------------------------------------------------------------

    /// Copies annotations (see [`Clipboard::from_annotations`]). Ids that are not in the
    /// document are ignored; the copies are in stacking order whatever order the ids are in.
    pub fn copy(&mut self, ids: &[AnnotId]) -> Result<Clipboard> {
        self.load_annots()?;
        let set = self.state().editor.set();
        Ok(Clipboard::from_annotations(
            set.iter().filter(|a| ids.contains(&a.id)),
        ))
    }

    /// Copies annotations and deletes the ones that were copied, as one undo step. What the
    /// clipboard cannot hold (see [`Clipboard::skipped`]) stays in the document; popups and
    /// replies go with their markup as in any delete.
    pub fn cut(&mut self, ids: &[AnnotId]) -> Result<Clipboard> {
        self.refuse_while_saving()?;
        let clip = self.copy(ids)?;
        let present: Vec<AnnotId> = {
            let set = self.state().editor.set();
            ids.iter()
                .filter(|id| set.get(id).is_some_and(Clipboard::can_hold))
                .cloned()
                .collect()
        };
        if !present.is_empty() {
            self.apply(Command::DeleteAnnotations(present))?;
        }
        Ok(clip)
    }

    /// Pastes a clipboard onto `page` as one undo step and selects what was pasted. Returns
    /// the new annotations' ids, bottom of the stack first. `stamp` gives the author and date
    /// the copies are created with.
    pub fn paste(
        &mut self,
        clipboard: &Clipboard,
        page: u32,
        placement: PastePlacement,
        stamp: &Stamp,
    ) -> Result<Vec<AnnotId>> {
        self.refuse_while_saving()?;
        self.load_annots()?;
        let new = clipboard.instantiate(page as usize, placement, stamp);
        let ids: Vec<AnnotId> = new.iter().map(|a| a.id.clone()).collect();
        if new.is_empty() {
            return Ok(ids);
        }
        self.apply(Command::AddAnnotations(new))?;
        self.state_mut().editor.select(ids.clone());
        Ok(ids)
    }

    /// The topmost annotation of `page` under `point`, if the store is loaded.
    pub fn hit_annotation(&self, page: u32, point: Point, tolerance: f64) -> Option<&Annotation> {
        self.editor()?
            .set()
            .hit_test(page as usize, point, tolerance)
    }
}
