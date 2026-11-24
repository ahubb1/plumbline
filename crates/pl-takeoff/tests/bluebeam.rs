//! Bluebeam custom columns and statuses, against redcolumn's A3 test set
//! (`samples/external/redcolumn`, Apache-2.0, written in Bluebeam's private format and checked
//! in Revu by its author; local only and never committed: every test here skips when the files
//! are absent).
//!
//! The manifest test reads `A3-00-ALL-combined.pdf` (931 markups) and compares every markup's
//! status and its values in the seven columns `manifest.csv` lists against what Plumbline reads,
//! reporting the mismatches per column. The values are compared twice: the text stored in
//! `/BSIColumnData`, byte for byte as the manifest has it, and the typed value the list shows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pl_annot::AnnotSet;
use pl_pdf::PdfDocument;
use pl_takeoff::bluebeam::stored_text;
use pl_takeoff::{
    to_csv, BluebeamColumns, ColumnId, ColumnType, CsvOptions, CustomColumn, ListContext, Schema,
    Table, Value,
};

fn sample(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("external")
        .join("redcolumn")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("REDCOLUMN SAMPLE SKIPPED: samples/external/redcolumn/{name} is not present");
        None
    }
}

fn open(path: &Path) -> PdfDocument {
    PdfDocument::from_bytes(std::fs::read(path).unwrap(), None).unwrap()
}

/// A small RFC 4180 reader: quoted fields, doubled quotes, line breaks inside quotes.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// The manifest's columns and the Bluebeam column each is.
const COLUMNS: [(&str, &str); 7] = [
    ("discipline", "Discipline"),
    ("priority", "Priority"),
    ("ref", "Ref No."),
    ("qty", "Qty"),
    ("unit_cost", "Unit Cost"),
    ("due", "Due Date"),
    ("verified", "Verified"),
];

/// The status id the manifest gives a state: redcolumn's ids, which are the state's name in
/// lower case, except for its own custom set's three (model `BSI_A3TESTSET0001`).
fn status_id(state: &str) -> String {
    match state {
        "" => "none".to_string(),
        "Open" => "a3-open".to_string(),
        "In Progress" => "a3-progress".to_string(),
        "Closed - No Action" => "a3-closed".to_string(),
        s => s.to_lowercase(),
    }
}

/// The manifest's form of a typed value: a checkmark `true`/`false`, a date `YYYY-MM-DD`,
/// anything that did not read as its type the stored text itself.
fn manifest_form(kind: ColumnType, value: &Value) -> String {
    match (kind, value) {
        (_, Value::Empty) => String::new(),
        (ColumnType::Checkmark, Value::Bool(b)) => b.to_string(),
        (_, v) => v.display(),
    }
}

/// The text a typed value came from, for the number columns: compared with the manifest
/// numerically (the manifest keeps the stored text, `13.50`, which the value is 13.5 of).
fn same_number(value: &Value, expected: &str) -> bool {
    match value {
        Value::Number(n) => expected.trim().parse::<f64>().ok() == Some(*n),
        other => other.display() == expected,
    }
}

