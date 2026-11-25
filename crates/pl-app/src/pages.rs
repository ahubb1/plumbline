//! The Pages page of the side panel: thumbnails, and the page operations on them (insert,
//! delete, rotate, extract, replace, move by dragging), and the "Combine files" dialog.
//!
//! Pages are chosen in the thumbnails: a click picks one page (and shows it), Ctrl+click adds
//! or removes a page, Shift+click picks a run. With one page picked the operations act on the
//! page in view. Every operation is one undo step (see [`crate::structure`]); deleting asks
//! first.

use std::collections::BTreeSet;
use std::path::PathBuf;

use egui::{Button, Id, RichText, Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo};
use pl_doc::pl_pdf::SignaturePolicy;
use pl_doc::pl_render::DocSource;
use pl_doc::{CombinePart, DocumentId, PageMap, PageSource, StructCommand};

use crate::app::{display_name, AppState};
use crate::canvas;
use crate::commands::Command;
use crate::icons::{icon_button, icon_button_tip, Icon};
use crate::theme::{self, Palette};

const THUMB_HEIGHT: f32 = 132.0;
const THUMB_ROW: f32 = THUMB_HEIGHT + 26.0;

/// The pages picked in a document's thumbnails.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PageSelection {
    pub pages: BTreeSet<u32>,
    /// Where a Shift+click run starts.
    pub anchor: Option<u32>,
}

impl PageSelection {
    /// Follows a structural change: picked pages go where their pages went.
    pub(crate) fn remap(&mut self, map: &PageMap) {
        self.pages = self.pages.iter().filter_map(|p| map.map(*p)).collect();
        self.anchor = self.anchor.and_then(|p| map.map(p));
    }
}

/// Thumbnails being dragged to a new place.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PageDrag {
    pub doc: DocumentId,
    /// The pages being moved, in order.
    pub pages: Vec<u32>,
    /// The gap they would be dropped into: before page `gap` (the page count for the end).
    pub gap: Option<u32>,
}

/// One file of the "Combine files" dialog.
pub(crate) struct CombineRow {
    pub path: PathBuf,
    pub page_count: u32,
    /// One-based pages, as typed; empty for every page.
    pub range: String,
}

/// The "Combine files" dialog: files and their pages, in the order they are combined.
#[derive(Default)]
pub(crate) struct CombineDialog {
    pub rows: Vec<CombineRow>,
    pub error: Option<String>,
    /// The combination being written on a worker thread; the dialog waits for it.
    pub running: Option<RunningCombine>,
}

/// A combination being written on a worker thread.
pub(crate) struct RunningCombine {
    pub rx: std::sync::mpsc::Receiver<Result<(), pl_doc::DocError>>,
    pub parts: Vec<CombinePart>,
    pub dest: std::path::PathBuf,
}

/// The gap a drop at height `y` over the thumbnail cells goes into: before the first page
/// whose middle is below it, or after the last cell shown.
pub(crate) fn drop_gap(cells: &[(u32, egui::Rect)], y: f32) -> Option<u32> {
    cells
        .iter()
        .find(|(_, r)| y < r.center().y)
        .map(|(p, _)| *p)
        .or_else(|| cells.last().map(|(p, _)| p + 1))
}

/// Where pages moved into gap `gap` (before page `gap` of the document as it is) land: the
/// `to` of [`StructCommand::MovePages`]. `None` when the move changes nothing.
pub(crate) fn move_target(pages: &[u32], gap: u32) -> Option<u32> {
    let before = pages.iter().filter(|p| **p < gap).count() as u32;
    let to = gap - before;
    // Already there: a contiguous run whose first page is `to`.
    let contiguous = pages.windows(2).all(|w| w[1] == w[0] + 1);
    if contiguous && pages.first() == Some(&to) {
        return None;
    }
    Some(to)
}

impl AppState {
    // ---- which pages -------------------------------------------------------------------------

    /// The pages picked in the active document's thumbnails, zero-based in order. With fewer
    /// than two picked this is the page in view.
    pub fn page_targets(&self) -> Vec<u32> {
        let Some(dv) = self.active_view() else {
            return Vec::new();
        };
        match self.structure.page_selection.get(&dv.doc) {
            Some(sel) if sel.pages.len() > 1 => sel.pages.iter().copied().collect(),
            _ => vec![dv.shown_page()],
        }
    }

    /// Picks pages of the active document, as Ctrl+clicks in the thumbnails do.
    pub fn select_pages(&mut self, pages: &[u32]) {
        let Some(doc) = self.active_doc_id() else {
            return;
        };
        let sel = self.structure.page_selection.entry(doc).or_default();
        sel.pages = pages.iter().copied().collect();
        sel.anchor = pages.first().copied();
    }

