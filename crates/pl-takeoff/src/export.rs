//! Exports of the list as it is displayed: CSV, XLSX and the plain-data summary report.
//!
//! All three write the layout's visible columns in display order and the rows in view order
//! (filtered, sorted, grouped), with each markup's replies under it when the layout shows
//! replies.

use rust_xlsxwriter::{ExcelDateTime, Format, Formula, Workbook, Worksheet};
use serde::{Deserialize, Serialize};

use crate::column::{ColumnId, ColumnType};
use crate::quantity::fixed;
use crate::rows::{Col, Table};
use crate::value::{is_day, Value};
use crate::view::{Body, Group, Layout, Line, View};

/// What can go wrong writing a workbook.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("the list has {rows} rows; a worksheet holds at most {max}")]
    TooManyRows { rows: usize, max: usize },
    #[error("the list has {columns} columns; a worksheet holds at most {max}")]
    TooManyColumns { columns: usize, max: usize },
    #[error("could not write the workbook: {0}")]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
}

// ---------------------------------------------------------------------------------------------
// CSV

/// How the CSV is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvOptions {
    /// Starts the file with a UTF-8 byte-order mark, which Excel needs to read non-ASCII text.
    pub bom: bool,
    /// The field separator: a comma for RFC 4180.
    pub delimiter: char,
    /// Puts an apostrophe before a text cell that starts with `=`, `+`, `-`, `@`, tab or
    /// carriage return, so that a spreadsheet opening the file does not run it as a formula.
    /// (Comment text comes from whoever wrote the PDF.) Numbers are never changed.
    pub guard_formulas: bool,
    /// Writes the totals the XLSX has: when the list is grouped, a `Subtotal: <group>` record
    /// after each group's rows, and a `Total` record at the end, each with the sum of every
    /// summed visible column (see [`to_csv`]). Off by default, so the file is the rows alone.
    #[serde(default)]
    pub totals: bool,
}

impl Default for CsvOptions {
    fn default() -> CsvOptions {
        CsvOptions {
            bom: true,
            delimiter: ',',
            guard_formulas: true,
            totals: false,
        }
    }
}

/// The decimal places a number is rounded to in a file beyond the places it is shown with:
/// enough to keep every digit that means anything, few enough to drop float noise such as the
/// `...00000000000003` of `150.00000000000003`.
pub const FILE_EXTRA_PLACES: u8 = 6;

/// A number as CSV and XLSX write it: rounded to the column's display places (none for a
/// column without fixed places) plus [`FILE_EXTRA_PLACES`]. Only what is written is rounded:
/// the table, its formula columns and the sums are worked out at full precision. A number too
/// large for the rounding to mean anything is left as it is.
pub fn file_number(value: f64, places: Option<u8>) -> f64 {
    let decimals = i32::from(places.unwrap_or(0).min(12) + FILE_EXTRA_PLACES);
    // 10^18 at most: exact in a double, so the division gives the nearest double to the
    // rounded decimal, which prints as that decimal.
    let scale = 10f64.powi(decimals);
    let scaled = value * scale;
    if !scaled.is_finite() || scaled.abs() >= 9.0e15 {
        return value;
    }
    scaled.round() / scale + 0.0
}