#[test]
fn every_markup_matches_the_manifest() {
    let (Some(pdf), Some(manifest)) = (sample("A3-00-ALL-combined.pdf"), sample("manifest.csv"))
    else {
        return;
    };
    let doc = open(&pdf);
    let set = AnnotSet::load(&doc);
    let bluebeam = BluebeamColumns::read(&doc);
    assert!(
        bluebeam.object.is_some(),
        "/BSIAnnotColumns is an indirect array"
    );
    assert_eq!(bluebeam.entries.len(), 11);
    let columns = bluebeam.columns();
    assert_eq!(columns.len(), 11);
    let schema = Schema {
        version: Schema::VERSION,
        columns: columns.clone(),
    };
    // Links are markups of the manifest (redcolumn lists them): the list's default options
    // show a hyperlink that carries Bluebeam column values, as Revu does.
    let table = Table::build(
        set.iter(),
        &ListContext {
            schema: Some(&schema),
            options: pl_takeoff::RowOptions::default(),
            ..ListContext::default()
        },
    );

    let text = std::fs::read_to_string(&manifest).unwrap();
    let mut records = parse_csv(&text).into_iter();
    let header = records.next().unwrap();
    let at = |name: &str| header.iter().position(|h| h == name).unwrap();
    let records: Vec<Vec<String>> = records.filter(|r| r.len() == header.len()).collect();
    assert_eq!(records.len(), 931, "the manifest lists 931 markups");

    // The markups of the list (not replies), by page and subject, in list order.
    let mut by_key: HashMap<(usize, String), Vec<usize>> = HashMap::new();
    for &i in &table.roots {
        let r = &table.rows[i];
        by_key
            .entry((r.page, r.subject.clone()))
            .or_default()
            .push(i);
    }
    let mut taken: HashMap<(usize, String), usize> = HashMap::new();

    let column =
        |title: &str| -> &CustomColumn { columns.iter().find(|c| c.title == title).unwrap() };
    let mut missing = 0;
    let mut status_wrong = 0;
    let mut stored_wrong: HashMap<&str, usize> = HashMap::new();
    let mut typed_wrong: HashMap<&str, usize> = HashMap::new();
    let mut first_problems: Vec<String> = Vec::new();
    let mut spaces = 0;
    for rec in &records {
        let page: usize = rec[at("page")].parse::<usize>().unwrap() - 1;
        // A space is a page's /BSISpaces entry, not an annotation: Bluebeam's format has no
        // /BSIColumnData for it (redcolumn keeps its values in its own private data only).
        if rec[at("type")] == "space" {
            spaces += 1;
            continue;
        }
        // A Cloud+ is written with Revu's subject for it on both of its parts.
        // A text box without a subject of its own is written with redcolumn's default one.
        let subject = if rec[at("group")].starts_with("cloudplus") {
            "Cloud+".to_string()
        } else if rec[at("subject")].is_empty() && rec[at("type")] == "text" {
            "Text Box".to_string()
        } else {
            rec[at("subject")].clone()
        };
        let key = (page, subject);
        let n = taken.entry(key.clone()).or_insert(0);
        let Some(&row) = by_key.get(&key).and_then(|v| v.get(*n)) else {
            missing += 1;
            if first_problems.len() < 40 {
                first_problems.push(format!("no row for page {} {:?}", page + 1, key.1));
            }
            continue;
        };
        *n += 1;
        let r = &table.rows[row];
        let a = set.get(&r.id).unwrap();

        if status_id(&r.status_name) != rec[at("status")] {
            status_wrong += 1;
            if first_problems.len() < 40 {
                first_problems.push(format!(
                    "status of {:?}: {:?}, manifest {:?}",
                    key.1,
                    r.status_name,
                    rec[at("status")]
                ));
            }
        }

        for (field, title) in COLUMNS {
            let c = column(title);
            let expected = &rec[at(field)];
            let index = c.bluebeam.as_ref().unwrap().index;
            let stored = stored_text(a, index).unwrap_or_default();
            // The stored text as the manifest writes it: redcolumn lower-cases a checkmark
            // and writes a PDF date as YYYY-MM-DD; everything else is the text itself.
            let stored_as_manifest = match c.kind {
                ColumnType::Checkmark => stored.to_lowercase(),
                ColumnType::Date => match stored.strip_prefix("D:") {
                    Some(d) if d.len() >= 8 => {
                        format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
                    }
                    _ => stored.clone(),
                },
                _ => stored.clone(),
            };
            if stored_as_manifest != *expected {
                *stored_wrong.entry(field).or_default() += 1;
                if first_problems.len() < 40 {
                    first_problems.push(format!(
                        "{field} of {:?}: stored {stored:?}, manifest {expected:?}",
                        key.1
                    ));
                }
            }
            let value = table.value(row, &ColumnId::custom(&c.id));
            let typed_ok = match c.kind {
                ColumnType::Number => same_number(&value, expected),
                kind => manifest_form(kind, &value) == *expected,
            };
            if !typed_ok {
                *typed_wrong.entry(field).or_default() += 1;
                if first_problems.len() < 40 {
                    first_problems.push(format!(
                        "{field} of {:?}: read {value:?}, manifest {expected:?}",
                        key.1
                    ));
                }
            }
        }
    }
    eprintln!(
        "manifest: {} markups ({spaces} spaces, which have no /BSIColumnData), {missing} without a row, {status_wrong} statuses wrong",
        records.len()
    );
    assert_eq!(spaces, 2);
    for (field, _) in COLUMNS {
        eprintln!(
            "  {field:>9}: {} stored texts wrong, {} typed values wrong",
            stored_wrong.get(field).copied().unwrap_or(0),
            typed_wrong.get(field).copied().unwrap_or(0)
        );
    }
    assert!(
        missing == 0
            && status_wrong == 0
            && stored_wrong.values().all(|n| *n == 0)
            && typed_wrong.values().all(|n| *n == 0),
        "mismatches: {first_problems:#?}"
    );
}