    fn page_click(&mut self, doc: DocumentId, page: u32, modifiers: egui::Modifiers) {
        let shown = self.active_view().map(|v| v.shown_page()).unwrap_or(0);
        let sel = self.structure.page_selection.entry(doc).or_default();
        if modifiers.command || modifiers.ctrl {
            if sel.pages.len() <= 1 {
                sel.pages = [shown].into_iter().collect();
            }
            if !sel.pages.remove(&page) {
                sel.pages.insert(page);
            }
            sel.anchor = Some(page);
            return;
        }
        if modifiers.shift {
            let anchor = sel.anchor.unwrap_or(shown);
            sel.pages = (anchor.min(page)..=anchor.max(page)).collect();
            return;
        }
        sel.pages = [page].into_iter().collect();
        sel.anchor = Some(page);
        if let Some(dv) = self.active_view_mut() {
            dv.go_to_page(page);
        }
    }

    // ---- the operations ------------------------------------------------------------------

    /// Inserts a blank page (the size of its neighbour) after the picked pages.
    pub fn insert_blank_page(&mut self) -> bool {
        let Some(at) = self.page_targets().last().map(|p| p + 1) else {
            return false;
        };
        self.apply_structure(StructCommand::InsertBlank {
            at,
            count: 1,
            media_box: None,
            rotate: 0,
        })
    }

    /// Asks for PDF files and inserts all their pages after the picked pages, one undo step per
    /// file.
    pub fn insert_pages_from_file(&mut self) -> bool {
        let Some(at) = self.page_targets().last().map(|p| p + 1) else {
            return false;
        };
        let Some(doc) = self.active_doc_id() else {
            return false;
        };
        let paths = self.dialogs.pick_pdfs();
        self.insert_files(doc, at, paths, SignaturePolicy::Refuse)
    }

    /// Inserts every page of each file at `at`, one after another, one undo step per file. A
    /// signed document refuses the first and asks once; the answer covers all of them.
    pub(crate) fn insert_files(
        &mut self,
        doc: DocumentId,
        mut at: u32,
        paths: Vec<std::path::PathBuf>,
        policy: SignaturePolicy,
    ) -> bool {
        let mut changed = false;
        let mut rest = paths.into_iter();
        while let Some(path) = rest.next() {
            let before = self.page_count_of(doc);
            let command = StructCommand::InsertDocument {
                at,
                source: PageSource::File(path.clone()),
                import_outline: false,
            };
            if !self.apply_structure_to(doc, command, policy) {
                // A signature question holds this file only: make it hold the rest too.
                if let Some(prompt) = self.structure.signature.as_mut() {
                    if prompt.doc == doc
                        && matches!(prompt.pending, crate::structure::Pending::Command(_))
                    {
                        let mut paths = vec![path];
                        paths.extend(rest);
                        prompt.pending = crate::structure::Pending::InsertFiles { at, paths };
                    }
                }
                break;
            }
            changed = true;
            at += self.page_count_of(doc).saturating_sub(before);
        }
        changed
    }

    fn page_count_of(&self, doc: DocumentId) -> u32 {
        self.session.document(doc).map_or(0, |d| d.page_count())
    }

    /// Asks, then deletes the picked pages.
    pub fn delete_pages(&mut self) {
        let Some(doc) = self.active_doc_id() else {
            return;
        };
        let pages = self.page_targets();
        if pages.is_empty() {
            return;
        }
        if pages.len() as u32 >= self.page_count_of(doc) {
            self.chrome.status = Some("A document must keep at least one page.".into());
            return;
        }
        let list = page_list(&pages);
        let title = if pages.len() == 1 {
            format!("Delete page {list}?")
        } else {
            format!("Delete {} pages?", pages.len())
        };
        let message = format!(
            "Page{} {list} will be removed, with {} markups and the links and bookmarks that \
             lead there. Undo brings {} back until the document is saved.",
            if pages.len() == 1 { "" } else { "s" },
            if pages.len() == 1 { "its" } else { "their" },
            if pages.len() == 1 { "it" } else { "them" },
        );
        self.ask_to_confirm(
            doc,
            title,
            message,
            "Delete",
            StructCommand::DeletePages { pages },
        );
    }

    /// Turns the picked pages in the file: clockwise for a positive number of degrees.
    pub fn rotate_pages(&mut self, degrees: i32) -> bool {
        let pages = self.page_targets();
        if pages.is_empty() {
            return false;
        }
        self.apply_structure(StructCommand::RotatePages { pages, degrees })
    }

