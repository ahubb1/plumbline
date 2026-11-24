//! The markups list: a table of the active document's markups, docked at the bottom of the
//! window.
//!
//! pl-takeoff is the engine: it turns the annotations into rows ([`Table`]), filters, sorts
//! and groups them ([`View`]) according to a [`Layout`], and exports them. This module shows
//! the result with `egui_table`, which lays out only the rows on screen, so a list of tens of
//! thousands of markups costs a frame no more than a short one. The rows are built again only
//! when the document's annotations, the layout or the user's columns change.
//!
//! What can be done here:
//!
//! * click a column heading to sort by it (Shift adds a key); its menu hides, moves and groups;
//!   drag a heading's edge to resize; the Columns menu shows, hides and reorders;
//! * group by a column: each group has a heading line that opens and closes it, with its count
//!   and the subtotal of every summed column;
//! * search across all columns, and filter each column from the filter row;
//! * set a markup's review status and checkmark, edit its comment and the user's own columns
//!   in place (double-click a cell); each is one undo step in the document;
//! * replies are child rows under their markup, and "Add reply" adds one;
//! * selecting rows selects the markups on the page and brings the first into view, and
//!   selecting markups on the page marks their rows and scrolls the list to them;
//! * export the list as CSV or XLSX and the markups as XFDF, and import XFDF.
//!
//! The layout and the user's column definitions are kept per installation, in
//! [`LIST_FILE_NAME`] under the app-data directory.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{
    Align, Button, Color32, Id, Key, Label, Layout as UiLayout, Panel, RichText, Role, Sense,
    Stroke, TextEdit, Ui, WidgetInfo,
};
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, PrefetchInfo, TableDelegate};
use pl_doc::pl_annot::{AnnotId, AnnotKind, Origin, PdfDate, ReviewState, Status, StatusSet};
use pl_doc::pl_takeoff::{
    export_xfdf, import_xfdf, to_csv, to_xlsx, Builtin, Col, ColumnFilter, ColumnId, ColumnType,
    Condition, CsvOptions, CustomColumn, GroupKey, Layout, Line, ListContext, Schema, Table, Value,
    View, XfdfOptions,
};
use pl_doc::{Command as DocCommand, DocumentId};
use serde::{Deserialize, Serialize};

use crate::app::{display_name, AppState};
use crate::commands::Command;
use crate::icons::{icon_button, Icon};
use crate::markup::Tool;
use crate::platform::ExportKind;
use crate::theme::{self, Palette};

/// The file the list's layout and the user's columns are kept in, under the app-data
/// directory.
pub const LIST_FILE_NAME: &str = "markups_list.json";
/// The height of one row of the list, in logical pixels.
pub const ROW_HEIGHT: f32 = 22.0;
const HEADER_HEIGHT: f32 = 24.0;
/// How long after the last change the list's file is written, in seconds.
const WRITE_DELAY: f64 = 0.4;
const TABLE_SALT: &str = "plumbline-markups-table";
const CELL_EDIT_ID: &str = "plumbline-list-cell-edit";

/// What is kept on disk.
#[derive(Serialize, Deserialize)]
struct ListFile {
    version: u32,
    #[serde(default)]
    open: bool,
    #[serde(default)]
    filter_row: bool,
    layout: Layout,
    #[serde(default)]
    schema: Schema,
}

/// One line of the list as it is shown.
#[derive(Clone, Debug, PartialEq)]
enum ListLine {
    Group {
        depth: usize,
        path: Vec<Value>,
        /// For example `Type: Rectangle`.
        heading: String,
        count: usize,
        subtotals: Vec<(ColumnId, f64)>,
        collapsed: bool,
    },
    Row {
        row: usize,
        group_depth: usize,
        reply_depth: usize,
    },
    Total {
        count: usize,
        subtotals: Vec<(ColumnId, f64)>,
    },
}

/// The list of one document at one revision.
struct ListCache {
    doc: DocumentId,
    revision: u64,
    /// The schema the table was built with ([`ListState::table_rev`]).
    table_rev: u64,
    rev: u64,
    /// The open and closed groups the lines were laid out with ([`ListState::fold_rev`]).
    fold_rev: u64,
    table: Table,
    view: View,
    lines: Vec<ListLine>,
    line_of: HashMap<AnnotId, usize>,
}

/// A cell being edited in place.
struct CellEdit {
    id: AnnotId,
    column: ColumnId,
    text: String,
    focus: bool,
}

/// A reply being written from the list.
struct ReplyDraft {
    parent: AnnotId,
    text: String,
    focus: bool,
}

/// The "Custom columns" dialog.
#[derive(Default)]
struct ColumnsDialog {
    name: String,
    kind: Option<ColumnType>,
    /// A text column whose values may run over several lines.
    multiline: bool,
    /// Whether a new column is kept in the PDF as a Bluebeam column (`/BSIAnnotColumns`);
    /// `None` until the user chooses, which means yes for a document that has Bluebeam
    /// columns already.
    in_file: Option<bool>,
    options: String,
    error: Option<String>,
    /// Text boxes of the existing columns, by column id: (title, options).
    edits: HashMap<String, (String, String)>,
}

/// The "Formula column" dialog.
#[derive(Default)]
struct FormulaDialog {
    name: String,
    formula: String,
    /// What is wrong with the formula as typed, worked out as it is typed.
    message: Option<String>,
    /// Why the last "Add" was refused (a name in use, say), with the name and the formula it
    /// was refused for: shown only while they are still what is typed.
    refused: Option<(String, String, String)>,
}

/// What the list did in the last frame (the tests read this).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListStats {
    /// How many times the lines have been laid out again since the app started (a change to
    /// the document, the sort, the grouping, the filter or the open groups).
    pub builds: u64,
    /// How many of those read the document's annotations again. Sorting, grouping, filtering
    /// and opening a group do not.
    pub tables: u64,
    /// How many of them sorted, grouped and filtered again. Opening or closing a group does
    /// not.
    pub views: u64,
    /// Lines in the list, group headings and replies included.
    pub lines: usize,
    /// Cells laid out in the last frame.
    pub cells: usize,
    /// Lines laid out in the last frame.
    pub rows: usize,
}

/// One line of the list as text, for tests and scripts.
#[derive(Clone, Debug, PartialEq)]
pub struct ListLineView {
    /// The markup or reply of a row; `None` for a group heading or the total.
    pub id: Option<AnnotId>,
    /// A group heading's text.
    pub group: Option<String>,
    /// How many markups a group heading or the total line stands for.
    pub count: usize,
    /// 0 for a markup, 1 and up for a reply.
    pub reply_depth: usize,
    /// The visible cells, in column order. For a group heading or the total: its subtotals,
    /// empty elsewhere.
    pub cells: Vec<String>,
    pub selected: bool,
}

/// Everything about the markups list.
pub(crate) struct ListState {
    pub open: bool,
    pub filter_row: bool,
    pub layout: Layout,
    pub schema: Schema,
    /// The active document's own columns (Bluebeam Revu's `/BSIAnnotColumns`), shown after
    /// the user's. They belong to the document and are never kept in the list's file.
    doc_columns: Vec<CustomColumn>,
    /// The status sets the active document's statuses can be chosen from: the review states,
    /// then each custom set (Bluebeam's) its status annotations use.
    status_sets: Vec<StatusSet>,
    path: Option<PathBuf>,
    dirty: Option<f64>,
    /// Goes up with every change to the layout or the schema that changes the rows.
    rev: u64,
    /// Goes up when the user's columns change: the rows hold their values.
    table_rev: u64,
    /// Goes up when a group is opened or closed.
    fold_rev: u64,
    cache: Option<ListCache>,
    /// The selected markups as a set, for the document and selection it was made from.
    selected_set: Option<(DocumentId, u64, std::sync::Arc<HashSet<AnnotId>>)>,
    /// Replies selected in the list (a reply is not on the page, so the document's selection
    /// cannot hold it).
    selected_replies: Vec<AnnotId>,
    /// Where a Shift-click range starts.
    anchor: Option<AnnotId>,
    /// The document's selection as the list last saw it: the document and its
    /// [`selection_serial`](pl_doc::Document::selection_serial).
    last_selection: Option<(DocumentId, u64)>,
    scroll_to: Option<u64>,
    editing: Option<CellEdit>,
    reply: Option<ReplyDraft>,
    columns_dialog: Option<ColumnsDialog>,
    formula_dialog: Option<FormulaDialog>,
    /// The widths in the layout are to be given to the table (they were loaded or set from
    /// outside it).
    push_widths: bool,
    pub stats: ListStats,
}

