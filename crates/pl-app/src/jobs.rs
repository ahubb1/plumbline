//! Work that runs on a worker thread while the window keeps drawing: saving a document, the
//! first read of its annotations and the reading of its layer list.
//!
//! pl-doc does the work ([`pl_doc::background`]); this module starts it, remembers the tickets
//! and asks once per frame whether each has finished ([`AppState::poll_jobs`]). That question
//! never waits. While a document is being saved it is read-only: its tools, panels and list
//! are disabled and no new tiles are asked for, and the status bar shows a small progress
//! mark until the save is taken in.
//!
//! The functions that return an answer at once ([`AppState::save_active`],
//! [`AppState::wait_idle`]) wait for the worker instead; nothing the window itself calls on a
//! frame goes through them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use pl_doc::pl_render::DocId;
use pl_doc::{DocError, Document, DocumentId, LoadTicket, MarkupSave, SaveTarget, SaveTicket};

use crate::app::{display_name, AppState, ViewId};
use crate::markup::SaveHow;

/// What happens to the view a save was asked from once the save has succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterSave {
    Stay,
    /// Close the view; with `quitting`, go on closing the window.
    Close {
        quitting: bool,
    },
}

/// A save in flight.
pub(crate) struct SaveJob {
    ticket: SaveTicket,
    view: ViewId,
    old_render: DocId,
    after: AfterSave,
    title: String,
}

/// What a load in flight is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadWhy {
    Annots {
        /// The user asked for the unsaved markups of an earlier run to be brought back.
        recovering: bool,
    },
    Layers,
}

struct LoadJob {
    ticket: LoadTicket,
    why: LoadWhy,
}

/// A save that waits for a load in flight to finish.
struct QueuedSave {
    view: ViewId,
    target: SaveTarget,
    after: AfterSave,
}

/// The background work of every open document.
#[derive(Default)]
pub(crate) struct Jobs {
    saves: HashMap<DocumentId, SaveJob>,
    loads: HashMap<DocumentId, LoadJob>,
    queued: HashMap<DocumentId, QueuedSave>,
    /// Documents whose annotations could not be read; they are not read again unasked.
    annots_failed: HashSet<DocumentId>,
    /// Documents whose layers the Layers page has asked for.
    pub layers_asked: HashSet<DocumentId>,
    /// How each document's last finished save went.
    last_save: HashMap<DocumentId, bool>,
    /// Saves and loads taken in so far (the tests count them).
    pub finished: u64,
}

impl Jobs {
    pub(crate) fn forget(&mut self, doc: DocumentId) {
        self.saves.remove(&doc);
        self.loads.remove(&doc);
        self.queued.remove(&doc);
        self.annots_failed.remove(&doc);
        self.layers_asked.remove(&doc);
        self.last_save.remove(&doc);
    }

    fn pending(&self) -> bool {
        !self.saves.is_empty() || !self.loads.is_empty() || !self.queued.is_empty()
    }
}

impl AppState {
    // ---- reading -----------------------------------------------------------------------------

    /// Whether the document a view shows is being saved right now (it is read-only until the
    /// save has finished).
    pub fn is_saving(&self, view: ViewId) -> bool {
        self.document_of(view).is_some_and(Document::is_saving)
    }

    pub(crate) fn doc_is_saving(&self, doc: DocumentId) -> bool {
        self.session.document(doc).is_ok_and(Document::is_saving)
    }

    /// Whether the active document may be edited: there is one, and it is not being saved.
    pub(crate) fn active_editable(&self) -> bool {
        self.active_document().is_some_and(|d| !d.is_saving())
    }

