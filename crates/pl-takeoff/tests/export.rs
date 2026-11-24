//! CSV, XLSX and the summary report.

use std::io::{Cursor, Read};

use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
use pl_annot::{AnnotSet, Annotation, Origin, PdfDate};
use pl_geom::{Point, Rect};
use pl_takeoff::{
    set_custom_value, to_csv, to_xlsx, Builtin, ColumnId, ColumnType, CsvOptions, Layout,
    ListContext, Report, ReportLine, Schema, SortKey, Table, Value, View, SHEET_GROUPS, SHEET_ROWS,
};

fn date(text: &str) -> PdfDate {
    PdfDate::parse(text).unwrap()
}

fn schema() -> Schema {
    let mut s = Schema::new();
    s.add("Trade", ColumnType::Choice, &["Civil", "Electrical"])
        .unwrap();
    s.add("Qty", ColumnType::Number, &[]).unwrap();
    s.add("Done", ColumnType::Checkmark, &[]).unwrap();
    s.add("Due", ColumnType::Date, &[]).unwrap();
    s
}

/// Three markups and one reply, with awkward text.
fn sample(s: &Schema) -> AnnotSet {
    let col = |id: &str| s.get(id).unwrap();
    let mut set = AnnotSet::new();

    let mut door = Annotation::square(
        0,
        Rect::new(10.0, 10.0, 60.0, 40.0),
        &Origin::new("Ada", date("D:20261103091500Z")),
    );
    door.meta.subject = Some("Door 2".into());
    door.meta.contents = Some("Check, \"hinge\"\nsecond line".into());
    set_custom_value(&mut door, col("trade"), Value::text("Civil")).unwrap();
    set_custom_value(&mut door, col("qty"), Value::Number(2.5)).unwrap();
    set_custom_value(&mut door, col("done"), Value::Bool(true)).unwrap();
    set_custom_value(&mut door, col("due"), Value::day(2026, 12, 1)).unwrap();
    set.add(door).unwrap();

    let mut cable = Annotation::line(
        1,
        Point::new(0.0, 0.0),
        Point::new(50.0, 0.0),
        &Origin::new("Bo", date("D:20261104180000+01'00'")),
    );
    cable.meta.subject = Some("Door 10".into());
    cable.meta.contents = Some("=SUM(A1)".into());
    set_custom_value(&mut cable, col("trade"), Value::text("Electrical")).unwrap();
    set_custom_value(&mut cable, col("qty"), Value::Number(4.0)).unwrap();
    set_custom_value(&mut cable, col("done"), Value::Bool(false)).unwrap();
    set.add(cable).unwrap();

    let note = Annotation::note(
        0,
        Rect::new(100.0, 100.0, 118.0, 118.0),
        "plain",
        &Origin::new("Chen", date("D:20261105120000Z")),
    );
    let note = set.add(note).unwrap();
    let reply = set
        .add_reply(
            &note,
            " Agreed; see détail 7 ",
            &Origin::new("Ada", date("D:20261106120000Z")),
        )
        .unwrap();
    // A number on a reply: shown in its row, never part of a sum.
    set_custom_value(
        set.get_mut(&reply).unwrap(),
        col("qty"),
        Value::Number(100.0),
    )
    .unwrap();
    set
}

/// The columns the export tests show, in this order.
fn layout(s: &Schema) -> Layout {
    let shown: Vec<ColumnId> = vec![
        Builtin::Subject.into(),
        Builtin::Page.into(),
        Builtin::Kind.into(),
        Builtin::Author.into(),
        Builtin::Comment.into(),
        ColumnId::custom("trade"),
        ColumnId::custom("qty"),
        ColumnId::custom("done"),
        ColumnId::custom("due"),
    ];
    let mut layout = Layout::for_schema(s);
    for c in layout.columns.clone() {
        layout.set_visible(&c.column, shown.contains(&c.column));
    }
    for c in shown.iter().rev() {
        let first = layout.columns[0].column.clone();
        layout.move_column(c, Some(&first));
    }
    assert_eq!(layout.visible(), shown);
    layout.sort = vec![SortKey::asc(Builtin::Subject)];
    layout
}

fn table(set: &AnnotSet, s: &Schema) -> Table {
    Table::build(
        set.iter(),
        &ListContext {
            schema: Some(s),
            ..ListContext::default()
        },
    )
}