impl ListState {
    pub(crate) fn new(data_dir: Option<&Path>) -> ListState {
        let path = data_dir.map(|d| d.join(LIST_FILE_NAME));
        let file = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|json| serde_json::from_str::<ListFile>(&json).ok());
        let (open, filter_row, mut layout, schema) = match file {
            Some(f) if f.schema.validate().is_ok() => (f.open, f.filter_row, f.layout, f.schema),
            _ => (false, false, Layout::default(), Schema::new()),
        };
        layout.reconcile(&schema);
        ListState {
            open,
            filter_row,
            layout,
            schema,
            doc_columns: Vec::new(),
            status_sets: vec![StatusSet::review()],
            path,
            dirty: None,
            rev: 1,
            table_rev: 1,
            fold_rev: 1,
            cache: None,
            selected_set: None,
            selected_replies: Vec::new(),
            anchor: None,
            last_selection: None,
            scroll_to: None,
            editing: None,
            reply: None,
            columns_dialog: None,
            formula_dialog: None,
            push_widths: true,
            stats: ListStats::default(),
        }
    }

    fn store(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = ListFile {
            version: 1,
            open: self.open,
            filter_row: self.filter_row,
            layout: self.layout.clone(),
            schema: self.schema.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// The user's columns and, after them, the active document's own (Bluebeam's) that do
    /// not share an id with one of the user's.
    pub(crate) fn effective(&self) -> Schema {
        let mut schema = self.schema.clone();
        for c in &self.doc_columns {
            if schema.get(&c.id).is_none() {
                schema.columns.push(c.clone());
            }
        }
        schema
    }

    /// A column of the list by id: one of the user's, else one of the document's.
    pub(crate) fn column(&self, id: &str) -> Option<CustomColumn> {
        self.schema
            .get(id)
            .or_else(|| self.doc_columns.iter().find(|c| c.id == id))
            .cloned()
    }

    /// Brings the layout in line with the columns there are.
    fn reconcile_layout(&mut self) {
        let schema = self.effective();
        self.layout.reconcile(&schema);
    }

    /// The layout or the schema changed in a way that changes the rows.
    fn changed(&mut self) {
        self.rev += 1;
        self.dirty = Some(f64::NAN);
    }

    /// The user's columns changed: the rows are read again.
    fn schema_changed(&mut self) {
        self.table_rev += 1;
        self.changed();
    }

    /// A group was opened or closed: the same rows, laid out again.
    fn folded(&mut self) {
        self.fold_rev += 1;
        self.dirty = Some(f64::NAN);
    }

    /// Something that is only kept (a width, the panel being open) changed.
    fn touched(&mut self) {
        self.dirty = Some(f64::NAN);
    }
}

/// What the user did in the list during a frame; carried out after the table is drawn.
enum Action {
    Search(String),
    Sort {
        column: ColumnId,
        add: bool,
    },
    SortExact {
        column: ColumnId,
        descending: bool,
    },
    GroupBy(Option<ColumnId>),
    ToggleGroup(Vec<Value>),
    FilterColumn {
        column: ColumnId,
        text: String,
    },
    ShowColumn {
        column: ColumnId,
        visible: bool,
    },
    MoveColumn {
        column: ColumnId,
        later: bool,
    },
    Width {
        column: ColumnId,
        width: f32,
    },
    ShowReplies(bool),
    FilterRow(bool),
    Click {
        id: AnnotId,
        reply: bool,
        modifiers: egui::Modifiers,
    },
    Status {
        id: AnnotId,
        state: ReviewState,
    },
    /// A state of a custom status set (or a review state), under its own state model.
    StatusNamed {
        id: AnnotId,
        status: Status,
    },
    Checked {
        id: AnnotId,
        checked: bool,
    },
    Custom {
        id: AnnotId,
        column: Box<CustomColumn>,
        value: Value,
    },
    BeginEdit {
        id: AnnotId,
        column: ColumnId,
        text: String,
    },
    CommitEdit,
    OpenNote(AnnotId),
    BeginReply,
    SendReply,
    CancelReply,
    DeleteRows,
    OpenColumnsDialog,
    Run(Command),
}

/// A visible column, looked up once per frame.
struct ColumnInfo {
    id: ColumnId,
    title: String,
    kind: ColumnType,
    col: Col,
    custom: Option<CustomColumn>,
    width: f32,
    /// A quantity or a worked-out number: its cells are written as pl-takeoff writes them
    /// (the unit's decimal places, a formula error's code).
    formatted: bool,
    /// The decimal places a subtotal of the column is written to.
    places: Option<u8>,
}

fn column_infos(table: &Table, layout: &Layout) -> Vec<ColumnInfo> {
    layout
        .columns
        .iter()
        .filter(|c| c.visible)
        .map(|c| {
            let kind = table.column_type(&c.column);
            let formatted = match &c.column {
                ColumnId::Builtin(b) => b.quantity_kind().is_some(),
                ColumnId::Custom(_) => matches!(kind, ColumnType::Number | ColumnType::Formula),
            };
            ColumnInfo {
                id: c.column.clone(),
                // A quantity's heading names its unit: "Length (ft)".
                title: table.heading(&c.column),
                kind,
                col: table.col(&c.column),
                // The user's columns and the document's own (Bluebeam's), as the rows hold them.
                custom: match &c.column {
                    ColumnId::Custom(id) => table.custom.iter().find(|x| x.id == *id).cloned(),
                    ColumnId::Builtin(_) => None,
                },
                width: c.width,
                formatted,
                places: table.places(&c.column),
            }
        })
        .collect()
}

/// A subtotal as the list writes it: to the column's places when it has them.
fn sum_text(places: Option<u8>, sum: f64) -> String {
    match places {
        Some(p) => format!("{sum:.*}", p as usize),
        None => number_text(sum),
    }
}

fn column_title(schema: &Schema, column: &ColumnId) -> String {
    match column {
        ColumnId::Builtin(b) => b.title().to_owned(),
        ColumnId::Custom(id) => schema
            .get(id)
            .map_or_else(|| id.clone(), |c| c.title.clone()),
    }
}

fn column_egui_id(column: &ColumnId) -> Id {
    Id::unique(("plumbline-list-column", column))
}

fn number_text(n: f64) -> String {
    Value::number(n).display()
}

fn flatten(table: &Table, view: &View, layout: &Layout) -> Vec<ListLine> {
    let sums = |subtotals: &[pl_doc::pl_takeoff::Subtotal]| -> Vec<(ColumnId, f64)> {
        subtotals
            .iter()
            .filter(|s| s.values > 0)
            .map(|s| (s.column.clone(), s.sum))
            .collect()
    };
    let mut lines: Vec<ListLine> = view
        .lines(table, layout.show_replies, &layout.collapsed)
        .into_iter()
        .map(|line| match line {
            Line::Group {
                depth,
                path,
                group,
                collapsed,
            } => ListLine::Group {
                depth,
                path: path.into_iter().cloned().collect(),
                heading: format!("{}: {}", table.title(&group.column), group.label),
                count: group.count,
                subtotals: sums(&group.subtotals),
                collapsed,
            },
            Line::Row {
                row,
                group_depth,
                reply_depth,
            } => ListLine::Row {
                row,
                group_depth,
                reply_depth,
            },
        })
        .collect();
    let totals = sums(&view.subtotals);
    if !totals.is_empty() {
        lines.push(ListLine::Total {
            count: view.count,
            subtotals: totals,
        });
    }
    lines
}

impl AppState {
    // ---- reading -----------------------------------------------------------------------------

    pub fn markups_list_open(&self) -> bool {
        self.list.open
    }

    pub fn set_markups_list_open(&mut self, open: bool) {
        if self.list.open != open {
            self.list.open = open;
            self.list.touched();
        }
    }

    /// How the list is laid out: columns, sort, grouping and filter.
    pub fn list_layout(&self) -> &Layout {
        &self.list.layout
    }

    /// Changes the list's layout, as its controls do.
    pub fn edit_list_layout(&mut self, edit: impl FnOnce(&mut Layout)) {
        edit(&mut self.list.layout);
        self.list.reconcile_layout();
        self.list.push_widths = true;
        self.list.changed();
    }

    /// The user's own columns.
    pub fn list_schema(&self) -> &Schema {
        &self.list.schema
    }

    /// Adds a column of the user's own to the list. Returns its id, or why it was refused.
    pub fn add_list_column(
        &mut self,
        title: &str,
        kind: ColumnType,
        options: &[&str],
    ) -> Result<String, String> {
        let id = self
            .list
            .schema
            .add(title, kind, options)
            .map_err(|e| e.to_string())?;
        self.list.reconcile_layout();
        self.list.push_widths = true;
        self.list.schema_changed();
        Ok(id)
    }

    /// The active document's own columns (Bluebeam's `/BSIAnnotColumns`), as the list shows
    /// them after the user's.
    pub fn document_columns(&self) -> &[CustomColumn] {
        &self.list.doc_columns
    }

    /// Whether the active document has Bluebeam custom columns.
    pub fn active_has_document_columns(&self) -> bool {
        self.active_document()
            .is_some_and(|d| d.has_bluebeam_columns())
    }

    /// Adds a column to the active document itself, in Bluebeam Revu's `/BSIAnnotColumns`
    /// (every markup's `/BSIColumnData` is padded to it), as one undo step. Returns its id, or
    /// why it was refused.
    pub fn add_document_column(
        &mut self,
        title: &str,
        kind: ColumnType,
        options: &[&str],
        multiline: bool,
    ) -> Result<String, String> {
        let doc_id = self
            .active_doc_id()
            .ok_or_else(|| "no document is open".to_string())?;
        if !self.ensure_annots(doc_id) {
            return Err("the markups are not read yet".to_string());
        }
        let editor = self
            .session
            .document(doc_id)
            .ok()
            .and_then(|d| d.editor())
            .ok_or_else(|| "the markups are not read yet".to_string())?;
        let (command, column) = pl_doc::markup_ops::add_bluebeam_column(
            editor.set(),
            editor.scales().bluebeam(),
            title,
            kind,
            options,
            multiline,
        )
        .map_err(|e| e.to_string())?;
        if self.exec(doc_id, command, None) {
            Ok(column.id)
        } else {
            Err(self
                .chrome
                .status
                .clone()
                .unwrap_or_else(|| "the column could not be added".to_string()))
        }
    }

    /// Adds a formula column to the list. Returns its id, or why it was refused: the
    /// formula's parse error or a column it names that the list does not have, as the
    /// formula dialog shows them.
    pub fn add_formula_column(&mut self, title: &str, formula: &str) -> Result<String, String> {
        if let Some(problem) = crate::measure::formula_problem(&self.list.schema, title, formula) {
            return Err(problem);
        }
        let id = self
            .list
            .schema
            .add_formula(title, formula)
            .map_err(|e| e.to_string())?;
        self.list.reconcile_layout();
        self.list.push_widths = true;
        self.list.schema_changed();
        Ok(id)
    }

    /// Adds a choice column whose options may stand for numbers (`Steel = 12.5`).
    pub fn add_choice_column(
        &mut self,
        title: &str,
        options: &[(&str, Option<f64>)],
    ) -> Result<String, String> {
        let id = self
            .list
            .schema
            .add_choice(title, options)
            .map_err(|e| e.to_string())?;
        self.list.reconcile_layout();
        self.list.push_widths = true;
        self.list.schema_changed();
        Ok(id)
    }

    /// Opens the dialog that adds a formula column.
    pub fn open_formula_dialog(&mut self) {
        if self.list.formula_dialog.is_none() {
            self.list.formula_dialog = Some(FormulaDialog::default());
        }
        self.set_markups_list_open(true);
    }

    pub fn formula_dialog_open(&self) -> bool {
        self.list.formula_dialog.is_some()
    }

    /// What the formula dialog says is wrong with the formula typed in it.
    pub fn formula_dialog_message(&self) -> Option<String> {
        self.list
            .formula_dialog
            .as_ref()
            .and_then(|d| d.message.clone())
    }

    /// Removes one of the user's columns from the list. The values stored in the markups stay
    /// in the file.
    pub fn remove_list_column(&mut self, id: &str) -> bool {
        if self.list.schema.remove(id).is_none() {
            return false;
        }
        self.list.reconcile_layout();
        self.list.schema_changed();
        true
    }

    pub fn list_stats(&self) -> ListStats {
        self.list.stats
    }

    /// The list line by line, as the panel shows it for the active document.
    pub fn list_lines(&mut self) -> Vec<ListLineView> {
        if !self.refresh_list() {
            return Vec::new();
        }
        let selected: HashSet<AnnotId> = self
            .active_document()
            .map(|d| d.selection().iter().cloned().collect())
            .unwrap_or_default();
        let Some(cache) = &self.list.cache else {
            return Vec::new();
        };
        let columns = column_infos(&cache.table, &self.list.layout);
        let sums = |subtotals: &[(ColumnId, f64)]| -> Vec<String> {
            columns
                .iter()
                .map(|c| {
                    subtotals
                        .iter()
                        .find(|(id, _)| *id == c.id)
                        .map(|(_, sum)| sum_text(c.places, *sum))
                        .unwrap_or_default()
                })
                .collect()
        };
        cache
            .lines
            .iter()
            .map(|line| match line {
                ListLine::Group {
                    heading,
                    count,
                    subtotals,
                    ..
                } => ListLineView {
                    id: None,
                    group: Some(heading.clone()),
                    count: *count,
                    reply_depth: 0,
                    cells: sums(subtotals),
                    selected: false,
                },
                ListLine::Total { count, subtotals } => ListLineView {
                    id: None,
                    group: None,
                    count: *count,
                    reply_depth: 0,
                    cells: sums(subtotals),
                    selected: false,
                },
                ListLine::Row {
                    row, reply_depth, ..
                } => {
                    let r = &cache.table.rows[*row];
                    ListLineView {
                        id: Some(r.id.clone()),
                        group: None,
                        count: 1,
                        reply_depth: *reply_depth,
                        cells: columns
                            .iter()
                            .map(|c| {
                                if c.formatted {
                                    return cache.table.cell_text(*row, &c.id);
                                }
                                let mut text = String::new();
                                r.display_into(c.col, &mut text);
                                text
                            })
                            .collect(),
                        selected: selected.contains(&r.id)
                            || self.list.selected_replies.contains(&r.id),
                    }
                }
            })
            .collect()
    }

    // ---- keeping ------------------------------------------------------------------------------

    /// Writes the list's file if something changed since it was last written.
    pub fn flush_list(&mut self) {
        if self.list.dirty.take().is_some() {
            if let Err(e) = self.list.store() {
                self.chrome.status = Some(format!("The list layout could not be saved: {e}"));
            }
        }
    }

    /// Once per frame: writes the list's file when the last change has settled.
    pub(crate) fn settle_list(&mut self, ctx: &egui::Context) {
        let Some(since) = self.list.dirty else {
            return;
        };
        let (now, held) = ctx.input(|i| (i.time, i.pointer.any_down()));
        let since = if since.is_finite() { since } else { now };
        self.list.dirty = Some(since);
        let age = now - since;
        if age >= WRITE_DELAY && !held {
            self.flush_list();
        } else {
            let wait = (WRITE_DELAY - age).clamp(0.05, WRITE_DELAY);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }

    // ---- the rows -----------------------------------------------------------------------------

    /// Brings the list in line with the active document. Returns whether there is a list to
    /// show (a document whose annotations have been read).
    pub(crate) fn refresh_list(&mut self) -> bool {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            self.list.cache = None;
            return false;
        };
        let Ok(doc) = self.session.document(doc_id) else {
            self.list.cache = None;
            return false;
        };
        let Some(editor) = doc.editor() else {
            self.list.cache = None;
            return false;
        };
        let revision = doc.edit_revision();
        let (rev, table_rev, fold_rev) = (self.list.rev, self.list.table_rev, self.list.fold_rev);
        let same_table =
            |c: &ListCache| c.doc == doc_id && c.revision == revision && c.table_rev == table_rev;
        let fresh = self
            .list
            .cache
            .as_ref()
            .is_some_and(|c| same_table(c) && c.rev == rev && c.fold_rev == fold_rev);
        if !fresh {
            // Only what changed is made again: the rows when the document or the user's
            // columns did, the order and the groups when the layout did, and for a group
            // that was opened or closed just the lines.
            let (table, view) = match self.list.cache.take() {
                Some(c) if same_table(&c) => {
                    let view = if c.rev == rev {
                        c.view
                    } else {
                        self.list.stats.views += 1;
                        View::of(&c.table, &self.list.layout)
                    };
                    (c.table, view)
                }
                _ => {
                    // The document's own columns and status sets come with it: shown as soon
                    // as it is open, gone with it.
                    let doc_columns = doc.bluebeam_columns();
                    let status_sets = doc.status_sets();
                    if doc_columns != self.list.doc_columns {
                        self.list.doc_columns = doc_columns;
                        self.list.reconcile_layout();
                        self.list.push_widths = true;
                        self.list.rev += 1;
                    }
                    self.list.status_sets = status_sets;
                    let layers: Vec<(pl_doc::pl_pdf::ObjectId, String)> = doc
                        .cached_layers()
                        .map(|layers| layers.iter().map(|l| (l.id, l.name.clone())).collect())
                        .unwrap_or_default();
                    let schema = self.list.effective();
                    let context = ListContext {
                        layers: &layers,
                        schema: Some(&schema),
                        ..ListContext::default()
                    };
                    // The spaces the document settings define count besides the polygons
                    // marked as spaces.
                    let spaces = crate::measure::settings_spaces(editor.scales().settings());
                    let units =
                        crate::measure::list_units(&self.measure.list_units, doc_id, editor);
                    let takeoff = pl_doc::pl_takeoff::TakeoffContext {
                        spaces: &spaces,
                        units: Some(units),
                        ..pl_doc::pl_takeoff::TakeoffContext::default()
                    };
                    let table = Table::build_takeoff(editor.set().iter(), &context, &takeoff);
                    let view = View::of(&table, &self.list.layout);
                    self.list.stats.tables += 1;
                    self.list.stats.views += 1;
                    (table, view)
                }
            };
            let lines = flatten(&table, &view, &self.list.layout);
            let mut line_of = HashMap::with_capacity(lines.len());
            for (i, line) in lines.iter().enumerate() {
                if let ListLine::Row { row, .. } = line {
                    line_of.insert(table.rows[*row].id.clone(), i);
                }
            }
            self.list.stats.builds += 1;
            self.list.stats.lines = lines.len();
            self.list.cache = Some(ListCache {
                doc: doc_id,
                revision,
                table_rev,
                rev,
                fold_rev,
                table,
                view,
                lines,
                line_of,
            });
            // Rows that are gone cannot stay selected or be edited.
            if let Some(cache) = &self.list.cache {
                self.list
                    .selected_replies
                    .retain(|id| cache.line_of.contains_key(id));
            }
        }
        // A selection made on the page: the list scrolls to its first row. (The selection is
        // known by its serial, so a large one is not compared id by id every frame.)
        let seen = Some((doc_id, doc.selection_serial()));
        if seen != self.list.last_selection {
            self.list.last_selection = seen;
            if let (Some(first), Some(cache)) = (doc.selection().first(), &self.list.cache) {
                self.list.scroll_to = cache.line_of.get(first).map(|line| *line as u64);
            }
        }
        true
    }

    // ---- actions ------------------------------------------------------------------------------

    fn list_selected_markups(&self) -> Vec<AnnotId> {
        self.active_document()
            .map(|d| d.selection().to_vec())
            .unwrap_or_default()
    }

    /// How many markups are selected, without copying them.
    fn list_selected_count(&self) -> usize {
        self.active_document().map_or(0, |d| d.selection().len())
    }

    /// The selected markups as a set. It is made again only when the selection has changed,
    /// not every frame.
    fn list_selected_set(&mut self) -> std::sync::Arc<HashSet<AnnotId>> {
        let Some(doc) = self.active_document() else {
            return std::sync::Arc::default();
        };
        let key = (doc.id(), doc.selection_serial());
        if let Some((id, serial, set)) = &self.list.selected_set {
            if (*id, *serial) == key {
                return set.clone();
            }
        }
        let set: std::sync::Arc<HashSet<AnnotId>> =
            std::sync::Arc::new(doc.selection().iter().cloned().collect());
        self.list.selected_set = Some((key.0, key.1, set.clone()));
        set
    }

    /// The list made the document's present selection: it does not scroll itself to it.
    fn list_saw_selection(&mut self, doc_id: DocumentId) {
        self.list.last_selection = self
            .session
            .document(doc_id)
            .ok()
            .map(|d| (doc_id, d.selection_serial()));
    }

    fn list_click(&mut self, id: AnnotId, reply: bool, modifiers: egui::Modifiers) {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return;
        };
        let toggle = modifiers.command || modifiers.ctrl;
        if reply {
            if toggle {
                match self.list.selected_replies.iter().position(|r| *r == id) {
                    Some(i) => {
                        self.list.selected_replies.remove(i);
                    }
                    None => self.list.selected_replies.push(id),
                }
            } else {
                self.list.selected_replies = vec![id];
                if let Ok(doc) = self.session.document_mut(doc_id) {
                    doc.clear_selection();
                }
                self.list_saw_selection(doc_id);
            }
            return;
        }
        let current = self.list_selected_markups();
        let ids: Vec<AnnotId> = if toggle {
            let mut ids = current;
            match ids.iter().position(|i| *i == id) {
                Some(i) => {
                    ids.remove(i);
                }
                None => ids.push(id.clone()),
            }
            ids
        } else if modifiers.shift {
            // Every markup between the anchor and this row, as the list shows them.
            let range = self.list.cache.as_ref().and_then(|cache| {
                let anchor = self.list.anchor.as_ref().or(current.first())?;
                let a = *cache.line_of.get(anchor)?;
                let b = *cache.line_of.get(&id)?;
                let (from, to) = (a.min(b), a.max(b));
                Some(
                    cache.lines[from..=to]
                        .iter()
                        .filter_map(|line| match line {
                            ListLine::Row {
                                row,
                                reply_depth: 0,
                                ..
                            } => Some(cache.table.rows[*row].id.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                )
            });
            range.unwrap_or_else(|| vec![id.clone()])
        } else {
            vec![id.clone()]
        };
        if !modifiers.shift {
            self.list.anchor = Some(id.clone());
        }
        if !toggle {
            self.list.selected_replies.clear();
        }
        // The page shows the selection with its handles, and the clicked markup comes into
        // view.
        self.commit_text_edit();
        self.cancel_gesture();
        self.set_tool(Tool::Select);
        let place = self.session.document(doc_id).ok().and_then(|d| {
            let a = d.annotation(&id)?;
            let page = a.page as u32;
            Some((page, a.bounds(), d.geometry_is_exact(page)))
        });
        if let Ok(doc) = self.session.document_mut(doc_id) {
            let _ = doc.select(ids.clone());
        }
        // The list made this selection: it does not scroll itself to it.
        self.list_saw_selection(doc_id);
        if let (Some((page, bounds, exact)), false) = (place, toggle || modifiers.shift) {
            if let Some(dv) = self.active_view_mut() {
                if exact {
                    dv.view.reveal_rect(page, bounds, 60.0);
                } else {
                    // Page coordinates mean nothing until the page's real geometry is in:
                    // the view goes to the page, and to the markup's place when it is known.
                    let centre = bounds.center();
                    dv.go_to_destination(page, Some(centre.x), Some(centre.y), false);
                }
            }
        }
        self.request_repaint_soon();
    }

    /// Deletes the rows selected in the list: the selected markups and the selected replies.
    pub fn delete_list_rows(&mut self) -> usize {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return 0;
        };
        // One command, so one undo brings every row back: the markups first, then the
        // replies. A reply whose markup (or the reply it answers) has gone by then went with
        // it, and the editor passes over it.
        let replies = std::mem::take(&mut self.list.selected_replies);
        let (mut ids, locked) = self.removable_selection(doc_id);
        self.report_locked(locked);
        let Ok(doc) = self.session.document(doc_id) else {
            return 0;
        };
        ids.extend(replies.into_iter().filter(|id| {
            doc.annotation(id)
                .is_some_and(|a| a.meta.reply_to.is_some())
        }));
        if ids.is_empty() {
            return 0;
        }
        let rows = ids.len();
        if self.exec(doc_id, DocCommand::DeleteAnnotations(ids), None) {
            rows
        } else {
            0
        }
    }

    fn list_commit_edit(&mut self) {
        let Some(edit) = self.list.editing.take() else {
            return;
        };
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return;
        };
        let when = PdfDate::now();
        let result = match &edit.column {
            ColumnId::Builtin(Builtin::Comment) => {
                let command = self
                    .session
                    .document(doc_id)
                    .ok()
                    .and_then(|d| d.editor())
                    .map(|e| pl_doc::markup_ops::set_text(e.set(), &edit.id, &edit.text, when));
                match command {
                    Some(Ok(command)) => {
                        self.exec(doc_id, command, None);
                        Ok(())
                    }
                    Some(Err(e)) => Err(e.to_string()),
                    None => Ok(()),
                }
            }
            ColumnId::Builtin(Builtin::Subject) => {
                // Nothing is changed, stamped or recorded when the subject is as it was
                // (clicking into the cell and away again commits it unchanged).
                let command = self
                    .session
                    .document(doc_id)
                    .ok()
                    .and_then(|d| d.editor())
                    .map(|e| pl_doc::markup_ops::set_subject(e.set(), &edit.id, &edit.text, when));
                match command {
                    Some(Ok(command)) => {
                        self.exec(doc_id, command, None);
                        Ok(())
                    }
                    Some(Err(e)) => Err(e.to_string()),
                    None => Ok(()),
                }
            }
            ColumnId::Custom(column) => match self.list.column(column) {
                Some(column) => self.list_set_custom_text(doc_id, &edit.id, &column, &edit.text),
                None => Ok(()),
            },
            ColumnId::Builtin(_) => Ok(()),
        };
        if let Err(e) = result {
            self.chrome.status = Some(format!("That value was not taken: {e}"));
        }
    }

    fn list_set_custom_text(
        &mut self,
        doc_id: DocumentId,
        id: &AnnotId,
        column: &CustomColumn,
        text: &str,
    ) -> Result<(), String> {
        let doc = self
            .session
            .document_mut(doc_id)
            .map_err(|e| e.to_string())?;
        doc.set_column_text(id, column, text, PdfDate::now())
            .map_err(|e| e.to_string())?;
        self.after_edit(doc_id);
        Ok(())
    }

    fn list_apply(&mut self, ctx: &egui::Context, action: Action) {
        let doc_id = self.active_view().map(|v| v.doc);
        match action {
            Action::Search(text) => {
                self.list.layout.filter.search = text;
                self.list.changed();
            }
            Action::Sort { column, add } => {
                self.list.layout.toggle_sort(&column, add);
                self.list.changed();
            }
            Action::SortExact { column, descending } => {
                self.list.layout.sort = vec![pl_doc::pl_takeoff::SortKey { column, descending }];
                self.list.changed();
            }
            Action::GroupBy(column) => {
                self.list.layout.group = column.into_iter().map(GroupKey::asc).collect();
                self.list.layout.collapsed.clear();
                self.list.changed();
            }
            Action::ToggleGroup(path) => {
                let same = |p: &Vec<Value>| {
                    p.len() == path.len() && p.iter().zip(&path).all(|(a, b)| a.same(b))
                };
                match self.list.layout.collapsed.iter().position(same) {
                    Some(i) => {
                        self.list.layout.collapsed.remove(i);
                    }
                    None => self.list.layout.collapsed.push(path),
                }
                self.list.folded();
            }
            Action::FilterColumn { column, text } => {
                let conditions = &mut self.list.layout.filter.conditions;
                conditions.retain(|c| {
                    !(c.column == column && matches!(c.condition, Condition::Contains(_)))
                });
                if !text.is_empty() {
                    conditions.push(ColumnFilter::new(column, Condition::Contains(text)));
                }
                self.list.changed();
            }
            Action::ShowColumn { column, visible } => {
                self.list.layout.set_visible(&column, visible);
                self.list.push_widths = true;
                self.list.changed();
            }
            Action::MoveColumn { column, later } => {
                // Past the next (or before the previous) visible column.
                let order: Vec<ColumnId> = self.list.layout.visible();
                if let Some(at) = order.iter().position(|c| *c == column) {
                    if later && at + 1 < order.len() {
                        let before = order.get(at + 2).cloned();
                        self.list.layout.move_column(&column, before.as_ref());
                    } else if !later && at > 0 {
                        let before = order[at - 1].clone();
                        self.list.layout.move_column(&column, Some(&before));
                    }
                }
                self.list.changed();
            }
            Action::Width { column, width } => {
                self.list.layout.set_width(&column, width);
                self.list.touched();
            }
            Action::ShowReplies(show) => {
                self.list.layout.show_replies = show;
                self.list.changed();
            }
            Action::FilterRow(show) => {
                self.list.filter_row = show;
                self.list.touched();
            }
            Action::Click {
                id,
                reply,
                modifiers,
            } => {
                self.list_commit_edit();
                self.list_click(id, reply, modifiers);
            }
            Action::Status { id, state } => {
                if let Some(doc_id) = doc_id {
                    self.set_review_status(doc_id, &id, state);
                }
            }
            Action::StatusNamed { id, status } => {
                if let Some(doc_id) = doc_id {
                    self.set_named_status(doc_id, &id, &status);
                }
            }
            Action::Checked { id, checked } => {
                let origin = Origin::now(self.markup.author.as_str());
                if let Some(doc) = doc_id.and_then(|d| self.session.document_mut(d).ok()) {
                    match doc.set_checked(&id, checked, &origin) {
                        Ok(_) => self.after_edit(doc_id.expect("checked")),
                        Err(e) => {
                            self.chrome.status = Some(format!("The checkmark was not set: {e}"));
                        }
                    }
                }
            }
            Action::Custom { id, column, value } => {
                if let Some(doc) = doc_id.and_then(|d| self.session.document_mut(d).ok()) {
                    match doc.set_column(&id, &column, value, PdfDate::now()) {
                        Ok(_) => self.after_edit(doc_id.expect("checked")),
                        Err(e) => {
                            self.chrome.status = Some(format!("That value was not taken: {e}"));
                        }
                    }
                }
            }
            Action::BeginEdit { id, column, text } => {
                self.list_commit_edit();
                self.list.editing = Some(CellEdit {
                    id,
                    column,
                    text,
                    focus: true,
                });
            }
            Action::CommitEdit => self.list_commit_edit(),
            Action::OpenNote(id) => {
                self.open_note(&id);
            }
            Action::BeginReply => {
                let parent = self
                    .list
                    .selected_replies
                    .first()
                    .cloned()
                    .or_else(|| self.list_selected_markups().first().cloned());
                if let Some(parent) = parent {
                    self.list.reply = Some(ReplyDraft {
                        parent,
                        text: String::new(),
                        focus: true,
                    });
                }
            }
            Action::SendReply => {
                if let (Some(reply), Some(doc_id)) = (self.list.reply.take(), doc_id) {
                    let text = reply.text.trim().to_owned();
                    if text.is_empty() || !self.add_reply_to(doc_id, &reply.parent, &text) {
                        self.list.reply = Some(reply);
                    }
                }
            }
            Action::CancelReply => self.list.reply = None,
            Action::DeleteRows => {
                self.delete_list_rows();
            }
            Action::OpenColumnsDialog => {
                self.list.columns_dialog = Some(ColumnsDialog::default());
            }
            Action::Run(command) => self.run(ctx, command),
        }
    }

    /// Escape in the list: stops the cell edit or the reply being written. Returns whether
    /// there was one.
    pub(crate) fn list_dismiss(&mut self) -> bool {
        if self.list.editing.take().is_some() {
            return true;
        }
        self.list.reply.take().is_some()
    }

    // ---- export and import --------------------------------------------------------------------

    /// Asks for a file and writes the list (CSV, XLSX) or the markups themselves (XFDF) to
    /// it. Returns the file written.
    pub fn export_markups(&mut self, kind: ExportKind) -> Option<PathBuf> {
        let doc_id = self.active_view().map(|v| v.doc)?;
        if !self.ensure_annots(doc_id) || !self.refresh_list() {
            return None;
        }
        let name = self.export_name(kind.extension());
        let path = self.dialogs.save_export(kind, &name)?;
        let cache = self.list.cache.as_ref()?;
        let mut note = String::new();
        let bytes: Result<Vec<u8>, String> = match kind {
            ExportKind::Csv => Ok(to_csv(
                &cache.table,
                &cache.view,
                &self.list.layout,
                &CsvOptions::default(),
            )
            .into_bytes()),
            ExportKind::Xlsx => {
                to_xlsx(&cache.table, &cache.view, &self.list.layout).map_err(|e| e.to_string())
            }
            ExportKind::Xfdf => {
                let doc = self.session.document(doc_id).ok()?;
                let editor = doc.editor()?;
                let href = doc.path().map(display_name);
                let export = export_xfdf(editor.set().iter(), &XfdfOptions { href });
                if !export.skipped.is_empty() {
                    note = format!(
                        " ({} of a kind XFDF cannot carry were left out)",
                        export.skipped.len()
                    );
                }
                Ok(export.xml.into_bytes())
            }
        };
        let count = cache.view.count;
        let written = bytes.and_then(|bytes| {
            let tmp = path.with_extension(format!("{}.tmp", kind.extension()));
            std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
        });
        match written {
            Ok(()) => {
                self.chrome.status = Some(match kind {
                    ExportKind::Xfdf => {
                        format!("Exported the markups to {}{note}", display_name(&path))
                    }
                    _ => format!("Exported {count} markup(s) to {}", display_name(&path)),
                });
                Some(path)
            }
            Err(e) => {
                self.chrome.status =
                    Some(format!("{} could not be written: {e}", display_name(&path)));
                None
            }
        }
    }

    /// Asks for an XFDF file and adds its markups to the active document, as one undo step.
    /// Markups for pages the document does not have, and markups it has already (by name),
    /// are left out. Returns how many were added.
    pub fn import_xfdf(&mut self) -> usize {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return 0;
        };
        if !self.ensure_annots(doc_id) {
            return 0;
        }
        let Some(path) = self.dialogs.pick_xfdf() else {
            return 0;
        };
        let name = display_name(&path);
        // The file is read and parsed here, in the frame: one of unreasonable size is refused
        // before it is read.
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_XFDF_BYTES {
            self.chrome.status = Some(format!(
                "{name} is too large to import ({} MB; the limit is {} MB)",
                size / (1024 * 1024),
                MAX_XFDF_BYTES / (1024 * 1024)
            ));
            return 0;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                self.chrome.status = Some(format!("{name} could not be read: {e}"));
                return 0;
            }
        };
        let import = match import_xfdf(&text) {
            Ok(import) => import,
            Err(e) => {
                self.chrome.status = Some(format!("{name} is not an XFDF file: {e}"));
                return 0;
            }
        };
        let Ok(doc) = self.session.document(doc_id) else {
            return 0;
        };
        let pages = doc.page_count() as usize;
        let total = import.annotations.len();
        // Each markup is taken or left out on its own: one the editor would refuse (text
        // outside what a text box can show, a name that is in the file twice or in the
        // document already, a page that is not there) does not keep the others out.
        let mut names: HashSet<AnnotId> = HashSet::new();
        let mut taken: Vec<pl_doc::pl_annot::Annotation> = import
            .annotations
            .into_iter()
            .filter(|a| a.page < pages && doc.annotation(&a.id).is_none())
            .filter(|a| doc.check_addable(a).is_ok())
            .filter(|a| names.insert(a.id.clone()))
            .collect();
        // A popup whose markup was left out has nothing to belong to, and neither has a
        // reply or a status whose markup was.
        let here = |id: &AnnotId| names.contains(id) || doc.annotation(id).is_some();
        taken.retain(|a| {
            (a.kind != AnnotKind::Popup || a.parent.as_ref().is_some_and(here))
                && a.meta.reply_to.as_ref().is_none_or(here)
        });
        let left_out = total - taken.len() + import.skipped.len();
        let count = taken.len();
        if count == 0 {
            self.chrome.status = Some(format!("{name} holds no markups that are new here"));
            return 0;
        }
        if !self.exec(doc_id, DocCommand::AddAnnotations(taken), None) {
            return 0;
        }
        self.chrome.status = Some(if left_out > 0 {
            format!("Imported {count} markup(s) from {name}; {left_out} were left out")
        } else {
            format!("Imported {count} markup(s) from {name}")
        });
        count
    }
}