    /// Moves pages so the first of them lands at `to` (zero-based, in the result).
    pub fn move_pages(&mut self, pages: Vec<u32>, to: u32) -> bool {
        self.apply_structure(StructCommand::MovePages { pages, to })
    }

    /// Asks for a file name and writes the picked pages to it.
    pub fn extract_pages(&mut self) -> bool {
        let Some(doc) = self.active_doc_id() else {
            return false;
        };
        let pages = self.page_targets();
        let title = self
            .session
            .document(doc)
            .map(|d| d.title())
            .unwrap_or_default();
        let stem = title.trim_end_matches(".pdf").trim_end_matches(".PDF");
        let suggested = format!(
            "{stem} - pages {}.pdf",
            page_list(&pages).replace(", ", ",")
        );
        let Some(dest) = self.dialogs.save_pdf(&suggested) else {
            return false;
        };
        self.extract_pages_to(doc, &pages, dest, SignaturePolicy::Refuse)
    }

    /// Asks for a PDF and replaces the picked pages (they must follow one another) with its
    /// first pages.
    pub fn replace_pages(&mut self) -> bool {
        let Some(doc) = self.active_doc_id() else {
            return false;
        };
        let pages = self.page_targets();
        if pages.windows(2).any(|w| w[1] != w[0] + 1) {
            self.chrome.status =
                Some("Pages can be replaced only as a run: pick pages next to each other.".into());
            return false;
        }
        let Some(path) = self.dialogs.pick_pdfs().into_iter().next() else {
            return false;
        };
        let source_pages = match self.file_page_count(&path) {
            Ok(n) => n,
            Err(e) => {
                self.chrome.status =
                    Some(format!("{} could not be read: {e}", display_name(&path)));
                return false;
            }
        };
        let n = (pages.len() as u32).min(source_pages);
        if n == 0 {
            self.chrome.status = Some(format!("{} has no pages.", display_name(&path)));
            return false;
        }
        let replaced = &pages[..n as usize];
        let markups = self.markups_on(doc, replaced);
        let list = page_list(replaced);
        let name = display_name(&path);
        let mut message = format!(
            "Page{} {list} will be replaced by the first {n} page{} of {name}.",
            if n == 1 { "" } else { "s" },
            if n == 1 { "" } else { "s" },
        );
        if (n as usize) < pages.len() {
            message.push_str(&format!(
                " {name} has only {source_pages} page{}, so page{} {} will stay as they are.",
                if source_pages == 1 { "" } else { "s" },
                if pages.len() - n as usize == 1 {
                    ""
                } else {
                    "s"
                },
                page_list(&pages[n as usize..]),
            ));
        }
        if markups > 0 {
            message.push_str(&format!(
                " The replaced pages have {markups} markup{}: kept, they move onto the new \
                 pages; not kept, they are removed with the old pages.",
                if markups == 1 { "" } else { "s" },
            ));
        }
        message.push_str(" Undo takes it back until the document is saved.");
        self.ask_to_confirm(
            doc,
            format!("Replace page{} {list}?", if n == 1 { "" } else { "s" }),
            message,
            "Replace",
            StructCommand::ReplacePages {
                at: pages[0],
                source: PageSource::File(path),
                pages: (0..n).collect(),
                keep_markups: true,
            },
        );
        true
    }

    /// How many markups (links and form fields aside) are on `pages` of a document.
    pub(crate) fn markups_on(&mut self, doc: DocumentId, pages: &[u32]) -> usize {
        let Ok(d) = self.session.document_mut(doc) else {
            return 0;
        };
        let Ok(editor) = d.annots() else {
            return 0;
        };
        editor
            .set()
            .iter()
            .filter(|a| pages.contains(&(a.page as u32)))
            .filter(|a| match &a.kind {
                pl_doc::pl_annot::AnnotKind::Link | pl_doc::pl_annot::AnnotKind::Popup => false,
                pl_doc::pl_annot::AnnotKind::Unknown(name) => name != "Widget",
                _ => true,
            })
            .count()
    }

    /// How many pages a PDF file has (read through the render backend).
    pub(crate) fn file_page_count(&self, path: &std::path::Path) -> Result<u32, String> {
        let backend = self.session.backend().clone();
        let info = backend
            .open(DocSource::file(path), None)
            .map_err(|e| e.to_string())?;
        let _ = backend.close(info.doc);
        Ok(info.page_count)
    }

    // ---- combine ------------------------------------------------------------------------------