#[test]
fn csv_matches_the_golden_text() {
    let s = schema();
    let set = sample(&s);
    let table = table(&set, &s);
    let mut layout = layout(&s);
    let view = View::of(&table, &layout);

    let golden = concat!(
        "\u{FEFF}Subject,Page,Type,Author,Comment,Trade,Qty,Done,Due\r\n",
        "Door 2,1,Rectangle,Ada,\"Check, \"\"hinge\"\"\nsecond line\",Civil,2.5,Yes,2026-12-01\r\n",
        "Door 10,2,Line,Bo,'=SUM(A1),Electrical,4,No,\r\n",
        "Note,1,Note,Chen,plain,,,,\r\n",
        "Re: Note,1,Reply,Ada,\" Agreed; see détail 7 \",,100,,\r\n",
    );
    let csv = to_csv(&table, &view, &layout, &CsvOptions::default());
    assert_eq!(csv, golden);
    assert_eq!(
        &csv.as_bytes()[..3],
        [0xEF, 0xBB, 0xBF],
        "UTF-8 byte-order mark"
    );

    // No mark, semicolons, formulas left as they are, replies hidden, grouped and reversed.
    layout.show_replies = false;
    layout.group = vec![SortKey::desc(ColumnId::custom("trade"))];
    let view = View::of(&table, &layout);
    let options = CsvOptions {
        bom: false,
        delimiter: ';',
        guard_formulas: false,
        ..CsvOptions::default()
    };
    let golden = concat!(
        "Subject;Page;Type;Author;Comment;Trade;Qty;Done;Due\r\n",
        "Door 10;2;Line;Bo;=SUM(A1);Electrical;4;No;\r\n",
        "Door 2;1;Rectangle;Ada;\"Check, \"\"hinge\"\"\nsecond line\";Civil;2.5;Yes;2026-12-01\r\n",
        "Note;1;Note;Chen;plain;;;;\r\n",
    );
    assert_eq!(to_csv(&table, &view, &layout, &options), golden);

    // Nothing to show is still a valid file: the headings.
    let empty = Table::build(
        &[] as &[Annotation],
        &ListContext {
            schema: Some(&s),
            ..ListContext::default()
        },
    );
    let csv = to_csv(&empty, &View::of(&empty, &layout), &layout, &options);
    assert_eq!(
        csv,
        "Subject;Page;Type;Author;Comment;Trade;Qty;Done;Due\r\n"
    );
}

fn open(bytes: &[u8]) -> Xlsx<Cursor<Vec<u8>>> {
    open_workbook_from_rs(Cursor::new(bytes.to_vec())).expect("a readable workbook")
}

fn part(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).unwrap();
    let mut out = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    out
}

/// Days since the spreadsheet epoch for a calendar date.
fn serial(year: u16, month: u8, day: u8) -> f64 {
    let d = PdfDate {
        year,
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
        utc_offset_minutes: None,
    };
    (d.to_unix() / 86_400 + 25_569) as f64
}