// ---- the panel -------------------------------------------------------------------------------

/// The largest XFDF file the import reads. A file of markups is kilobytes; even one for a
/// whole set of sheets is a few megabytes.
pub const MAX_XFDF_BYTES: u64 = 64 * 1024 * 1024;

/// The markups list, docked at the bottom of the window.
pub fn panel(app: &mut AppState, ui: &mut Ui) {
    let mut actions: Vec<Action> = Vec::new();
    Panel::bottom("plumbline-markups")
        .resizable(true)
        .default_size(232.0)
        .size_range(120.0..=560.0)
        .show(ui, |ui| {
            // The list is of the annotations: they are read (on a worker thread) for it.
            if let Some(doc) = app.active_view().map(|v| v.doc) {
                app.want_annots(doc);
            }
            let ready = app.refresh_list();
            let editable = app.active_editable();
            ui.add_space(2.0);
            strip(app, ui, ready, &mut actions);
            reply_strip(app, ui, &mut actions);
            ui.add_space(1.0);
            if ready {
                ui.add_enabled_ui(editable, |ui| table(app, ui, &mut actions));
            } else {
                app.list.stats.cells = 0;
                app.list.stats.rows = 0;
                ui.add_space(8.0);
                let text = if app.active.is_none() {
                    "Open a document to list its markups."
                } else {
                    "Reading this document's markups\u{2026}"
                };
                ui.weak(text);
                // Take up the panel, so it keeps its size.
                ui.allocate_space(ui.available_size());
            }
        });
    let ctx = ui.ctx().clone();
    for action in actions {
        app.list_apply(&ctx, action);
    }
    columns_dialog(app, &ctx);
}