fn csv_field(out: &mut String, field: &str, delimiter: char) {
    let quote = field.contains(['"', '\r', '\n', delimiter])
        || field.starts_with(' ')
        || field.ends_with(' ');
    if !quote {
        out.push_str(field);
        return;
    }
    out.push('"');
    for c in field.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

/// The list as CSV text (RFC 4180: CRLF line ends, fields holding a separator, a quote or a
/// line break are quoted, quotes doubled). The first record is the headings, a quantity column
/// with its unit (`Length (ft)`); group headings are not written. Quantity cells are written
/// to the list's decimal places, other numbers (formula cells as their values) rounded as
/// [`file_number`] does. The byte-order mark, when asked for, is the first character.
///
/// With [`CsvOptions::totals`], the totals of the XLSX are written too: after each group's
/// rows (innermost groups first) a record with `Subtotal: <group label>` in the first visible
/// column that is not summed and the group's sum in every summed column, and after all rows a
/// `Total` record with the sums over every listed markup. A sum is left empty when no markup
/// of the group has a number in the column. As in the XLSX, a reply's summed cells are left
/// empty, so the sums equal the rows above them.
pub fn to_csv(table: &Table, view: &View, layout: &Layout, options: &CsvOptions) -> String {
    let columns = layout.visible();
    let cols: Vec<Col> = columns.iter().map(|c| table.col(c)).collect();
    let mut out = String::new();
    if options.bom {
        out.push('\u{FEFF}');
    }
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            out.push(options.delimiter);
        }
        csv_field(&mut out, &table.heading(c), options.delimiter);
    }
    out.push_str("\r\n");
    let csv = CsvWriter {
        table,
        layout,
        options,
        // Quantity columns are written as the list shows them: in its unit, to its places.
        fixed_places: columns
            .iter()
            .map(|c| match c {
                ColumnId::Builtin(b) if b.quantity_kind().is_some() => table.places(c),
                _ => None,
            })
            .collect(),
        places: columns.iter().map(|c| table.places(c)).collect(),
        summed: columns
            .iter()
            .map(|c| options.totals && table.is_summed(c))
            .collect(),
        label_column: columns.iter().position(|c| !table.is_summed(c)),
        columns: &columns,
        cols,
    };
    let any_summed = csv.summed.iter().any(|s| *s);
    let mut cell = String::new();
    csv.body(&view.body, any_summed, &mut out, &mut cell);
    if any_summed && view.count > 0 {
        csv.totals("Total", &view.subtotals, &mut out, &mut cell);
    }
    out
}

/// What [`to_csv`] writes the records with.
struct CsvWriter<'a> {
    table: &'a Table,
    layout: &'a Layout,
    options: &'a CsvOptions,
    columns: &'a [ColumnId],
    cols: Vec<Col>,
    /// Per column: the places a quantity column is written with.
    fixed_places: Vec<Option<u8>>,
    /// Per column: its display places, for [`file_number`].
    places: Vec<Option<u8>>,
    /// Per column: whether totals are written for it (only with [`CsvOptions::totals`]).
    summed: Vec<bool>,
    /// The column a total's label goes in.
    label_column: Option<usize>,
}

impl CsvWriter<'_> {
    fn number(&self, i: usize, n: f64, cell: &mut String) {
        match self.fixed_places[i] {
            // Already rounded, to fewer places (and rounded once, not twice).
            Some(p) => cell.push_str(&fixed(n, p)),
            None => Value::Number(file_number(n, self.places[i])).display_into(cell),
        }
    }

    fn body(&self, body: &Body, totals: bool, out: &mut String, cell: &mut String) {
        match body {
            Body::Groups(groups) => {
                for g in groups {
                    self.body(&g.body, totals, out, cell);
                    if totals {
                        let label = format!("Subtotal: {}", g.label);
                        self.totals(&label, &g.subtotals, out, cell);
                    }
                }
            }
            Body::Rows(rows) => {
                for root in rows {
                    self.row(*root, false, out, cell);
                    if self.layout.show_replies {
                        for reply in self.table.thread(*root) {
                            self.row(reply, true, out, cell);
                        }
                    }
                }
            }
        }
    }

    fn row(&self, row: usize, reply: bool, out: &mut String, cell: &mut String) {
        let row = &self.table.rows[row];
        let delimiter = self.options.delimiter;
        for (i, col) in self.cols.iter().enumerate() {
            if i > 0 {
                out.push(delimiter);
            }
            if reply && self.summed[i] {
                continue;
            }
            let value = row.value(*col);
            cell.clear();
            if self.options.guard_formulas
                && matches!(&value, Value::Text(s)
                    if s.starts_with(['=', '+', '-', '@', '\t', '\r']))
            {
                cell.push('\'');
            }
            match &value {
                Value::Number(n) => self.number(i, *n, cell),
                _ => value.display_into(cell),
            }
            csv_field(out, cell, delimiter);
        }
        out.push_str("\r\n");
    }

    fn totals(
        &self,
        label: &str,
        subtotals: &[crate::view::Subtotal],
        out: &mut String,
        cell: &mut String,
    ) {
        let delimiter = self.options.delimiter;
        for (i, id) in self.columns.iter().enumerate() {
            if i > 0 {
                out.push(delimiter);
            }
            cell.clear();
            if self.label_column == Some(i) {
                cell.push_str(label);
            } else if self.summed[i] {
                if let Some(s) = subtotals.iter().find(|s| s.column == *id && s.values > 0) {
                    self.number(i, s.sum, cell);
                }
            }
            csv_field(out, cell, delimiter);
        }
        out.push_str("\r\n");
    }
}