    /// What is going on in the background for the active document, for the status bar:
    /// `Saving plan.pdf` or `Reading markups`. `None` when nothing is.
    pub fn background_activity(&self) -> Option<String> {
        let dv = self.active_view()?;
        if let Some(job) = self.jobs.saves.get(&dv.doc) {
            return Some(format!("Saving {}", job.title));
        }
        if self.jobs.queued.contains_key(&dv.doc) {
            return Some(format!("Saving {}", dv.title));
        }
        match self.jobs.loads.get(&dv.doc).map(|l| l.why) {
            Some(LoadWhy::Annots { .. }) => Some("Reading markups".to_owned()),
            Some(LoadWhy::Layers) => Some("Reading layers".to_owned()),
            None => None,
        }
    }

    /// How many saves and loads have been taken in since the app started.
    pub fn finished_jobs(&self) -> u64 {
        self.jobs.finished
    }

    // ---- loading -----------------------------------------------------------------------------

    /// Starts reading a document's annotations on a worker thread, unless they are there or
    /// being read already. Returns whether a load is in flight afterwards.
    pub(crate) fn start_annots_load(&mut self, doc: DocumentId, recovering: bool) -> bool {
        if let Some(job) = self.jobs.loads.get_mut(&doc) {
            if let LoadWhy::Annots { recovering: r } = &mut job.why {
                *r |= recovering;
                return true;
            }
            // The layer list is being read; the annotations are asked for again after it.
            return false;
        }
        let Ok(d) = self.session.document_mut(doc) else {
            return false;
        };
        if d.annots_loaded() || d.is_saving() {
            return false;
        }
        match d.start_load_annots() {
            Ok(ticket) => {
                self.jobs.loads.insert(
                    doc,
                    LoadJob {
                        ticket,
                        why: LoadWhy::Annots { recovering },
                    },
                );
                self.request_repaint_soon();
                true
            }
            Err(e) => {
                self.jobs.annots_failed.insert(doc);
                self.chrome.status = Some(format!("The markups could not be read: {e}"));
                false
            }
        }
    }

    /// Something is about to need a document's annotations (the pointer is over its page with
    /// a markup tool, the markups list is showing it): they are read on a worker thread, so
    /// that the click that follows finds them there. Nothing is said and nothing waits. A
    /// document with unsaved markups from an earlier run waits for the user's answer first.
    pub(crate) fn want_annots(&mut self, doc: DocumentId) {
        if self.jobs.loads.contains_key(&doc)
            || self.jobs.annots_failed.contains(&doc)
            || self.markup.recovery == Some(doc)
        {
            return;
        }
        let Ok(d) = self.session.document(doc) else {
            return;
        };
        if d.annots_loaded() || d.is_saving() {
            return;
        }
        self.start_annots_load(doc, false);
    }

    /// Whether a document's annotations are there, for something that needs them now.
    ///
    /// On a frame this never waits: when they are not there, reading them is started on a
    /// worker thread and `false` is returned, so the caller drops what it was about to do
    /// (the pointer hovering over the page has usually started the read long before a click
    /// arrives). Called from outside a frame (a script, a test, the calls that answer at
    /// once) it waits for the worker instead.
    pub(crate) fn ensure_annots(&mut self, doc: DocumentId) -> bool {
        let Ok(d) = self.session.document(doc) else {
            return false;
        };
        if d.annots_loaded() {
            return true;
        }
        if d.is_saving() {
            return false;
        }
        self.jobs.annots_failed.remove(&doc);
        let started = self.start_annots_load(doc, false);
        if !self.in_frame {
            self.drain_jobs();
            if self.start_annots_load(doc, false) {
                // A read of the layer list was in the way the first time.
                self.drain_jobs();
            }
            return self
                .session
                .document(doc)
                .is_ok_and(Document::annots_loaded);
        }
        if started {
            self.chrome.status =
                Some("Still reading this document's markups; try again in a moment.".to_owned());
        }
        false
    }