fn strip(app: &mut AppState, ui: &mut Ui, ready: bool, actions: &mut Vec<Action>) {
    let muted = Palette::of(ui.visuals()).muted_text;
    let (count, total) = app
        .list
        .cache
        .as_ref()
        .map_or((0, 0), |c| (c.view.count, c.view.total));
    let selected_markups = app.list_selected_count();
    let selected = selected_markups + app.list.selected_replies.len();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Markups").font(theme::bold(13.0)));
        if ready {
            let text = if count == total {
                format!("{total}")
            } else {
                format!("{count} of {total}")
            };
            ui.label(RichText::new(text).color(muted));
        }
        ui.separator();

        let mut search = app.list.layout.filter.search.clone();
        let response = ui
            .add(
                TextEdit::singleline(&mut search)
                    .id(Id::unique("plumbline-list-search"))
                    .desired_width(150.0)
                    .hint_text("Search"),
            )
            .accessible_name("Search markups");
        if response.changed() {
            actions.push(Action::Search(search));
        }

        let grouped = app.list.layout.group.first().map(|k| k.column.clone());
        let current = grouped.as_ref().map_or_else(
            || "No grouping".to_owned(),
            |c| column_title(&app.list.effective(), c),
        );
        egui::ComboBox::from_id_salt("plumbline-list-group")
            .selected_text(current)
            .width(118.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(grouped.is_none(), "No grouping")
                    .clicked()
                {
                    actions.push(Action::GroupBy(None));
                }
                for state in &app.list.layout.columns {
                    let title = column_title(&app.list.effective(), &state.column);
                    let on = grouped.as_ref() == Some(&state.column);
                    if ui.selectable_label(on, title).clicked() {
                        actions.push(Action::GroupBy(Some(state.column.clone())));
                    }
                }
            })
            .response
            .accessible_name("Group by")
            .on_hover_text("Group the markups by a column");

        let mut filter_row = app.list.filter_row;
        if ui
            .toggle_value(&mut filter_row, "Filter row")
            .on_hover_text("A box under each heading that filters its column")
            .changed()
        {
            actions.push(Action::FilterRow(filter_row));
        }
        let mut replies = app.list.layout.show_replies;
        if ui
            .toggle_value(&mut replies, "Replies")
            .on_hover_text("Show replies as rows under their markup")
            .changed()
        {
            actions.push(Action::ShowReplies(replies));
        }

        // Several columns are usually ticked in one go: the menu stays open until a click
        // outside it.
        let stay_open = egui::containers::menu::MenuConfig::new()
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        egui::containers::menu::MenuButton::new("Columns")
            .config(stay_open)
            .ui(ui, |ui| {
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("plumbline-list-columns")
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for state in &app.list.layout.columns {
                            let title = column_title(&app.list.effective(), &state.column);
                            ui.horizontal(|ui| {
                                let mut visible = state.visible;
                                if ui.checkbox(&mut visible, title.as_str()).changed() {
                                    actions.push(Action::ShowColumn {
                                        column: state.column.clone(),
                                        visible,
                                    });
                                }
                                if state.visible {
                                    ui.with_layout(UiLayout::right_to_left(Align::Center), |ui| {
                                        if icon_button(
                                            ui,
                                            Icon::ChevronDown,
                                            &format!("Move {title} later"),
                                            false,
                                        )
                                        .clicked()
                                        {
                                            actions.push(Action::MoveColumn {
                                                column: state.column.clone(),
                                                later: true,
                                            });
                                        }
                                        if icon_button(
                                            ui,
                                            Icon::ChevronUp,
                                            &format!("Move {title} earlier"),
                                            false,
                                        )
                                        .clicked()
                                        {
                                            actions.push(Action::MoveColumn {
                                                column: state.column.clone(),
                                                later: false,
                                            });
                                        }
                                    });
                                }
                            });
                        }
                    });
                theme::name_scroll_bars(ui.ctx(), scroll.id, "Columns");
                ui.separator();
                if ui.button("Custom columns\u{2026}").clicked() {
                    actions.push(Action::OpenColumnsDialog);
                    ui.close();
                }
                if ui.button("Formula column\u{2026}").clicked() {
                    actions.push(Action::Run(Command::FormulaColumn));
                    ui.close();
                }
                if ui
                    .button("Takeoff layout")
                    .on_hover_text("The quantity columns, grouped by subject with totals")
                    .clicked()
                {
                    actions.push(Action::Run(Command::TakeoffLayout));
                    ui.close();
                }
            });

        ui.menu_button("Export", |ui| {
            for kind in ExportKind::ALL {
                let text = format!("Export as {}\u{2026}", kind.label());
                if ui.add_enabled(ready, Button::new(text)).clicked() {
                    actions.push(Action::Run(Command::ExportMarkups(kind)));
                    ui.close();
                }
            }
            ui.separator();
            if ui
                .add_enabled(ready, Button::new("Import XFDF\u{2026}"))
                .clicked()
            {
                actions.push(Action::Run(Command::ImportXfdf));
                ui.close();
            }
        });

        let editable = app.active_editable();
        if ui
            .add_enabled(editable && selected == 1, Button::new("Add reply"))
            .on_hover_text("Reply to the selected row")
            .clicked()
        {
            actions.push(Action::BeginReply);
        }
        if ui
            .add_enabled(editable && selected > 0, Button::new("Delete rows"))
            .on_hover_text("Delete the selected markups and replies")
            .clicked()
        {
            actions.push(Action::DeleteRows);
        }

        ui.with_layout(UiLayout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, Icon::Cross, "Close markups list", false).clicked() {
                actions.push(Action::Run(Command::ToggleMarkupsList));
            }
        });
    });
}