#[test]
fn xlsx_keeps_types_freezes_the_heading_and_sums_with_formulas() {
    let s = schema();
    let set = sample(&s);
    let table = table(&set, &s);
    let mut layout = layout(&s);
    layout.set_visible(&Builtin::Modified.into(), true);
    layout.set_width(&Builtin::Comment.into(), 300.0);
    layout.group = vec![SortKey::asc(ColumnId::custom("trade"))];
    let view = View::of(&table, &layout);
    let bytes = to_xlsx(&table, &view, &layout).unwrap();

    let mut book = open(&bytes);
    assert_eq!(book.sheet_names(), [SHEET_ROWS, SHEET_GROUPS]);
    let rows = book.worksheet_range(SHEET_ROWS).unwrap();
    let cell = |r: u32, c: u32| rows.get_value((r, c)).cloned().unwrap_or(Data::Empty);
    let text = |s: &str| Data::String(s.to_string());

    // Headings, then the rows as displayed: Civil, Electrical, then the ones with no trade.
    let headings: Vec<Data> = (0..10).map(|c| cell(0, c)).collect();
    let expected = [
        "Subject", "Page", "Type", "Author", "Comment", "Trade", "Qty", "Done", "Due", "Modified",
    ];
    assert_eq!(headings, expected.map(text));
    assert_eq!(
        (1..=4).map(|r| cell(r, 0)).collect::<Vec<_>>(),
        ["Door 2", "Door 10", "Note", "Re: Note"].map(text)
    );
    // Types: text, number, boolean, date.
    assert_eq!(cell(1, 1), Data::Float(1.0));
    assert_eq!(cell(1, 4), text("Check, \"hinge\"\nsecond line"));
    assert_eq!(
        cell(2, 4),
        text("=SUM(A1)"),
        "text that looks like a formula stays text"
    );
    assert_eq!(cell(1, 6), Data::Float(2.5));
    assert_eq!(cell(2, 6), Data::Float(4.0));
    assert_eq!(cell(3, 6), Data::Empty);
    assert_eq!(
        cell(4, 6),
        Data::Empty,
        "a reply's number is not written into a summed column"
    );
    assert_eq!(cell(1, 7), Data::Bool(true));
    assert_eq!(cell(2, 7), Data::Bool(false));
    match cell(1, 8) {
        Data::DateTime(d) => assert_eq!(d.as_f64(), serial(2026, 12, 1)),
        other => panic!("the due date should be a date cell, not {other:?}"),
    }
    assert_eq!(cell(2, 8), Data::Empty);
    match cell(2, 9) {
        // 18:00 as written, whatever its time zone.
        Data::DateTime(d) => assert_eq!(d.as_f64(), serial(2026, 11, 4) + 0.75),
        other => panic!("the modified date should be a date cell, not {other:?}"),
    }

    // The total row: a SUM formula over the data rows with its result cached.
    assert_eq!(cell(5, 0), Data::Empty, "an empty row separates the total");
    assert_eq!(cell(6, 0), text("Total"));
    assert_eq!(cell(6, 6), Data::Float(6.5));
    let formulas = book.worksheet_formula(SHEET_ROWS).unwrap();
    let formula = |r: u32, c: u32| formulas.get_value((r, c)).cloned().unwrap_or_default();
    assert_eq!(formula(6, 6), "SUM(G2:G5)");
    assert_eq!(formula(2, 4), "", "the comment is not a formula");
    let all_formulas = formulas
        .used_cells()
        .filter(|(_, _, f)| !f.is_empty())
        .count();
    assert_eq!(
        all_formulas, 1,
        "only the visible summed column is totalled"
    );

    // The groups sheet: a SUM per group over exactly its rows, and the overall total.
    let groups = book.worksheet_range(SHEET_GROUPS).unwrap();
    let g = |r: u32, c: u32| groups.get_value((r, c)).cloned().unwrap_or(Data::Empty);
    assert_eq!(
        [g(0, 0), g(0, 1), g(0, 2)],
        ["Trade", "Markups", "Qty"].map(text)
    );
    let lines: Vec<[Data; 3]> = (1..=4).map(|r| [g(r, 0), g(r, 1), g(r, 2)]).collect();
    assert_eq!(
        lines,
        [
            [text("Civil"), Data::Float(1.0), Data::Float(2.5)],
            [text("Electrical"), Data::Float(1.0), Data::Float(4.0)],
            [text("(none)"), Data::Float(1.0), Data::Float(0.0)],
            [text("Total"), Data::Float(3.0), Data::Float(6.5)],
        ]
    );
    let group_formulas = book.worksheet_formula(SHEET_GROUPS).unwrap();
    let gf: Vec<String> = (1..=4)
        .map(|r| {
            group_formulas
                .get_value((r, 2))
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        gf,
        [
            "SUM(Markups!G2:G2)",
            "SUM(Markups!G3:G3)",
            "SUM(Markups!G4:G5)",
            "SUM(Markups!G2:G5)"
        ]
    );

    // The heading row is frozen on both sheets, and the column width came from the layout.
    for sheet in ["xl/worksheets/sheet1.xml", "xl/worksheets/sheet2.xml"] {
        let xml = part(&bytes, sheet);
        assert!(
            xml.contains("<pane ySplit=\"1\""),
            "{sheet}: {}",
            &xml[..xml.len().min(600)]
        );
        assert!(xml.contains("state=\"frozen\""));
        assert!(xml.contains("topLeftCell=\"A2\""));
    }
    let sheet1 = part(&bytes, "xl/worksheets/sheet1.xml");
    assert!(
        sheet1.contains("<f>SUM(G2:G5)</f><v>6.5</v>"),
        "formula with its cached result"
    );
    assert!(
        sheet1.contains("<col min=\"5\" max=\"5\" width=\""),
        "the comment column has its own width"
    );
    let styles = part(&bytes, "xl/styles.xml");
    assert!(styles.contains("yyyy-mm-dd hh:mm:ss") && styles.contains("formatCode=\"yyyy-mm-dd\""));
}

/// A larger list grouped two levels deep: every SUM formula covers exactly the rows of its
/// group, checked by adding up the cells it names.
#[test]
fn xlsx_group_formulas_cover_exactly_their_rows() {
    let mut s = Schema::new();
    s.add("Zone", ColumnType::Choice, &["North", "South", "East"])
        .unwrap();
    s.add("Qty", ColumnType::Number, &[]).unwrap();
    s.add("Cost", ColumnType::Number, &[]).unwrap();
    let col = |id: &str| s.get(id).unwrap();
    let authors = ["Ada", "Bo", "Chen"];
    let mut set = AnnotSet::new();
    let mut state = 11u64;
    let mut next = |n: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % n
    };
    for i in 0..400 {
        let origin = Origin::new(
            authors[next(3) as usize],
            PdfDate::from_unix(1_780_000_000 + i),
        );
        let mut a = Annotation::square(next(5) as usize, Rect::new(0.0, 0.0, 9.0, 9.0), &origin);
        if next(4) > 0 {
            set_custom_value(
                &mut a,
                col("zone"),
                Value::text(["North", "South", "East"][next(3) as usize]),
            )
            .unwrap();
        }
        if next(5) > 0 {
            set_custom_value(&mut a, col("qty"), Value::Number(next(50) as f64 * 0.5)).unwrap();
        }
        set_custom_value(&mut a, col("cost"), Value::Number(next(1000) as f64 * 0.25)).unwrap();
        let id = set.add(a).unwrap();
        if next(6) == 0 {
            set.add_reply(&id, "noted", &origin).unwrap();
        }
    }
    let table = table(&set, &s);
    let mut layout = Layout::for_schema(&s);
    for quantity in [Builtin::Length, Builtin::Area, Builtin::Count] {
        layout.set_visible(&quantity.into(), true);
    }
    layout.sort = vec![SortKey::desc(ColumnId::custom("cost"))];
    layout.group = vec![
        SortKey::asc(ColumnId::custom("zone")),
        SortKey::asc(Builtin::Author),
    ];
    let view = View::of(&table, &layout);
    let bytes = to_xlsx(&table, &view, &layout).unwrap();

    let visible = layout.visible();
    let qty_col = visible
        .iter()
        .position(|c| *c == ColumnId::custom("qty"))
        .unwrap() as u32;
    let cost_col = visible
        .iter()
        .position(|c| *c == ColumnId::custom("cost"))
        .unwrap() as u32;
    let mut book = open(&bytes);
    let rows = book.worksheet_range(SHEET_ROWS).unwrap();
    let groups = book.worksheet_range(SHEET_GROUPS).unwrap();
    let formulas = book.worksheet_formula(SHEET_GROUPS).unwrap();
    let data_rows = table.rows.len() as u32;
    assert_eq!(
        rows.height() as u32,
        1 + data_rows + 2,
        "heading, rows, gap, total"
    );

    // Adds up the cells a formula like SUM(Markups!K12:K40) names.
    let evaluate = |formula: &str| -> (f64, u32, u32) {
        let inner = formula
            .strip_prefix("SUM(Markups!")
            .and_then(|f| f.strip_suffix(')'))
            .unwrap_or_else(|| panic!("not a SUM over the rows sheet: {formula}"));
        let (from, to) = inner.split_once(':').unwrap();
        let split = |cell: &str| {
            let digits = cell.find(|c: char| c.is_ascii_digit()).unwrap();
            let column = cell[..digits]
                .bytes()
                .fold(0u32, |n, b| n * 26 + u32::from(b - b'A') + 1)
                - 1;
            (column, cell[digits..].parse::<u32>().unwrap() - 1)
        };
        let ((c0, r0), (c1, r1)) = (split(from), split(to));
        assert_eq!(c0, c1);
        let sum = (r0..=r1)
            .filter_map(|r| match rows.get_value((r, c0)) {
                Some(Data::Float(f)) => Some(*f),
                Some(Data::Int(i)) => Some(*i as f64),
                _ => None,
            })
            .sum();
        assert!(c0 == qty_col || c0 == cost_col);
        (sum, r0, r1)
    };

    // Walk the groups sheet beside the view's own groups.
    fn flatten<'a>(
        body: &'a pl_takeoff::Body,
        depth: u32,
        out: &mut Vec<(u32, &'a pl_takeoff::Group)>,
    ) {
        if let pl_takeoff::Body::Groups(groups) = body {
            for g in groups {
                out.push((depth, g));
                flatten(&g.body, depth + 1, out);
            }
        }
    }
    let mut expected = Vec::new();
    flatten(&view.body, 0, &mut expected);
    assert_eq!(
        expected.iter().filter(|(d, _)| *d == 0).count(),
        4,
        "three zones and none"
    );
    assert!(expected.len() > 12);
    // Columns: zone label, author label, count, then one sum per summed visible column.
    let summed: Vec<ColumnId> = visible
        .iter()
        .filter(|c| table.is_summed(c))
        .cloned()
        .collect();
    assert_eq!(summed.len(), 5, "length, area, count, qty, cost");
    let mut covered_by_leaves = 0;
    for (i, (depth, group)) in expected.iter().enumerate() {
        let r = i as u32 + 1;
        assert_eq!(
            groups.get_value((r, *depth)),
            Some(&Data::String(group.label.clone()))
        );
        assert_eq!(
            groups.get_value((r, 2)),
            Some(&Data::Float(group.count as f64))
        );
        for (n, column) in summed.iter().enumerate() {
            let c = 3 + n as u32;
            let formula = formulas.get_value((r, c)).unwrap();
            let want = group
                .subtotals
                .iter()
                .find(|t| t.column == *column)
                .unwrap()
                .sum;
            let cached = match groups.get_value((r, c)) {
                Some(Data::Float(f)) => *f,
                other => panic!("no cached result: {other:?}"),
            };
            assert_eq!(cached, want, "{formula}");
            if matches!(column, ColumnId::Custom(_)) {
                let (sum, r0, r1) = evaluate(formula);
                assert_eq!(sum, want, "{formula} for group {}", group.label);
                if *depth == 1 && n == 3 {
                    // Leaf ranges tile the data rows without gaps or overlaps.
                    assert_eq!(r0, covered_by_leaves + 1);
                    covered_by_leaves = r1;
                }
            }
        }
    }
    assert_eq!(covered_by_leaves, data_rows);
    // The total line and the total row on the first sheet agree with the view.
    let last = expected.len() as u32 + 1;
    assert_eq!(
        groups.get_value((last, 0)),
        Some(&Data::String("Total".into()))
    );
    assert_eq!(groups.get_value((last, 2)), Some(&Data::Float(400.0)));
    let total_cost = view
        .subtotals
        .iter()
        .find(|t| t.column == ColumnId::custom("cost"))
        .unwrap()
        .sum;
    assert!(total_cost > 0.0);
    assert_eq!(
        groups.get_value((last, 3 + 4)),
        Some(&Data::Float(total_cost))
    );
    assert_eq!(
        rows.get_value((data_rows + 2, cost_col)),
        Some(&Data::Float(total_cost))
    );
    let (sum, r0, r1) = evaluate(formulas.get_value((last, 3 + 4)).unwrap());
    assert_eq!((sum, r0, r1), (total_cost, 1, data_rows));
}