    /// Whether a document's layer list is there for the Layers page. When it is not, reading
    /// it is started in the background. This never waits.
    pub(crate) fn ensure_layers(&mut self, doc: DocumentId) -> bool {
        self.jobs.layers_asked.insert(doc);
        let Ok(d) = self.session.document_mut(doc) else {
            return false;
        };
        if d.cached_layers().is_some() {
            return true;
        }
        if self.markup.layers_failed.contains(&doc) || d.is_saving() {
            return false;
        }
        if self.jobs.loads.contains_key(&doc) {
            // The annotations are being read, and the layer list comes with them.
            return false;
        }
        if d.annots_loaded() {
            // The read that brought the annotations found no layer list to bring.
            self.markup.layers_failed.push(doc);
            return false;
        }
        match d.start_load_layers() {
            Ok(ticket) => {
                self.jobs.loads.insert(
                    doc,
                    LoadJob {
                        ticket,
                        why: LoadWhy::Layers,
                    },
                );
                self.request_repaint_soon();
            }
            Err(e) => {
                self.markup.layers_failed.push(doc);
                self.chrome.status = Some(format!("The layers could not be read: {e}"));
            }
        }
        false
    }

    /// Whether the layer list of a document is still on its way (as opposed to unreadable).
    pub(crate) fn layers_loading(&self, doc: DocumentId) -> bool {
        self.jobs.loads.contains_key(&doc)
    }

    fn load_finished(&mut self, doc: DocumentId, why: LoadWhy, result: Result<(), DocError>) {
        self.jobs.finished += 1;
        match (why, result) {
            (LoadWhy::Annots { recovering }, Ok(())) => {
                let recovered = self
                    .session
                    .document(doc)
                    .map_or(0, Document::recovered_changes);
                if recovering {
                    self.chrome.status = Some(format!("Recovered {recovered} unsaved change(s)"));
                }
                if recovering || recovered > 0 {
                    self.after_edit(doc);
                }
            }
            (LoadWhy::Annots { recovering }, Err(e)) => {
                self.jobs.annots_failed.insert(doc);
                self.chrome.status = Some(if recovering {
                    format!("The unsaved markups could not be recovered: {e}")
                } else {
                    format!("The markups could not be read: {e}")
                });
            }
            (LoadWhy::Layers, Ok(())) => {}
            (LoadWhy::Layers, Err(e)) => {
                self.markup.layers_failed.push(doc);
                self.chrome.status = Some(format!("The layers could not be read: {e}"));
            }
        }
        // The Layers page asked while the annotations were being read and they brought no
        // layer list with them: it is read by itself now.
        let wants_layers = self.jobs.layers_asked.contains(&doc)
            && matches!(why, LoadWhy::Annots { .. })
            && self
                .session
                .document(doc)
                .is_ok_and(|d| d.cached_layers().is_none());
        if wants_layers {
            self.ensure_layers(doc);
        }
        self.request_repaint_soon();
    }

    // ---- saving ------------------------------------------------------------------------------

    /// Starts saving the document a view shows, on a worker thread. Returns whether a save
    /// was started (or queued behind a load in flight): `false` when the user cancelled the
    /// file dialog or the save could not begin, with the reason in the status bar.
    pub(crate) fn begin_save(&mut self, id: ViewId, how: SaveHow, after: AfterSave) -> bool {
        self.commit_text_edit();
        self.cancel_gesture();
        let Some(doc_id) = self.views.get(&id).map(|v| v.doc) else {
            return false;
        };
        let Ok(doc) = self.session.document(doc_id) else {
            return false;
        };
        if doc.is_saving() || self.jobs.queued.contains_key(&doc_id) {
            self.chrome.status = Some(format!("{} is being saved already", doc.title()));
            return false;
        }
        let has_file = doc.path().is_some();
        let target = if how == SaveHow::As || !has_file {
            let suggested = doc.title();
            match self.dialogs.save_pdf(&suggested) {
                Some(path) => SaveTarget::As(path),
                None => return false,
            }
        } else if how == SaveHow::Optimised {
            SaveTarget::Optimised
        } else {
            SaveTarget::InPlace
        };
        if self.jobs.loads.contains_key(&doc_id) {
            // The save is of what the load reads: it starts when the load has finished.
            self.jobs.queued.insert(
                doc_id,
                QueuedSave {
                    view: id,
                    target,
                    after,
                },
            );
            self.request_repaint_soon();
            return true;
        }
        self.launch_save(doc_id, id, target, after)
    }

