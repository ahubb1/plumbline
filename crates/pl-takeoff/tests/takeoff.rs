//! Takeoff: quantity columns from measurements, formula and choice columns, totals, legends,
//! spaces, sequences, the summary and the exports, with analytic expected values. Measurements
//! are also saved to and read back from the plain, rotated, `/UserUnit 2` and box-offset
//! fixture pages.

use std::io::{Cursor, Read};
use std::time::Instant;

use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
use pl_annot::{
    AnnotId, AnnotSet, Annotation, CountSymbol, DrawList, Geometry, MeasureFormat, MeasureSpec,
    Origin, PdfDate,
};
use pl_draw::DrawOp;
use pl_geom::{
    point_in_region, AreaFormat, AreaUnit, LengthFormat, Point, Rect, Scale, Unit, VolumeUnit,
};
use pl_pdf::{PdfFile, SaveOptions};
use pl_takeoff::{
    legend_draw_list, set_custom_value, set_space_name, space_at, to_csv, to_xlsx, Builtin,
    ColumnId, ColumnType, CsvOptions, Filter, FormulaError, Layout, LegendDef, LegendStyle,
    ListContext, NumberDisplay, QuantityUnits, Schema, Sequence, SortKey, Space, SwatchShape,
    Table, TakeoffContext, TakeoffSummary, Value, View, SHEET_GROUPS, SHEET_ROWS,
};

const FT: f64 = 0.3048;

fn og() -> Origin {
    Origin::new("Takeoff Tester", PdfDate::from_unix(1_790_000_000))
}

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

/// 1" = 20' (72 page units = 20 ft at user unit 1), feet and square feet.
fn eng(user_unit: f64) -> MeasureSpec {
    MeasureSpec::new(
        &Scale::engineering(20.0).unwrap(),
        user_unit,
        &MeasureFormat::new(
            LengthFormat::decimal(Unit::Foot, 2),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        ),
    )
}

/// 1:100 in metres.
fn metric(user_unit: f64) -> MeasureSpec {
    MeasureSpec::new(
        &Scale::ratio(100.0).unwrap(),
        user_unit,
        &MeasureFormat::new(
            LengthFormat::decimal(Unit::Metre, 2),
            AreaFormat::new(AreaUnit::SqMetre, 2),
        ),
    )
}