fn reply_strip(app: &mut AppState, ui: &mut Ui, actions: &mut Vec<Action>) {
    let Some(reply) = app.list.reply.as_mut() else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label("Reply");
        let response = ui
            .add(
                TextEdit::singleline(&mut reply.text)
                    .id(Id::unique("plumbline-list-reply"))
                    .desired_width(320.0)
                    .hint_text("Write a reply"),
            )
            .accessible_name("Reply to the selected row");
        let pointer_busy = ui.input(|i| i.pointer.any_down() || i.pointer.any_released());
        if reply.focus && !pointer_busy {
            response.request_focus();
            reply.focus = false;
        } else if reply.focus {
            ui.ctx().request_repaint();
        }
        let entered = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if ui.button("Send reply").clicked() || entered {
            actions.push(Action::SendReply);
        }
        if ui.button("Cancel reply").clicked() {
            actions.push(Action::CancelReply);
        }
    });
}

fn table(app: &mut AppState, ui: &mut Ui, actions: &mut Vec<Action>) {
    let Some(cache) = app.list.cache.take() else {
        return;
    };
    let columns = column_infos(&cache.table, &app.list.layout);
    if columns.is_empty() {
        ui.add_space(8.0);
        ui.weak("Every column is hidden. Show some from the Columns menu.");
        ui.allocate_space(ui.available_size());
        app.list.cache = Some(cache);
        return;
    }
    let selected = app.list_selected_set();
    let mut editing = app.list.editing.take();
    let mut headers = vec![HeaderRow::new(HEADER_HEIGHT)];
    if app.list.filter_row {
        headers.push(HeaderRow::new(HEADER_HEIGHT));
    }
    let mut table = egui_table::Table::new()
        .id_salt(TABLE_SALT)
        .num_rows(cache.lines.len() as u64)
        .columns(
            columns
                .iter()
                .map(|c| {
                    Column::new(c.width)
                        .range(Layout::MIN_WIDTH..=Layout::MAX_WIDTH)
                        .id(column_egui_id(&c.id))
                        .resizable(true)
                })
                .collect::<Vec<Column>>(),
        )
        .headers(headers)
        .num_sticky_cols(0);
    if let Some(line) = app.list.scroll_to.take() {
        if line < cache.lines.len() as u64 {
            table = table.scroll_to_row(line, None);
        }
    }
    // The widths are the layout's: the table is given them before it is first shown (it would
    // size the columns to their content otherwise) and whenever they were set from outside.
    let table_id = table.get_id(ui);
    let state = egui_table::TableState::load(ui.ctx(), table_id);
    if state.is_none() || app.list.push_widths {
        let mut state = state.unwrap_or_default();
        for c in &columns {
            state.col_widths.insert(column_egui_id(&c.id), c.width);
        }
        state.store(ui.ctx(), table_id);
        app.list.push_widths = false;
    }

    let palette = Palette::of(ui.visuals());
    let mut delegate = Delegate {
        table: &cache.table,
        lines: &cache.lines,
        columns: &columns,
        layout: &app.list.layout,
        selected: &selected,
        selected_replies: &app.list.selected_replies,
        editing: &mut editing,
        status_sets: &app.list.status_sets,
        palette,
        actions: Vec::new(),
        cells: 0,
        rows: 0..0,
        scratch: String::new(),
        scroll_area: None,
    };
    table.show(ui, &mut delegate);
    if let Some(area) = delegate.scroll_area {
        theme::name_scroll_bars(ui.ctx(), area, "Markups list");
    }
    let (cells, rows) = (delegate.cells, delegate.rows.clone());
    actions.append(&mut delegate.actions);

    // A heading's edge was dragged: the layout follows.
    if let Some(state) = egui_table::TableState::load(ui.ctx(), table_id) {
        for c in &columns {
            if let Some(width) = state.col_widths.get(&column_egui_id(&c.id)) {
                if (width - c.width).abs() > 0.5 {
                    actions.push(Action::Width {
                        column: c.id.clone(),
                        width: *width,
                    });
                }
            }
        }
    }
    app.list.stats.cells = cells;
    app.list.stats.rows = (rows.end - rows.start) as usize;
    app.list.editing = editing;
    app.list.cache = Some(cache);
}