/// The edge values of page Z-101 read as the text they are where they do not fit the column's
/// type, and the formulas Plumbline works out give Bluebeam's own arithmetic.
#[test]
fn edge_values_and_formulas() {
    let Some(pdf) = sample("A3-00-ALL-combined.pdf") else {
        return;
    };
    let doc = open(&pdf);
    let set = AnnotSet::load(&doc);
    let columns = BluebeamColumns::read(&doc).columns();
    let schema = Schema {
        version: Schema::VERSION,
        columns: columns.clone(),
    };
    let table = Table::build(
        set.iter(),
        &ListContext {
            schema: Some(&schema),
            ..ListContext::default()
        },
    );
    let id = |title: &str| ColumnId::custom(&columns.iter().find(|c| c.title == title).unwrap().id);
    let row = |subject_start: &str| {
        table
            .roots
            .iter()
            .copied()
            .find(|i| table.rows[*i].subject.starts_with(subject_start))
            .unwrap()
    };
    let total = columns.iter().find(|c| c.title == "Total Cost").unwrap();
    assert_eq!(total.kind, ColumnType::Formula);
    assert_eq!(
        total.bluebeam.as_ref().unwrap().expression,
        "[Qty] * [Unit Cost]"
    );
    let verified_qty = columns.iter().find(|c| c.title == "Verified Qty").unwrap();
    assert_eq!(verified_qty.formula, "[bb_qty] * if([bb_verified], 1, 0)");
    let multiline = columns.iter().find(|c| c.title == "Review Notes").unwrap();
    assert!(multiline.multiline && multiline.kind == ColumnType::Text);
    assert!(columns.iter().any(|c| c.title == "Zone <A&B> \"L2\""));

    let cc22 = row("CC22");
    assert_eq!(
        table.value(cc22, &id("Total Cost")),
        Value::Number(3.0 * 9.99)
    );
    assert_eq!(table.cell_text(cc22, &id("Total Cost")), "29.97");
    assert_eq!(table.value(cc22, &id("Verified Qty")), Value::Number(3.0));
    let cc23 = row("CC23");
    assert_eq!(table.value(cc23, &id("Verified Qty")), Value::Number(0.0));
    // A number column holding "abc": the text, and the formula that uses it is an error, not
    // a number.
    let cc15 = row("CC15");
    assert_eq!(table.value(cc15, &id("Qty")), Value::text("abc"));
    assert_eq!(table.value(cc15, &id("Unit Cost")), Value::text("1,5"));
    assert!(table.rows[cc15]
        .formula_error(
            table
                .custom
                .iter()
                .position(|c| c.title == "Total Cost")
                .unwrap()
        )
        .is_some());
    assert_eq!(
        table.value(row("CC18"), &id("Due Date")),
        Value::text("tomorrow")
    );
    assert_eq!(
        table.value(row("CC16"), &id("Due Date")),
        Value::day(2028, 2, 29)
    );
    // A date is typed as the column's /Format shows it (MM/dd/yyyy), or as ISO.
    let due = columns.iter().find(|c| c.title == "Due Date").unwrap();
    assert_eq!(
        due.bluebeam.as_ref().unwrap().date_format.as_deref(),
        Some("MM/dd/yyyy")
    );
    assert_eq!(due.parse("12/25/2027"), Ok(Value::day(2027, 12, 25)));
    assert_eq!(due.parse("2027-12-25"), Ok(Value::day(2027, 12, 25)));
    assert!(due.parse("25/12/2027").is_err());
    assert_eq!(
        table.value(row("CC10"), &id("Discipline")),
        Value::text("Orphan value")
    );
    assert_eq!(table.value(row("CC13"), &id("Qty")), Value::Number(-12.0));
    assert_eq!(table.cell_text(row("CC14"), &id("Unit Cost")), "0.01");
    let cc03 = row("CC03");
    let a = set.get(&table.rows[cc03].id).unwrap();
    assert_eq!(stored_text(a, 2).unwrap(), "R".repeat(255));
    // A Bluebeam formula none of whose columns holds anything is blank, not a made-up 0: on a
    // markup with no Qty or Unit Cost, and on every row with no /BSIColumnData at all.
    assert_eq!(table.value(cc03, &id("Total Cost")), Value::Empty);
    assert_eq!(table.value(cc03, &id("Verified Qty")), Value::Empty);
    let mut bare = 0;
    for (i, r) in table.rows.iter().enumerate() {
        if set.get(&r.id).is_some_and(|a| a.column_data.is_none()) {
            bare += 1;
            assert_eq!(table.value(i, &id("Total Cost")), Value::Empty);
            assert_eq!(table.value(i, &id("Verified Qty")), Value::Empty);
        }
    }
    assert!(bare > 0, "replies carry no column values");
    let cc04 = row("CC04");
    assert_eq!(
        table.value(cc04, &id("Ref No.")),
        Value::text("Réf № ① ≥ ± °")
    );
}