    fn launch_save(
        &mut self,
        doc_id: DocumentId,
        view: ViewId,
        target: SaveTarget,
        after: AfterSave,
    ) -> bool {
        // Queued tiles of the present render document would fail once it is replaced.
        for dv in self.views.values_mut().filter(|v| v.doc == doc_id) {
            self.scheduler.advance(dv.stream);
            dv.requested = None;
        }
        let Ok(doc) = self.session.document(doc_id) else {
            return false;
        };
        let old_render = doc.render_id();
        let title = doc.title();
        // Save As goes through the session, which refuses a file that another tab has open.
        let started = match &target {
            SaveTarget::As(path) => self.session.start_save_as(doc_id, path),
            SaveTarget::Optimised => self
                .session
                .document_mut(doc_id)
                .and_then(Document::start_save_optimised),
            SaveTarget::InPlace => self
                .session
                .document_mut(doc_id)
                .and_then(Document::start_save),
        };
        match started {
            Ok(ticket) => {
                self.jobs.last_save.remove(&doc_id);
                self.jobs.saves.insert(
                    doc_id,
                    SaveJob {
                        ticket,
                        view,
                        old_render,
                        after,
                        title,
                    },
                );
                self.request_repaint_soon();
                true
            }
            Err(e) => {
                self.jobs.last_save.insert(doc_id, false);
                self.chrome.status = Some(format!("Could not save {title}: {e}"));
                self.request_repaint_soon();
                false
            }
        }
    }

    fn save_finished(
        &mut self,
        doc_id: DocumentId,
        job: SaveJob,
        result: Result<MarkupSave, DocError>,
    ) {
        self.jobs.finished += 1;
        let Ok(doc) = self.session.document(doc_id) else {
            return;
        };
        let new_render = doc.render_id();
        let title = doc.title();
        let path = doc.path().map(Path::to_path_buf);
        if new_render != job.old_render {
            self.forget_render(job.old_render);
        }
        self.markup
            .overlay_cache
            .retain(|(doc, _), _| *doc != doc_id);
        for dv in self.views.values_mut().filter(|v| v.doc == doc_id) {
            dv.requested = None;
        }
        let saved = match result {
            Ok(saved) => {
                for dv in self.views.values_mut().filter(|v| v.doc == doc_id) {
                    dv.title = title.clone();
                    dv.snap_mark = None;
                }
                if let (Some(recent), Some(path)) = (&mut self.recent, &path) {
                    recent.touch(path);
                    let _ = recent.save();
                }
                self.chrome.status = Some(if saved.wrote() {
                    format!("Saved {}", display_name(&saved.path))
                } else {
                    "Nothing to save".to_owned()
                });
                true
            }
            Err(e) => {
                self.chrome.status = Some(format!("Could not save {title}: {e}"));
                false
            }
        };
        self.jobs.last_save.insert(doc_id, saved);
        self.request_repaint_soon();
        if let (true, AfterSave::Close { quitting }) = (saved, job.after) {
            self.close_view(job.view);
            if quitting {
                let ctx = self.repaint_context();
                if let Some(ctx) = ctx {
                    self.request_quit(&ctx);
                }
            }
        }
    }

    // ---- taking results in -------------------------------------------------------------------

    /// Once per frame: takes in whatever the workers have finished. Never waits. Returns
    /// whether a load and whether a save is still in flight.
    pub(crate) fn poll_jobs(&mut self) -> (bool, bool) {
        if !self.jobs.pending() {
            return (false, false);
        }
        self.take_jobs(false);
        (
            !self.jobs.loads.is_empty(),
            !self.jobs.saves.is_empty() || !self.jobs.queued.is_empty(),
        )
    }

