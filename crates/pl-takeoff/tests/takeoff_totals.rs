//! The takeoff fixes from the M7 verification: CSV subtotals and a total, the Angle, Radius,
//! Diameter and Base Area columns (a Volume's area is not in Area), counts as one row per
//! count whatever carries their items, and float noise rounded out of the files.

use std::io::Cursor;

use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
use pl_annot::{AnnotSet, Annotation, CountSymbol, MeasureFormat, MeasureSpec, Origin, PdfDate};
use pl_geom::{AreaFormat, AreaUnit, LengthFormat, Point, Rect, Scale, Unit};
use pl_takeoff::{
    count_group_key, file_number, to_csv, to_xlsx, Builtin, ColumnId, ColumnType, CsvOptions,
    Layout, ListContext, NumberDisplay, QuantityUnits, Schema, SortKey, Table, TakeoffContext,
    Value, View, SHEET_ROWS,
};

fn og() -> Origin {
    Origin::new("Totals Tester", PdfDate::from_unix(1_790_000_000))
}

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

/// 1" = 20': 72 page units are 20 ft.
fn eng() -> MeasureSpec {
    MeasureSpec::new(
        &Scale::engineering(20.0).unwrap(),
        1.0,
        &MeasureFormat::new(
            LengthFormat::decimal(Unit::Foot, 2),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        ),
    )
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn imperial(set: &AnnotSet, schema: Option<&Schema>) -> Table {
    Table::build_takeoff(
        set.iter(),
        &ListContext {
            schema,
            ..ListContext::default()
        },
        &TakeoffContext {
            units: Some(QuantityUnits::imperial()),
            ..TakeoffContext::default()
        },
    )
}

/// Only these columns visible, in this order.
fn showing(schema: &Schema, shown: &[ColumnId]) -> Layout {
    let mut layout = Layout::for_schema(schema);
    for c in layout.columns.clone() {
        layout.set_visible(&c.column, shown.contains(&c.column));
    }
    for c in shown.iter().rev() {
        let first = layout.columns[0].column.clone();
        layout.move_column(c, Some(&first));
    }
    assert_eq!(layout.visible(), shown);
    layout
}

fn length(subject: &str, units: f64) -> Annotation {
    let mut a = Annotation::measure_length(0, p(0.0, 0.0), p(units, 0.0), eng(), &og());
    a.meta.subject = Some(subject.into());
    a
}

#[test]
fn csv_totals_write_a_subtotal_per_group_and_a_total() {
    let mut set = AnnotSet::new();
    // Fence 20 ft + 40 ft, Kerb 60 ft, and a Kerb note with no length.
    for (subject, units) in [("Fence", 72.0), ("Kerb", 216.0), ("Fence", 144.0)] {
        set.add(length(subject, units)).unwrap();
    }
    let mut note = Annotation::note(0, Rect::new(0.0, 0.0, 10.0, 10.0), "", &og());
    note.meta.subject = Some("Kerb".into());
    let note = set.add(note).unwrap();
    // A reply's number never counts, so its summed cells are left empty.
    set.add_reply(&note, "ok", &og()).unwrap();
    let schema = Schema::new();
    let table = imperial(&set, None);
    let mut layout = showing(
        &schema,
        &[Builtin::Subject.into(), Builtin::Length.into(), Builtin::Kind.into()],
    );
    layout.group = vec![SortKey::asc(Builtin::Subject)];
    let view = View::of(&table, &layout);
    let options = CsvOptions {
        bom: false,
        totals: true,
        ..CsvOptions::default()
    };
    let golden = concat!(
        "Subject,Length (ft),Type\r\n",
        "Fence,20.00,Line\r\n",
        "Fence,40.00,Line\r\n",
        "Subtotal: Fence,60.00,\r\n",
        "Kerb,60.00,Line\r\n",
        "Kerb,,Note\r\n",
        "Re: Kerb,,Reply\r\n",
        "Subtotal: Kerb,60.00,\r\n",
        "Total,120.00,\r\n",
    );
    assert_eq!(to_csv(&table, &view, &layout, &options), golden);

    // Ungrouped: the rows and the total only.
    layout.group.clear();
    layout.show_replies = false;
    let view = View::of(&table, &layout);
    let csv = to_csv(&table, &view, &layout, &options);
    assert!(!csv.contains("Subtotal"), "{csv}");
    assert!(csv.ends_with("Total,120.00,\r\n"), "{csv}");

    // A summed first column: the label goes in the first column that is not summed.
    let layout = showing(
        &schema,
        &[Builtin::Length.into(), Builtin::Subject.into()],
    );
    let view = View::of(&table, &layout);
    let csv = to_csv(&table, &view, &layout, &options);
    assert!(csv.ends_with("120.00,Total\r\n"), "{csv}");

    // Off by default: the rows alone, as before.
    let csv = to_csv(&table, &view, &layout, &CsvOptions::default());
    assert!(!csv.contains("Total"), "{csv}");

    // Nothing summed shown: no total record.
    let layout = showing(&schema, &[Builtin::Subject.into()]);
    let view = View::of(&table, &layout);
    assert!(!to_csv(&table, &view, &layout, &options).contains("Total"));
}

#[test]
fn takeoff_layout_shows_angle_radius_diameter_and_base_area_and_volumes_stay_out_of_area() {
    let mut set = AnnotSet::new();
    let square = |dx: f64| {
        vec![
            p(dx, 0.0),
            p(dx + 72.0, 0.0),
            p(dx + 72.0, 72.0),
            p(dx, 72.0),
        ]
    };
    // A 20 x 20 ft area (400 sq ft) and a 20 x 20 ft slab 3 ft deep.
    let area = set
        .add(Annotation::measure_area(0, square(0.0), vec![], eng(), &og()))
        .unwrap();
    let slab = set
        .add(Annotation::measure_volume(
            0,
            square(100.0),
            vec![],
            3.0 * 0.3048,
            eng(),
            &og(),
        ))
        .unwrap();
    let angle = set
        .add(Annotation::measure_angle(
            0,
            p(0.0, 100.0),
            p(0.0, 150.0),
            p(50.0, 150.0),
            None,
            &og(),
        ))
        .unwrap();
    let radius = set
        .add(Annotation::measure_radius(
            0,
            p(200.0, 200.0),
            p(236.0, 200.0),
            eng(),
            &og(),
        ))
        .unwrap();
    let diameter = set
        .add(Annotation::measure_diameter(
            0,
            p(300.0, 200.0),
            p(372.0, 200.0),
            eng(),
            &og(),
        ))
        .unwrap();
    let schema = Schema::new();
    let table = imperial(&set, None);
    let layout = Layout::takeoff(&schema);
    let visible = layout.visible();
    assert_eq!(
        visible,
        Layout::TAKEOFF_COLUMNS
            .iter()
            .map(|b| ColumnId::Builtin(*b))
            .collect::<Vec<_>>()
    );
    assert_eq!(layout.group, vec![SortKey::asc(Builtin::Subject)]);
    for b in [Builtin::Angle, Builtin::Radius, Builtin::Diameter] {
        assert!(!table.is_summed(&b.into()), "{b:?} is not summed");
    }
    assert!(table.is_summed(&Builtin::BaseArea.into()));
    assert_eq!(table.heading(&Builtin::Diameter.into()), "Diameter (ft)");
    assert_eq!(table.heading(&Builtin::BaseArea.into()), "Base Area (sq ft)");
    assert_eq!(table.heading(&Builtin::Angle.into()), "Angle (\u{b0})");

    let cell = |id, b: Builtin| table.value(table.row_of(id).unwrap(), &b.into());
    let num = |id, b| cell(id, b).as_number().unwrap();
    assert!(close(num(&area, Builtin::Area), 400.0));
    assert_eq!(cell(&slab, Builtin::Area), Value::Empty);
    assert!(close(num(&slab, Builtin::BaseArea), 400.0));
    assert!(close(num(&slab, Builtin::Volume), 1200.0 / 27.0));
    assert!(close(num(&angle, Builtin::Angle), 90.0));
    assert!(close(num(&radius, Builtin::Radius), 10.0));
    assert!(close(num(&radius, Builtin::Diameter), 20.0));
    assert!(close(num(&diameter, Builtin::Diameter), 20.0));
    assert_eq!(cell(&diameter, Builtin::Length), Value::Empty);

    // The totals: Area is the area alone; the slab's base area has its own total.
    let view = View::of(&table, &layout);
    let total = |b: Builtin| {
        view.subtotals
            .iter()
            .find(|s| s.column == b.into())
            .map(|s| (s.sum, s.values))
    };
    let (sum, n) = total(Builtin::Area).unwrap();
    assert!(close(sum, 400.0) && n == 1, "{sum} over {n}");
    let (sum, n) = total(Builtin::BaseArea).unwrap();
    assert!(close(sum, 400.0) && n == 1, "{sum} over {n}");
    assert_eq!(total(Builtin::Length).map(|t| t.1), Some(0));
    assert!(total(Builtin::Angle).is_none() && total(Builtin::Diameter).is_none());

    // The CSV of the takeoff: the non-adding columns have values on their rows, nothing in
    // the subtotals.
    let csv = to_csv(
        &table,
        &view,
        &layout,
        &CsvOptions {
            bom: false,
            totals: true,
            ..CsvOptions::default()
        },
    );
    let header: Vec<&str> = csv.lines().next().unwrap().split(',').collect();
    let at = |h: &str| header.iter().position(|x| *x == h).unwrap();
    let total_line: Vec<&str> = csv.lines().last().unwrap().split(',').collect();
    assert_eq!(total_line[at("Subject")], "Total");
    assert_eq!(total_line[at("Area (sq ft)")], "400.00");
    assert_eq!(total_line[at("Base Area (sq ft)")], "400.00");
    assert_eq!(total_line[at("Angle (\u{b0})")], "");
    assert_eq!(total_line[at("Diameter (ft)")], "");
    assert!(csv.contains(",90.0,"), "{csv}");
}

#[test]
fn a_count_is_one_row_with_its_item_total_and_type_count() {
    let mut set = AnnotSet::new();
    let count = |page: usize, xs: &[f64], symbol: CountSymbol, subject: &str| {
        let points: Vec<Point> = xs.iter().map(|x| p(*x, 100.0)).collect();
        let mut a = Annotation::count(page, &points, symbol, &og());
        a.meta.subject = Some(subject.into());
        a
    };
    // Revu style: one annotation per item, three items of one count, the further ones grouped
    // under the first with /IRT + /RT /Group.
    let first = set
        .add(count(0, &[10.0], CountSymbol::Ring, "Outlets"))
        .unwrap();
    let member = |x: f64| {
        let mut a = count(0, &[x], CountSymbol::Ring, "Outlets");
        a.meta.reply_to = Some(first.clone());
        a.meta.reply_type = Some("Group".into());
        a
    };
    let second = set.add(member(20.0)).unwrap();
    // Something else in between does not split the group.
    set.add(length("Fence", 72.0)).unwrap();
    let third = set.add(member(400.0)).unwrap();
    // Plumbline style: one annotation holding two items. It looks like the Revu count (same
    // subject, page, paint and marker) but is a count of its own.
    let more = set
        .add(count(0, &[300.0, 400.0], CountSymbol::Ring, "Outlets"))
        .unwrap();
    // Another page, another marker, another subject: three more counts.
    let other_page = set
        .add(count(1, &[10.0], CountSymbol::Ring, "Outlets"))
        .unwrap();
    let other_marker = set
        .add(count(0, &[10.0, 20.0], CountSymbol::Dot, "Outlets"))
        .unwrap();
    let other_subject = set
        .add(count(0, &[10.0], CountSymbol::Ring, "Switches"))
        .unwrap();
    // A reply to an item of the count is a reply to the count's row.
    let reply = set.add_reply(&second, "two of these are spares", &og()).unwrap();

    assert_eq!(
        count_group_key(set.get(&first).unwrap()),
        count_group_key(set.get(&third).unwrap())
    );
    assert_ne!(
        count_group_key(set.get(&first).unwrap()),
        count_group_key(set.get(&more).unwrap())
    );
    assert!(count_group_key(set.get(&reply).unwrap()).is_none());

    let table = imperial(&set, None);
    // Fence, the Revu group, Plumbline's count, Outlets p2, Outlets Dot, Switches.
    assert_eq!(table.roots.len(), 6);
    let row = table.row_of(&first).unwrap();
    assert_eq!(table.row_of(&second), Some(row));
    assert_eq!(table.row_of(&third), Some(row));
    assert_ne!(table.row_of(&more), Some(row));
    let r = &table.rows[row];
    assert_eq!(r.members, vec![second.clone(), third.clone()]);
    assert_eq!(r.count, Some(3.0));
    assert_eq!(r.kind_name, "Count");
    assert_eq!(table.value(row, &Builtin::Kind.into()), Value::text("Count"));
    // The row covers every item.
    assert!(r.x <= 10.0 && r.x + r.width >= 400.0, "{} {}", r.x, r.width);
    assert_eq!(r.children, vec![table.row_of(&reply).unwrap()]);
    assert_eq!(r.replies, 1);
    for (id, n) in [
        (&more, 2.0),
        (&other_page, 1.0),
        (&other_marker, 2.0),
        (&other_subject, 1.0),
    ] {
        let row = table.row_of(id).unwrap();
        assert_eq!(table.rows[row].count, Some(n));
        assert!(table.rows[row].members.is_empty());
        assert_eq!(table.rows[row].kind_name, "Count");
    }
    // The Count total is every item once.
    let layout = Layout::takeoff(&Schema::new());
    let view = View::of(&table, &layout);
    let total = view
        .subtotals
        .iter()
        .find(|s| s.column == Builtin::Count.into())
        .unwrap();
    assert_eq!((total.sum, total.values), (9.0, 5));
}

#[test]
fn files_carry_no_float_noise_and_the_table_keeps_full_precision() {
    assert_eq!(file_number(150.000_000_000_000_03, Some(2)), 150.0);
    assert_eq!(file_number(0.1 + 0.2, None), 0.3);
    // Places plus six are kept.
    assert_eq!(file_number(1.234_567_891_2, Some(2)), 1.234_567_89);
    assert_eq!(file_number(-0.000_000_000_1, Some(0)), 0.0);
    assert!(file_number(-0.0, None).is_sign_positive());
    assert_eq!(file_number(1.0e300, Some(2)), 1.0e300);
    assert!(file_number(f64::NAN, None).is_nan());

    let mut schema = Schema::new();
    let qty = schema.add("Qty", ColumnType::Number, &[]).unwrap();
    schema
        .set_display(&qty, Some(NumberDisplay::places(2)))
        .unwrap();
    let noise = schema.add_formula("Noise", "0.1 + 0.2").unwrap();
    let tenfold = schema.add_formula("Tenfold", "[Noise] * 10").unwrap();
    let mut set = AnnotSet::new();
    let mut a = length("Fence", 72.0);
    pl_takeoff::set_custom_value(
        &mut a,
        schema.get(&qty).unwrap(),
        Value::Number(150.000_000_000_000_03),
    )
    .unwrap();
    let id = set.add(a).unwrap();
    let table = imperial(&set, Some(&schema));
    let row = table.row_of(&id).unwrap();
    // The table and its formulas work at full precision.
    let value = |c: &str| table.value(row, &ColumnId::custom(c)).as_number().unwrap();
    assert_eq!(value(&noise), 0.1 + 0.2);
    assert_eq!(value(&tenfold), (0.1 + 0.2) * 10.0);
    assert_ne!(value(&tenfold), 3.0);

    let layout = showing(
        &schema,
        &[
            Builtin::Subject.into(),
            ColumnId::custom(&qty),
            ColumnId::custom(&noise),
            ColumnId::custom(&tenfold),
        ],
    );
    let view = View::of(&table, &layout);
    let options = CsvOptions {
        bom: false,
        totals: true,
        ..CsvOptions::default()
    };
    assert_eq!(
        to_csv(&table, &view, &layout, &options),
        "Subject,Qty,Noise,Tenfold\r\nFence,150,0.3,3\r\nTotal,150,0.3,3\r\n"
    );

    let bytes = to_xlsx(&table, &view, &layout).unwrap();
    let mut book: Xlsx<_> = open_workbook_from_rs(Cursor::new(bytes)).unwrap();
    let rows = book.worksheet_range(SHEET_ROWS).unwrap();
    let float = |r: u32, c: u32| match rows.get_value((r, c)) {
        Some(Data::Float(f)) => *f,
        Some(Data::Int(i)) => *i as f64,
        other => panic!("({r}, {c}) is not a number: {other:?}"),
    };
    assert_eq!(float(1, 1), 150.0);
    assert_eq!(float(1, 2), 0.3);
    assert_eq!(float(1, 3), 3.0);
    // The total row's cached results are rounded too.
    assert_eq!(float(3, 1), 150.0);
    assert_eq!(float(3, 3), 3.0);
}
