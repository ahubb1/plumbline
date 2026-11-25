//! Structural edits from the window (milestone M8): the glue between the panels that ask for
//! page operations, bookmark and link edits and flatten, and pl-doc's
//! [`Document::apply_structure`](pl_doc::Document::apply_structure).
//!
//! Every structural edit is one undo step of the document (Edit > Undo takes it back until the
//! document is saved; a save writes the whole file again). The window asks before a step
//! that removes pages, and before any step on a signed document: pl-doc refuses such a step
//! until the user agrees that the signatures will no longer be valid, and this module shows
//! the question and applies the step again with the agreement. After a step the display is a
//! new render document: [`AppState::follow_structure_changes`] moves every view of the
//! document to where its page went and drops what was cached for the old pages.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Id, RichText};
use pl_doc::pl_pdf::SignaturePolicy;
use pl_doc::{DocError, DocumentId, FlattenScope, StructCommand, StructReport, StructureOutcome};

use crate::app::{display_name, AppState, ViewId};
use crate::bookmarks::{BookmarkDrag, BookmarkEdit, OutlineCache};
use crate::jobs::AfterSave;
use crate::links::{LinkCache, LinkDialog, LinkDrag, NavHistory};
use crate::markup::SaveHow;
use crate::pages::{CombineDialog, PageDrag, PageSelection};
use crate::theme;

/// Something waiting for the user to agree that signatures will be invalidated.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Pending {
    /// A structural command.
    Command(StructCommand),
    /// Pages to be written to a new file.
    Extract { pages: Vec<u32>, dest: PathBuf },
    /// Files whose pages are to be inserted at `at`, one after another.
    InsertFiles { at: u32, paths: Vec<PathBuf> },
    /// Files to be combined into a new one.
    Combine {
        parts: Vec<pl_doc::CombinePart>,
        dest: PathBuf,
    },
    /// "Save optimised", which rewrites the whole file.
    SaveOptimised,
}

/// "This document is signed": the fields whose signatures a step would invalidate.
pub(crate) struct SignaturePrompt {
    pub doc: DocumentId,
    pub pending: Pending,
    pub fields: Vec<String>,
}

/// "Delete these pages?": a step that removes something, waiting for a yes.
pub(crate) struct ConfirmPrompt {
    pub doc: DocumentId,
    pub title: String,
    pub message: String,
    /// The wording of the button that goes ahead.
    pub action: String,
    pub command: StructCommand,
}

/// Which markups the flatten dialog burns in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlattenChoice {
    All,
    Selected,
    Pages,
}

/// The flatten dialog.
pub(crate) struct FlattenDialog {
    pub doc: DocumentId,
    pub choice: FlattenChoice,
    /// One-based pages, as typed (`1-3, 5`).
    pub pages: String,
    pub forms: bool,
    pub error: Option<String>,
}

/// Everything the M8 panels and dialogs keep between frames.
#[derive(Default)]
pub(crate) struct StructureUi {
    pub signature: Option<SignaturePrompt>,
    pub confirm: Option<ConfirmPrompt>,
    pub flatten: Option<FlattenDialog>,
    /// A web address a link leads to, waiting for the user's yes before the browser opens.
    pub url_prompt: Option<String>,
    /// A PDF outside the document's folder a link leads to (and the page), waiting for the
    /// user's yes before it is opened.
    pub file_prompt: Option<(std::path::PathBuf, u32)>,
    pub link_dialog: Option<LinkDialog>,
    pub link_drag: Option<LinkDrag>,
    /// The target of the link under the pointer, as words (for the tooltip and the tests).
    pub hovered_link: Option<String>,
    /// A link clicked on a page, followed once the frame's tabs are laid out (following it
    /// may open a tab, which cannot happen while the tabs are being drawn).
    pub clicked_link: Option<(ViewId, pl_doc::LinkInfo)>,
    /// When and where the last click on a page was, to tell a double-click.
    pub last_click: Option<(f64, egui::Pos2)>,
    pub links: HashMap<DocumentId, LinkCache>,
    pub outlines: HashMap<DocumentId, OutlineCache>,
    pub bookmark_selected: Option<(DocumentId, Vec<usize>)>,
    pub bookmark_edit: Option<BookmarkEdit>,
    pub bookmark_drag: Option<BookmarkDrag>,
    pub history: HashMap<ViewId, NavHistory>,
    pub page_selection: HashMap<DocumentId, PageSelection>,
    pub page_drag: Option<PageDrag>,
    pub combine: Option<CombineDialog>,
}