struct Delegate<'a> {
    table: &'a Table,
    lines: &'a [ListLine],
    columns: &'a [ColumnInfo],
    layout: &'a Layout,
    selected: &'a HashSet<AnnotId>,
    selected_replies: &'a [AnnotId],
    editing: &'a mut Option<CellEdit>,
    /// The status sets the Status cell offers.
    status_sets: &'a [StatusSet],
    palette: Palette,
    actions: Vec<Action>,
    cells: usize,
    rows: std::ops::Range<u64>,
    scratch: String,
    /// The table's scroll area, found from inside it (its scroll bars are named afterwards).
    scroll_area: Option<Id>,
}

impl Delegate<'_> {
    fn is_selected(&self, id: &AnnotId) -> bool {
        self.selected.contains(id) || self.selected_replies.contains(id)
    }

    fn heading(&mut self, ui: &mut Ui, column: &ColumnInfo) {
        let sort = self
            .layout
            .sort
            .iter()
            .position(|k| k.column == column.id)
            .map(|i| (i, self.layout.sort[i].descending));
        ui.add_space(4.0);
        let button = Button::new(RichText::new(&column.title).font(theme::bold(12.0)))
            .frame(false)
            .truncate();
        let response = ui.add(button);
        let enabled = ui.is_enabled();
        let name = match sort {
            Some((_, false)) => format!("Sort by {}, now ascending", column.title),
            Some((_, true)) => format!("Sort by {}, now descending", column.title),
            None => format!("Sort by {}", column.title),
        };
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &name));
        if response.clicked() {
            let add = ui.input(|i| i.modifiers.shift);
            self.actions.push(Action::Sort {
                column: column.id.clone(),
                add,
            });
        }
        if let Some((_, descending)) = sort {
            // A small arrow: up for ascending.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
            let c = rect.center();
            let k = if descending { 1.0 } else { -1.0 };
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - 3.5, c.y - 2.5 * k),
                    egui::pos2(c.x + 3.5, c.y - 2.5 * k),
                    egui::pos2(c.x, c.y + 3.0 * k),
                ],
                self.palette.muted_text,
                Stroke::NONE,
            ));
        }
        let id = column.id.clone();
        let title = column.title.clone();
        response.context_menu(|ui| {
            if ui.button("Sort ascending").clicked() {
                self.actions.push(Action::SortExact {
                    column: id.clone(),
                    descending: false,
                });
                ui.close();
            }
            if ui.button("Sort descending").clicked() {
                self.actions.push(Action::SortExact {
                    column: id.clone(),
                    descending: true,
                });
                ui.close();
            }
            ui.separator();
            if ui.button(format!("Group by {title}")).clicked() {
                self.actions.push(Action::GroupBy(Some(id.clone())));
                ui.close();
            }
            if ui.button("Move earlier").clicked() {
                self.actions.push(Action::MoveColumn {
                    column: id.clone(),
                    later: false,
                });
                ui.close();
            }
            if ui.button("Move later").clicked() {
                self.actions.push(Action::MoveColumn {
                    column: id.clone(),
                    later: true,
                });
                ui.close();
            }
            if ui.button("Hide this column").clicked() {
                self.actions.push(Action::ShowColumn {
                    column: id.clone(),
                    visible: false,
                });
                ui.close();
            }
        });
    }

    fn filter_box(&mut self, ui: &mut Ui, column: &ColumnInfo) {
        let mut text = self
            .layout
            .filter
            .conditions
            .iter()
            .find_map(|c| match &c.condition {
                Condition::Contains(text) if c.column == column.id => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        ui.add_space(2.0);
        let response = ui
            .add(
                TextEdit::singleline(&mut text)
                    .id(Id::unique(("plumbline-list-filter", &column.id)))
                    .desired_width((ui.available_width() - 4.0).max(8.0))
                    .hint_text("Filter"),
            )
            .accessible_name(format!("Filter {}", column.title));
        if response.changed() {
            self.actions.push(Action::FilterColumn {
                column: column.id.clone(),
                text,
            });
        }
    }

    /// A cell that holds text and can be edited in place.
    #[allow(clippy::too_many_arguments)]
    fn text_cell(
        &mut self,
        ui: &mut Ui,
        id: &AnnotId,
        column: &ColumnInfo,
        text: &str,
        editable: bool,
        row_name: &str,
        background: &egui::Response,
    ) {
        let editing_here = self
            .editing
            .as_ref()
            .is_some_and(|e| e.id == *id && e.column == column.id);
        if editing_here {
            let Some(edit) = self.editing.as_mut() else {
                return;
            };
            let response = ui
                .add(
                    TextEdit::singleline(&mut edit.text)
                        .id(Id::unique(CELL_EDIT_ID))
                        .desired_width((ui.available_width() - 2.0).max(8.0)),
                )
                .accessible_name(format!("Edit {}, {row_name}", column.title));
            let pointer_busy = ui.input(|i| i.pointer.any_down() || i.pointer.any_released());
            if edit.focus && !pointer_busy {
                response.request_focus();
                edit.focus = false;
            } else if edit.focus {
                ui.ctx().request_repaint();
            } else if response.lost_focus() {
                // Enter or a click elsewhere keeps the value (Escape is taken earlier).
                self.actions.push(Action::CommitEdit);
            }
            return;
        }
        ui.add(Label::new(text).selectable(false).truncate());
        if editable && background.double_clicked() {
            self.actions.push(Action::BeginEdit {
                id: id.clone(),
                column: column.id.clone(),
                text: text.to_owned(),
            });
        }
    }
}

impl TableDelegate for Delegate<'_> {
    fn prepare(&mut self, info: &PrefetchInfo) {
        self.rows = info.visible_rows.clone();
    }

    fn default_row_height(&self) -> f32 {
        ROW_HEIGHT
    }

    fn header_cell_ui(&mut self, ui: &mut Ui, cell: &HeaderCellInfo) {
        let Some(column) = self.columns.get(cell.col_range.start) else {
            return;
        };
        if self.scroll_area.is_none() {
            // The headings are drawn in a sibling of the Ui the table scrolls in; siblings
            // share their scope id, and the scroll area takes its id from that.
            self.scroll_area = ui
                .stack()
                .parent
                .as_ref()
                .map(|top| top.scope_id.with(egui::IdSalt::new("scroll_area")));
        }
        if cell.row_nr == 0 {
            self.heading(ui, column);
        } else {
            self.filter_box(ui, column);
        }
    }

    fn row_ui(&mut self, ui: &mut Ui, row_nr: u64) {
        let Some(line) = self.lines.get(row_nr as usize) else {
            return;
        };
        let rect = ui.max_rect();
        let painter = ui.painter();
        let heading = |text: String, indent: f32| {
            painter.text(
                egui::pos2(rect.left() + 20.0 + indent, rect.center().y),
                egui::Align2::LEFT_CENTER,
                text,
                theme::bold(12.0),
                ui.visuals().strong_text_color(),
            );
        };
        match line {
            ListLine::Group {
                depth,
                heading: text,
                count,
                collapsed,
                ..
            } => {
                painter.rect_filled(rect, 0.0, self.palette.row_group);
                let indent = 12.0 * *depth as f32;
                // The open/closed mark: a triangle pointing right when closed, down when open.
                let c = egui::pos2(rect.left() + 10.0 + indent, rect.center().y);
                let points = if *collapsed {
                    vec![
                        egui::pos2(c.x - 2.5, c.y - 4.0),
                        egui::pos2(c.x + 3.5, c.y),
                        egui::pos2(c.x - 2.5, c.y + 4.0),
                    ]
                } else {
                    vec![
                        egui::pos2(c.x - 4.0, c.y - 2.5),
                        egui::pos2(c.x + 4.0, c.y - 2.5),
                        egui::pos2(c.x, c.y + 3.5),
                    ]
                };
                painter.add(egui::Shape::convex_polygon(
                    points,
                    self.palette.muted_text,
                    Stroke::NONE,
                ));
                heading(format!("{text}  ({count})"), indent);
            }
            ListLine::Total { count, .. } => {
                painter.rect_filled(rect, 0.0, self.palette.row_group);
                heading(format!("Total  ({count})"), -12.0);
            }
            ListLine::Row { row, .. } => {
                if self.is_selected(&self.table.rows[*row].id) {
                    painter.rect_filled(rect, 0.0, self.palette.row_selected);
                }
            }
        }
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, self.palette.page_edge.gamma_multiply(0.45)),
        );
    }

    fn cell_ui(&mut self, ui: &mut Ui, cell: &CellInfo) {
        let (Some(line), Some(column)) = (
            self.lines.get(cell.row_nr as usize),
            self.columns.get(cell.col_nr),
        ) else {
            return;
        };
        self.cells += 1;
        let enabled = ui.is_enabled();
        let n = cell.row_nr + 1;
        let subtotal = |subtotals: &[(ColumnId, f64)]| {
            subtotals
                .iter()
                .find(|(id, _)| *id == column.id)
                .map(|(_, sum)| sum_text(column.places, *sum))
        };
        // A cell is a keyboard stop only when it has a name to be announced by: every cell
        // of a markup's row, and on a group or total line the first cell and the cells that
        // hold a subtotal. The other cells of those lines still take a click of the mouse.
        let named = match line {
            ListLine::Row { .. } => true,
            ListLine::Group { subtotals, .. } | ListLine::Total { subtotals, .. } => {
                cell.col_nr == 0 || subtotal(subtotals).is_some()
            }
        };
        let sense = if named { Sense::click() } else { Sense::CLICK };
        let background = ui.interact(ui.max_rect(), ui.scope_id().with("cell"), sense);
        match line {
            ListLine::Group {
                path,
                heading,
                count,
                collapsed,
                subtotals,
                ..
            } => {
                // The first cell is the heading's button for a screen reader; a cell with a
                // subtotal says what it holds. A click anywhere on the line opens or closes.
                let state = if *collapsed { "closed" } else { "open" };
                if let Some(sum) = subtotal(subtotals) {
                    let name = format!("{} subtotal of {heading}: {sum}", column.title);
                    background.widget_info(|| WidgetInfo::labeled(Role::Cell, enabled, &name));
                } else if cell.col_nr == 0 {
                    let name = format!("Group {heading}, {count} markups, {state}");
                    background.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &name));
                }
                if background.clicked() {
                    self.actions.push(Action::ToggleGroup(path.clone()));
                }
                if let Some(sum) = subtotal(subtotals) {
                    subtotal_text(ui, &sum, self.palette);
                }
            }
            ListLine::Total { subtotals, count } => {
                if let Some(sum) = subtotal(subtotals) {
                    let name = format!("Total of {}: {sum}", column.title);
                    background.widget_info(|| WidgetInfo::labeled(Role::Cell, enabled, &name));
                } else if cell.col_nr == 0 {
                    let name = format!("Total, {count} markups");
                    background.widget_info(|| WidgetInfo::labeled(Role::Cell, enabled, &name));
                }
                if let Some(sum) = subtotal(subtotals) {
                    subtotal_text(ui, &sum, self.palette);
                }
            }
            ListLine::Row {
                row,
                group_depth,
                reply_depth,
            } => {
                let r = &self.table.rows[*row];
                let is_reply = *reply_depth > 0;
                self.scratch.clear();
                if column.formatted {
                    // A quantity in its unit's places, a formula's error by its code.
                    self.scratch = self.table.cell_text(*row, &column.id);
                } else {
                    r.display_into(column.col, &mut self.scratch);
                }
                let text = std::mem::take(&mut self.scratch);
                let row_name = format!("row {n}");
                let selected = self.is_selected(&r.id);
                let name = format!("{} {text}, {row_name}", column.title);
                background
                    .widget_info(|| WidgetInfo::selected(Role::Cell, enabled, selected, &name));
                if background.clicked() {
                    self.actions.push(Action::Click {
                        id: r.id.clone(),
                        reply: is_reply,
                        modifiers: ui.input(|i| i.modifiers),
                    });
                }
                // The first column shows where a row sits: inside its groups, under its markup.
                if cell.col_nr == 0 {
                    let indent = 12.0 * *group_depth as f32 + 14.0 * *reply_depth as f32;
                    ui.add_space(4.0 + indent);
                    if is_reply {
                        let (mark, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 12.0), Sense::hover());
                        let stroke = Stroke::new(1.2, self.palette.muted_text);
                        ui.painter().line(
                            vec![
                                egui::pos2(mark.left() + 2.0, mark.top()),
                                egui::pos2(mark.left() + 2.0, mark.center().y + 1.0),
                                egui::pos2(mark.right(), mark.center().y + 1.0),
                            ],
                            stroke,
                        );
                    }
                } else {
                    ui.add_space(4.0);
                }
                let id = r.id.clone();
                match (&column.id, &column.custom) {
                    (ColumnId::Builtin(Builtin::Status), _) if !is_reply => {
                        let mut picked = None;
                        let mut picked_named = None;
                        let sets = self.status_sets;
                        let in_review = r.status_model.is_empty()
                            || r.status_model == pl_doc::pl_annot::REVIEW_MODEL;
                        egui::ComboBox::from_id_salt(("plumbline-list-status", cell.row_nr))
                            .selected_text(r.status_name.as_str())
                            .width((ui.available_width() - 6.0).max(24.0))
                            // Every state in view, custom sets included.
                            .height(f32::INFINITY)
                            .show_ui(ui, |ui| {
                                for state in ReviewState::ALL {
                                    if ui
                                        .selectable_label(in_review && r.status == state, state.name())
                                        .clicked()
                                    {
                                        picked = Some(state);
                                    }
                                }
                                // The document's custom status sets (Bluebeam's), each under
                                // its own state model.
                                for set in sets
                                    .iter()
                                    .filter(|s| s.model != pl_doc::pl_annot::REVIEW_MODEL)
                                {
                                    ui.separator();
                                    for state in &set.states {
                                        let current =
                                            r.status_model == set.model && r.status_name == *state;
                                        if ui
                                            .selectable_label(current, state.as_str())
                                            .on_hover_text(format!("Status set {}", set.model))
                                            .clicked()
                                        {
                                            picked_named =
                                                Some(Status::new(set.model.as_str(), state.as_str()));
                                        }
                                    }
                                }
                            })
                            .response
                            .accessible_name(format!("Status, {row_name}"));
                        if let Some(state) = picked {
                            self.actions.push(Action::Status { id, state });
                        } else if let Some(status) = picked_named {
                            self.actions.push(Action::StatusNamed { id, status });
                        }
                    }
                    (ColumnId::Builtin(Builtin::Checkmark), _) if !is_reply => {
                        let mut checked = r.checked;
                        let response = ui
                            .add(egui::Checkbox::without_text(&mut checked))
                            .accessible_name(format!("Checked, {row_name}"));
                        if response.changed() {
                            self.actions.push(Action::Checked { id, checked });
                        }
                    }
                    (ColumnId::Builtin(Builtin::Checkmark | Builtin::Status), _) => {}
                    (ColumnId::Builtin(Builtin::Locked), _) => {
                        if r.locked {
                            ui.add(Label::new("Locked").selectable(false).truncate());
                        }
                    }
                    (ColumnId::Builtin(Builtin::Colour | Builtin::Fill), _) => {
                        let colour = if column.id == ColumnId::Builtin(Builtin::Colour) {
                            r.colour
                        } else {
                            r.fill
                        };
                        if let Some([red, green, blue]) = colour {
                            let (swatch, _) =
                                ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                            ui.painter().rect_filled(
                                swatch,
                                2.0,
                                Color32::from_rgb(red, green, blue),
                            );
                            ui.add(Label::new(text.as_str()).selectable(false).truncate());
                        }
                    }
                    (ColumnId::Builtin(Builtin::Comment), _) => {
                        // A note's own window has its replies and status beside the text.
                        if background.double_clicked() && r.kind == AnnotKind::Text && !is_reply {
                            self.actions.push(Action::OpenNote(id.clone()));
                            ui.add(Label::new(text.as_str()).selectable(false).truncate());
                        } else {
                            self.text_cell(
                                ui,
                                &id,
                                column,
                                &text,
                                !r.locked,
                                &row_name,
                                &background,
                            );
                        }
                    }
                    (ColumnId::Builtin(Builtin::Subject), _) => {
                        self.text_cell(ui, &id, column, &text, !r.locked, &row_name, &background);
                    }
                    (ColumnId::Custom(_), Some(custom)) => match custom.kind {
                        ColumnType::Checkmark => {
                            let mut checked = text == Value::Bool(true).display();
                            let response = ui
                                .add(egui::Checkbox::without_text(&mut checked))
                                .accessible_name(format!("{}, {row_name}", custom.title));
                            if response.changed() {
                                self.actions.push(Action::Custom {
                                    id,
                                    column: Box::new(custom.clone()),
                                    value: Value::Bool(checked),
                                });
                            }
                        }
                        ColumnType::Choice => {
                            let mut picked = None;
                            egui::ComboBox::from_id_salt((
                                "plumbline-list-choice",
                                cell.row_nr,
                                cell.col_nr,
                            ))
                            .selected_text(text.as_str())
                            .width((ui.available_width() - 6.0).max(24.0))
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(text.is_empty(), "(none)").clicked() {
                                    picked = Some(Value::Empty);
                                }
                                for option in &custom.options {
                                    if ui.selectable_label(text == *option, option).clicked() {
                                        picked = Some(Value::text(option.as_str()));
                                    }
                                }
                            })
                            .response
                            .accessible_name(format!("{}, {row_name}", custom.title));
                            if let Some(value) = picked {
                                self.actions.push(Action::Custom {
                                    id,
                                    column: Box::new(custom.clone()),
                                    value,
                                });
                            }
                        }
                        _ => {
                            // A column shown from what Bluebeam stored is not typed in.
                            let editable =
                                !custom.is_read_only() || custom.kind == ColumnType::Formula;
                            self.text_cell(ui, &id, column, &text, editable, &row_name, &background);
                        }
                    },
                    _ => {
                        ui.add(Label::new(text.as_str()).selectable(false).truncate());
                    }
                }
                let _ = column.kind;
                self.scratch = text;
            }
        }
    }
}