    /// Shows the "Combine files" dialog, starting with the active document's file.
    pub fn open_combine_dialog(&mut self) {
        let mut dialog = CombineDialog::default();
        if let Some(path) = self
            .active_document()
            .and_then(|d| d.path().map(|p| p.to_path_buf()))
        {
            let page_count = self.file_page_count(&path).unwrap_or(0);
            dialog.rows.push(CombineRow {
                path,
                page_count,
                range: String::new(),
            });
        }
        self.structure.combine = Some(dialog);
    }

    pub fn combine_dialog_open(&self) -> bool {
        self.structure.combine.is_some()
    }

    fn combine_add_files(&mut self) {
        let paths = self.dialogs.pick_pdfs();
        for path in paths {
            match self.file_page_count(&path) {
                Ok(page_count) => {
                    if let Some(dialog) = self.structure.combine.as_mut() {
                        dialog.rows.push(CombineRow {
                            path,
                            page_count,
                            range: String::new(),
                        });
                        dialog.error = None;
                    }
                }
                Err(e) => {
                    if let Some(dialog) = self.structure.combine.as_mut() {
                        dialog.error =
                            Some(format!("{} could not be read: {e}", display_name(&path)));
                    }
                }
            }
        }
    }

    /// Asks where to write the combination, writes it and opens it. Returns whether it was
    /// written.
    pub(crate) fn run_combine(&mut self) -> bool {
        let Some(dialog) = self.structure.combine.as_ref() else {
            return false;
        };
        let mut parts = Vec::new();
        for row in &dialog.rows {
            let pages = if row.range.trim().is_empty() {
                None
            } else {
                match pl_doc::pl_render::parse_page_range(&row.range, row.page_count) {
                    Ok(pages) => Some(pages),
                    Err(e) => {
                        let message = format!("{}: {e}", display_name(&row.path));
                        if let Some(d) = self.structure.combine.as_mut() {
                            d.error = Some(message);
                        }
                        return false;
                    }
                }
            };
            parts.push((row.path.clone(), pages));
        }
        let parts: Vec<CombinePart> = parts
            .into_iter()
            .map(|(path, pages)| self.combine_part(&path, pages))
            .collect();
        if parts.is_empty() {
            if let Some(d) = self.structure.combine.as_mut() {
                d.error = Some("Add the files to combine first.".into());
            }
            return false;
        }
        let Some(dest) = self.dialogs.save_pdf("Combined.pdf") else {
            return false;
        };
        self.start_combine(parts, dest, SignaturePolicy::Refuse)
    }

    /// A part of a combination for `path`: the file, or, for a document open here with
    /// unsaved changes, the document as it stands (its unsaved markups and page edits in).
    fn combine_part(&mut self, path: &std::path::Path, pages: Option<Vec<u32>>) -> CombinePart {
        let mut part = CombinePart::file(path, pages);
        let open = self
            .session
            .documents()
            .find(|d| {
                d.path()
                    .is_some_and(|p| pl_doc::journal::same_path(p, path))
            })
            .map(|d| d.id());
        if let Some(doc) = open.and_then(|id| self.session.document_mut(id).ok()) {
            part.password = doc.open_password().map(str::to_owned);
            if let Ok(pl_doc::PrintSource::Bytes(bytes)) = doc.print_source() {
                part.bytes = Some(std::sync::Arc::new(bytes));
            }
        }
        part
    }

