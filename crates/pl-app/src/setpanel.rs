//! Drawing sets in the window: the Set page of the side panel lists the sheets of the open
//! set (number, title and file), a click opens that sheet (in the tab its file already has,
//! or a new one), Alt+Page Down and Alt+Page Up go to the next and previous sheet, and the
//! search box searches every file of the set, listing the hits by file and page. Files that
//! are not where the set says are marked, and can be pointed at their new place.
//!
//! The set itself is pl-doc's [`DrawingSet`], kept in a `.plset` file next to the drawings.

use std::path::{Path, PathBuf};

use egui::{Button, Key, RichText, TextEdit, Ui};
use pl_doc::pl_render::SearchOptions;
use pl_doc::{
    spawn_set_search, DrawingSet, SetHit, SetSearch, SetSearchEvent, SetSearchJob, SheetRef,
};

use crate::app::{display_name, AppState, SidePage, ViewId};
use crate::theme::{self, Palette};

/// The open drawing set and its search.
#[derive(Default)]
pub(crate) struct SetsUi {
    pub set: Option<DrawingSet>,
    pub query: String,
    pub job: Option<SetSearchJob>,
    pub hits: Vec<SetHit>,
    /// Files the search could not read, with why.
    pub skipped: Vec<(usize, String)>,
    /// The query the hits are for.
    pub searched: Option<String>,
    pub cancelled: bool,
    /// The sheets of set files whose documents have unsaved page changes, as they are for the
    /// file on disk: the set shows them moved with the pages, and takes the moved ones for
    /// good when the document is saved.
    pub file_sheets: std::collections::HashMap<PathBuf, Vec<pl_doc::SheetInfo>>,
    /// Files being read on a worker thread to be added to the set (each with its sheets).
    pub adding: Option<std::sync::mpsc::Receiver<AddedFiles>>,
}

/// Files read for the set: each with its sheets, or why it could not be read.
pub(crate) type AddedFiles = Vec<(PathBuf, Result<Vec<pl_doc::SheetInfo>, String>)>;

/// The summary of a set search the user stopped.
pub(crate) fn set_search_stopped(count: usize, query: &str) -> String {
    format!(
        "Stopped: {count} match{} for \u{201c}{query}\u{201d} so far; not every file was searched",
        if count == 1 { "" } else { "es" }
    )
}

impl AppState {
    /// The open drawing set.
    pub fn drawing_set(&self) -> Option<&DrawingSet> {
        self.sets.set.as_ref()
    }

    /// Opens a drawing set from a `.plset` file and shows the Set page.
    pub fn open_set(&mut self, path: &Path) -> bool {
        match DrawingSet::open(path) {
            Ok(set) => {
                let missing = set.files().iter().filter(|f| f.missing).count();
                self.chrome.status = Some(if missing == 0 {
                    format!("Opened the set {} ({} files).", set.name, set.len())
                } else {
                    format!(
                        "Opened the set {}: {missing} of its {} files are missing.",
                        set.name,
                        set.len()
                    )
                });
                self.install_set(set);
                true
            }
            Err(e) => {
                self.chrome.status = Some(format!(
                    "The set {} could not be opened: {e}",
                    display_name(path)
                ));
                false
            }
        }
    }

    fn install_set(&mut self, set: DrawingSet) {
        if let Some(job) = self.sets.job.take() {
            job.cancel();
        }
        self.sets = SetsUi {
            set: Some(set),
            ..SetsUi::default()
        };
        self.chrome.side_open = true;
        self.chrome.side_page = SidePage::Set;
    }