    /// Waits for every save and load in flight and takes the results in. For tests, shutdown
    /// and the calls that answer at once; never called on a frame.
    pub(crate) fn drain_jobs(&mut self) {
        // A finished load may start a queued save: go round until nothing is left.
        for _ in 0..8 {
            if !self.jobs.pending() {
                break;
            }
            self.take_jobs(true);
        }
    }

    fn take_jobs(&mut self, wait: bool) {
        let loading: Vec<DocumentId> = self.jobs.loads.keys().copied().collect();
        for doc_id in loading {
            let Some(ticket) = self.jobs.loads.get(&doc_id).map(|j| j.ticket) else {
                continue;
            };
            let result = match self.session.document_mut(doc_id) {
                Ok(doc) if wait => Some(doc.finish_load(ticket)),
                Ok(doc) => match doc.try_finish_load(ticket) {
                    Some(result) => Some(result),
                    // Somebody else took the load in (a blocking call waited for it).
                    None if doc.load_in_flight() != Some(ticket) => Some(Ok(())),
                    None => None,
                },
                Err(_) => {
                    self.jobs.loads.remove(&doc_id);
                    continue;
                }
            };
            let Some(result) = result else {
                continue;
            };
            let Some(job) = self.jobs.loads.remove(&doc_id) else {
                continue;
            };
            let result = match result {
                Err(DocError::NoSuchTicket) => Ok(()),
                other => other,
            };
            self.load_finished(doc_id, job.why, result);
            if let Some(queued) = self.jobs.queued.remove(&doc_id) {
                self.launch_save(doc_id, queued.view, queued.target, queued.after);
            }
        }
        // A save queued behind a load that is no longer there.
        let orphans: Vec<DocumentId> = self
            .jobs
            .queued
            .keys()
            .copied()
            .filter(|d| !self.jobs.loads.contains_key(d))
            .collect();
        for doc_id in orphans {
            if let Some(queued) = self.jobs.queued.remove(&doc_id) {
                self.launch_save(doc_id, queued.view, queued.target, queued.after);
            }
        }
        let saving: Vec<DocumentId> = self.jobs.saves.keys().copied().collect();
        for doc_id in saving {
            let Some(ticket) = self.jobs.saves.get(&doc_id).map(|j| j.ticket) else {
                continue;
            };
            let result = match self.session.document_mut(doc_id) {
                Ok(doc) if wait => Some(doc.finish_save(ticket)),
                Ok(doc) => doc.try_finish_save(ticket),
                Err(_) => {
                    self.jobs.saves.remove(&doc_id);
                    continue;
                }
            };
            let Some(result) = result else {
                continue;
            };
            if let Some(job) = self.jobs.saves.remove(&doc_id) {
                self.save_finished(doc_id, job, result);
            }
        }
    }

    /// Saves the document a view shows and waits for the save. Returns whether it is saved
    /// now. The window's own Save commands do not wait (see [`AppState::begin_save`]).
    pub(crate) fn save_view(&mut self, id: ViewId, how: SaveHow) -> bool {
        let Some(doc_id) = self.views.get(&id).map(|v| v.doc) else {
            return false;
        };
        if !self.begin_save(id, how, AfterSave::Stay) {
            return false;
        }
        self.drain_jobs();
        self.jobs.last_save.get(&doc_id).copied().unwrap_or(false)
    }

    /// The file an export of the active document is offered under: its own name with another
    /// extension.
    pub(crate) fn export_name(&self, extension: &str) -> String {
        let title = self
            .active_view()
            .map(|v| v.title.clone())
            .unwrap_or_default();
        let stem = PathBuf::from(&title)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "markups".to_owned());
        format!("{stem}.{extension}")
    }
}