/// The CSV and XLSX exports carry the Bluebeam columns, headed by their names.
#[test]
fn csv_and_xlsx_exports_carry_the_bluebeam_columns() {
    let Some(pdf) = sample("A3-07-columns-statuses-metadata.pdf") else {
        return;
    };
    let doc = open(&pdf);
    let set = AnnotSet::load(&doc);
    let columns = BluebeamColumns::read(&doc).columns();
    let schema = Schema {
        version: Schema::VERSION,
        columns: columns.clone(),
    };
    let table = Table::build(
        set.iter(),
        &ListContext {
            schema: Some(&schema),
            ..ListContext::default()
        },
    );
    let mut layout = pl_takeoff::Layout::default();
    layout.reconcile(&schema);
    let view = pl_takeoff::View::build(&table, &layout.filter, &layout.sort, &layout.group);
    let csv = to_csv(&table, &view, &layout, &CsvOptions::default());
    let first = csv.lines().next().unwrap();
    for c in &columns {
        let heading = if c.title.contains(['"', ',']) {
            format!("\"{}\"", c.title.replace('"', "\"\""))
        } else {
            c.title.clone()
        };
        assert!(first.contains(&heading), "{heading} in {first}");
    }
    assert!(csv.contains("Structural"));

    // The workbook too: every Bluebeam column is a heading of the rows sheet, with its values.
    use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
    let bytes = pl_takeoff::to_xlsx(&table, &view, &layout).unwrap();
    let mut book: Xlsx<_> = open_workbook_from_rs(std::io::Cursor::new(bytes)).unwrap();
    let rows = book.worksheet_range(pl_takeoff::SHEET_ROWS).unwrap();
    let headings: Vec<String> = rows
        .rows()
        .next()
        .unwrap()
        .iter()
        .map(|d| d.to_string())
        .collect();
    for c in &columns {
        assert!(headings.contains(&c.title), "{} in {headings:?}", c.title);
    }
    let discipline = headings.iter().position(|h| h == "Discipline").unwrap();
    assert!(rows
        .rows()
        .any(|r| r[discipline] == Data::String("Structural".into())));
    let cost = headings.iter().position(|h| h == "Unit Cost").unwrap();
    assert!(rows.rows().any(|r| r[cost] == Data::Float(13.5)));
    // Numbers are written as numbers (13.50 is 13.5), as every number column of the CSV is.
    assert!(csv.contains(",13.5,"));
}

/// A3-07's statuses: the custom set and its states are found, and a markup whose history mixes
/// the review model and the custom set shows its latest status.
#[test]
fn custom_status_sets_are_found() {
    let Some(pdf) = sample("A3-07-columns-statuses-metadata.pdf") else {
        return;
    };
    let doc = open(&pdf);
    let set = AnnotSet::load(&doc);
    let sets = set.status_sets();
    let custom = sets
        .iter()
        .find(|s| s.model == "BSI_A3TESTSET0001")
        .expect("the custom status set");
    for state in ["Open", "In Progress", "Closed - No Action"] {
        assert!(custom.states.iter().any(|s| s == state), "{state}");
    }
    let mixed = set
        .iter()
        .find(|a| a.meta.subject.as_deref() == Some("SS mixed-model history"))
        .unwrap();
    let history = set.status_history(&mixed.id);
    assert!(history.len() >= 2, "the whole history is kept");
    let models: std::collections::HashSet<&str> = history
        .iter()
        .filter_map(|a| a.meta.state_model.as_deref())
        .collect();
    assert!(models.len() >= 2, "{models:?}");
    assert_eq!(set.status(&mixed.id).unwrap().state, "In Progress");
}