    /// Makes a new set of the documents open now (those with a file) and writes it to `path`.
    pub fn new_set_at(&mut self, path: &Path) -> bool {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Drawing set".into());
        let mut set = DrawingSet::new(name);
        for id in self.view_ids() {
            let Some(file) = self
                .views
                .get(&id)
                .and_then(|v| self.session.document(v.doc).ok())
                .and_then(|d| d.path().map(Path::to_path_buf))
            else {
                continue;
            };
            if set.position(&file).is_none() {
                if let Err(e) = set.add_file(&file) {
                    self.chrome.status =
                        Some(format!("{} could not be added: {e}", display_name(&file)));
                }
            }
        }
        match set.save(path) {
            Ok(()) => {
                self.chrome.status = Some(format!(
                    "Made the set {} with {} file{}.",
                    set.name,
                    set.len(),
                    if set.len() == 1 { "" } else { "s" }
                ));
                self.install_set(set);
                true
            }
            Err(e) => {
                self.chrome.status = Some(format!("The set could not be written: {e}"));
                false
            }
        }
    }

    /// Asks for PDF files and adds them to the open set, which is saved at once.
    /// The files are read (each parsed for its pages and labels) on a worker thread.
    pub(crate) fn add_files_to_set(&mut self) {
        if self.sets.set.is_none() || self.sets.adding.is_some() {
            return;
        }
        let paths = self.dialogs.pick_pdfs();
        if paths.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = self.repaint_context();
        let spawned = std::thread::Builder::new()
            .name("plumbline-set-files".into())
            .spawn(move || {
                let read: AddedFiles = paths
                    .into_iter()
                    .map(|p| {
                        let sheets = pl_doc::sheets_for(&p).map_err(|e| e.to_string());
                        (p, sheets)
                    })
                    .collect();
                let _ = tx.send(read);
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        match spawned {
            Ok(_) => {
                self.sets.adding = Some(rx);
                self.chrome.status = Some("Reading the files for the set\u{2026}".into());
            }
            Err(e) => self.chrome.status = Some(format!("The files could not be read: {e}")),
        }
    }

    /// Takes in files read for the set; with `wait`, waits for them. Returns whether some are
    /// still being read.
    pub(crate) fn poll_set_adding(&mut self, wait: bool) -> bool {
        let Some(rx) = self.sets.adding.as_ref() else {
            return false;
        };
        let read = if wait {
            rx.recv().ok()
        } else {
            match rx.try_recv() {
                Ok(read) => Some(read),
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            }
        };
        self.sets.adding = None;
        let Some(set) = self.sets.set.as_mut() else {
            return false;
        };
        let mut added = 0;
        let mut failed = Vec::new();
        for (path, sheets) in read.unwrap_or_default() {
            match sheets.and_then(|sheets| {
                let at = set.len();
                set.insert_with_sheets(at, &path, sheets)
                    .map_err(|e| e.to_string())
            }) {
                Ok(_) => added += 1,
                Err(e) => failed.push(format!("{} could not be added: {e}", display_name(&path))),
            }
        }
        self.chrome.status = Some(match failed.first() {
            Some(first) => first.clone(),
            None => format!(
                "{added} file{} added to the set.",
                if added == 1 { "" } else { "s" }
            ),
        });
        if added > 0 {
            self.save_set();
        }
        false
    }

    /// Points a missing file of the set at a file the user picks.
    pub(crate) fn relink_set_file(&mut self, index: usize) {
        let Some(path) = self.dialogs.pick_pdfs().into_iter().next() else {
            return;
        };
        let Some(set) = self.sets.set.as_mut() else {
            return;
        };
        match set.relink(index, &path) {
            Ok(()) => self.save_set(),
            Err(e) => {
                self.chrome.status = Some(format!("{} could not be used: {e}", display_name(&path)))
            }
        }
    }

    fn save_set(&mut self) {
        let Some(set) = self.sets.set.as_mut() else {
            return;
        };
        let Some(at) = set.location().map(Path::to_path_buf) else {
            return;
        };
        if let Err(e) = set.save(&at) {
            self.chrome.status = Some(format!("The set could not be saved: {e}"));
        }
    }

    /// Opens a sheet of the set: its file in the tab it already has (or a new one), at the
    /// sheet's page. Returns the view.
    pub fn open_sheet(&mut self, sheet: SheetRef) -> Option<ViewId> {
        let file = self.sets.set.as_ref()?.file(sheet.file)?.clone();
        if file.missing || !file.path.is_file() {
            self.chrome.status = Some(format!(
                "{} is missing. Use Find next to it in the Set page to say where it is now.",
                display_name(&file.path)
            ));
            return None;
        }
        let view = self.open_path(&file.path)?;
        if let Some(dv) = self.views.get_mut(&view) {
            dv.go_to_page(sheet.page);
        }
        Some(view)
    }

    /// The sheet of the set the active view shows, if its file is in the set.
    pub fn current_sheet(&self) -> Option<SheetRef> {
        let set = self.sets.set.as_ref()?;
        let dv = self.active_view()?;
        let path = self.session.document(dv.doc).ok()?.path()?.to_path_buf();
        let file = set.position(&path)?;
        Some(SheetRef {
            file,
            page: dv.shown_page(),
        })
    }

    /// Goes to the next (or previous) sheet of the set.
    pub fn step_sheet(&mut self, forward: bool) -> bool {
        let Some(set) = self.sets.set.as_ref() else {
            return false;
        };
        let target = match self.current_sheet() {
            Some(at) if forward => set.next_sheet(at),
            Some(at) => set.prev_sheet(at),
            None if forward => set.first_sheet(),
            None => set.last_sheet(),
        };
        match target {
            Some(sheet) => self.open_sheet(sheet).is_some(),
            None => false,
        }
    }

    // ---- search ------------------------------------------------------------------------------

    /// Starts searching every file of the set for `query` on a worker thread; the hits come in
    /// as frames take them ([`AppState::set_search_hits`]).
    pub fn start_set_search(&mut self, query: &str) -> bool {
        let query = query.trim().to_owned();
        let Some(set) = self.sets.set.as_ref() else {
            return false;
        };
        if let Some(job) = self.sets.job.take() {
            job.cancel();
        }
        self.sets.hits.clear();
        self.sets.skipped.clear();
        self.sets.cancelled = false;
        self.sets.searched = None;
        if query.is_empty() {
            return false;
        }
        let options = SearchOptions::default();
        let search = SetSearch::in_session(&self.session, set, &query, options);
        match spawn_set_search(search) {
            Ok(job) => {
                self.sets.job = Some(job);
                self.sets.searched = Some(query);
                true
            }
            Err(e) => {
                self.chrome.status = Some(format!("The set could not be searched: {e}"));
                false
            }
        }
    }

    /// Takes in what the set search has found since the last frame. Returns whether it is
    /// still running.
    pub(crate) fn poll_set_search(&mut self) -> bool {
        let Some(job) = self.sets.job.as_mut() else {
            return false;
        };
        for event in job.try_events() {
            match event {
                SetSearchEvent::Hits(hits) => self.sets.hits.extend(hits),
                SetSearchEvent::Skipped { file, reason } => self.sets.skipped.push((file, reason)),
                SetSearchEvent::Finished { cancelled } => self.sets.cancelled = cancelled,
            }
        }
        if job.is_finished() {
            self.sets.job = None;
            return false;
        }
        true
    }

    /// Whether the last set search was stopped before it had searched every file.
    pub fn set_search_cancelled(&self) -> bool {
        self.sets.cancelled
    }

    /// Follows a structural change of a document (see
    /// [`AppState::follow_structure_changes`]) in the set: the search hits in its file move
    /// with their pages (a hit on a removed page goes), and so do the file's sheets.
    pub(crate) fn follow_set_pages(
        &mut self,
        doc_id: pl_doc::DocumentId,
        change: &pl_doc::PageMap,
    ) {
        let Some(set) = self.sets.set.as_mut() else {
            return;
        };
        let Ok(doc) = self.session.document(doc_id) else {
            return;
        };
        let Some(path) = doc.path().map(Path::to_path_buf) else {
            return;
        };
        let Some(index) = set.position(&path) else {
            return;
        };
        self.sets.hits.retain_mut(|hit| {
            if hit.file != index {
                return true;
            }
            match change.map(hit.page) {
                Some(page) => {
                    hit.page = page;
                    true
                }
                None => false,
            }
        });
        let baseline = self
            .sets
            .file_sheets
            .entry(path.clone())
            .or_insert_with(|| set.files()[index].sheets.clone())
            .clone();
        match doc.page_map_from_file() {
            Some(from_file) => {
                set.set_sheets(index, baseline);
                set.follow_pages(index, &from_file);
            }
            // Too many steps ago to know: follow this one.
            None => {
                set.follow_pages(index, change);
            }
        }
        for hit in self.sets.hits.iter_mut().filter(|h| h.file == index) {
            hit.sheet = set.sheet(hit.sheet_ref()).cloned();
        }
        if !doc.has_structure_changes() {
            self.sets.file_sheets.remove(&path);
        }
    }

    /// A document was saved: the set keeps its file's sheets as they now are, and is saved.
    pub(crate) fn set_file_saved(&mut self, path: &Path) {
        let key = self
            .sets
            .file_sheets
            .keys()
            .find(|k| pl_doc::journal::same_path(k, path))
            .cloned();
        if let Some(key) = key {
            self.sets.file_sheets.remove(&key);
            self.save_set();
        }
    }

    pub fn set_search_running(&self) -> bool {
        self.sets.job.is_some()
    }

    /// The hits of the last set search so far.
    pub fn set_search_hits(&self) -> &[SetHit] {
        &self.sets.hits
    }

    /// Opens the sheet of a hit and shows the hit.
    pub fn open_set_hit(&mut self, index: usize) -> Option<ViewId> {
        let hit = self.sets.hits.get(index)?.clone();
        let view = self.open_sheet(hit.sheet_ref())?;
        if let Some(rect) = hit.rects.first() {
            let exact = self
                .views
                .get(&view)
                .and_then(|v| self.session.document(v.doc).ok())
                .is_some_and(|d| d.geometry_is_exact(hit.page));
            if let Some(dv) = self.views.get_mut(&view) {
                // The hit's top-left corner, with a little room above it.
                dv.go_to_destination(hit.page, Some(rect.x0 - 24.0), Some(rect.y1 + 24.0), exact);
            }
        }
        Some(view)
    }
}

/// One file of the set as the panel lists it.
struct FileRow {
    index: usize,
    path: PathBuf,
    missing: bool,
    /// Number and title of each sheet.
    sheets: Vec<(String, String)>,
}

/// The Set page of the side panel.
pub fn panel(app: &mut AppState, ui: &mut Ui) {
    let Some(set) = app.sets.set.as_ref() else {
        ui.weak("No drawing set is open. File > Open set opens one.");
        return;
    };
    let palette = Palette::of(ui.visuals());
    let name = set.name.clone();
    let files: Vec<FileRow> = set
        .files()
        .iter()
        .enumerate()
        .map(|(i, f)| FileRow {
            index: i,
            path: f.path.clone(),
            missing: f.missing,
            sheets: f
                .sheets
                .iter()
                .map(|s| (s.number.clone(), s.title.clone()))
                .collect(),
        })
        .collect();
    let current = app.current_sheet();

    ui.horizontal(|ui| {
        ui.label(RichText::new(&name).font(theme::bold(13.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(Button::new("Add files\u{2026}").small())
                .on_hover_text("Add PDF files to the set")
                .clicked()
            {
                app.add_files_to_set();
            }
        });
    });

    // ---- search ----------------------------------------------------------------------------------
    let mut search = false;
    ui.horizontal(|ui| {
        let field = ui
            .add(
                TextEdit::singleline(&mut app.sets.query)
                    .desired_width(ui.available_width() - 64.0)
                    .hint_text("Search the set"),
            )
            .accessible_name("Search the set")
            .on_hover_text("Text to find in every file of the set");
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            search = true;
        }
        if app.sets.job.is_some() {
            if ui
                .add(Button::new("Stop").small())
                .on_hover_text("Stop searching")
                .clicked()
            {
                if let Some(job) = &app.sets.job {
                    job.cancel();
                }
            }
        } else if ui
            .add(Button::new("Search").small())
            .on_hover_text("Search every file of the set")
            .clicked()
        {
            search = true;
        }
    });
    if search {
        let query = app.sets.query.clone();
        app.start_set_search(&query);
    }
    ui.separator();

    let mut open: Option<SheetRef> = None;
    let mut open_hit: Option<usize> = None;
    let mut relink: Option<usize> = None;
    let scroll = egui::ScrollArea::vertical()
        .id_salt("plumbline-set")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // ---- the search results, by file and page ----------------------------------------
            if let Some(query) = app.sets.searched.clone() {
                let running = app.sets.job.is_some();
                let count = app.sets.hits.len();
                let summary = if running {
                    format!("Searching for \u{201c}{query}\u{201d}\u{2026} {count} found")
                } else if app.sets.cancelled {
                    set_search_stopped(count, &query)
                } else if count == 0 {
                    format!("No matches for \u{201c}{query}\u{201d}")
                } else {
                    format!(
                        "{count} match{} for \u{201c}{query}\u{201d}",
                        if count == 1 { "" } else { "es" }
                    )
                };
                ui.label(RichText::new(summary).color(palette.muted_text));
                let mut last_file = None;
                for (i, hit) in app.sets.hits.iter().enumerate() {
                    if last_file != Some(hit.file) {
                        last_file = Some(hit.file);
                        ui.label(RichText::new(display_name(&hit.path)).strong());
                    }
                    let sheet = hit
                        .sheet
                        .as_ref()
                        .map(|s| format!(" ({})", s.number))
                        .unwrap_or_default();
                    let text = format!("Page {}{sheet}", hit.page + 1);
                    let name = format!("{}, page {}", display_name(&hit.path), hit.page + 1);
                    let response = ui
                        .horizontal(|ui| {
                            ui.add_space(10.0);
                            ui.add(Button::selectable(false, text).truncate())
                                .on_hover_text(format!("Show this match ({name})"))
                        })
                        .inner;
                    if response.clicked() {
                        open_hit = Some(i);
                    }
                }
                for (file, reason) in &app.sets.skipped {
                    let file_name = files
                        .get(*file)
                        .map(|f| display_name(&f.path))
                        .unwrap_or_default();
                    ui.label(
                        RichText::new(format!("{file_name} was not searched: {reason}"))
                            .color(palette.muted_text),
                    );
                }
                ui.separator();
            }

            // ---- the sheets --------------------------------------------------------------------
            for FileRow {
                index,
                path,
                missing,
                sheets,
            } in &files
            {
                ui.horizontal(|ui| {
                    let label = RichText::new(display_name(path)).strong();
                    ui.add(egui::Label::new(label).truncate())
                        .on_hover_text(path.to_string_lossy());
                    if *missing {
                        ui.label(RichText::new("missing").color(palette.muted_text));
                        if ui
                            .add(Button::new("Find\u{2026}").small())
                            .on_hover_text("Say where this file is now")
                            .clicked()
                        {
                            relink = Some(*index);
                        }
                    }
                });
                for (page, (number, title)) in sheets.iter().enumerate() {
                    let sheet = SheetRef {
                        file: *index,
                        page: page as u32,
                    };
                    let is_current = current == Some(sheet);
                    let text = format!("{number}  {title}");
                    let response = ui
                        .horizontal(|ui| {
                            ui.add_space(10.0);
                            ui.add_enabled(
                                !missing,
                                Button::selectable(is_current, text.as_str()).truncate(),
                            )
                            .on_hover_text(format!(
                                "Open sheet {number} ({}, page {})",
                                display_name(path),
                                page + 1
                            ))
                        })
                        .inner;
                    if response.clicked() {
                        open = Some(sheet);
                    }
                }
            }
        });
    theme::name_scroll_bars(ui.ctx(), scroll.id, "Set");
    if let Some(sheet) = open {
        app.open_sheet(sheet);
    }
    if let Some(i) = open_hit {
        app.open_set_hit(i);
    }
    if let Some(i) = relink {
        app.relink_set_file(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stopped_search_says_it_did_not_search_everything() {
        let text = set_search_stopped(3, "C-101");
        assert!(text.starts_with("Stopped: 3 matches"), "{text}");
        assert!(text.contains("not every file was searched"));
        assert!(set_search_stopped(1, "x").contains("1 match for"));
    }
}