// ---------------------------------------------------------------------------------------------
// Summary report

/// One column of a [`Report`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportColumn {
    pub column: ColumnId,
    pub title: String,
    pub kind: ColumnType,
    /// The column's width in the list, as a hint for its share of the page.
    pub width: f32,
    /// Whether groups carry a subtotal for it.
    pub summed: bool,
    /// The unit of a quantity column.
    #[serde(default)]
    pub unit: Option<String>,
}

/// One line of a [`Report`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReportLine {
    Group {
        /// 0 for the outermost grouping.
        depth: usize,
        /// The heading of the column grouped by.
        column: String,
        label: String,
        /// Markups in the group.
        count: usize,
        /// One entry per report column: the sum for a summed column that has values.
        subtotals: Vec<Option<f64>>,
    },
    Row {
        /// The annotation's id.
        id: String,
        /// Zero-based page index, for a link back to the markup.
        page: usize,
        group_depth: usize,
        /// 0 for a markup, 1 and up for replies.
        reply_depth: usize,
        /// One value per report column.
        cells: Vec<Value>,
    },
}

/// The list as plain data, grouped as displayed: what a PDF (or any other) summary is laid out
/// from. Nothing here knows about pages, fonts or drawing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub title: String,
    pub columns: Vec<ReportColumn>,
    pub lines: Vec<ReportLine>,
    /// Markups listed.
    pub count: usize,
    /// Markups in the document before the filter.
    pub total: usize,
    /// One entry per report column, over every markup listed.
    pub totals: Vec<Option<f64>>,
}

fn sums_for(columns: &[ColumnId], subtotals: &[crate::view::Subtotal]) -> Vec<Option<f64>> {
    columns
        .iter()
        .map(|c| {
            subtotals
                .iter()
                .find(|s| s.column == *c && s.values > 0)
                .map(|s| s.sum)
        })
        .collect()
}