fn rect_ring(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point> {
    vec![p(x0, y0), p(x1, y0), p(x1, y1), p(x0, y1)]
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn num(v: Value) -> f64 {
    v.as_number()
        .unwrap_or_else(|| panic!("not a number: {v:?}"))
}

fn table_of(set: &AnnotSet, schema: Option<&Schema>, units: Option<QuantityUnits>) -> Table {
    Table::build_takeoff(
        set.iter(),
        &ListContext {
            schema,
            ..ListContext::default()
        },
        &TakeoffContext {
            units,
            ..TakeoffContext::default()
        },
    )
}

/// One of each kind, at 1" = 20' on a page with this user unit; positions in page units.
struct Kinds {
    length: AnnotId,
    poly: AnnotId,
    area: AnnotId,
    volume: AnnotId,
    angle: AnnotId,
    radius: AnnotId,
    diameter: AnnotId,
    count: AnnotId,
    perimeter: AnnotId,
}

fn every_kind(
    set: &mut AnnotSet,
    page: usize,
    o: Point,
    uu: f64,
    schema: Option<&Schema>,
) -> Kinds {
    let s = eng(uu);
    let at = |x: f64, y: f64| p(o.x + x, o.y + y);
    let og = og();
    let mut length = Annotation::measure_length(page, at(0.0, 0.0), at(72.0, 0.0), s.clone(), &og);
    if let Some(h) = schema.and_then(|s| s.get("height")) {
        set_custom_value(&mut length, h, Value::Number(10.0)).unwrap();
    }
    let poly = Annotation::measure_polylength(
        page,
        vec![at(0.0, 20.0), at(72.0, 20.0), at(72.0, 164.0)],
        s.clone(),
        &og,
    );
    let outer = |dx: f64| {
        vec![
            at(dx, 200.0),
            at(dx + 144.0, 200.0),
            at(dx + 144.0, 344.0),
            at(dx, 344.0),
        ]
    };
    let hole = |dx: f64| {
        vec![
            at(dx + 36.0, 236.0),
            at(dx + 108.0, 236.0),
            at(dx + 108.0, 308.0),
            at(dx + 36.0, 308.0),
        ]
    };
    let mut area = Annotation::measure_area(page, outer(0.0), vec![hole(0.0)], s.clone(), &og);
    if let Some(h) = schema.and_then(|s| s.get("height")) {
        set_custom_value(&mut area, h, Value::Number(10.0)).unwrap();
    }
    let volume = Annotation::measure_volume(
        page,
        outer(200.0),
        vec![hole(200.0)],
        3.0 * FT,
        s.clone(),
        &og,
    );
    let perimeter = Annotation::measure_perimeter(page, outer(400.0), s.clone(), &og);
    let angle = Annotation::measure_angle(
        page,
        at(100.0, 0.0),
        at(100.0, 50.0),
        at(150.0, 50.0),
        None,
        &og,
    );
    let radius = Annotation::measure_radius(page, at(300.0, 50.0), at(336.0, 50.0), s.clone(), &og);
    let diameter =
        Annotation::measure_diameter(page, at(400.0, 50.0), at(472.0, 50.0), s.clone(), &og);
    let count = Annotation::count(
        page,
        &[
            at(10.0, 400.0),
            at(20.0, 400.0),
            at(30.0, 400.0),
            at(40.0, 400.0),
            at(50.0, 400.0),
        ],
        CountSymbol::Ring,
        &og,
    );
    let mut add = |a: Annotation| set.add(a).unwrap();
    Kinds {
        length: add(length),
        poly: add(poly),
        area: add(area),
        volume: add(volume),
        angle: add(angle),
        radius: add(radius),
        diameter: add(diameter),
        count: add(count),
        perimeter: add(perimeter),
    }
}

fn cell(table: &Table, id: &AnnotId, b: Builtin) -> Value {
    table.value(table.row_of(id).unwrap(), &b.into())
}

/// Checks every quantity of `every_kind` against its analytic value, in feet (`k` real feet
/// per 20 ft of the user-unit-1 drawing: the user unit).
fn assert_imperial(table: &Table, k: &Kinds, uu: f64) {
    let f = |id: &AnnotId, b: Builtin| num(cell(table, id, b));
    // 72 units = 20 ft at user unit 1; the user unit scales the paper.
    assert!(close(f(&k.length, Builtin::Length), 20.0 * uu));
    assert!(close(f(&k.poly, Builtin::Length), 60.0 * uu));
    // Outer 40 x 40 ft, hole 20 x 20 ft.
    assert!(close(f(&k.area, Builtin::Area), 1200.0 * uu * uu));
    // Perimeter holds perimeters only: an Area's outline is not in it.
    assert_eq!(cell(table, &k.area, Builtin::Perimeter), Value::Empty);
    assert_eq!(cell(table, &k.volume, Builtin::Perimeter), Value::Empty);
    assert!(close(f(&k.perimeter, Builtin::Perimeter), 160.0 * uu));
    assert_eq!(cell(table, &k.perimeter, Builtin::Area), Value::Empty);
    // 1200 sq ft x 3 ft = 3600 cu ft = 133 1/3 cu yd.
    assert!(close(
        f(&k.volume, Builtin::Volume),
        3600.0 * uu * uu / 27.0
    ));
    assert!(close(f(&k.volume, Builtin::Depth), 3.0));
    assert!(close(f(&k.angle, Builtin::Angle), 90.0));
    assert!(close(f(&k.radius, Builtin::Radius), 10.0 * uu));
    assert_eq!(cell(table, &k.radius, Builtin::Length), Value::Empty);
    // A diameter is in Diameter (and Radius), never in the Length total.
    assert_eq!(cell(table, &k.diameter, Builtin::Length), Value::Empty);
    assert!(close(f(&k.diameter, Builtin::Diameter), 20.0 * uu));
    assert!(close(f(&k.diameter, Builtin::Radius), 10.0 * uu));
    assert!(close(f(&k.radius, Builtin::Diameter), 20.0 * uu));
    // A Volume's base area is in Base Area, never in the Area total.
    assert_eq!(cell(table, &k.volume, Builtin::Area), Value::Empty);
    assert!(close(f(&k.volume, Builtin::BaseArea), 1200.0 * uu * uu));
    assert_eq!(cell(table, &k.area, Builtin::BaseArea), Value::Empty);
    assert_eq!(f(&k.count, Builtin::Count), 5.0);
    let text = |id: &AnnotId, b: Builtin| cell(table, id, b).display();
    assert_eq!(text(&k.length, Builtin::Unit), "ft");
    assert_eq!(text(&k.area, Builtin::Unit), "sq ft");
    assert_eq!(text(&k.volume, Builtin::Unit), "cu yd");
    assert_eq!(text(&k.angle, Builtin::Unit), "\u{b0}");
    assert_eq!(text(&k.count, Builtin::Unit), "ea");
    assert_eq!(text(&k.length, Builtin::Scale), eng(uu).ratio);
}

/// A height given by a formula column is worked out before Wall Area, in the list's current
/// units, and a formula that uses Wall Area comes after both.
#[test]
fn wall_area_takes_a_formula_height_worked_out_first() {
    let mut schema = Schema::new();
    let depth = schema.add("Depth ft", ColumnType::Number, &[]).unwrap();
    let height = schema.add_formula("Wall Ht", "[Depth ft] * 3").unwrap();
    let paint = schema.add_formula("Paint", "[Wall Area] * 2").unwrap();
    let mut set = AnnotSet::new();
    // 72 units at 1" = 20' is 20 ft.
    let mut length = Annotation::measure_length(0, p(0.0, 0.0), p(72.0, 0.0), eng(1.0), &og());
    set_custom_value(&mut length, schema.get(&depth).unwrap(), Value::Number(1.0)).unwrap();
    let id = set.add(length).unwrap();
    let build = |units: Option<QuantityUnits>| {
        Table::build_takeoff(
            set.iter(),
            &ListContext {
                schema: Some(&schema),
                ..ListContext::default()
            },
            &TakeoffContext {
                units,
                height_column: Some(&height),
                height_unit: Some(Unit::Foot),
                ..TakeoffContext::default()
            },
        )
    };
    let mut table = build(Some(QuantityUnits::imperial()));
    let row = table.row_of(&id).unwrap();
    let custom = |t: &Table, c: &str| num(t.value(row, &ColumnId::Custom(c.to_string())));
    assert!(close(custom(&table, &height), 3.0));
    // 20 ft x 3 ft.
    assert!(close(num(cell(&table, &id, Builtin::WallArea)), 60.0));
    assert!(close(custom(&table, &paint), 120.0));
    // In metric the height (typed in feet) still means 3 ft: 60 sq ft in square metres.
    table.set_units(QuantityUnits::metric());
    assert!(close(
        num(cell(&table, &id, Builtin::WallArea)),
        60.0 * FT * FT
    ));
    assert!(close(custom(&table, &paint), 120.0 * FT * FT));

    // A height that uses Wall Area is a loop, reported as such, not a stale value.
    let mut looped = Schema::new();
    let h = looped.add_formula("Wall Ht", "[Wall Area] / 10").unwrap();
    let table = Table::build_takeoff(
        set.iter(),
        &ListContext {
            schema: Some(&looped),
            ..ListContext::default()
        },
        &TakeoffContext {
            height_column: Some(&h),
            ..TakeoffContext::default()
        },
    );
    let row = table.row_of(&id).unwrap();
    assert!(matches!(
        table.formula_error(row, &ColumnId::Custom(h.clone())),
        Some(FormulaError::Cycle(_))
    ));
    assert_eq!(cell(&table, &id, Builtin::WallArea), Value::Empty);
}

#[test]
fn quantity_columns_have_their_analytic_values_and_convert() {
    let mut schema = Schema::new();
    schema.add("Height", ColumnType::Number, &[]).unwrap();
    for uu in [1.0, 2.0] {
        let mut set = AnnotSet::new();
        let k = every_kind(&mut set, 0, p(50.0, 50.0), uu, Some(&schema));
        // Units follow the measurements: these are imperial.
        let mut table = table_of(&set, Some(&schema), None);
        assert_eq!(table.units, QuantityUnits::imperial());
        assert_imperial(&table, &k, uu);
        // Wall Area: Length (or Perimeter) times the row's Height (10 ft).
        assert!(close(
            num(cell(&table, &k.length, Builtin::WallArea)),
            200.0 * uu
        ));
        assert!(close(
            num(cell(&table, &k.area, Builtin::WallArea)),
            1600.0 * uu
        ));
        assert_eq!(cell(&table, &k.poly, Builtin::WallArea), Value::Empty);
        // A markup that is not a measurement has no quantities.
        let row = table.row_of(&k.length).unwrap();
        assert_eq!(table.heading(&Builtin::Length.into()), "Length (ft)");
        assert_eq!(
            table.cell_text(row, &Builtin::Length.into()),
            format!("{:.2}", 20.0 * uu)
        );

        // To metric.
        table.set_units(QuantityUnits::metric());
        let f = |id: &AnnotId, b: Builtin| num(cell(&table, id, b));
        assert!(close(f(&k.length, Builtin::Length), 20.0 * uu * FT));
        assert!(close(f(&k.area, Builtin::Area), 1200.0 * uu * uu * FT * FT));
        assert!(close(
            f(&k.volume, Builtin::Volume),
            3600.0 * uu * uu * FT * FT * FT
        ));
        assert!(close(f(&k.volume, Builtin::Depth), 3.0 * FT));
        // Wall Area keeps its meaning: heights stay in the unit they were typed in (feet).
        assert!(close(f(&k.length, Builtin::WallArea), 200.0 * uu * FT * FT));
        assert_eq!(table.heading(&Builtin::Volume.into()), "Volume (cu m)");
        assert_eq!(cell(&table, &k.area, Builtin::Unit).display(), "sq m");

        // Square yards and cubic feet.
        let mut yards = QuantityUnits::imperial();
        yards.area = AreaUnit::SqYard;
        yards.volume = VolumeUnit::CuFoot;
        table.set_units(yards);
        assert!(close(
            num(cell(&table, &k.area, Builtin::Area)),
            1200.0 * uu * uu / 9.0
        ));
        assert!(close(
            num(cell(&table, &k.volume, Builtin::Volume)),
            3600.0 * uu * uu
        ));
        // Feet and inches.
        let mut arch = QuantityUnits::imperial();
        arch.length = Unit::FeetInches;
        arch.length_places = 0;
        table.set_units(arch);
        let row = table.row_of(&k.poly).unwrap();
        assert_eq!(
            table.cell_text(row, &Builtin::Length.into()),
            format!("{}'-0\"", 60.0 * uu)
        );
    }

    // A metric drawing chooses metric units by itself.
    let mut set = AnnotSet::new();
    let id = set
        .add(Annotation::measure_length(
            0,
            p(0.0, 0.0),
            p(72.0, 0.0),
            metric(1.0),
            &og(),
        ))
        .unwrap();
    let table = table_of(&set, None, None);
    assert_eq!(table.units, QuantityUnits::metric());
    // 72 pt = 25.4 mm of paper = 2.54 m at 1:100.
    assert!(close(num(cell(&table, &id, Builtin::Length)), 2.54));
}

#[test]
fn totals_and_subtotals_cover_quantities_and_formulas() {
    let mut schema = Schema::new();
    schema.add("Rate", ColumnType::Number, &[]).unwrap();
    schema.add_formula("Cost", "[Length] * [Rate]").unwrap();
    let rate = schema.get("rate").unwrap().clone();
    let mut set = AnnotSet::new();
    // Fence: 20 ft + 40 ft at 3/ft; Kerb: 60 ft at 5/ft; a note with nothing.
    for (subject, len, r) in [
        ("Fence", 72.0, 3.0),
        ("Fence", 144.0, 3.0),
        ("Kerb", 216.0, 5.0),
    ] {
        let mut a = Annotation::measure_length(0, p(0.0, 0.0), p(len, 0.0), eng(1.0), &og());
        a.meta.subject = Some(subject.into());
        set_custom_value(&mut a, &rate, Value::Number(r)).unwrap();
        set.add(a).unwrap();
    }
    let mut note = Annotation::note(0, Rect::new(0.0, 0.0, 10.0, 10.0), "", &og());
    note.meta.subject = Some("Kerb".into());
    set.add(note).unwrap();
    let table = table_of(&set, Some(&schema), None);
    assert!(table.is_summed(&ColumnId::custom("cost")));
    let view = View::build(
        &table,
        &Filter::default(),
        &[],
        &[SortKey::asc(Builtin::Subject)],
    );
    let sum = |subs: &[pl_takeoff::Subtotal], c: ColumnId| {
        let s = subs.iter().find(|s| s.column == c).unwrap();
        (s.sum, s.values)
    };
    let assert_sum = |(got, n): (f64, usize), want: f64, values: usize| {
        assert!(
            close(got, want) && n == values,
            "{got} over {n}, wanted {want} over {values}"
        );
    };
    assert_sum(sum(&view.subtotals, Builtin::Length.into()), 120.0, 3);
    // The note has no length: an empty cell is 0 in arithmetic, so its Cost is 0.
    assert_sum(sum(&view.subtotals, ColumnId::custom("cost")), 480.0, 4);
    let pl_takeoff::Body::Groups(groups) = &view.body else {
        panic!("grouped")
    };
    assert_eq!(groups[0].label, "Fence");
    assert_sum(sum(&groups[0].subtotals, Builtin::Length.into()), 60.0, 2);
    assert_sum(
        sum(&groups[0].subtotals, ColumnId::custom("cost")),
        180.0,
        2,
    );
    assert_eq!(groups[1].count, 2);
    assert_sum(sum(&groups[1].subtotals, Builtin::Length.into()), 60.0, 1);
    assert_sum(
        sum(&groups[1].subtotals, ColumnId::custom("cost")),
        300.0,
        2,
    );

    // The summary by subject says the same.
    let summary = TakeoffSummary::by_subject(&table);
    assert!(close(
        summary.total(&Builtin::Length.into()).unwrap(),
        120.0
    ));
    assert!(close(
        summary.total(&ColumnId::custom("cost")).unwrap(),
        480.0
    ));
    let fence = &summary.lines[0];
    assert_eq!(
        (fence.path.as_slice(), fence.count),
        (&["Fence".to_string()][..], 2)
    );
    let length_at = summary
        .columns
        .iter()
        .position(|c| c.column == Builtin::Length.into())
        .unwrap();
    assert!(close(fence.totals[length_at].unwrap(), 60.0));
    assert_eq!(summary.columns[length_at].unit.as_deref(), Some("ft"));
    // Columns with nothing in them are left out.
    assert!(!summary
        .columns
        .iter()
        .any(|c| c.column == Builtin::Volume.into()));
}

#[test]
fn formula_columns_evaluate_in_dependency_order_with_typed_errors() {
    let mut s = Schema::new();
    s.add_choice(
        "Material",
        &[
            ("Steel", Some(12.5)),
            ("Timber", Some(4.0)),
            ("Concrete", None),
        ],
    )
    .unwrap();
    s.add("Divisor", ColumnType::Number, &[]).unwrap();
    // Total uses Markup, which is defined after it.
    s.add_formula("Total", "round([Cost] * (1 + [Markup]), 2)")
        .unwrap();
    s.add_formula("Cost", "[Length] * [Material]").unwrap();
    s.add_formula("Markup", "0.1").unwrap();
    s.add_formula("Ratio", "[Length] / [Divisor]").unwrap();
    s.add_formula("Loop A", "[Loop B] + 1").unwrap();
    s.add_formula("Loop B", "[loop a] * 2").unwrap();
    s.add_formula("Uses Loop", "[Loop A] + 1").unwrap();
    s.add_formula("Self", "[Self] + 1").unwrap();
    s.add_formula("Ghost", "[Nonexistent] + 1").unwrap();
    s.add_formula("Mismatch", "[Subject] * 2").unwrap();
    s.add_formula("Label", "if([Length] > 30, \"long\", \"short\")")
        .unwrap();
    s.add_formula("Big", "[Material] = \"Timber\" or [Length] >= 40")
        .unwrap();
    s.set_display("total", Some(NumberDisplay::currency("$")))
        .unwrap();
    s.validate().unwrap();
    assert!(matches!(
        s.add_formula("Broken", "[Length] *"),
        Err(pl_takeoff::ColumnError::Formula { .. })
    ));
    assert!(s.get("broken").is_none());

    let mut set = AnnotSet::new();
    let mut ids = Vec::new();
    // 20 ft steel /2, 40 ft timber /0, 60 ft concrete /3, 20 ft with no material.
    for (len, material, divisor) in [
        (72.0, Some("Steel"), 2.0),
        (144.0, Some("timber"), 0.0),
        (216.0, Some("Concrete"), 3.0),
        (72.0, None, 4.0),
    ] {
        let mut a = Annotation::measure_length(0, p(0.0, 0.0), p(len, 0.0), eng(1.0), &og());
        if let Some(m) = material {
            set_custom_value(&mut a, s.get("material").unwrap(), Value::text(m)).unwrap();
        }
        set_custom_value(&mut a, s.get("divisor").unwrap(), Value::Number(divisor)).unwrap();
        ids.push(set.add(a).unwrap());
    }
    let table = table_of(&set, Some(&s), None);
    let v =
        |i: usize, col: &str| table.value(table.row_of(&ids[i]).unwrap(), &ColumnId::custom(col));
    let e = |i: usize, col: &str| {
        table
            .formula_error(table.row_of(&ids[i]).unwrap(), &ColumnId::custom(col))
            .cloned()
    };

    // Choice values in formulas; dependency order.
    assert!(close(num(v(0, "cost")), 250.0));
    assert!(close(num(v(1, "cost")), 160.0));
    assert!(close(num(v(0, "total")), 275.0));
    assert!(close(num(v(1, "total")), 176.0));
    assert_eq!(
        table.cell_text(table.row_of(&ids[0]).unwrap(), &ColumnId::custom("total")),
        "$275.00"
    );
    // An option without a number is text: text times a length is a type mismatch, and the
    // error passes on to Total.
    assert!(matches!(
        e(2, "cost"),
        Some(FormulaError::TypeMismatch { .. })
    ));
    assert_eq!(v(2, "cost"), Value::text("#VALUE!"));
    assert!(matches!(
        e(2, "total"),
        Some(FormulaError::TypeMismatch { .. })
    ));
    // No material at all is an empty cell: zero.
    assert!(close(num(v(3, "cost")), 0.0));
    // Division by zero in one row only.
    assert!(close(num(v(0, "ratio")), 10.0));
    assert_eq!(e(1, "ratio"), Some(FormulaError::DivisionByZero));
    assert_eq!(v(1, "ratio"), Value::text("#DIV/0!"));
    assert!(close(num(v(2, "ratio")), 20.0));
    // Loops.
    let lp = FormulaError::Cycle(vec!["Loop A".into(), "Loop B".into()]);
    for i in 0..4 {
        assert_eq!(e(i, "loop_a"), Some(lp.clone()));
        assert_eq!(e(i, "loop_b"), Some(lp.clone()));
        assert_eq!(
            e(i, "uses_loop"),
            Some(lp.clone()),
            "a loop's error passes on"
        );
        assert_eq!(e(i, "self"), Some(FormulaError::Cycle(vec!["Self".into()])));
        assert_eq!(
            e(i, "ghost"),
            Some(FormulaError::UnknownColumn("Nonexistent".into()))
        );
        assert!(matches!(
            e(i, "mismatch"),
            Some(FormulaError::TypeMismatch { .. })
        ));
        assert_eq!(v(i, "loop_a"), Value::text("#CYCLE!"));
        assert_eq!(v(i, "ghost"), Value::text("#REF!"));
    }
    let status = |id: &str| {
        table
            .formula_status
            .iter()
            .find(|(c, _)| c == id)
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(status("loop_a"), Some(lp.clone()));
    assert_eq!(status("uses_loop"), None, "its own formula is fine");
    assert_eq!(status("total"), None);
    // Text and truth values.
    assert_eq!(v(0, "label"), Value::text("short"));
    assert_eq!(v(2, "label"), Value::text("long"));
    assert_eq!(v(0, "big"), Value::Bool(false));
    assert_eq!(v(1, "big"), Value::Bool(true));
    assert_eq!(v(2, "big"), Value::Bool(true));
    // Sums skip the error cells: Cost = 250 + 160 + 0.
    let view = View::build(&table, &Filter::default(), &[], &[]);
    let cost = view
        .subtotals
        .iter()
        .find(|t| t.column == ColumnId::custom("cost"))
        .unwrap();
    assert!(close(cost.sum, 410.0) && cost.values == 3);
    // A formula column cannot be typed into.
    let mut a = set.get(&ids[0]).unwrap().clone();
    assert!(matches!(
        set_custom_value(&mut a, s.get("cost").unwrap(), Value::Number(1.0)),
        Err(pl_takeoff::ColumnError::Computed(_))
    ));
    // The schema keeps option numbers through edits and serialisation.
    let mut s2 = s.clone();
    s2.set_options("material", &["Timber", "Steel", "Brick"])
        .unwrap();
    let m = s2.get("material").unwrap();
    assert_eq!(m.option_values, [Some(4.0), Some(12.5), None]);
    s2.set_option_value("material", "brick", Some(9.0)).unwrap();
    assert_eq!(
        s2.get("material")
            .unwrap()
            .option_value(&Value::text("BRICK")),
        Some(9.0)
    );
    let json = serde_json::to_string(&s2).unwrap();
    let back: Schema = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s2);
    back.validate().unwrap();

    // Editing a formula and recalculating.
    let mut table = table;
    let at = table.custom.iter().position(|c| c.id == "markup").unwrap();
    table.custom[at].formula = "0.5".into();
    table.recalculate();
    assert_eq!(
        table.value(table.row_of(&ids[0]).unwrap(), &ColumnId::custom("total")),
        Value::Number(375.0)
    );
}

#[test]
fn legends_evaluate_and_follow_edits() {
    let mut set = AnnotSet::new();
    let mut fence_id = None;
    for (subject, len, colour) in [
        ("Fence", 72.0, [200u8, 0, 0]),
        ("Kerb", 216.0, [0, 0, 200]),
        ("Fence", 144.0, [200, 0, 0]),
    ] {
        let mut a = Annotation::measure_length(0, p(0.0, 0.0), p(len, 0.0), eng(1.0), &og());
        a.meta.subject = Some(subject.into());
        a.style.stroke = Some(pl_annot::Rgb::from_u8(colour[0], colour[1], colour[2]));
        let id = set.add(a).unwrap();
        fence_id.get_or_insert(id);
    }
    let mut count = Annotation::count(0, &[p(1.0, 1.0), p(2.0, 2.0)], CountSymbol::Dot, &og());
    count.meta.subject = Some("Gully".into());
    set.add(count).unwrap();

    let def = LegendDef::new(
        "Linear items",
        Builtin::Subject,
        Some(Builtin::Length.into()),
    );
    let legend = def.evaluate(&table_of(&set, None, None));
    let rows: Vec<(&str, &str, &str, usize)> = legend
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.text.as_str(), r.unit.as_str(), r.count))
        .collect();
    assert_eq!(
        rows,
        [
            ("Fence", "60.00", "ft", 2),
            ("Gully", "", "ft", 1),
            ("Kerb", "60.00", "ft", 1)
        ]
    );
    assert_eq!(legend.rows[0].swatch.shape, SwatchShape::Line);
    assert_eq!(legend.rows[0].swatch.colour, Some([200, 0, 0]));
    assert_eq!(legend.rows[1].swatch.shape, SwatchShape::Marker);
    assert_eq!(legend.rows[1].swatch.symbol.as_deref(), Some("Dot"));
    assert_eq!(legend.total.as_ref().unwrap().text, "120.00");
    assert_eq!(
        legend.contents(),
        "Linear items\nFence: 60.00 ft\nGully: ft\nKerb: 60.00 ft\nTotal: 120.00 ft"
    );

    let style = LegendStyle::default();
    let drawn = legend_draw_list(&legend, p(500.0, 700.0), &style).unwrap();
    let texts = |list: &DrawList| {
        fn walk(ops: &[DrawOp], out: &mut Vec<String>) {
            for op in ops {
                match op {
                    DrawOp::Text(t) => out.push(t.text.clone()),
                    DrawOp::Group(g) => walk(&g.ops, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&list.ops, &mut out);
        out
    };
    let before = texts(&drawn.list);
    assert!(before.contains(&"60.00".to_string()) && before.contains(&"120.00".to_string()));
    // The top-left corner is where it was asked for; the box hangs down and to the right.
    assert!(close(drawn.rect.x0, 500.0) && close(drawn.rect.y1, 700.0));
    assert!(drawn.rect.width() > 50.0 && drawn.rect.height() > 40.0);
    let bounds = drawn.list.bounds().unwrap();
    let frame = drawn.rect.inflate(1.0, 1.0);
    assert_eq!(
        frame.union(bounds),
        frame,
        "everything drawn is inside the box"
    );

    // An edit: the first fence gets 30 ft longer. The legend, evaluated again, says so.
    let a = set.get_mut(fence_id.as_ref().unwrap()).unwrap();
    a.geometry = Geometry::Line {
        a: p(0.0, 0.0),
        b: p(72.0 + 108.0, 0.0),
    };
    a.sync_measurement();
    let legend = def.evaluate(&table_of(&set, None, None));
    assert_eq!(legend.rows[0].text, "90.00");
    assert_eq!(legend.total.as_ref().unwrap().text, "150.00");
    let after = texts(
        &legend_draw_list(&legend, p(500.0, 700.0), &style)
            .unwrap()
            .list,
    );
    assert!(after.contains(&"90.00".to_string()) && after.contains(&"150.00".to_string()));
    assert!(!after.contains(&"120.00".to_string()));

    // A count legend, and a legend on a page shown rotated by 90 degrees reads upright: its
    // box runs along +y in page space from the anchor.
    let counts =
        LegendDef::new("Items", Builtin::Subject, None).evaluate(&table_of(&set, None, None));
    assert_eq!(
        counts
            .rows
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>(),
        ["2", "1", "1"]
    );
    assert_eq!(counts.total.as_ref().unwrap().text, "4");
    let upright = legend_draw_list(&counts, p(0.0, 0.0), &style).unwrap().rect;
    let turned = legend_draw_list(
        &counts,
        p(0.0, 0.0),
        &LegendStyle {
            rotation: 90,
            ..style
        },
    )
    .unwrap()
    .rect;
    assert!(close(turned.height(), upright.width()) && close(turned.width(), upright.height()));
    assert!(close(turned.y0, 0.0) && close(turned.x0, 0.0));

    // Text the standard fonts cannot write is refused, not dropped.
    let mut bad = counts.clone();
    bad.title = "\u{65e5}\u{672c}".into();
    assert!(legend_draw_list(&bad, p(0.0, 0.0), &style).is_err());
}

#[test]
fn spaces_assign_by_anchor_with_edges_nesting_and_holes() {
    let floor = Space::new("Floor", 0, rect_ring(0.0, 0.0, 400.0, 300.0), vec![]);
    let room_a = Space::new("Room A", 0, rect_ring(0.0, 0.0, 200.0, 300.0), vec![]);
    let room_b = Space::new("Room B", 0, rect_ring(200.0, 0.0, 400.0, 150.0), vec![]);
    let closet = Space::new("Closet", 0, rect_ring(50.0, 50.0, 100.0, 100.0), vec![]);
    let atrium = Space::new(
        "Atrium",
        0,
        rect_ring(250.0, 175.0, 390.0, 290.0),
        vec![rect_ring(300.0, 200.0, 340.0, 260.0)],
    );
    let other_page = Space::new("Upstairs", 1, rect_ring(0.0, 0.0, 1000.0, 1000.0), vec![]);
    let spaces = vec![floor, room_a, room_b, closet, atrium, other_page];
    let at = |x: f64, y: f64, page: usize| {
        space_at(&spaces, page, p(x, y), None).map(|s| s.name.as_str())
    };
    assert_eq!(at(300.0, 160.0, 0), Some("Floor"));
    assert_eq!(at(75.0, 75.0, 0), Some("Closet"), "the innermost space");
    assert_eq!(at(150.0, 200.0, 0), Some("Room A"));
    // On the edge two rooms share: the smaller room.
    assert_eq!(at(200.0, 100.0, 0), Some("Room B"));
    // On the outer edge, and on a corner: still inside.
    assert_eq!(at(0.0, 200.0, 0), Some("Room A"));
    assert_eq!(at(400.0, 300.0, 0), Some("Floor"));
    assert_eq!(at(50.0, 75.0, 0), Some("Closet"));
    // In the hole of the atrium: the floor; on the hole's edge: the atrium.
    assert_eq!(at(320.0, 230.0, 0), Some("Floor"));
    assert_eq!(at(300.0, 230.0, 0), Some("Atrium"));
    assert_eq!(at(280.0, 230.0, 0), Some("Atrium"));
    // Outside everything, and on another page.
    assert_eq!(at(401.0, 10.0, 0), None);
    assert_eq!(at(75.0, 75.0, 1), Some("Upstairs"));
    assert_eq!(at(75.0, 75.0, 2), None);
    // Equal areas: the later one.
    let twins = vec![
        Space::new("First", 0, rect_ring(0.0, 0.0, 10.0, 10.0), vec![]),
        Space::new("Second", 0, rect_ring(0.0, 0.0, 10.0, 10.0), vec![]),
    ];
    assert_eq!(
        space_at(&twins, 0, p(5.0, 5.0), None).unwrap().name,
        "Second"
    );

    // Markups get the Space column from their anchors.
    let og = og();
    let mut set = AnnotSet::new();
    let note = set
        .add(Annotation::note(
            0,
            Rect::new(70.0, 70.0, 80.0, 80.0),
            "",
            &og,
        ))
        .unwrap();
    // A line whose middle is on the A/B wall.
    let line = set
        .add(Annotation::line(0, p(150.0, 100.0), p(250.0, 100.0), &og))
        .unwrap();
    // An L-shaped area whose centroid is outside it; its visual centre is inside Room A.
    let ell = vec![
        p(10.0, 150.0),
        p(190.0, 150.0),
        p(190.0, 170.0),
        p(30.0, 170.0),
        p(30.0, 290.0),
        p(10.0, 290.0),
    ];
    let l_area = set
        .add(Annotation::measure_area(0, ell, vec![], eng(1.0), &og))
        .unwrap();
    // A count's first marker decides.
    let count = set
        .add(Annotation::count(
            0,
            &[p(320.0, 230.0), p(75.0, 75.0)],
            CountSymbol::Dot,
            &og,
        ))
        .unwrap();
    // A space drawn as a marked polygon is picked up, and is its own space.
    let mut hall = Annotation::polygon(0, rect_ring(0.0, 310.0, 400.0, 340.0), &og);
    assert!(set_space_name(&mut hall, Some("Hall")));
    assert!(!set_space_name(&mut hall, Some(" Hall ")));
    let hall = set.add(hall).unwrap();
    let in_hall = set
        .add(Annotation::note(
            0,
            Rect::new(100.0, 320.0, 110.0, 330.0),
            "",
            &og,
        ))
        .unwrap();
    let table = Table::build_takeoff(
        set.iter(),
        &ListContext::default(),
        &TakeoffContext {
            spaces: &spaces,
            ..TakeoffContext::default()
        },
    );
    let space = |id: &AnnotId| cell(&table, id, Builtin::Space).display();
    assert_eq!(space(&note), "Closet");
    assert_eq!(space(&line), "Room B");
    assert_eq!(space(&l_area), "Room A");
    assert_eq!(space(&count), "Floor");
    assert_eq!(space(&hall), "Hall");
    assert_eq!(space(&in_hall), "Hall");
    assert_eq!(table.spaces.len(), spaces.len() + 1);
    // Grouping by space.
    let summary = TakeoffSummary::by_space(&table);
    let top: Vec<&str> = summary
        .lines
        .iter()
        .filter(|l| l.depth == 0)
        .map(|l| l.path[0].as_str())
        .collect();
    assert_eq!(top, ["Closet", "Floor", "Hall", "Room A", "Room B"]);
    // A formula can read the space.
    let mut s = Schema::new();
    s.add_formula("In Hall", "[Space] = \"hall\"").unwrap();
    let table = Table::build_takeoff(
        set.iter(),
        &ListContext {
            schema: Some(&s),
            ..ListContext::default()
        },
        &TakeoffContext {
            spaces: &spaces,
            ..TakeoffContext::default()
        },
    );
    assert_eq!(
        table.value(
            table.row_of(&in_hall).unwrap(),
            &ColumnId::custom("in_hall")
        ),
        Value::Bool(true)
    );
    assert_eq!(
        table.value(table.row_of(&note).unwrap(), &ColumnId::custom("in_hall")),
        Value::Bool(false)
    );
}

#[test]
fn sequences_number_labels() {
    let s = Sequence::new("D-", 1, 1).with_digits(2);
    assert_eq!(s.labels(3), ["D-01", "D-02", "D-03"]);
    assert_eq!(s.label(99), "D-100");
    let tens = Sequence::new("", 100, 10).with_suffix("a");
    assert_eq!(tens.labels(3), ["100a", "110a", "120a"]);
    let down = Sequence::new("L", 3, -1);
    assert_eq!(down.labels(5), ["L3", "L2", "L1", "L0", "L-1"]);
    assert_eq!(down.parse("L-1"), Some(-1));
    // Continuing what is already there, ignoring what does not belong.
    assert_eq!(s.parse("d-07"), Some(7));
    assert_eq!(s.parse("D-7x"), None);
    assert_eq!(s.parse("D-"), None);
    assert_eq!(s.parse("X-07"), None);
    assert_eq!(s.next(["D-03", "D-11", "Door", "D-x", "E-40"]), "D-12");
    assert_eq!(s.next(Vec::<&str>::new()), "D-01");
    assert_eq!(down.next(["L3", "L2"]), "L1");
    assert_eq!(
        s.continue_from(["D-01", "D-02"], 3),
        ["D-03", "D-04", "D-05"]
    );
    // Overflow saturates instead of panicking.
    let huge = Sequence::new("", i64::MAX - 1, 5);
    assert_eq!(huge.label(3), i64::MAX.to_string());
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(serde_json::from_str::<Sequence>(&json).unwrap(), s);
}

#[test]
fn exports_carry_quantities_formulas_and_sums() {
    let mut s = Schema::new();
    s.add("Rate", ColumnType::Number, &[]).unwrap();
    s.add("Divisor", ColumnType::Number, &[]).unwrap();
    s.add_formula("Cost", "[Length] * [Rate]").unwrap();
    s.add_formula("Per", "[Length] / [Divisor]").unwrap();
    s.set_display("cost", Some(NumberDisplay::currency("$")))
        .unwrap();
    let mut set = AnnotSet::new();
    for (subject, len, rate, divisor) in [
        ("Fence", 72.0, 3.0, 2.0),
        ("Kerb", 216.0, 5.0, 0.0),
        ("Fence", 144.0, 3.0, 4.0),
    ] {
        let mut a = Annotation::measure_length(0, p(0.0, 0.0), p(len, 0.0), eng(1.0), &og());
        a.meta.subject = Some(subject.into());
        set_custom_value(&mut a, s.get("rate").unwrap(), Value::Number(rate)).unwrap();
        set_custom_value(&mut a, s.get("divisor").unwrap(), Value::Number(divisor)).unwrap();
        set.add(a).unwrap();
    }
    let mut units = QuantityUnits::imperial();
    units.length_places = 3;
    let table = table_of(&set, Some(&s), Some(units));
    let mut layout = Layout::for_schema(&s);
    for c in layout.columns.iter_mut() {
        c.visible = matches!(
            &c.column,
            ColumnId::Builtin(Builtin::Subject | Builtin::Length)
        ) || matches!(&c.column, ColumnId::Custom(id) if id == "cost" || id == "per");
    }
    layout.group = vec![SortKey::asc(Builtin::Subject)];
    let view = View::of(&table, &layout);
    let visible = layout.visible();
    let col = |c: ColumnId| visible.iter().position(|v| *v == c).unwrap() as u32;
    let (length_col, cost_col, per_col) = (
        col(Builtin::Length.into()),
        col(ColumnId::custom("cost")),
        col(ColumnId::custom("per")),
    );

    let csv = to_csv(
        &table,
        &view,
        &layout,
        &CsvOptions {
            bom: false,
            ..CsvOptions::default()
        },
    );
    let first = csv.lines().next().unwrap();
    assert!(first.contains("Length (ft)"), "{first}");
    assert!(csv.contains(",20.000,") && csv.contains("#DIV/0!"), "{csv}");

    let bytes = to_xlsx(&table, &view, &layout).unwrap();
    let mut book: Xlsx<_> = open_workbook_from_rs(Cursor::new(bytes.clone())).unwrap();
    let rows = book.worksheet_range(SHEET_ROWS).unwrap();
    assert_eq!(
        rows.get_value((0, length_col)),
        Some(&Data::String("Length (ft)".into()))
    );
    // Rows in view order: Fence 20, Fence 40, Kerb 60.
    let float = |r: u32, c: u32| match rows.get_value((r, c)) {
        Some(Data::Float(f)) => *f,
        Some(Data::Int(i)) => *i as f64,
        other => panic!("({r}, {c}) is not a number: {other:?}"),
    };
    assert!(close(float(1, length_col), 20.0) && close(float(3, length_col), 60.0));
    assert!(
        close(float(1, cost_col), 60.0),
        "a formula column is written as its value"
    );
    assert!(close(float(3, cost_col), 300.0));
    assert!(close(float(1, per_col), 10.0));
    assert_eq!(
        rows.get_value((3, per_col)),
        Some(&Data::String("#DIV/0!".into()))
    );
    // The total row: SUMs with their results.
    let formulas = book.worksheet_formula(SHEET_ROWS).unwrap();
    let letter = |c: u32| char::from(b'A' + c as u8);
    let total_row = 5;
    for c in [length_col, cost_col, per_col] {
        assert_eq!(
            formulas.get_value((total_row, c)).map(String::as_str),
            Some(format!("SUM({0}2:{0}4)", letter(c)).as_str())
        );
    }
    assert!(close(float(total_row, length_col), 120.0));
    assert!(close(float(total_row, cost_col), 480.0));
    assert!(
        close(float(total_row, per_col), 20.0),
        "the error cell is not added"
    );
    // The groups sheet sums each group.
    let groups = book.worksheet_range(SHEET_GROUPS).unwrap();
    let group_formulas = book.worksheet_formula(SHEET_GROUPS).unwrap();
    assert_eq!(
        groups.get_value((1, 0)),
        Some(&Data::String("Fence".into()))
    );
    let header: Vec<String> = (0..groups.width() as u32)
        .map(|c| {
            groups
                .get_value((0, c))
                .map(|d| d.to_string())
                .unwrap_or_default()
        })
        .collect();
    let at = header.iter().position(|h| h == "Length (ft)").unwrap() as u32;
    assert_eq!(
        group_formulas.get_value((1, at)).map(String::as_str),
        Some(format!("SUM(Markups!{0}2:{0}3)", letter(length_col)).as_str())
    );
    // Quantity and currency columns carry their decimal places as a number format.
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut styles = String::new();
    zip.by_name("xl/styles.xml")
        .unwrap()
        .read_to_string(&mut styles)
        .unwrap();
    assert!(styles.contains("formatCode=\"0.000\""), "{styles}");
}

/// The area of a region with a cutout matches a raster estimate of the same region.
#[test]
fn cutout_area_matches_a_raster_estimate() {
    let outer = vec![
        p(10.0, 10.0),
        p(310.0, 30.0),
        p(260.0, 240.0),
        p(120.0, 300.0),
        p(0.0, 180.0),
    ];
    let holes = vec![
        vec![p(60.0, 60.0), p(140.0, 70.0), p(110.0, 150.0)],
        vec![
            p(180.0, 120.0),
            p(240.0, 120.0),
            p(240.0, 200.0),
            p(180.0, 200.0),
        ],
    ];
    let mut set = AnnotSet::new();
    let id = set
        .add(Annotation::measure_area(
            0,
            outer.clone(),
            holes.clone(),
            eng(1.0),
            &og(),
        ))
        .unwrap();
    let table = table_of(&set, None, None);
    let sq_ft = num(cell(&table, &id, Builtin::Area));
    // Count quarter-unit pixel centres inside the region.
    let step = 0.25;
    let mut inside = 0u64;
    let mut y = step / 2.0;
    while y < 310.0 {
        let mut x = step / 2.0;
        while x < 320.0 {
            if point_in_region(p(x, y), &outer, &holes) {
                inside += 1;
            }
            x += step;
        }
        y += step;
    }
    let units_per_ft = 72.0 / 20.0;
    let raster = inside as f64 * step * step / (units_per_ft * units_per_ft);
    assert!(
        (raster - sq_ft).abs() / sq_ft < 0.002,
        "raster {raster} sq ft, computed {sq_ft} sq ft"
    );
}

/// 50,000 rows with five formula columns (chained) recalculate within the budget.
#[test]
fn fifty_thousand_rows_with_five_formulas_recalculate_within_budget() {
    const N: usize = 50_000;
    let mut s = Schema::new();
    s.add_choice(
        "Material",
        &[("A", Some(2.0)), ("B", Some(3.5)), ("C", None)],
    )
    .unwrap();
    s.add("Rate", ColumnType::Number, &[]).unwrap();
    s.add_formula("F5", "round([F4] * 1.2, 2)").unwrap();
    s.add_formula("F1", "[Length] * [Material]").unwrap();
    s.add_formula("F2", "if([Rate] > 5, [F1] * [Rate], [F1] / max([Rate], 1))")
        .unwrap();
    s.add_formula("F3", "[F2] + ceil([Length] / 8) * 3")
        .unwrap();
    s.add_formula("F4", "min([F3], 1e9) + abs(-[Rate])")
        .unwrap();
    let materials = ["A", "B", "C"];
    let spec = eng(1.0);
    let og = og();
    let mut annots = Vec::with_capacity(N);
    for i in 0..N {
        let len = 18.0 + (i % 97) as f64 * 3.6;
        let mut a = Annotation::measure_length(i % 40, p(0.0, 0.0), p(len, 0.0), spec.clone(), &og);
        set_custom_value(
            &mut a,
            s.get("material").unwrap(),
            Value::text(materials[i % 3]),
        )
        .unwrap();
        set_custom_value(
            &mut a,
            s.get("rate").unwrap(),
            Value::Number((i % 11) as f64),
        )
        .unwrap();
        annots.push(a);
    }
    let t = Instant::now();
    let mut table = Table::build_takeoff(
        annots.iter(),
        &ListContext {
            schema: Some(&s),
            ..ListContext::default()
        },
        &TakeoffContext::default(),
    );
    let build = t.elapsed();
    let t = Instant::now();
    table.recalculate();
    let recalc = t.elapsed();
    eprintln!(
        "TIMING 50k rows: build with quantities and 5 formulas {build:?}; recalculate alone {recalc:?}"
    );
    assert!(recalc.as_secs_f64() < 5.0, "recalculate took {recalc:?}");
    assert!(build.as_secs_f64() < 20.0, "build took {build:?}");

    // Spot checks against the same arithmetic done here.
    for i in [0usize, 1, 2, 17, 4_321, N - 1] {
        let rate = (i % 11) as f64;
        let row = table.row_of(&annots[i].id).unwrap();
        let len_ft = num(table.value(row, &Builtin::Length.into()));
        assert!((len_ft - (18.0 + (i % 97) as f64 * 3.6) * 20.0 / 72.0).abs() < 1e-9);
        let f = |c: &str| table.value(row, &ColumnId::custom(c));
        match materials[i % 3] {
            "C" => {
                assert_eq!(f("f5"), Value::text("#VALUE!"), "row {i}");
                continue;
            }
            m => {
                let k = if m == "A" { 2.0 } else { 3.5 };
                let f1 = len_ft * k;
                let f2 = if rate > 5.0 {
                    f1 * rate
                } else {
                    f1 / rate.max(1.0)
                };
                let f3 = f2 + (len_ft / 8.0).ceil() * 3.0;
                let f4 = f3.min(1e9) + rate;
                let f5 = (f4 * 1.2 * 100.0).round() / 100.0;
                assert!((num(f("f4")) - f4).abs() < 1e-9, "row {i}");
                assert!((num(f("f5")) - f5).abs() < 1e-9, "row {i}");
            }
        }
    }
}

/// Measurements saved into the fixture pages (plain, rotated 90/180/270, `/UserUnit 2`, and
/// the three box-offset pages) and read back give the analytic quantities, the right spaces
/// and a legend that reads upright for the page.
#[test]
fn fixture_pages_round_trip_with_their_quantities() {
    type Target = (&'static str, fn() -> pl_testkit::Fixture, usize);
    let targets: [Target; 8] = [
        ("plain", pl_testkit::plain, 0),
        ("rotate90", pl_testkit::rotated_pages, 1),
        ("rotate180", pl_testkit::rotated_pages, 2),
        ("rotate270", pl_testkit::rotated_pages, 3),
        ("userunit", pl_testkit::user_unit, 0),
        ("cropbox", pl_testkit::crop_box, 0),
        ("mediabox-offset", pl_testkit::crop_box, 1),
        ("cropbox-rotate90", pl_testkit::crop_box, 2),
    ];
    for (name, fixture, page) in targets {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{name}.pdf"));
        std::fs::write(&path, fixture().bytes).unwrap();
        let mut file = PdfFile::open(&path).unwrap();
        let geometry = file.document().unwrap().page(page).unwrap().geometry;
        let uu = geometry.user_unit();
        let crop = geometry.crop_box();
        // Everything fits in 500 x 420 units of the smallest page; scale it into the visible
        // box (a user unit of 2 halves the box).
        let o = p(crop.x0 + 4.0, crop.y0 + 4.0);
        let fit = ((crop.width() - 8.0) / 500.0)
            .min((crop.height() - 8.0) / 420.0)
            .min(1.0);
        assert!(fit > 0.4, "{name}: box too small");
        let mut set = AnnotSet::new();
        // Lay the kinds out at the fitted size, with the scale stretched to match, so the
        // real quantities are the same as unfitted.
        let spec = MeasureSpec::new(
            &Scale::from_factor(Scale::engineering(20.0).unwrap().factor() / fit, Unit::Foot)
                .unwrap(),
            uu,
            &MeasureFormat::new(
                LengthFormat::decimal(Unit::Foot, 2),
                AreaFormat::new(AreaUnit::SqFoot, 2),
            ),
        );
        let at = |x: f64, y: f64| p(o.x + x * fit, o.y + y * fit);
        let og = og();
        let rotation = geometry.rotate().degrees();
        let mut add = |mut a: Annotation| {
            a.set_rotation(rotation);
            set.add(a).unwrap()
        };
        let length = add(Annotation::measure_length(
            page,
            at(0.0, 0.0),
            at(72.0, 0.0),
            spec.clone(),
            &og,
        ));
        let area = add(Annotation::measure_area(
            page,
            vec![
                at(0.0, 200.0),
                at(144.0, 200.0),
                at(144.0, 344.0),
                at(0.0, 344.0),
            ],
            vec![vec![
                at(36.0, 236.0),
                at(108.0, 236.0),
                at(108.0, 308.0),
                at(36.0, 308.0),
            ]],
            spec.clone(),
            &og,
        ));
        let volume = add(Annotation::measure_volume(
            page,
            vec![
                at(200.0, 200.0),
                at(344.0, 200.0),
                at(344.0, 344.0),
                at(200.0, 344.0),
            ],
            vec![],
            2.0 * FT,
            spec.clone(),
            &og,
        ));
        let count = add(Annotation::count(
            page,
            &[at(400.0, 10.0), at(420.0, 10.0)],
            CountSymbol::Dot,
            &og,
        ));
        let plan = set.plan_save(file.document().unwrap()).unwrap();
        file.save_incremental(&plan.edits, &SaveOptions::default())
            .unwrap();
        drop(file);

        let mut reopened = PdfFile::open(&path).unwrap();
        let loaded = AnnotSet::load(reopened.document().unwrap());
        let room = Space::new(
            "Room",
            page,
            vec![
                at(-2.0, 190.0),
                at(360.0, 190.0),
                at(360.0, 360.0),
                at(-2.0, 360.0),
            ],
            vec![],
        );
        let table = Table::build_takeoff(
            loaded.iter(),
            &ListContext::default(),
            &TakeoffContext {
                spaces: std::slice::from_ref(&room),
                ..TakeoffContext::default()
            },
        );
        assert_eq!(table.rows.len(), 4, "{name}");
        let f = |id: &AnnotId, b: Builtin| num(cell(&table, id, b));
        // The user unit scales the paper, and so the real quantities.
        let tol = |a: f64, b: f64| (a - b).abs() <= 1e-6 * b.abs().max(1.0);
        assert!(
            tol(f(&length, Builtin::Length), 20.0 * uu),
            "{name}: {}",
            f(&length, Builtin::Length)
        );
        assert!(tol(f(&area, Builtin::Area), 1200.0 * uu * uu), "{name}");
        // The outline (in metres) is scaled too; it is not in the Perimeter column.
        let outline = table.rows[table.row_of(&area).unwrap()]
            .measure
            .as_ref()
            .and_then(|m| m.outline())
            .unwrap();
        assert!(tol(outline, 160.0 * uu * FT), "{name}");
        assert_eq!(
            cell(&table, &area, Builtin::Perimeter),
            Value::Empty,
            "{name}"
        );
        assert!(
            tol(f(&volume, Builtin::Volume), 1600.0 * uu * uu * 2.0 / 27.0),
            "{name}"
        );
        assert_eq!(f(&count, Builtin::Count), 2.0);
        let space = |id: &AnnotId| cell(&table, id, Builtin::Space).display();
        assert_eq!(
            (space(&area), space(&volume), space(&length)),
            ("Room".into(), "Room".into(), String::new()),
            "{name}"
        );

        let legend = LegendDef::new("Takeoff", Builtin::Subject, Some(Builtin::Area.into()))
            .evaluate(&table);
        let area_row = legend.rows.iter().find(|r| r.label == "Area").unwrap();
        assert_eq!(area_row.text, format!("{:.2}", 1200.0 * uu * uu), "{name}");
        let style = LegendStyle {
            rotation: rotation as u16,
            ..LegendStyle::default()
        };
        let drawn = legend_draw_list(&legend, crop.center(), &style).unwrap();
        // The legend hangs from its anchor towards the displayed bottom-right.
        let c = crop.center();
        let (r, d) = match rotation {
            90 => ((0.0, 1.0), (1.0, 0.0)),
            180 => ((-1.0, 0.0), (0.0, 1.0)),
            270 => ((0.0, -1.0), (-1.0, 0.0)),
            _ => ((1.0, 0.0), (0.0, -1.0)),
        };
        let probe = p(c.x + 5.0 * (r.0 + d.0), c.y + 5.0 * (r.1 + d.1));
        assert!(
            drawn.rect.contains(probe),
            "{name}: rotation {rotation}, {:?}",
            drawn.rect
        );
    }
}

/// The real sample sheet, when it is there: a takeoff on a copy of it. Skipped when absent;
/// nothing from the file is printed.
#[test]
fn a_takeoff_on_the_real_sample_sheet() {
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    if !sample.exists() {
        eprintln!("SKIPPED: the real sample is not there");
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("pl-takeoff-sample");
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("sheet.pdf");
    std::fs::copy(&sample, &copy).unwrap();
    let mut file = PdfFile::open(&copy).unwrap();
    let doc = file.document().unwrap();
    let geometry = doc.page(0).unwrap().geometry;
    let uu = geometry.user_unit();
    let media = geometry.media_box();
    let mut set = AnnotSet::load(doc);
    let before = set.iter().count();
    let o = p(media.x0 + 200.0, media.y0 + 200.0);
    let og = og();
    // Ten inches of paper at 1" = 20': 200 ft; a 5 x 5 inch square: 10,000 sq ft.
    let length = set
        .add(Annotation::measure_length(
            0,
            o,
            p(o.x + 720.0 / uu, o.y),
            eng(uu),
            &og,
        ))
        .unwrap();
    let side = 360.0 / uu;
    let area = set
        .add(Annotation::measure_area(
            0,
            vec![
                o,
                p(o.x + side, o.y),
                p(o.x + side, o.y + side),
                p(o.x, o.y + side),
            ],
            vec![],
            eng(uu),
            &og,
        ))
        .unwrap();
    let table = table_of(&set, None, None);
    assert_eq!(table.rows.iter().filter(|r| r.measure.is_some()).count(), 2);
    assert!(close(num(cell(&table, &length, Builtin::Length)), 200.0));
    assert!(close(num(cell(&table, &area, Builtin::Area)), 10_000.0));
    let summary = TakeoffSummary::by_subject(&table);
    assert_eq!(summary.count, table.roots.len());
    assert!(table.rows.len() >= before);
    drop(file);
    let _ = std::fs::remove_file(&copy);
}