impl StructureUi {
    /// Whether a dialog of this module is open (the shortcuts wait for it).
    pub(crate) fn modal_open(&self) -> bool {
        self.signature.is_some()
            || self.confirm.is_some()
            || self.flatten.is_some()
            || self.url_prompt.is_some()
            || self.file_prompt.is_some()
            || self.link_dialog.is_some()
            || self.combine.is_some()
    }
}

/// What the status bar says after a structural step.
fn describe(outcome: &StructureOutcome) -> String {
    let mut text = format!(
        "Done: {}. Undo takes it back; the file changes when it is saved.",
        outcome.command
    );
    match &outcome.report {
        StructReport::Pages(report) => {
            if !report.links_removed.is_empty() {
                text.push_str(&format!(
                    " {} link{} that led to removed pages went with them.",
                    report.links_removed.len(),
                    if report.links_removed.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ));
            }
            let bookmarks = report.outline_items_removed.len();
            if bookmarks > 0 {
                text.push_str(&format!(
                    " {bookmarks} bookmark{} went with them.",
                    if bookmarks == 1 { "" } else { "s" }
                ));
            }
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            if report.markups_kept > 0 {
                text.push_str(&format!(
                    " {} markup{} moved onto the new pages.",
                    report.markups_kept,
                    plural(report.markups_kept)
                ));
            }
            if report.markups_dropped > 0 {
                text.push_str(&format!(
                    " {} markup{} of the replaced pages removed.",
                    report.markups_dropped,
                    plural(report.markups_dropped)
                ));
            }
            if !report.fields_removed.is_empty() {
                text.push_str(&format!(
                    " {} form field{} removed.",
                    report.fields_removed.len(),
                    plural(report.fields_removed.len())
                ));
            }
            for (from, to) in &report.fields_renamed {
                text.push_str(&format!(" Form field {from} renamed {to}."));
            }
        }
        StructReport::Flatten(report) => {
            text = format!(
                "{} markup{} flattened. Undo takes it back until the document is saved; after \
                 that it is permanent.",
                report.flattened.len(),
                if report.flattened.len() == 1 { "" } else { "s" }
            );
        }
        StructReport::Edit => {}
    }
    if !outcome.settings_dropped.is_empty() {
        text.push_str(&format!(
            " Removed with their pages: {}.",
            outcome.settings_dropped.join(", ")
        ));
    }
    if !outcome.signatures_invalidated.is_empty() {
        text.push_str(" Saving will invalidate the document's signatures.");
    }
    text
}

impl AppState {
    /// The active view's document, to change it from outside the window (a script, a test).
    pub fn active_document_mut(&mut self) -> Option<&mut pl_doc::Document> {
        let doc = self.active_doc_id()?;
        self.session.document_mut(doc).ok()
    }

    /// Every page's fingerprint of the active document as its structure stands (empty when it
    /// cannot be read).
    pub fn page_fingerprints(&mut self) -> Vec<pl_doc::PageFingerprint> {
        self.active_document_mut()
            .and_then(|d| d.page_fingerprints().ok())
            .unwrap_or_default()
    }

    // ---- applying ----------------------------------------------------------------------------