fn subtotal_text(ui: &mut Ui, sum: &str, palette: Palette) {
    ui.with_layout(UiLayout::right_to_left(Align::Center), |ui| {
        ui.add_space(6.0);
        ui.add(
            Label::new(RichText::new(sum).font(theme::bold(12.0)))
                .selectable(false)
                .truncate(),
        );
        ui.add(
            Label::new(RichText::new("Sum").color(palette.muted_text))
                .selectable(false)
                .truncate(),
        );
    });
}

// ---- the custom columns dialog ---------------------------------------------------------------

fn split_options(text: &str) -> Vec<String> {
    text.split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Choice options as typed, each optionally with the number it stands for after `=`.
fn choice_values(options: &[String]) -> Result<Vec<(String, Option<f64>)>, String> {
    options
        .iter()
        .map(|o| match o.split_once('=') {
            Some((name, value)) => match value.trim().parse::<f64>() {
                Ok(v) if v.is_finite() => Ok((name.trim().to_owned(), Some(v))),
                _ => Err(format!(
                    "\"{}\" is not a number (option {})",
                    value.trim(),
                    name.trim()
                )),
            },
            None => Ok((o.clone(), None)),
        })
        .collect()
}

/// The dialog that adds a formula column, with the problem of the formula shown as it is
/// typed.
pub fn formula_dialog(app: &mut AppState, ctx: &egui::Context) {
    let Some(mut dialog) = app.list.formula_dialog.take() else {
        return;
    };
    let palette = Palette::of(&ctx.global_style().visuals);
    let names = crate::measure::formula_names(&app.list.schema);
    let mut add = false;
    let mut close = false;
    let mut insert: Option<String> = None;
    egui::Window::new("Formula column")
        .id(Id::unique("plumbline-formula-column"))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(
                RichText::new(
                    "A column worked out for each markup from its other columns: name them in \
                     brackets, for example [Length] * [Rate]. + - * / ^, comparisons, and, or, \
                     not, and round, roundup, rounddown, ceil, floor, min, max, abs, sqrt, if.",
                )
                .color(palette.muted_text),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(
                    TextEdit::singleline(&mut dialog.name)
                        .id(Id::unique("plumbline-formula-name"))
                        .desired_width(160.0),
                )
                .accessible_name("Formula column name");
            });
            ui.horizontal(|ui| {
                ui.label("Formula");
                let response = ui
                    .add(
                        TextEdit::singleline(&mut dialog.formula)
                            .id(Id::unique("plumbline-formula-text"))
                            .desired_width(260.0)
                            .hint_text("[Length] * 2"),
                    )
                    .accessible_name("Formula text");
                add |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            });
            egui::ComboBox::from_id_salt("plumbline-formula-insert")
                .selected_text("Insert a column")
                .width(180.0)
                .height(260.0)
                .show_ui(ui, |ui| {
                    for name in &names {
                        if ui.selectable_label(false, name).clicked() {
                            insert = Some(name.clone());
                        }
                    }
                })
                .response
                .accessible_name("Insert a column into the formula");
            let refused = dialog
                .refused
                .as_ref()
                .filter(|(name, formula, _)| *name == dialog.name && *formula == dialog.formula)
                .map(|(_, _, why)| why.clone());
            if refused.is_none() {
                dialog.refused = None;
            }
            let name_taken = {
                let name = dialog.name.trim().to_lowercase();
                (!name.is_empty()
                    && app
                        .list
                        .schema
                        .columns
                        .iter()
                        .any(|c| c.title.trim().to_lowercase() == name))
                .then(|| format!("There is already a column called \"{}\".", dialog.name.trim()))
            };
            dialog.message =
                crate::measure::formula_problem(&app.list.schema, &dialog.name, &dialog.formula)
                    .filter(|_| !dialog.formula.trim().is_empty())
                    .or(name_taken)
                    .or(refused);
            match &dialog.message {
                Some(message) => {
                    ui.label(RichText::new(message).color(palette.warning));
                }
                None if !dialog.formula.trim().is_empty() => {
                    ui.label(RichText::new("The formula can be used.").color(palette.muted_text));
                }
                None => {}
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                add |= ui.button("Add formula column").clicked();
                close |= ui.button("Close formula column").clicked();
            });
        });
    if let Some(name) = insert {
        if !dialog.formula.is_empty() && !dialog.formula.ends_with(' ') {
            dialog.formula.push(' ');
        }
        dialog.formula.push_str(&format!("[{name}]"));
    }
    if add {
        match app.add_formula_column(&dialog.name, &dialog.formula) {
            Ok(_) => {
                close = true;
            }
            Err(e) => {
                dialog.message = Some(e.clone());
                dialog.refused = Some((dialog.name.clone(), dialog.formula.clone(), e));
            }
        }
    }
    if !close {
        app.list.formula_dialog = Some(dialog);
    }
}