    /// Writes a combination on a worker thread; [`AppState::poll_combine`] takes the result
    /// in. Returns whether it started.
    pub(crate) fn start_combine(
        &mut self,
        parts: Vec<CombinePart>,
        dest: std::path::PathBuf,
        policy: SignaturePolicy,
    ) -> bool {
        let dialog = self
            .structure
            .combine
            .get_or_insert_with(CombineDialog::default);
        if dialog.running.is_some() {
            return false;
        }
        dialog.error = None;
        let backend = self.session.backend().clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = self.repaint_context();
        let (work_parts, work_dest) = (parts.clone(), dest.clone());
        let spawned = std::thread::Builder::new()
            .name("plumbline-combine".into())
            .spawn(move || {
                let result =
                    pl_doc::combine_files(&backend, &work_parts, &work_dest, policy).map(|_| ());
                let _ = tx.send(result);
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        let Some(dialog) = self.structure.combine.as_mut() else {
            return false;
        };
        match spawned {
            Ok(_) => {
                dialog.running = Some(RunningCombine { rx, parts, dest });
                true
            }
            Err(e) => {
                dialog.error = Some(format!("The files could not be combined: {e}"));
                false
            }
        }
    }

    /// Whether a combination is being written.
    pub fn combine_running(&self) -> bool {
        self.structure
            .combine
            .as_ref()
            .is_some_and(|d| d.running.is_some())
    }

    /// Takes in a finished combination: opens it, or says why it was not made (a signed
    /// part brings up the signature question). With `wait`, waits for it. Returns whether one
    /// is still running.
    pub(crate) fn poll_combine(&mut self, wait: bool) -> bool {
        let Some(running) = self
            .structure
            .combine
            .as_ref()
            .and_then(|d| d.running.as_ref())
        else {
            return false;
        };
        let stopped = || pl_doc::DocError::Worker("the combination stopped".into());
        let result = if wait {
            running.rx.recv().unwrap_or_else(|_| Err(stopped()))
        } else {
            match running.rx.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(stopped()),
            }
        };
        let Some(running) = self
            .structure
            .combine
            .as_mut()
            .and_then(|d| d.running.take())
        else {
            return false;
        };
        match result {
            Ok(()) => {
                self.structure.combine = None;
                self.open_path(&running.dest);
                self.chrome.status = Some(format!(
                    "{} files combined into {}.",
                    running.parts.len(),
                    display_name(&running.dest)
                ));
            }
            Err(e) if e.signatures_to_confirm().is_some() => {
                // The question is about files, not a document: the id is not used.
                let doc = self.active_doc_id().unwrap_or(DocumentId(u32::MAX));
                self.structure_refused(
                    doc,
                    crate::structure::Pending::Combine {
                        parts: running.parts,
                        dest: running.dest,
                    },
                    e,
                );
            }
            Err(e) => {
                if let Some(d) = self.structure.combine.as_mut() {
                    d.error = Some(format!("The files could not be combined: {e}"));
                }
            }
        }
        false
    }
}

/// One-based page numbers for messages: `3`, `2, 5, 7`, `4-9`.
pub(crate) fn page_list(pages: &[u32]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let start = pages[i];
        let mut end = start;
        while i + 1 < pages.len() && pages[i + 1] == end + 1 {
            i += 1;
            end = pages[i];
        }
        if end > start + 1 {
            parts.push(format!("{}-{}", start + 1, end + 1));
        } else if end == start + 1 {
            parts.push(format!("{}, {}", start + 1, end + 1));
        } else {
            parts.push(format!("{}", start + 1));
        }
        i += 1;
    }
    parts.join(", ")
}

// ---- the panel ----------------------------------------------------------------------------------

/// The page operations, as menu entries (the context menu of a thumbnail and the panel's menu).
fn operations_menu(ui: &mut Ui, out: &mut Vec<Command>, several: bool) {
    let pages = if several { "pages" } else { "page" };
    let mut item = |ui: &mut Ui, text: &str, tip: &str, command: Command| {
        if ui.button(text).on_hover_text(tip).clicked() {
            out.push(command);
            ui.close();
        }
    };
    item(
        ui,
        "Insert blank page after",
        "Insert a blank page the size of its neighbour",
        Command::InsertBlankPage,
    );
    item(
        ui,
        "Insert pages from file\u{2026}",
        "Insert every page of another PDF after this",
        Command::InsertPagesFromFile,
    );
    item(
        ui,
        &format!("Replace {pages} from file\u{2026}"),
        "Put the first pages of another PDF in their place",
        Command::ReplacePages,
    );
    item(
        ui,
        &format!("Extract {pages} to file\u{2026}"),
        "Write a copy of them to a new PDF",
        Command::ExtractPages,
    );
    ui.separator();
    item(
        ui,
        &format!("Rotate {pages} clockwise"),
        "Turn them in the file, a quarter clockwise",
        Command::RotatePages(90),
    );
    item(
        ui,
        &format!("Rotate {pages} anticlockwise"),
        "Turn them in the file, a quarter anticlockwise",
        Command::RotatePages(-90),
    );
    ui.separator();
    item(
        ui,
        &format!("Delete {pages}\u{2026}"),
        "Remove them from the document (asks first)",
        Command::DeletePages,
    );
    ui.separator();
    item(
        ui,
        "Combine files\u{2026}",
        "Make a new PDF from pages of several files",
        Command::CombineFiles,
    );
}