impl Report {
    /// Builds the report for the layout's visible columns.
    pub fn build(title: &str, table: &Table, view: &View, layout: &Layout) -> Report {
        let ids = layout.visible();
        let cols: Vec<Col> = ids.iter().map(|c| table.col(c)).collect();
        let columns = ids
            .iter()
            .map(|c| ReportColumn {
                column: c.clone(),
                title: table.title(c),
                kind: table.column_type(c),
                width: layout.state(c).map_or(Layout::CUSTOM_WIDTH, |s| s.width),
                summed: table.is_summed(c),
                unit: table.unit_label(c).map(str::to_string),
            })
            .collect();
        let lines = view
            .lines(table, layout.show_replies, &[])
            .into_iter()
            .map(|line| match line {
                Line::Group { depth, group, .. } => ReportLine::Group {
                    depth,
                    column: table.title(&group.column),
                    label: group.label.clone(),
                    count: group.count,
                    subtotals: sums_for(&ids, &group.subtotals),
                },
                Line::Row {
                    row,
                    group_depth,
                    reply_depth,
                } => {
                    let r = &table.rows[row];
                    ReportLine::Row {
                        id: r.id.as_str().to_string(),
                        page: r.page,
                        group_depth,
                        reply_depth,
                        cells: cols.iter().map(|c| r.value(*c)).collect(),
                    }
                }
            })
            .collect();
        Report {
            title: title.to_string(),
            columns,
            lines,
            count: view.count,
            total: view.total,
            totals: sums_for(&ids, &view.subtotals),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// XLSX

/// The name of the worksheet that holds the rows.
pub const SHEET_ROWS: &str = "Markups";
/// The name of the worksheet that holds one line per group (written only when grouped).
pub const SHEET_GROUPS: &str = "Groups";

const MAX_ROWS: usize = 1_048_576;
const MAX_COLUMNS: usize = 16_384;
const MAX_CELL_UTF16: usize = 32_767;

/// `A`, `B`, ... `Z`, `AA`, ... for a zero-based column.
fn column_letters(mut col: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Cuts text to what one cell can hold.
fn cell_text(s: &str) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > MAX_CELL_UTF16 {
            return &s[..i];
        }
    }
    s
}

struct Formats {
    header: Format,
    bold: Format,
    date: Format,
    date_time: Format,
    /// Per column: the number format of a column with fixed decimal places.
    numbers: Vec<Option<Format>>,
    /// The same, bold, for the totals.
    totals: Vec<Option<Format>>,
    /// Per column: its display places, for [`file_number`].
    places: Vec<Option<u8>>,
}

/// An Excel number format with `places` decimal places.
fn places_format(places: u8) -> String {
    if places == 0 {
        "0".to_string()
    } else {
        format!("0.{}", "0".repeat(usize::from(places.min(12))))
    }
}

fn write_cell(
    sheet: &mut Worksheet,
    row: u32,
    col: u16,
    value: &Value,
    formats: &Formats,
) -> Result<(), ExportError> {
    match value {
        Value::Empty => {}
        Value::Text(s) => {
            sheet.write_string(row, col, cell_text(s))?;
        }
        Value::Number(n) => {
            let places = formats.places.get(usize::from(col)).copied().flatten();
            let n = file_number(*n, places);
            match formats
                .numbers
                .get(usize::from(col))
                .and_then(Option::as_ref)
            {
                Some(f) => {
                    sheet.write_number_with_format(row, col, n, f)?;
                }
                None => {
                    sheet.write_number(row, col, n)?;
                }
            }
        }
        Value::Bool(b) => {
            sheet.write_boolean(row, col, *b)?;
        }
        Value::Date(d) => {
            // The date as written (its own local time); a year a workbook cannot hold is
            // written as text.
            let excel = ExcelDateTime::from_ymd(d.year, d.month, d.day)
                .and_then(|x| x.and_hms(u16::from(d.hour), d.minute, f64::from(d.second)));
            match excel {
                Ok(x) => {
                    let format = if is_day(d) {
                        &formats.date
                    } else {
                        &formats.date_time
                    };
                    sheet.write_datetime_with_format(row, col, &x, format)?;
                }
                Err(_) => {
                    sheet.write_string(row, col, value.display())?;
                }
            }
        }
        Value::Colour(_) => {
            sheet.write_string(row, col, value.display())?;
        }
    }
    Ok(())
}

/// `=SUM(...)` over rows `first..=last` of a column, carrying `result` (the full-precision
/// sum) rounded as [`file_number`] rounds the column's cells.
fn sum_formula(
    sheet: Option<&str>,
    letters: &str,
    first: u32,
    last: u32,
    result: f64,
    places: Option<u8>,
) -> Formula {
    let prefix = sheet.map_or(String::new(), |s| format!("{s}!"));
    // Rows are numbered from 1 in a formula.
    Formula::new(format!(
        "=SUM({prefix}{letters}{}:{letters}{})",
        first + 1,
        last + 1
    ))
    .set_result(Value::Number(file_number(result, places)).display())
}

struct GroupLine<'a> {
    depth: usize,
    group: &'a Group,
    first: u32,
    last: u32,
}

/// The list as an XLSX workbook.
///
/// **Sheet `Markups`:** a bold heading row (a quantity column with its unit), frozen, then one
/// row per list row in display order. Cell types are kept: numbers are numbers (rounded as
/// [`file_number`] does, to the column's places plus six, which keeps the precision and drops
/// float noise; a quantity in the list's unit, shown to the list's places; a formula column as its
/// value, an error as its code in text), checkmarks booleans, dates real dates (with a date
/// format), everything else text. Under the rows, after one empty row, a `Total`
/// row holds `=SUM(...)` over the rows for every summed column that is visible (Length, Area,
/// Count and the user's number columns). A reply's cell in a summed column is left empty, so
/// the sums equal the list's own totals.
///
/// **Sheet `Groups`** (only when the list is grouped): one row per group, nested groups under
/// their parent with the label one column further right per level, then the group's count and,
/// for every summed visible column, `=SUM(Markups!...)` over exactly that group's rows; and a
/// `Total` row over everything.
///
/// Every formula carries its computed result, so the numbers show in viewers that do not
/// recalculate.
pub fn to_xlsx(table: &Table, view: &View, layout: &Layout) -> Result<Vec<u8>, ExportError> {
    let ids = layout.visible();
    if ids.len() > MAX_COLUMNS {
        return Err(ExportError::TooManyColumns {
            columns: ids.len(),
            max: MAX_COLUMNS,
        });
    }
    let cols: Vec<Col> = ids.iter().map(|c| table.col(c)).collect();
    let summed: Vec<bool> = ids.iter().map(|c| table.is_summed(c)).collect();
    let places: Vec<Option<u8>> = ids.iter().map(|c| table.places(c)).collect();
    let formats = Formats {
        header: Format::new().set_bold(),
        bold: Format::new().set_bold(),
        date: Format::new().set_num_format("yyyy-mm-dd"),
        date_time: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
        numbers: places
            .iter()
            .map(|p| p.map(|p| Format::new().set_num_format(places_format(p))))
            .collect(),
        totals: places
            .iter()
            .map(|p| p.map(|p| Format::new().set_bold().set_num_format(places_format(p))))
            .collect(),
        places: places.clone(),
    };

    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name(SHEET_ROWS)?;
    for (i, id) in ids.iter().enumerate() {
        let col = i as u16;
        sheet.write_string_with_format(0, col, cell_text(&table.heading(id)), &formats.header)?;
        let width = layout.state(id).map_or(Layout::CUSTOM_WIDTH, |s| s.width);
        sheet.set_column_width_pixels(col, width.round().max(1.0) as u32)?;
    }
    sheet.set_freeze_panes(1, 0)?;

    // Rows, remembering which sheet rows each group covers.
    let mut next: u32 = 1;
    let mut group_lines: Vec<GroupLine<'_>> = Vec::new();
    #[allow(clippy::too_many_arguments)]
    fn walk<'a>(
        body: &'a Body,
        depth: usize,
        table: &Table,
        layout: &Layout,
        cols: &[Col],
        summed: &[bool],
        formats: &Formats,
        sheet: &mut Worksheet,
        next: &mut u32,
        group_lines: &mut Vec<GroupLine<'a>>,
    ) -> Result<(), ExportError> {
        match body {
            Body::Groups(groups) => {
                for g in groups {
                    let at = group_lines.len();
                    let first = *next;
                    group_lines.push(GroupLine {
                        depth,
                        group: g,
                        first,
                        last: first,
                    });
                    walk(
                        &g.body,
                        depth + 1,
                        table,
                        layout,
                        cols,
                        summed,
                        formats,
                        sheet,
                        next,
                        group_lines,
                    )?;
                    group_lines[at].last = next.saturating_sub(1).max(first);
                }
            }
            Body::Rows(rows) => {
                for root in rows {
                    let replies = if layout.show_replies {
                        table.thread(*root)
                    } else {
                        Vec::new()
                    };
                    for (n, r) in std::iter::once(*root).chain(replies).enumerate() {
                        if *next as usize >= MAX_ROWS - 2 {
                            return Err(ExportError::TooManyRows {
                                rows: table.rows.len(),
                                max: MAX_ROWS - 3,
                            });
                        }
                        let row = &table.rows[r];
                        for (i, col) in cols.iter().enumerate() {
                            if n > 0 && summed[i] {
                                continue;
                            }
                            write_cell(sheet, *next, i as u16, &row.value(*col), formats)?;
                        }
                        *next += 1;
                    }
                }
            }
        }
        Ok(())
    }
    walk(
        &view.body,
        0,
        table,
        layout,
        &cols,
        &summed,
        &formats,
        sheet,
        &mut next,
        &mut group_lines,
    )?;
    let last_data = next - 1;
    let has_rows = last_data >= 1;
    let total_for = |id: &ColumnId| {
        view.subtotals
            .iter()
            .find(|s| s.column == *id)
            .map_or(0.0, |s| s.sum)
    };