/// The dialog that defines the user's own columns.
fn columns_dialog(app: &mut AppState, ctx: &egui::Context) {
    let Some(mut dialog) = app.list.columns_dialog.take() else {
        return;
    };
    let muted = Palette::of(&ctx.global_style().visuals).muted_text;
    let warning = Palette::of(&ctx.global_style().visuals).warning;
    let existing: Vec<CustomColumn> = app.list.schema.columns.clone();
    let doc_columns: Vec<CustomColumn> = app.list.doc_columns.clone();
    let has_document = app.active_document().is_some();
    let doc_has_columns = app.active_has_document_columns();
    let mut close = false;
    let mut add = false;
    let mut remove: Option<String> = None;
    let mut rename: Option<(String, String)> = None;
    let mut options: Option<(String, String)> = None;
    egui::Window::new("Custom columns")
        .id(Id::unique("plumbline-custom-columns"))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(
                RichText::new(
                    "Columns of your own for the markups list. Their values are kept in each \
                     markup, in the PDF.",
                )
                .color(muted),
            );
            ui.add_space(4.0);
            if existing.is_empty() {
                ui.label(RichText::new("No custom columns yet.").color(muted));
            }
            for column in &existing {
                let entry = dialog
                    .edits
                    .entry(column.id.clone())
                    .or_insert_with(|| (column.title.clone(), column.options.join(", ")));
                ui.horizontal(|ui| {
                    let response = ui
                        .add(
                            TextEdit::singleline(&mut entry.0)
                                .id(Id::unique(("plumbline-column-title", &column.id)))
                                .desired_width(130.0),
                        )
                        .accessible_name(format!("Name of column {}", column.title));
                    if response.lost_focus() && entry.0.trim() != column.title {
                        rename = Some((column.id.clone(), entry.0.clone()));
                    }
                    ui.label(RichText::new(column.kind.name()).color(muted));
                    if column.kind == ColumnType::Choice {
                        let response = ui
                            .add(
                                TextEdit::singleline(&mut entry.1)
                                    .id(Id::unique(("plumbline-column-options", &column.id)))
                                    .desired_width(130.0)
                                    .hint_text("Choices, comma separated"),
                            )
                            .accessible_name(format!("Choices of column {}", column.title));
                        if response.lost_focus() && split_options(&entry.1) != column.options {
                            options = Some((column.id.clone(), entry.1.clone()));
                        }
                    }
                    ui.with_layout(UiLayout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(Button::new(format!("Remove {}", column.title)).frame(false))
                            .clicked()
                        {
                            remove = Some(column.id.clone());
                        }
                    });
                });
            }
            if !doc_columns.is_empty() {
                ui.separator();
                ui.label(RichText::new("In this PDF (Bluebeam columns)").font(theme::bold(12.5)));
                for column in &doc_columns {
                    let kind = if column.multiline {
                        "Multiline text".to_string()
                    } else if column.is_read_only() {
                        format!("{}, as stored", column.kind.name())
                    } else {
                        column.kind.name().to_string()
                    };
                    ui.horizontal(|ui| {
                        ui.label(column.title.as_str());
                        ui.label(RichText::new(kind).color(muted));
                    });
                }
            }
            ui.separator();
            ui.label(RichText::new("New column").font(theme::bold(12.5)));
            ui.horizontal(|ui| {
                let response = ui
                    .add(
                        TextEdit::singleline(&mut dialog.name)
                            .id(Id::unique("plumbline-new-column-name"))
                            .desired_width(130.0)
                            .hint_text("Name"),
                    )
                    .accessible_name("New column name");
                add |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let kind = dialog.kind.unwrap_or(ColumnType::Text);
                let shown = if kind == ColumnType::Text && dialog.multiline {
                    "Multiline text"
                } else {
                    kind.name()
                };
                egui::ComboBox::from_id_salt("plumbline-new-column-type")
                    .selected_text(shown)
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        for option in ColumnType::CUSTOM {
                            let current = kind == option && !dialog.multiline;
                            if ui.selectable_label(current, option.name()).clicked() {
                                dialog.kind = Some(option);
                                dialog.multiline = false;
                            }
                            if option == ColumnType::Text {
                                let current = kind == option && dialog.multiline;
                                if ui.selectable_label(current, "Multiline text").clicked() {
                                    dialog.kind = Some(ColumnType::Text);
                                    dialog.multiline = true;
                                }
                            }
                        }
                    })
                    .response
                    .accessible_name("New column type");
                if dialog.kind == Some(ColumnType::Choice) {
                    ui.add(
                        TextEdit::singleline(&mut dialog.options)
                            .id(Id::unique("plumbline-new-column-options"))
                            .desired_width(120.0)
                            .hint_text("Choices, comma separated"),
                    )
                    .accessible_name("New column choices");
                }
            });
            if has_document {
                let mut in_file = dialog.in_file.unwrap_or(doc_has_columns);
                if ui
                    .checkbox(&mut in_file, "Keep it in this PDF as a Bluebeam column")
                    .on_hover_text(
                        "Stored the way Bluebeam Revu keeps its custom columns, so Revu shows it \
                         too. Otherwise it is a column of your own list.",
                    )
                    .changed()
                {
                    dialog.in_file = Some(in_file);
                }
            }
            if let Some(error) = &dialog.error {
                ui.label(RichText::new(error).color(warning));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                add |= ui.button("Add column").clicked();
                close = ui.button("Close custom columns").clicked();
            });
        });
    if add {
        let kind = dialog.kind.unwrap_or(ColumnType::Text);
        let choices = split_options(&dialog.options);
        let valued = choice_values(&choices);
        let in_file = has_document && dialog.in_file.unwrap_or(doc_has_columns);
        let result = match (&valued, kind) {
            // A Bluebeam column: its choices are text only.
            (Ok(options), ColumnType::Choice) if in_file && options.iter().any(|(_, v)| v.is_some()) => {
                Err("a Bluebeam column's choices cannot stand for numbers".to_string())
            }
            _ if in_file => {
                let choices: Vec<&str> = choices.iter().map(String::as_str).collect();
                app.add_document_column(&dialog.name, kind, &choices, dialog.multiline)
            }
            // Options that stand for numbers: `Steel = 12.5, Timber = 4`.
            (Ok(options), ColumnType::Choice) if options.iter().any(|(_, v)| v.is_some()) => {
                let options: Vec<(&str, Option<f64>)> =
                    options.iter().map(|(o, v)| (o.as_str(), *v)).collect();
                app.add_choice_column(&dialog.name, &options)
            }
            (Err(e), ColumnType::Choice) => Err(e.clone()),
            _ => {
                let choices: Vec<&str> = choices.iter().map(String::as_str).collect();
                let added = app.add_list_column(&dialog.name, kind, &choices);
                if let (Ok(id), true) = (&added, dialog.multiline) {
                    app.list.schema.set_multiline(id, true);
                }
                added
            }
        };
        match result {
            Ok(_) => {
                dialog.name.clear();
                dialog.options.clear();
                dialog.error = None;
            }
            Err(e) => dialog.error = Some(e),
        }
    }
    if let Some((id, title)) = rename {
        match app.list.schema.rename(&id, &title) {
            Ok(()) => {
                dialog.error = None;
                app.list.schema_changed();
            }
            Err(e) => {
                dialog.error = Some(e.to_string());
                dialog.edits.remove(&id);
            }
        }
    }
    if let Some((id, text)) = options {
        let choices = split_options(&text);
        let choices: Vec<&str> = choices.iter().map(String::as_str).collect();
        match app.list.schema.set_options(&id, &choices) {
            Ok(()) => {
                dialog.error = None;
                app.list.schema_changed();
            }
            Err(e) => {
                dialog.error = Some(e.to_string());
                dialog.edits.remove(&id);
            }
        }
    }
    if let Some(id) = remove {
        app.remove_list_column(&id);
        dialog.edits.remove(&id);
    }
    if !close {
        app.list.columns_dialog = Some(dialog);
    }
}