/// The thumbnails with their page operations.
pub fn thumbnails(app: &mut AppState, ui: &mut Ui, out: &mut Vec<Command>) {
    let Some(id) = app.active else {
        return;
    };
    let Some(dv) = app.views.get(&id) else {
        return;
    };
    let doc_id = dv.doc;
    let Ok((render_doc, saving)) = app
        .session
        .document(doc_id)
        .map(|d| (d.render_id(), d.is_saving()))
    else {
        return;
    };
    let count = dv.view.page_count() as usize;
    let current = dv.shown_page();
    let quarter_turns = dv.view.rotation().quarter_turns();
    let picked: BTreeSet<u32> = app
        .structure
        .page_selection
        .get(&doc_id)
        .filter(|s| s.pages.len() > 1)
        .map(|s| s.pages.clone())
        .unwrap_or_default();
    let several = !picked.is_empty();
    let palette = Palette::of(ui.visuals());

    // ---- the operations bar --------------------------------------------------------------------
    ui.add_enabled_ui(!saving, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let what = if several {
                "the picked pages"
            } else {
                "this page"
            };
            if icon_button_tip(
                ui,
                Icon::Plus,
                "Insert blank page",
                "Insert a blank page after this one",
                false,
            )
            .clicked()
            {
                out.push(Command::InsertBlankPage);
            }
            if icon_button_tip(
                ui,
                Icon::TurnLeft,
                "Rotate pages anticlockwise",
                &format!("Turn {what} anticlockwise in the file"),
                false,
            )
            .clicked()
            {
                out.push(Command::RotatePages(-90));
            }
            if icon_button_tip(
                ui,
                Icon::TurnRight,
                "Rotate pages clockwise",
                &format!("Turn {what} clockwise in the file"),
                false,
            )
            .clicked()
            {
                out.push(Command::RotatePages(90));
            }
            if icon_button_tip(
                ui,
                Icon::Cross,
                "Delete pages",
                &format!("Delete {what} (asks first)"),
                false,
            )
            .clicked()
            {
                out.push(Command::DeletePages);
            }
            let menu = ui.menu_button("More", |ui| operations_menu(ui, out, several));
            menu.response
                .on_hover_text("Insert, replace or extract pages, or combine files");
        });
    });
    if several {
        ui.label(
            RichText::new(format!("{} pages picked", picked.len()))
                .color(palette.muted_text)
                .small(),
        );
    }
    ui.separator();

    // ---- the thumbnails ------------------------------------------------------------------------
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let released = ui.input(|i| i.pointer.any_released());
    // The keys held with the click itself (the release that made it a click).
    let modifiers = ui.input(|i| {
        i.events
            .iter()
            .rev()
            .find_map(|e| match e {
                egui::Event::PointerButton {
                    pressed: false,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(i.modifiers)
    });
    let mut click: Option<u32> = None;
    let mut context: Option<u32> = None;
    let mut drag_start: Option<u32> = None;
    let mut cells: Vec<(u32, egui::Rect)> = Vec::new();
    ui.spacing_mut().item_spacing.y = 0.0;
    let dragging = app
        .structure
        .page_drag
        .as_ref()
        .is_some_and(|d| d.doc == doc_id);
    let scroll = egui::ScrollArea::vertical()
        .id_salt("plumbline-thumbnails")
        .auto_shrink([false, false])
        .show_rows(ui, THUMB_ROW, count, |ui, rows| {
            for row in rows {
                let page = row as u32;
                let (cell, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), THUMB_ROW),
                    Sense::click_and_drag(),
                );
                cells.push((page, cell));
                let is_current = page == current;
                let is_picked = picked.contains(&page);
                response.widget_info(|| {
                    WidgetInfo::selected(
                        Role::Button,
                        true,
                        is_current || is_picked,
                        format!("Page {} thumbnail", page + 1),
                    )
                });
                let response = response.on_hover_text(
                    "Click to show this page; Ctrl or Shift adds pages. Drag to move pages. \
                     Right-click for the page operations.",
                );
                if response.clicked() {
                    click = Some(page);
                }
                if response.secondary_clicked() {
                    context = Some(page);
                }
                if response.drag_started_by(egui::PointerButton::Primary) && !saving {
                    drag_start = Some(page);
                }
                response.context_menu(|ui| operations_menu(ui, out, several || is_picked));
                if !ui.is_rect_visible(cell) {
                    continue;
                }
                if !saving {
                    app.textures.request_thumb(&app.scheduler, render_doc, page);
                }
                let dv = app.views.get(&id).expect("checked above");
                let aspect = dv
                    .view
                    .page_geometry(page)
                    .map(|g| g.display_size_points())
                    .map(|s| (s.width / s.height.max(1e-6)) as f32)
                    .unwrap_or(0.77);
                let max_w = (cell.width() - 28.0).max(20.0);
                let mut size = egui::vec2(THUMB_HEIGHT * aspect, THUMB_HEIGHT);
                if size.x > max_w {
                    size *= max_w / size.x;
                }
                let image = egui::Rect::from_center_size(
                    egui::pos2(cell.center().x, cell.top() + 6.0 + THUMB_HEIGHT / 2.0),
                    size,
                );
                let painter = ui.painter();
                if is_picked {
                    painter.rect_filled(cell.shrink(2.0), 3.0, palette.selection);
                }
                painter.rect_filled(image, 0.0, palette.paper);
                if let Some(texture) = app.textures.thumb(render_doc, page) {
                    canvas::paint_texture(painter, texture, image, quarter_turns);
                }
                let edge = if is_current {
                    Stroke::new(2.0, palette.accent)
                } else if response.hovered() {
                    Stroke::new(1.0, ui.visuals().widgets.hovered.bg_stroke.color)
                } else {
                    Stroke::new(1.0, palette.page_edge)
                };
                painter.rect_stroke(image, 0.0, edge, StrokeKind::Outside);
                let color = if is_current || is_picked {
                    ui.visuals().strong_text_color()
                } else {
                    palette.muted_text
                };
                painter.text(
                    egui::pos2(cell.center().x, cell.top() + 10.0 + THUMB_HEIGHT),
                    egui::Align2::CENTER_TOP,
                    (page + 1).to_string(),
                    egui::FontId::proportional(11.5),
                    color,
                );
            }
            // Near the top or bottom edge, a drag scrolls the thumbnails along.
            if let (true, Some(pos)) = (dragging, pointer) {
                let visible = ui.clip_rect();
                let edge = 28.0;
                if visible.x_range().contains(pos.x) {
                    let step = if pos.y < visible.top() + edge && pos.y > visible.top() - edge {
                        12.0
                    } else if pos.y > visible.bottom() - edge && pos.y < visible.bottom() + edge {
                        -12.0
                    } else {
                        0.0
                    };
                    if step != 0.0 {
                        ui.scroll_with_delta(egui::vec2(0.0, step));
                        ui.ctx().request_repaint();
                    }
                }
            }
            // The drop indicator: a line in the gap the pages would go to.
            if let (true, Some(gap)) = (
                dragging,
                app.structure.page_drag.as_ref().and_then(|d| d.gap),
            ) {
                let y = cells
                    .iter()
                    .find(|(p, _)| *p == gap)
                    .map(|(_, r)| r.top())
                    .or_else(|| {
                        cells
                            .iter()
                            .find(|(p, _)| *p + 1 == gap)
                            .map(|(_, r)| r.bottom())
                    });
                if let (Some(y), Some((_, first))) = (y, cells.first()) {
                    let x0 = first.left() + 6.0;
                    let x1 = first.right() - 6.0;
                    ui.painter().line_segment(
                        [egui::pos2(x0, y), egui::pos2(x1, y)],
                        Stroke::new(2.5, palette.accent),
                    );
                }
            }
        });
    theme::name_scroll_bars(ui.ctx(), scroll.id, "Pages");

    // ---- picking ---------------------------------------------------------------------------
    if let Some(page) = context {
        let sel = app.structure.page_selection.entry(doc_id).or_default();
        if !(sel.pages.len() > 1 && sel.pages.contains(&page)) {
            sel.pages = [page].into_iter().collect();
            sel.anchor = Some(page);
            if let Some(dv) = app.views.get_mut(&id) {
                dv.go_to_page(page);
            }
        }
    }
    if let Some(page) = click {
        app.page_click(doc_id, page, modifiers);
    }

    // ---- dragging ------------------------------------------------------------------------------
    if let Some(page) = drag_start {
        let pages = if picked.contains(&page) {
            picked.iter().copied().collect()
        } else {
            vec![page]
        };
        app.structure.page_drag = Some(PageDrag {
            doc: doc_id,
            pages,
            gap: None,
        });
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        // Escape drops the drag: nothing moves.
        if app
            .structure
            .page_drag
            .as_ref()
            .is_some_and(|d| d.doc == doc_id)
        {
            app.structure.page_drag = None;
        }
    }
    if let Some(drag) = app.structure.page_drag.as_mut().filter(|d| d.doc == doc_id) {
        // Only over the thumbnails is there a gap to drop into: released anywhere else, the
        // drag changes nothing.
        drag.gap = pointer
            .filter(|pos| scroll.inner_rect.contains(*pos))
            .and_then(|pos| drop_gap(&cells, pos.y));
        ui.ctx().set_cursor_icon(if drag.gap.is_some() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::NoDrop
        });
    }
    if released {
        if let Some(drag) = app.structure.page_drag.take() {
            let same_doc = app.views.get(&id).is_some_and(|v| v.doc == drag.doc);
            if let (true, Some(to)) = (
                same_doc,
                drag.gap.and_then(|gap| move_target(&drag.pages, gap)),
            ) {
                app.move_pages(drag.pages, to);
            }
        }
    }
}