#[test]
fn xlsx_without_groups_or_rows_is_still_a_workbook() {
    let s = schema();
    let layout = layout(&s);
    let empty = Table::build(
        &[] as &[Annotation],
        &ListContext {
            schema: Some(&s),
            ..ListContext::default()
        },
    );
    let bytes = to_xlsx(&empty, &View::of(&empty, &layout), &layout).unwrap();
    let mut book = open(&bytes);
    assert_eq!(book.sheet_names(), [SHEET_ROWS]);
    let rows = book.worksheet_range(SHEET_ROWS).unwrap();
    assert_eq!(rows.height(), 1);
    assert_eq!(rows.get_value((0, 8)), Some(&Data::String("Due".into())));
    assert_eq!(
        book.worksheet_formula(SHEET_ROWS)
            .unwrap()
            .used_cells()
            .filter(|c| !c.2.is_empty())
            .count(),
        0
    );

    // A very long comment is cut to what a cell holds instead of failing the export.
    let set = {
        let mut set = AnnotSet::new();
        let o = Origin::new("Ada", PdfDate::from_unix(1_780_000_000));
        let mut a = Annotation::square(0, Rect::new(0.0, 0.0, 9.0, 9.0), &o);
        a.meta.contents = Some("x".repeat(40_000));
        a.meta.created = PdfDate::parse("D:18500101000000Z");
        set.add(a).unwrap();
        set
    };
    let table = table(&set, &s);
    let mut layout = layout;
    layout.set_visible(&Builtin::Created.into(), true);
    let bytes = to_xlsx(&table, &View::of(&table, &layout), &layout).unwrap();
    let rows = open(&bytes).worksheet_range(SHEET_ROWS).unwrap();
    match rows.get_value((1, 4)) {
        Some(Data::String(text)) => assert_eq!(text.len(), 32_767),
        other => panic!("{other:?}"),
    }
    // A date before the spreadsheet epoch is written as text rather than lost.
    assert_eq!(
        rows.get_value((1, 9)),
        Some(&Data::String("1850-01-01 00:00:00".into()))
    );
}