    /// Applies a structural command to the active document as one undo step. A command a
    /// signed document refuses brings up the signature question instead (see
    /// [`AppState::answer_signature_prompt`]). Returns whether the document changed.
    pub fn apply_structure(&mut self, command: StructCommand) -> bool {
        let Some(doc) = self.active_doc_id() else {
            return false;
        };
        self.apply_structure_to(doc, command, SignaturePolicy::Refuse)
    }

    pub(crate) fn apply_structure_to(
        &mut self,
        doc_id: DocumentId,
        command: StructCommand,
        policy: SignaturePolicy,
    ) -> bool {
        self.commit_text_edit();
        self.cancel_gesture();
        // A structural step first writes the unsaved markups into the structure, so the
        // markups must have been read: a read still running on a worker is waited for.
        if self.annots_loading(doc_id) {
            self.drain_jobs();
        }
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return false;
        };
        match doc.apply_structure(command.clone(), policy) {
            Ok(outcome) => {
                self.chrome.status = Some(describe(&outcome));
                self.after_edit(doc_id);
                true
            }
            Err(e) => {
                self.structure_refused(doc_id, Pending::Command(command), e);
                false
            }
        }
    }

    /// A structural step (or an extract) was refused: a signed document brings up the
    /// question, anything else goes to the status bar.
    pub(crate) fn structure_refused(&mut self, doc: DocumentId, pending: Pending, e: DocError) {
        if let Some(fields) = e.signatures_to_confirm() {
            self.structure.signature = Some(SignaturePrompt {
                doc,
                pending,
                fields: fields.to_vec(),
            });
            return;
        }
        self.chrome.status = Some(match e {
            DocError::Saving => {
                "The document is being saved; it can be changed again in a moment.".to_owned()
            }
            DocError::NotPermitted { .. } => {
                format!("Nothing was changed: {e}. The file's security settings forbid it.")
            }
            e => format!("That could not be done: {e}"),
        });
    }

    /// The signature fields the open signature question is about; `None` when no question is
    /// open.
    pub fn signature_prompt(&self) -> Option<&[String]> {
        self.structure
            .signature
            .as_ref()
            .map(|p| p.fields.as_slice())
    }

    /// Answers the signature question: `true` goes ahead (the signatures will be invalid once
    /// the document is saved), `false` drops the step.
    pub fn answer_signature_prompt(&mut self, invalidate: bool) {
        let Some(prompt) = self.structure.signature.take() else {
            return;
        };
        if !invalidate {
            self.chrome.status = Some("Nothing was changed; the signatures are kept.".into());
            return;
        }
        match prompt.pending {
            Pending::Command(command) => {
                self.apply_structure_to(prompt.doc, command, SignaturePolicy::Invalidate);
            }
            Pending::Extract { pages, dest } => {
                self.extract_pages_to(prompt.doc, &pages, dest, SignaturePolicy::Invalidate);
            }
            Pending::InsertFiles { at, paths } => {
                self.insert_files(prompt.doc, at, paths, SignaturePolicy::Invalidate);
            }
            Pending::Combine { parts, dest } => {
                self.start_combine(parts, dest, SignaturePolicy::Invalidate);
            }
            Pending::SaveOptimised => {
                let view = self
                    .views
                    .values()
                    .filter(|v| v.doc == prompt.doc)
                    .map(|v| v.id)
                    .min();
                if let Some(view) = view {
                    self.begin_save(view, SaveHow::Optimised, AfterSave::Stay);
                }
            }
        }
    }

    /// "Save optimised" rewrites the whole file, which invalidates digital signatures: a
    /// signed document asks first.
    pub(crate) fn save_optimised(&mut self) {
        let Some(view) = self.active else {
            return;
        };
        let Some(doc_id) = self.views.get(&view).map(|v| v.doc) else {
            return;
        };
        let fields = self
            .session
            .document_mut(doc_id)
            .ok()
            .filter(|d| !d.is_saving())
            .and_then(|d| d.structure_signatures().ok())
            .unwrap_or_default();
        if fields.is_empty() {
            self.begin_save(view, SaveHow::Optimised, AfterSave::Stay);
        } else {
            self.structure.signature = Some(SignaturePrompt {
                doc: doc_id,
                pending: Pending::SaveOptimised,
                fields,
            });
        }
    }

    /// Asks before a step that removes something; [`AppState::answer_confirm_prompt`] carries
    /// it out.
    pub(crate) fn ask_to_confirm(
        &mut self,
        doc: DocumentId,
        title: String,
        message: String,
        action: &str,
        command: StructCommand,
    ) {
        self.structure.confirm = Some(ConfirmPrompt {
            doc,
            title,
            message,
            action: action.to_owned(),
            command,
        });
    }

    /// The title of the open confirmation, if one is open.
    pub fn confirm_prompt(&self) -> Option<&str> {
        self.structure.confirm.as_ref().map(|c| c.title.as_str())
    }

    /// Answers the open confirmation: `true` carries the step out.
    pub fn answer_confirm_prompt(&mut self, go_ahead: bool) {
        let Some(confirm) = self.structure.confirm.take() else {
            return;
        };
        if go_ahead {
            self.apply_structure_to(confirm.doc, confirm.command, SignaturePolicy::Refuse);
        }
    }

    // ---- following the display -----------------------------------------------------------

    /// Takes in the structural changes of every document since the last frame (a step, its
    /// undo or its redo): each view goes to where its page went, and tiles, thumbnails, text,
    /// links and bookmarks of the old display are dropped.
    pub(crate) fn follow_structure_changes(&mut self) {
        let mut changes = Vec::new();
        for doc in self.session.documents_mut() {
            if let Some(change) = doc.take_structure_change() {
                changes.push((doc.id(), change));
            }
        }
        for (doc_id, change) in changes {
            self.forget_render(change.old_render);
            self.markup
                .overlay_cache
                .retain(|(doc, _), _| *doc != doc_id);
            self.structure.links.remove(&doc_id);
            self.structure.outlines.remove(&doc_id);
            if self
                .structure
                .bookmark_edit
                .as_ref()
                .is_some_and(|e| e.doc == doc_id)
            {
                self.structure.bookmark_edit = None;
            }
            self.structure.bookmark_drag = None;
            self.structure.page_drag = None;
            if let Some(selection) = self.structure.page_selection.get_mut(&doc_id) {
                selection.remap(&change.page_map);
            }
            self.follow_set_pages(doc_id, &change.page_map);
            let Ok(doc) = self.session.document(doc_id) else {
                continue;
            };
            let count = doc.page_count();
            let ids: Vec<ViewId> = self
                .views
                .values()
                .filter(|v| v.doc == doc_id)
                .map(|v| v.id)
                .collect();
            for id in ids {
                if let Some(history) = self.structure.history.get_mut(&id) {
                    history.remap(&change.page_map);
                }
                let Some(dv) = self.views.get_mut(&id) else {
                    continue;
                };
                self.scheduler.advance(dv.stream);
                let page = dv.shown_page();
                let target = change
                    .page_map
                    .map(page)
                    .unwrap_or_else(|| page.min(count.saturating_sub(1)));
                dv.requested = None;
                dv.selection = None;
                dv.pending_text = None;
                dv.search = None;
                dv.snap_mark = None;
                dv.drag_anchor = None;
                dv.pending_nav = None;
                dv.reset_page_list(doc);
                dv.go_to_page(target);
            }
        }
    }

    // ---- flatten -----------------------------------------------------------------------------

    /// Shows the flatten dialog for the active document.
    pub fn open_flatten_dialog(&mut self) {
        let Some(doc) = self.active_doc_id() else {
            return;
        };
        let choice = if self.selected_count() > 0 {
            FlattenChoice::Selected
        } else {
            FlattenChoice::All
        };
        self.structure.flatten = Some(FlattenDialog {
            doc,
            choice,
            pages: String::new(),
            forms: false,
            error: None,
        });
    }

    pub fn flatten_dialog_open(&self) -> bool {
        self.structure.flatten.is_some()
    }

    /// Carries out the flatten dialog's choice. Returns whether the document changed.
    pub(crate) fn run_flatten_dialog(&mut self) -> bool {
        let Some(dialog) = self.structure.flatten.as_ref() else {
            return false;
        };
        let doc_id = dialog.doc;
        let scope = match dialog.choice {
            FlattenChoice::All => FlattenScope::All,
            FlattenChoice::Selected => {
                let ids: Vec<String> = self
                    .session
                    .document(doc_id)
                    .map(|d| {
                        d.selection()
                            .iter()
                            .map(|id| id.as_str().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                if ids.is_empty() {
                    if let Some(d) = self.structure.flatten.as_mut() {
                        d.error = Some("No markups are selected.".into());
                    }
                    return false;
                }
                FlattenScope::Annotations(ids)
            }
            FlattenChoice::Pages => {
                let count = self
                    .session
                    .document(doc_id)
                    .map(|d| d.page_count())
                    .unwrap_or(0);
                match pl_doc::pl_render::parse_page_range(&dialog.pages, count) {
                    Ok(mut pages) => {
                        pages.sort_unstable();
                        pages.dedup();
                        FlattenScope::Pages(pages)
                    }
                    Err(e) => {
                        let message = e.to_string();
                        if let Some(d) = self.structure.flatten.as_mut() {
                            d.error = Some(message);
                        }
                        return false;
                    }
                }
            }
        };
        let forms = dialog.forms;
        self.structure.flatten = None;
        self.apply_structure_to(
            doc_id,
            StructCommand::Flatten {
                scope,
                print: false,
                include_form_fields: forms,
            },
            SignaturePolicy::Refuse,
        )
    }

    /// Writes pages of a document to a new file (an extract); a signed document asks first.
    pub(crate) fn extract_pages_to(
        &mut self,
        doc_id: DocumentId,
        pages: &[u32],
        dest: PathBuf,
        policy: SignaturePolicy,
    ) -> bool {
        if self.annots_loading(doc_id) {
            self.drain_jobs();
        }
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return false;
        };
        match doc.extract_to(pages, &dest, policy) {
            Ok(_) => {
                self.chrome.status = Some(format!(
                    "{} page{} written to {}.",
                    pages.len(),
                    if pages.len() == 1 { "" } else { "s" },
                    display_name(&dest)
                ));
                true
            }
            Err(e) => {
                self.structure_refused(
                    doc_id,
                    Pending::Extract {
                        pages: pages.to_vec(),
                        dest,
                    },
                    e,
                );
                false
            }
        }
    }
}

// ---- dialogs ----------------------------------------------------------------------------------

/// The questions and dialogs of this module, drawn over the window when they are open.
pub fn dialogs(app: &mut AppState, ctx: &egui::Context) {
    signature_prompt(app, ctx);
    confirm_prompt(app, ctx);
    flatten_dialog(app, ctx);
    crate::links::url_prompt(app, ctx);
    crate::links::file_prompt(app, ctx);
    crate::links::link_dialog(app, ctx);
    crate::pages::combine_dialog(app, ctx);
}

fn signature_prompt(app: &mut AppState, ctx: &egui::Context) {
    let Some(prompt) = app.structure.signature.as_ref() else {
        return;
    };
    let fields = prompt.fields.clone();
    let saving = prompt.pending == Pending::SaveOptimised;
    let mut answer = None;
    let modal = egui::Modal::new(Id::unique("plumbline-signature-prompt")).show(ctx, |ui| {
        ui.set_width(400.0);
        ui.label(RichText::new("This document is signed").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label(if saving {
            "Save optimised rewrites the whole file, and the signatures below will no longer \
             be valid. Save (Ctrl+S) appends to the file instead and keeps them."
        } else {
            "This change rewrites the whole file when it is saved, and the signatures below \
             will no longer be valid. The change can be undone until the document is saved."
        });
        ui.add_space(4.0);
        for field in &fields {
            ui.label(format!("\u{2022} {field}"));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button("Change it anyway")
                .on_hover_text("Make the change; saving will invalidate the signatures")
                .clicked()
            {
                answer = Some(true);
            }
            if ui
                .button("Cancel")
                .on_hover_text("Leave the document and its signatures as they are")
                .clicked()
            {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    if let Some(answer) = answer {
        app.answer_signature_prompt(answer);
    }
}

fn confirm_prompt(app: &mut AppState, ctx: &egui::Context) {
    let Some(confirm) = app.structure.confirm.as_ref() else {
        return;
    };
    let (title, message, action) = (
        confirm.title.clone(),
        confirm.message.clone(),
        confirm.action.clone(),
    );
    let mut answer = None;
    let modal = egui::Modal::new(Id::unique("plumbline-confirm-prompt")).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.label(RichText::new(&title).font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label(&message);
        if let Some(StructCommand::ReplacePages { keep_markups, .. }) =
            app.structure.confirm.as_mut().map(|c| &mut c.command)
        {
            ui.add_space(4.0);
            ui.checkbox(keep_markups, "Keep the markups on the new pages")
                .on_hover_text("Move the replaced pages' markups onto the pages that replace them");
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(&action).on_hover_text(&title).clicked() {
                answer = Some(true);
            }
            if ui
                .button("Cancel")
                .on_hover_text("Change nothing")
                .clicked()
            {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    if let Some(answer) = answer {
        app.answer_confirm_prompt(answer);
    }
}

fn flatten_dialog(app: &mut AppState, ctx: &egui::Context) {
    if app.structure.flatten.is_none() {
        return;
    }
    let selected = app.selected_count();
    let mut go = false;
    let mut cancel = false;
    let modal = egui::Modal::new(Id::unique("plumbline-flatten")).show(ctx, |ui| {
        let Some(dialog) = app.structure.flatten.as_mut() else {
            return;
        };
        ui.set_width(380.0);
        ui.label(RichText::new("Flatten markups").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label(
            "Flattening draws the markups into the page itself: they look the same but can no \
             longer be selected, edited or listed.",
        );
        ui.add_space(6.0);
        ui.radio_value(&mut dialog.choice, FlattenChoice::All, "All markups")
            .on_hover_text("Every markup on every page");
        ui.add_enabled_ui(selected > 0, |ui| {
            ui.radio_value(
                &mut dialog.choice,
                FlattenChoice::Selected,
                format!("Selected markups ({selected})"),
            )
            .on_hover_text("Only the markups selected now");
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut dialog.choice, FlattenChoice::Pages, "Pages")
                .on_hover_text("Every markup on the pages given");
            let field = ui
                .add_enabled(
                    dialog.choice == FlattenChoice::Pages,
                    egui::TextEdit::singleline(&mut dialog.pages)
                        .desired_width(140.0)
                        .hint_text("1-3, 5"),
                )
                .accessible_name("Pages to flatten")
                .on_hover_text("Page numbers and ranges, such as 1-3, 5");
            if field.changed() {
                dialog.error = None;
            }
        });
        ui.checkbox(&mut dialog.forms, "Also flatten form fields")
            .on_hover_text("Draw the form fields' values into the page too");
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Once the document is saved this cannot be undone. Until then, Undo takes it \
                 back.",
            )
            .strong(),
        );
        if let Some(error) = &dialog.error {
            ui.label(error);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            go = ui
                .button("Flatten")
                .on_hover_text("Flatten the chosen markups")
                .clicked();
            cancel = ui
                .button("Cancel")
                .on_hover_text("Close without flattening")
                .clicked();
        });
    });
    if cancel || (!go && modal.should_close()) {
        app.structure.flatten = None;
    } else if go {
        app.run_flatten_dialog();
    }
}