// ---- combine files ------------------------------------------------------------------------------

pub fn combine_dialog(app: &mut AppState, ctx: &egui::Context) {
    if app.structure.combine.is_none() {
        return;
    }
    let mut add = false;
    let mut combine = false;
    let mut cancel = false;
    let running = app.combine_running();
    let modal = egui::Modal::new(Id::unique("plumbline-combine")).show(ctx, |ui| {
        let Some(dialog) = app.structure.combine.as_mut() else {
            return;
        };
        ui.set_width(460.0);
        if running {
            ui.label(RichText::new("Combine files").font(theme::bold(15.0)));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "Writing and checking the combined file ({} files)\u{2026}",
                    dialog.rows.len()
                ));
            });
            return;
        }
        ui.label(RichText::new("Combine files").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label(
            "A new PDF is made of these files, top to bottom. Leave Pages empty for every page, \
             or give page numbers and ranges, such as 1-3, 5.",
        );
        ui.add_space(6.0);
        let mut swap: Option<(usize, usize)> = None;
        let mut remove: Option<usize> = None;
        let rows = dialog.rows.len();
        egui::ScrollArea::vertical()
            .id_salt("plumbline-combine-rows")
            .max_height(260.0)
            .show(ui, |ui| {
                for (i, row) in dialog.rows.iter_mut().enumerate() {
                    let name = display_name(&row.path);
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(i > 0, |ui| {
                            if icon_button(ui, Icon::ChevronUp, &format!("Move {name} up"), false)
                                .clicked()
                            {
                                swap = Some((i - 1, i));
                            }
                        });
                        ui.add_enabled_ui(i + 1 < rows, |ui| {
                            if icon_button(
                                ui,
                                Icon::ChevronDown,
                                &format!("Move {name} down"),
                                false,
                            )
                            .clicked()
                            {
                                swap = Some((i, i + 1));
                            }
                        });
                        ui.add(egui::Label::new(&name).truncate())
                            .on_hover_text(row.path.to_string_lossy());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, Icon::Cross, &format!("Remove {name}"), false)
                                .clicked()
                            {
                                remove = Some(i);
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut row.range)
                                    .desired_width(90.0)
                                    .hint_text(format!("all {}", row.page_count)),
                            )
                            .accessible_name(format!("Pages of {name}"))
                            .on_hover_text("Pages to take from this file; empty for all");
                        });
                    });
                }
            });
        if let Some((a, b)) = swap {
            dialog.rows.swap(a, b);
        }
        if let Some(i) = remove {
            dialog.rows.remove(i);
        }
        if let Some(error) = &dialog.error {
            ui.label(error);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            add = ui
                .button("Add files\u{2026}")
                .on_hover_text("Choose more PDF files to combine")
                .clicked();
            combine = ui
                .add_enabled(!dialog.rows.is_empty(), Button::new("Combine\u{2026}"))
                .on_hover_text("Choose where to save the new PDF, then make it")
                .clicked();
            cancel = ui
                .button("Cancel")
                .on_hover_text("Close without making anything")
                .clicked();
        });
    });
    if running {
        // The worker cannot be stopped half-way; the dialog closes when it is done.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    if cancel || (!add && !combine && modal.should_close()) {
        app.structure.combine = None;
    } else if add {
        app.combine_add_files();
    } else if combine {
        app.run_combine();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_lists_read_as_people_write_them() {
        assert_eq!(page_list(&[2]), "3");
        assert_eq!(page_list(&[0, 1]), "1, 2");
        assert_eq!(page_list(&[0, 1, 2, 5]), "1-3, 6");
        assert_eq!(page_list(&[1, 4, 6]), "2, 5, 7");
    }

    #[test]
    fn a_drop_gap_becomes_a_move_target() {
        // Page 0 dropped after page 2 (gap 3): it becomes page 2.
        assert_eq!(move_target(&[0], 3), Some(2));
        // Page 3 dropped before page 1.
        assert_eq!(move_target(&[3], 1), Some(1));
        // Dropped where it already is.
        assert_eq!(move_target(&[2], 2), None);
        assert_eq!(move_target(&[2], 3), None);
        // Two pages to the end of five.
        assert_eq!(move_target(&[0, 2], 5), Some(3));
        // Two pages that are apart, dropped between them, are brought together.
        assert_eq!(move_target(&[0, 2], 2), Some(1));
    }

    #[test]
    fn picked_pages_follow_a_page_map() {
        let mut sel = PageSelection {
            pages: [0, 2, 3].into_iter().collect(),
            anchor: Some(3),
        };
        sel.remap(&PageMap {
            old_to_new: vec![Some(1), None, Some(0), Some(2)],
            new_page_count: 3,
        });
        assert_eq!(sel.pages.iter().copied().collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(sel.anchor, Some(2));
    }
}