#[test]
fn the_summary_report_is_the_list_as_displayed() {
    let s = schema();
    let set = sample(&s);
    let table = table(&set, &s);
    let mut layout = layout(&s);
    layout.group = vec![SortKey::asc(ColumnId::custom("trade"))];
    let view = View::of(&table, &layout);
    let report = Report::build("Level 2 markups", &table, &view, &layout);

    assert_eq!(report.title, "Level 2 markups");
    assert_eq!((report.count, report.total), (3, 3));
    let titles: Vec<&str> = report.columns.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Subject", "Page", "Type", "Author", "Comment", "Trade", "Qty", "Done", "Due"]
    );
    assert_eq!(report.columns[6].kind, ColumnType::Number);
    assert!(report.columns[6].summed && !report.columns[1].summed);
    assert_eq!(report.columns[4].width, 260.0);
    let mut totals = vec![None; 9];
    totals[6] = Some(6.5);
    assert_eq!(report.totals, totals);

    let shape: Vec<String> = report
        .lines
        .iter()
        .map(|l| match l {
            ReportLine::Group {
                depth,
                column,
                label,
                count,
                subtotals,
            } => {
                format!(
                    "group {depth} {column}={label} n={count} qty={:?}",
                    subtotals[6]
                )
            }
            ReportLine::Row {
                group_depth,
                reply_depth,
                cells,
                page,
                ..
            } => {
                format!(
                    "row g{group_depth} r{reply_depth} p{page} {} | {}",
                    cells[0].display(),
                    cells[6].display()
                )
            }
        })
        .collect();
    assert_eq!(
        shape,
        [
            "group 0 Trade=Civil n=1 qty=Some(2.5)",
            "row g1 r0 p0 Door 2 | 2.5",
            "group 0 Trade=Electrical n=1 qty=Some(4.0)",
            "row g1 r0 p1 Door 10 | 4",
            "group 0 Trade=(none) n=1 qty=None",
            "row g1 r0 p0 Note | ",
            "row g1 r1 p0 Re: Note | 100",
        ]
    );
    match &report.lines[1] {
        ReportLine::Row { id, cells, .. } => {
            assert_eq!(id, table.rows[0].id.as_str());
            assert_eq!(cells.len(), 9);
            assert_eq!(cells[7], Value::Bool(true));
            assert_eq!(cells[8], Value::day(2026, 12, 1));
        }
        other => panic!("{other:?}"),
    }
    // Plain data: it serialises and comes back equal.
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);

    // Without replies and without groups it is just the rows.
    layout.group.clear();
    layout.show_replies = false;
    let flat = Report::build("", &table, &View::of(&table, &layout), &layout);
    assert_eq!(flat.lines.len(), 3);
    assert!(flat.lines.iter().all(|l| matches!(
        l,
        ReportLine::Row {
            group_depth: 0,
            reply_depth: 0,
            ..
        }
    )));
}