    if has_rows && summed.iter().any(|s| *s) {
        let total_row = last_data + 2;
        if !summed[0] {
            sheet.write_string_with_format(total_row, 0, "Total", &formats.bold)?;
        }
        for (i, id) in ids.iter().enumerate() {
            if summed[i] {
                let f = sum_formula(
                    None,
                    &column_letters(i),
                    1,
                    last_data,
                    total_for(id),
                    places[i],
                );
                let format = formats.totals[i].as_ref().unwrap_or(&formats.bold);
                sheet.write_formula_with_format(total_row, i as u16, f, format)?;
            }
        }
    }

    if !group_lines.is_empty() {
        let levels = group_lines.iter().map(|g| g.depth).max().unwrap_or(0) + 1;
        let groups = workbook.add_worksheet();
        groups.set_name(SHEET_GROUPS)?;
        let level_title = |level: usize| {
            group_lines
                .iter()
                .find(|g| g.depth == level)
                .map_or(String::new(), |g| table.title(&g.group.column))
        };
        for level in 0..levels {
            groups.write_string_with_format(
                0,
                level as u16,
                cell_text(&level_title(level)),
                &formats.header,
            )?;
            groups.set_column_width_pixels(level as u16, 140)?;
        }
        let count_col = levels as u16;
        groups.write_string_with_format(0, count_col, "Markups", &formats.header)?;
        let sum_cols: Vec<usize> = (0..ids.len()).filter(|i| summed[*i]).collect();
        for (n, i) in sum_cols.iter().enumerate() {
            groups.write_string_with_format(
                0,
                count_col + 1 + n as u16,
                cell_text(&table.heading(&ids[*i])),
                &formats.header,
            )?;
        }
        groups.set_freeze_panes(1, 0)?;
        let mut at: u32 = 1;
        for line in &group_lines {
            groups.write_string(at, line.depth as u16, cell_text(&line.group.label))?;
            groups.write_number(at, count_col, line.group.count as f64)?;
            for (n, i) in sum_cols.iter().enumerate() {
                let result = line
                    .group
                    .subtotals
                    .iter()
                    .find(|s| s.column == ids[*i])
                    .map_or(0.0, |s| s.sum);
                let f = sum_formula(
                    Some(SHEET_ROWS),
                    &column_letters(*i),
                    line.first,
                    line.last,
                    result,
                    places[*i],
                );
                groups.write_formula(at, count_col + 1 + n as u16, f)?;
            }
            at += 1;
        }
        groups.write_string_with_format(at, 0, "Total", &formats.bold)?;
        groups.write_number_with_format(at, count_col, view.count as f64, &formats.bold)?;
        for (n, i) in sum_cols.iter().enumerate() {
            let f = sum_formula(
                Some(SHEET_ROWS),
                &column_letters(*i),
                1,
                last_data,
                total_for(&ids[*i]),
                places[*i],
            );
            groups.write_formula_with_format(at, count_col + 1 + n as u16, f, &formats.bold)?;
        }
    }
    Ok(workbook.save_to_buffer()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_letters_follow_the_spreadsheet_scheme() {
        let names: Vec<String> = [0, 1, 25, 26, 27, 51, 52, 701, 702, 16_383]
            .into_iter()
            .map(column_letters)
            .collect();
        assert_eq!(
            names,
            ["A", "B", "Z", "AA", "AB", "AZ", "BA", "ZZ", "AAA", "XFD"]
        );
    }

    #[test]
    fn long_text_is_cut_at_a_character_boundary() {
        let long = "é".repeat(40_000);
        assert_eq!(cell_text(&long).chars().count(), MAX_CELL_UTF16);
        let astral = "😀".repeat(20_000);
        assert_eq!(cell_text(&astral).chars().count(), MAX_CELL_UTF16 / 2);
        assert_eq!(cell_text("short"), "short");
    }

    #[test]
    fn csv_fields_are_quoted_only_when_needed() {
        let field = |s: &str| {
            let mut out = String::new();
            csv_field(&mut out, s, ',');
            out
        };
        assert_eq!(field("plain text"), "plain text");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("two\nlines"), "\"two\nlines\"");
        assert_eq!(field(" padded"), "\" padded\"");
        assert_eq!(field(""), "");
        let mut semi = String::new();
        csv_field(&mut semi, "a;b,c", ';');
        assert_eq!(semi, "\"a;b,c\"");
    }
}
