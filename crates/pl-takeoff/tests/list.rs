//! The markups list: rows, threads, user columns, sort, group, filter, layout.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Instant;

use pl_annot::{
    AnnotId, AnnotKind, AnnotSet, Annotation, Flags, LinkTarget, Origin, PdfDate, ReviewState, Rgb,
    PRIVATE_KEY,
};
use pl_geom::{Point, Rect};
use pl_pdf::{Object, PdfFile, SaveOptions};
use pl_takeoff::{
    checkmark_status, custom_value, natural_cmp, set_custom_value, Body, Builtin, ColumnFilter,
    ColumnId, ColumnType, Condition, Filter, Group, Layout, Line, ListContext, RowOptions, Schema,
    SortKey, Table, Value, View, COLUMNS_KEY, MARKED_MODEL, NO_VALUE_LABEL,
};

fn origin_at(author: &str, seconds: i64) -> Origin {
    Origin::new(author, PdfDate::from_unix(1_780_000_000 + seconds))
}

/// A small deterministic generator, so the data is the same on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn schema() -> Schema {
    let mut s = Schema::new();
    s.add(
        "Trade",
        ColumnType::Choice,
        &["Civil", "Electrical", "Mechanical"],
    )
    .unwrap();
    s.add("Qty", ColumnType::Number, &[]).unwrap();
    s.add("Done", ColumnType::Checkmark, &[]).unwrap();
    s.add("Due", ColumnType::Date, &[]).unwrap();
    s.add("Ref", ColumnType::Text, &[]).unwrap();
    s
}

/// `n` markups over 40 pages with many ties and many empty cells.
fn generate(n: usize, schema: &Schema) -> Vec<Annotation> {
    let authors = ["Ada", "ada", "Bo", "Chen", "", "Dmitri"];
    let trades = ["Civil", "Electrical", "Mechanical"];
    let mut rng = Lcg(7);
    let col = |id: &str| schema.get(id).unwrap();
    (0..n)
        .map(|i| {
            let page = rng.below(40) as usize;
            let x = rng.below(500) as f64;
            let y = rng.below(700) as f64;
            let origin = origin_at(authors[rng.below(6) as usize], rng.below(5000) as i64);
            let mut a = match rng.below(4) {
                0 => Annotation::square(page, Rect::new(x, y, x + 40.0, y + 20.0), &origin),
                1 => Annotation::circle(page, Rect::new(x, y, x + 10.0, y + 10.0), &origin),
                2 => Annotation::line(page, Point::new(x, y), Point::new(x + 30.0, y), &origin),
                _ => Annotation::note(page, Rect::new(x, y, x + 18.0, y + 18.0), "", &origin),
            };
            if a.meta.author.as_deref() == Some("") {
                a.meta.author = None;
            }
            // Subjects with numbers in them: "Door 2" must come before "Door 10".
            a.meta.subject = match rng.below(5) {
                0 => None,
                k => Some(format!(
                    "{} {}",
                    ["Door", "door", "Window", "Pipe"][k as usize - 1],
                    rng.below(30)
                )),
            };
            a.meta.contents = (rng.below(3) > 0)
                .then(|| format!("check item {} on level {}", i % 97, rng.below(6)));
            if rng.below(3) > 0 {
                set_custom_value(
                    &mut a,
                    col("trade"),
                    Value::text(trades[rng.below(3) as usize]),
                )
                .unwrap();
            }
            if rng.below(4) > 0 {
                // Quarter steps: sums are exact whatever order they are added in.
                set_custom_value(
                    &mut a,
                    col("qty"),
                    Value::Number(rng.below(200) as f64 * 0.25),
                )
                .unwrap();
            }
            if rng.below(2) > 0 {
                set_custom_value(&mut a, col("done"), Value::Bool(rng.below(2) == 0)).unwrap();
            }
            if rng.below(3) == 0 {
                set_custom_value(
                    &mut a,
                    col("due"),
                    Value::day(2026, 1 + rng.below(12) as u8, 1 + rng.below(28) as u8),
                )
                .unwrap();
            }
            a
        })
        .collect()
}

fn build<'a>(annotations: &'a [Annotation], schema: &'a Schema) -> Table {
    Table::build(
        annotations,
        &ListContext {
            schema: Some(schema),
            ..ListContext::default()
        },
    )
}

/// A search for words that must all appear in the comment (in any order, anywhere in it).
fn comment_search(words: &str) -> Filter {
    let mut f = Filter::search(words);
    f.search_in = vec![Builtin::Comment.into()];
    f
}

fn custom(id: &str) -> ColumnId {
    ColumnId::custom(id)
}

/// An independent statement of the list's order for the columns the tests sort by.
fn reference_cmp(a: &Value, b: &Value, descending: bool) -> Ordering {
    let key = |v: &Value| -> Option<(u8, f64, String)> {
        match v {
            Value::Empty => None,
            Value::Text(s) if s.trim().is_empty() => None,
            Value::Number(n) => Some((0, *n, String::new())),
            Value::Date(d) => Some((1, d.to_unix() as f64, String::new())),
            Value::Bool(b) => Some((2, f64::from(u8::from(*b)), String::new())),
            Value::Colour(c) => Some((
                3,
                f64::from(c[0]) * 65536.0 + f64::from(c[1]) * 256.0 + f64::from(c[2]),
                String::new(),
            )),
            Value::Text(s) => Some((4, 0.0, s.clone())),
        }
    };
    match (key(a), key(b)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let o =
                x.0.cmp(&y.0)
                    .then(x.1.partial_cmp(&y.1).unwrap())
                    .then_with(|| natural_cmp(&x.2, &y.2));
            if descending {
                o.reverse()
            } else {
                o
            }
        }
    }
}

fn assert_sorted(table: &Table, rows: &[usize], keys: &[SortKey]) {
    for pair in rows.windows(2) {
        let mut order = Ordering::Equal;
        for k in keys {
            order = reference_cmp(
                &table.value(pair[0], &k.column),
                &table.value(pair[1], &k.column),
                k.descending,
            );
            if order != Ordering::Equal {
                break;
            }
        }
        assert_ne!(order, Ordering::Greater, "rows {pair:?} are out of order");
        if order == Ordering::Equal {
            assert!(
                table.rows[pair[0]].source < table.rows[pair[1]].source,
                "ties must stay in document order: {pair:?}"
            );
        }
    }
}

#[test]
fn natural_order_is_what_the_subject_column_sorts_by() {
    let s = Schema::new();
    let o = origin_at("Ada", 0);
    let subjects = [
        "Door 10", "door 2", "Door 1", "", "Door 2", "Pipe", "door 010",
    ];
    let annots: Vec<Annotation> = subjects
        .iter()
        .map(|subject| {
            let mut a = Annotation::square(0, Rect::new(0.0, 0.0, 5.0, 5.0), &o);
            a.meta.subject = (!subject.is_empty()).then(|| subject.to_string());
            a
        })
        .collect();
    let table = build(&annots, &s);
    let sorted = |descending| {
        let key = SortKey {
            column: Builtin::Subject.into(),
            descending,
        };
        View::build(&table, &Filter::default(), &[key], &[])
            .rows()
            .iter()
            .map(|r| table.rows[*r].subject.clone())
            .collect::<Vec<_>>()
    };
    // "door 2" and "Door 2" tie and keep their document order; so do "Door 10", "door 010".
    assert_eq!(
        sorted(false),
        ["Door 1", "door 2", "Door 2", "Door 10", "door 010", "Pipe", ""]
    );
    // Descending reverses the values but the empty cell stays last and ties stay in order.
    assert_eq!(
        sorted(true),
        ["Pipe", "Door 10", "door 010", "door 2", "Door 2", "Door 1", ""]
    );
}

#[test]
fn multi_key_sort_is_correct_and_stable_on_generated_data() {
    let s = schema();
    let annots = generate(4000, &s);
    let table = build(&annots, &s);
    assert_eq!(table.roots.len(), 4000);
    let cases: Vec<Vec<SortKey>> = vec![
        vec![SortKey::asc(Builtin::Author)],
        vec![SortKey::desc(Builtin::Subject)],
        vec![SortKey::asc(custom("trade")), SortKey::desc(custom("qty"))],
        vec![
            SortKey::desc(Builtin::Page),
            SortKey::asc(Builtin::Author),
            SortKey::asc(Builtin::Subject),
        ],
        vec![SortKey::asc(custom("done")), SortKey::desc(custom("due"))],
        vec![SortKey::desc(Builtin::Created), SortKey::asc(Builtin::Kind)],
        // A column that does not exist sorts nothing: document order.
        vec![SortKey::asc(custom("nope"))],
    ];
    for keys in cases {
        let view = View::build(&table, &Filter::default(), &keys, &[]);
        let rows = view.rows();
        assert_eq!(rows.len(), 4000);
        let mut seen = rows.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 4000, "a permutation of the rows");
        assert_sorted(&table, &rows, &keys);
    }
    // The data really has ties and empty cells in the sorted columns.
    let authors: Vec<Value> = (0..4000)
        .map(|r| table.value(r, &Builtin::Author.into()))
        .collect();
    assert!(authors.iter().filter(|v| v.is_empty()).count() > 300);
    assert!(authors.iter().filter(|v| **v == Value::text("ada")).count() > 300);
}

fn groups(body: &Body) -> &[Group] {
    match body {
        Body::Groups(g) => g,
        Body::Rows(_) => panic!("expected groups"),
    }
}

fn rows_of(body: &Body) -> Vec<usize> {
    match body {
        Body::Rows(r) => r.clone(),
        Body::Groups(g) => g.iter().flat_map(|g| rows_of(&g.body)).collect(),
    }
}

#[test]
fn nested_groups_have_counts_and_subtotals() {
    let s = schema();
    let annots = generate(3000, &s);
    let table = build(&annots, &s);
    let sort = [SortKey::desc(custom("qty"))];
    let group = [
        SortKey::asc(custom("trade")),
        SortKey::desc(Builtin::Author),
    ];
    let view = View::build(&table, &Filter::default(), &sort, &group);

    // The reference: a plain map from (trade, author without case) to count and quantity.
    let mut expected: BTreeMap<(String, String), (usize, f64)> = BTreeMap::new();
    for a in &annots {
        let trade = custom_value(a, s.get("trade").unwrap()).display();
        let author = a.meta.author.clone().unwrap_or_default().to_lowercase();
        let qty = custom_value(a, s.get("qty").unwrap())
            .as_number()
            .unwrap_or(0.0);
        let e = expected.entry((trade, author)).or_default();
        e.0 += 1;
        e.1 += qty;
    }

    let top = groups(&view.body);
    let labels: Vec<&str> = top.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Civil", "Electrical", "Mechanical", NO_VALUE_LABEL]
    );
    assert_eq!(top.iter().map(|g| g.count).sum::<usize>(), 3000);
    assert_eq!(view.count, 3000);
    let qty_of = |subtotals: &[pl_takeoff::Subtotal]| {
        subtotals
            .iter()
            .find(|t| t.column == custom("qty"))
            .unwrap()
            .sum
    };
    let mut leaves = 0;
    for g in top {
        assert_eq!(g.column, custom("trade"));
        let inner = groups(&g.body);
        // Descending by author, without regard to case, the unnamed last.
        let names: Vec<&str> = inner.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(names.last(), Some(&NO_VALUE_LABEL));
        let named = &names[..names.len() - 1];
        assert_eq!(named.len(), 4, "Ada and ada are one group: {names:?}");
        assert!(named
            .windows(2)
            .all(|w| natural_cmp(w[0], w[1]) == Ordering::Greater));
        assert_eq!(inner.iter().map(|g| g.count).sum::<usize>(), g.count);
        assert_eq!(
            inner.iter().map(|g| qty_of(&g.subtotals)).sum::<f64>(),
            qty_of(&g.subtotals)
        );
        for leaf in inner {
            leaves += 1;
            let trade = if g.key.is_empty() {
                String::new()
            } else {
                g.label.clone()
            };
            let author = leaf.key.display().to_lowercase();
            let (count, qty) = expected[&(trade, author)];
            assert_eq!(leaf.count, count);
            assert_eq!(qty_of(&leaf.subtotals), qty);
            let rows = rows_of(&leaf.body);
            assert_eq!(rows.len(), count);
            // Inside a group the rows keep the sort.
            assert_sorted(&table, &rows, &sort);
            for r in rows {
                assert!(table.value(r, &custom("trade")).same(&g.key));
                assert!(table.value(r, &Builtin::Author.into()).same(&leaf.key));
            }
        }
    }
    assert_eq!(leaves, expected.len());
    assert_eq!(
        qty_of(&view.subtotals),
        expected.values().map(|e| e.1).sum::<f64>()
    );
    // Subtotals exist for the quantity placeholders too, with nothing in them yet.
    let length = view
        .subtotals
        .iter()
        .find(|t| t.column == Builtin::Length.into())
        .unwrap();
    assert_eq!((length.sum, length.values), (0.0, 0));

    // Lines: a heading per group, then its rows; a collapsed group is one line.
    let lines = view.lines(&table, true, &[]);
    assert_eq!(lines.len(), 4 + leaves + 3000);
    let first_leaf = groups(&top[0].body)[0].key.clone();
    let collapsed = vec![vec![Value::text("civil"), first_leaf]];
    let fewer = view.lines(&table, true, &collapsed);
    assert_eq!(lines.len() - fewer.len(), groups(&top[0].body)[0].count);
    assert!(matches!(
        &fewer[1],
        Line::Group {
            depth: 1,
            collapsed: true,
            ..
        }
    ));
    assert!(matches!(
        &fewer[2],
        Line::Group {
            depth: 1,
            collapsed: false,
            ..
        }
    ));
}

#[test]
fn filters_agree_with_a_plain_reference() {
    let s = schema();
    let annots = generate(3000, &s);
    let table = build(&annots, &s);
    let qty = |a: &Annotation| custom_value(a, s.get("qty").unwrap()).as_number();
    let trade = |a: &Annotation| custom_value(a, s.get("trade").unwrap()).display();
    let author = |a: &Annotation| a.meta.author.clone().unwrap_or_default();
    let comment = |a: &Annotation| a.meta.contents.clone().unwrap_or_default();

    type Reference<'a> = Box<dyn Fn(&Annotation) -> bool + 'a>;
    let cases: Vec<(Filter, Reference<'_>)> = vec![
        (
            Filter::default().with(ColumnFilter::new(
                Builtin::Author,
                Condition::Equals(Value::text("ADA")),
            )),
            Box::new(|a| author(a).eq_ignore_ascii_case("ada")),
        ),
        (
            Filter::default().with(ColumnFilter::not(
                Builtin::Author,
                Condition::Equals(Value::text("Bo")),
            )),
            Box::new(|a| author(a) != "Bo"),
        ),
        (
            Filter::default().with(ColumnFilter::new(
                Builtin::Comment,
                Condition::Contains("ITEM 4".into()),
            )),
            Box::new(|a| comment(a).contains("item 4")),
        ),
        (
            Filter::default().with(ColumnFilter::new(
                custom("qty"),
                Condition::Range {
                    min: Some(Value::Number(10.0)),
                    max: Some(Value::Number(20.0)),
                },
            )),
            Box::new(|a| qty(a).is_some_and(|q| (10.0..=20.0).contains(&q))),
        ),
        (
            Filter::default().with(ColumnFilter::new(
                custom("qty"),
                Condition::Range {
                    min: None,
                    max: Some(Value::Number(5.0)),
                },
            )),
            Box::new(|a| qty(a).is_some_and(|q| q <= 5.0)),
        ),
        (
            Filter::default().with(ColumnFilter::new(custom("trade"), Condition::IsEmpty)),
            Box::new(|a| trade(a).is_empty()),
        ),
        (
            Filter::default().with(ColumnFilter::not(custom("qty"), Condition::IsEmpty)),
            Box::new(|a| qty(a).is_some()),
        ),
        (
            Filter::default().with(ColumnFilter::new(
                custom("trade"),
                Condition::OneOf(vec![Value::text("civil"), Value::Empty]),
            )),
            Box::new(|a| trade(a).is_empty() || trade(a) == "Civil"),
        ),
        (
            Filter::default().with(ColumnFilter::new(
                custom("due"),
                Condition::Range {
                    min: Some(Value::day(2026, 6, 1)),
                    max: Some(Value::day(2026, 6, 30)),
                },
            )),
            Box::new(
                |a| matches!(custom_value(a, s.get("due").unwrap()), Value::Date(d) if d.month == 6),
            ),
        ),
        (
            Filter::default().with(ColumnFilter::new(
                custom("done"),
                Condition::Equals(Value::Bool(true)),
            )),
            Box::new(|a| custom_value(a, s.get("done").unwrap()) == Value::Bool(true)),
        ),
        // The combined set: every condition must hold.
        (
            comment_search("level 3")
                .with(ColumnFilter::new(
                    custom("trade"),
                    Condition::Equals(Value::text("Electrical")),
                ))
                .with(ColumnFilter::new(
                    Builtin::Page,
                    Condition::Range {
                        min: Some(Value::Number(5.0)),
                        max: None,
                    },
                ))
                .with(ColumnFilter::not(
                    Builtin::Kind,
                    Condition::Equals(Value::text("Note")),
                )),
            Box::new(|a| {
                comment(a).contains("level")
                    && comment(a).contains('3')
                    && trade(a) == "Electrical"
                    && a.page + 1 >= 5
                    && a.kind != AnnotKind::Text
            }),
        ),
        // A range of the wrong type matches nothing.
        (
            Filter::default().with(ColumnFilter::new(
                custom("qty"),
                Condition::Range {
                    min: Some(Value::text("a")),
                    max: None,
                },
            )),
            Box::new(|_| false),
        ),
        // A column that does not exist is empty everywhere.
        (
            Filter::default().with(ColumnFilter::new(custom("nope"), Condition::IsEmpty)),
            Box::new(|_| true),
        ),
    ];
    for (i, (filter, reference)) in cases.iter().enumerate() {
        let view = View::build(&table, filter, &[], &[]);
        let got: Vec<usize> = view.rows().iter().map(|r| table.rows[*r].source).collect();
        let want: Vec<usize> = (0..annots.len())
            .filter(|i| reference(&annots[*i]))
            .collect();
        assert_eq!(got, want, "filter case {i}");
        assert_eq!((view.count, view.total), (want.len(), 3000));
        if i < 11 {
            assert!(
                !want.is_empty() && want.len() < 3000,
                "case {i} must select something"
            );
        }
    }
}

#[test]
fn text_search_reads_every_column_and_every_word() {
    let s = schema();
    let o = origin_at("Renée", 0);
    let mut a = Annotation::square(0, Rect::new(0.0, 0.0, 5.0, 5.0), &o);
    a.meta.subject = Some("Fire Door".into());
    a.meta.contents = Some("Replace CLOSER".into());
    set_custom_value(&mut a, s.get("ref").unwrap(), Value::text("FD-30")).unwrap();
    let mut b = Annotation::circle(1, Rect::new(0.0, 0.0, 5.0, 5.0), &origin_at("Bo", 5));
    b.meta.contents = Some("door closer".into());
    let annots = [a, b];
    let table = build(&annots, &s);
    let hits = |filter: &Filter| {
        View::build(&table, filter, &[], &[])
            .rows()
            .iter()
            .map(|r| table.rows[*r].source)
            .collect::<Vec<_>>()
    };
    assert_eq!(hits(&Filter::search("door")), [0, 1]);
    assert_eq!(hits(&Filter::search("  DOOR   closer ")), [0, 1]);
    // Words may be in different columns, but all must be found.
    assert_eq!(hits(&Filter::search("fd-30 closer renée")), [0]);
    assert_eq!(hits(&Filter::search("RENÉE")), [0]);
    assert_eq!(hits(&Filter::search("ellipse")), [1]);
    assert_eq!(hits(&Filter::search("door window")), Vec::<usize>::new());
    // A word does not match across two cells.
    assert_eq!(hits(&Filter::search("doorreplace")), Vec::<usize>::new());
    // Restricted to one column.
    let mut only_subject = Filter::search("closer");
    only_subject.search_in = vec![Builtin::Subject.into()];
    assert_eq!(hits(&only_subject), Vec::<usize>::new());
    only_subject.search = "fire".into();
    assert_eq!(hits(&only_subject), [0]);
    assert_eq!(hits(&Filter::search("   ")), [0, 1]);
}

#[test]
fn replies_are_child_rows_and_statuses_are_folded_in() {
    let s = Schema::new();
    let ada = origin_at("Ada", 0);
    let bo = origin_at("Bo", 100);
    let mut set = AnnotSet::new();
    let mut markup = Annotation::square(2, Rect::new(10.0, 20.0, 110.0, 80.0), &ada);
    markup.meta.contents = Some("Wall type?".into());
    markup.meta.flags = markup.meta.flags.with(Flags::LOCKED, true);
    let m = set.add(markup).unwrap();
    let other = set
        .add(Annotation::line(
            0,
            Point::new(0.0, 0.0),
            Point::new(9.0, 0.0),
            &ada,
        ))
        .unwrap();
    let page_box = Rect::new(0.0, 0.0, 612.0, 792.0);
    let popup = set.ensure_popup(&m, page_box, &ada).unwrap();
    let r1 = set.add_reply(&m, "Type W3", &bo).unwrap();
    let r2 = set
        .add_reply(&r1, "Thanks", &origin_at("Ada", 200))
        .unwrap();
    let r3 = set
        .add_reply(&m, "Also check the head detail", &origin_at("Chen", 300))
        .unwrap();
    set.set_review_state(&m, ReviewState::Rejected, &origin_at("Bo", 400))
        .unwrap();
    set.set_review_state(&m, ReviewState::Accepted, &origin_at("Bo", 500))
        .unwrap();
    // A checkmark, set and then (earlier in time, later in the file) not set: the date wins.
    let parent = set.get(&m).unwrap().clone();
    set.add(checkmark_status(&parent, true, &origin_at("Ada", 700)))
        .unwrap();
    set.add(checkmark_status(&parent, false, &origin_at("Ada", 600)))
        .unwrap();
    // A reply to something that is not there, a link, and two replies that answer each other.
    let mut orphan = Annotation::reply(&parent, "lost", &bo);
    orphan.meta.reply_to = Some(AnnotId::from("no-such-annotation"));
    let orphan = set.add(orphan).unwrap();
    set.add(Annotation::link(
        0,
        Rect::new(0.0, 0.0, 9.0, 9.0),
        LinkTarget::Uri("https://example.com/".into()),
        &ada,
    ))
    .unwrap();
    let mut x = Annotation::reply(&parent, "x", &bo);
    let mut y = Annotation::reply(&parent, "y", &bo);
    x.meta.reply_to = Some(y.id.clone());
    y.meta.reply_to = Some(x.id.clone());
    let (x, y) = (set.add(x).unwrap(), set.add(y).unwrap());

    let labels: Vec<String> = ["i", "ii", "A-101"].iter().map(|l| l.to_string()).collect();
    let ctx = ListContext {
        page_labels: &labels,
        schema: Some(&s),
        ..ListContext::default()
    };
    let table = Table::build(set.iter(), &ctx);
    let row = |id: &AnnotId| table.row_of(id).unwrap();
    let ids = |rows: &[usize]| {
        rows.iter()
            .map(|r| table.rows[*r].id.clone())
            .collect::<Vec<_>>()
    };

    assert!(table.row_of(&popup).is_none(), "a popup is not a row");
    assert_eq!(
        table.rows.len(),
        8,
        "markup, line, three replies, orphan, x and y"
    );
    let top = &table.rows[row(&m)];
    assert_eq!(ids(&top.children), [r1.clone(), r3.clone()]);
    assert_eq!(
        ids(&table.thread(row(&m))),
        [r1.clone(), r2.clone(), r3.clone()]
    );
    assert_eq!((top.replies, top.depth, top.parent), (3, 0, None));
    assert_eq!(table.rows[row(&r1)].replies, 1);
    assert_eq!(table.rows[row(&r2)].depth, 2);
    assert_eq!(table.rows[row(&r2)].parent, Some(row(&r1)));
    assert_eq!(top.status, ReviewState::Accepted);
    assert!(top.checked, "the checkmark with the later date counts");
    assert!(top.locked);
    assert_eq!((top.page, top.page_label.as_str()), (2, "A-101"));
    assert_eq!(
        (top.x, top.y, top.width, top.height),
        (10.0, 20.0, 100.0, 60.0)
    );
    assert_eq!(top.kind_name, "Rectangle");
    assert_eq!(top.colour, Some(Rgb::new(0.8, 0.0, 0.0).to_u8()));
    assert_eq!(table.rows[row(&r1)].kind_name, "Reply");
    assert_eq!(table.rows[row(&other)].status, ReviewState::None);
    assert!(!table.rows[row(&other)].checked);
    assert_eq!(table.rows[row(&other)].page_label, "i");

    // Cells.
    let cell = |id: &AnnotId, b: Builtin| table.value(row(id), &b.into());
    assert_eq!(cell(&m, Builtin::Status), Value::text("Accepted"));
    assert_eq!(cell(&other, Builtin::Status), Value::Empty);
    assert_eq!(cell(&m, Builtin::Replies), Value::Number(3.0));
    assert_eq!(cell(&m, Builtin::Page), Value::Number(3.0));
    assert_eq!(cell(&m, Builtin::PageLabel), Value::text("A-101"));
    assert_eq!(cell(&m, Builtin::Checkmark), Value::Bool(true));
    assert_eq!(cell(&m, Builtin::Created), Value::Date(ada.when));
    assert_eq!(cell(&m, Builtin::Comment), Value::text("Wall type?"));
    assert_eq!(cell(&m, Builtin::Fill), Value::Empty);
    for quantity in [Builtin::Length, Builtin::Area, Builtin::Count] {
        assert_eq!(cell(&m, quantity), Value::Empty);
    }
    assert_eq!((top.length, top.area, top.count), (None, None, None));

    // The roots: the two markups, the orphan, and one of the two that answer each other.
    let roots = ids(&table.roots);
    assert_eq!(roots.len(), 4);
    assert_eq!(roots[..3], [m.clone(), other.clone(), orphan.clone()]);
    let (rx, ry) = (&table.rows[row(&x)], &table.rows[row(&y)]);
    assert!(
        (rx.parent.is_none() && ry.parent == Some(row(&x)))
            || (ry.parent.is_none() && rx.parent == Some(row(&y))),
        "the cycle is cut in one place"
    );

    // Links join only when asked for; hidden annotations can be left out.
    let with_links = Table::build(
        set.iter(),
        &ListContext {
            options: RowOptions {
                include_links: true,
                include_hidden: true,
            },
            ..ctx
        },
    );
    assert_eq!(with_links.rows.len(), 9);
    assert_eq!(
        with_links
            .rows
            .iter()
            .filter(|r| r.kind == AnnotKind::Link)
            .count(),
        1
    );

    // A thread is shown when any row in it passes the filter, and stays under its markup
    // whatever the sort.
    let view = View::build(
        &table,
        &Filter::search("thanks"),
        &[SortKey::desc(Builtin::Author)],
        &[],
    );
    assert_eq!(ids(&view.rows()), std::slice::from_ref(&m));
    let lines = view.lines(&table, true, &[]);
    let shown: Vec<(AnnotId, usize)> = lines
        .iter()
        .map(|l| match l {
            Line::Row {
                row, reply_depth, ..
            } => (table.rows[*row].id.clone(), *reply_depth),
            Line::Group { .. } => panic!("no groups here"),
        })
        .collect();
    assert_eq!(shown, [(m.clone(), 0), (r1, 1), (r2, 2), (r3, 1)]);
    assert_eq!(view.lines(&table, false, &[]).len(), 1);
    // Counts are of markups, not replies.
    let all = View::build(
        &table,
        &Filter::default(),
        &[],
        &[SortKey::asc(Builtin::Author)],
    );
    assert_eq!((all.count, all.total), (4, 4));
}

#[test]
fn layer_names_come_from_the_context() {
    let o = origin_at("Ada", 0);
    let mut a = Annotation::square(0, Rect::new(0.0, 0.0, 5.0, 5.0), &o);
    a.meta.layer = Some((12, 0));
    let mut b = a.duplicate(&o);
    b.meta.layer = Some((99, 0));
    let c = Annotation::square(0, Rect::new(0.0, 0.0, 5.0, 5.0), &o);
    let layers = [((12, 0), "Electrical".to_string())];
    let annots = [a, b, c];
    let table = Table::build(
        &annots,
        &ListContext {
            layers: &layers,
            ..ListContext::default()
        },
    );
    let names: Vec<&str> = table.rows.iter().map(|r| r.layer.as_str()).collect();
    assert_eq!(names, ["Electrical", "Layer 99", ""]);
}

#[test]
fn layout_state_serialises_and_follows_the_schema() {
    let mut s = schema();
    let mut layout = Layout::for_schema(&s);
    assert_eq!(layout.columns.len(), Builtin::ALL.len() + 5);
    let visible = layout.visible();
    assert_eq!(visible[0], Builtin::Subject.into());
    assert!(visible.contains(&custom("qty")) && !visible.contains(&Builtin::X.into()));

    // Order, visibility, width.
    assert!(layout.move_column(&custom("qty"), Some(&Builtin::Subject.into())));
    assert_eq!(layout.visible()[0], custom("qty"));
    assert!(layout.move_column(&Builtin::Subject.into(), None));
    assert_eq!(
        layout.columns.last().unwrap().column,
        Builtin::Subject.into()
    );
    assert!(!layout.move_column(&custom("nope"), None));
    assert!(layout.set_visible(&Builtin::X.into(), true));
    assert!(layout.set_visible(&Builtin::Author.into(), false));
    assert!(layout.set_width(&custom("qty"), 5.0));
    assert!(layout.set_width(&custom("trade"), 1.0e9));
    assert!(layout.set_width(&custom("ref"), 187.5));
    assert!(!layout.set_width(&custom("nope"), 50.0));
    assert_eq!(
        layout.state(&custom("qty")).unwrap().width,
        Layout::MIN_WIDTH
    );
    assert_eq!(
        layout.state(&custom("trade")).unwrap().width,
        Layout::MAX_WIDTH
    );

    // Heading clicks.
    let author: ColumnId = Builtin::Author.into();
    layout.toggle_sort(&author, false);
    assert_eq!(layout.sort, [SortKey::asc(Builtin::Author)]);
    layout.toggle_sort(&author, false);
    assert_eq!(layout.sort, [SortKey::desc(Builtin::Author)]);
    layout.toggle_sort(&custom("qty"), true);
    assert_eq!(
        layout.sort,
        [SortKey::desc(Builtin::Author), SortKey::asc(custom("qty"))]
    );
    layout.toggle_sort(&custom("qty"), true);
    layout.toggle_sort(&author, true);
    assert_eq!(layout.sort, [SortKey::desc(custom("qty"))]);
    layout.toggle_sort(&custom("qty"), false);
    assert!(layout.sort.is_empty());
    layout.sort = vec![SortKey::asc(custom("trade")), SortKey::desc(custom("qty"))];

    layout.group = vec![SortKey::asc(custom("trade")), SortKey::desc(Builtin::Page)];
    layout.collapsed = vec![
        vec![Value::text("Civil")],
        vec![Value::Empty, Value::Number(3.0)],
    ];
    layout.show_replies = false;
    layout.filter = Filter::search("door")
        .with(ColumnFilter::new(
            custom("due"),
            Condition::Range {
                min: Some(Value::day(2026, 1, 1)),
                max: None,
            },
        ))
        .with(ColumnFilter::not(custom("qty"), Condition::IsEmpty))
        .with(ColumnFilter::new(
            Builtin::Colour,
            Condition::Equals(Value::Colour([204, 0, 0])),
        ));
    layout.filter.search_in = vec![Builtin::Comment.into(), custom("ref")];

    let json = serde_json::to_string_pretty(&layout).unwrap();
    let back: Layout = serde_json::from_str(&json).unwrap();
    assert_eq!(back, layout);

    // Removing a column from the schema removes it everywhere; a new one appears at the end.
    s.remove("qty").unwrap();
    s.add("Zone", ColumnType::Text, &[]).unwrap();
    let mut reconciled = back.clone();
    reconciled.reconcile(&s);
    assert!(reconciled.state(&custom("qty")).is_none());
    assert_eq!(reconciled.columns.last().unwrap().column, custom("zone"));
    assert_eq!(reconciled.sort, [SortKey::asc(custom("trade"))]);
    assert_eq!(reconciled.filter.conditions.len(), 2);
    assert_eq!(reconciled.state(&custom("ref")).unwrap().width, 187.5);
    assert_eq!(reconciled.columns.len(), Builtin::ALL.len() + 5);
    let again = {
        let mut l = reconciled.clone();
        l.reconcile(&s);
        l
    };
    assert_eq!(again, reconciled, "reconciling twice changes nothing");

    // A stored layout from before a built-in column existed, with a duplicate and a bad width.
    let old = r#"{"version":1,"columns":[
        {"column":{"Builtin":"Comment"},"visible":true,"width":300.0},
        {"column":{"Builtin":"Comment"},"visible":false,"width":10.0},
        {"column":{"Custom":"gone"},"visible":true,"width":80.0},
        {"column":{"Builtin":"Author"},"visible":true,"width":-4.0}]}"#;
    let mut old: Layout = serde_json::from_str(old).unwrap();
    assert!(old.show_replies && old.sort.is_empty());
    old.reconcile(&s);
    assert_eq!(old.columns.len(), Builtin::ALL.len() + 5);
    assert_eq!(old.columns[0].column, Builtin::Comment.into());
    assert_eq!(old.columns[1].column, Builtin::Author.into());
    assert_eq!(old.columns[1].width, Layout::MIN_WIDTH);
    assert_eq!(
        old.visible().len(),
        2 + 5,
        "added built-in columns are hidden, the user's shown"
    );

    // The view of a layout is the view of its parts.
    let annots = generate(500, &schema());
    let table = build(&annots, &s);
    assert_eq!(
        View::of(&table, &reconciled),
        View::build(
            &table,
            &reconciled.filter,
            &reconciled.sort,
            &reconciled.group
        )
    );
}

/// The custom values survive a real save: they are in the file under the one private key and
/// come back as the same typed values.
#[test]
fn custom_columns_round_trip_through_the_private_key_of_a_saved_pdf() {
    let s = schema();
    let fixture = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("columns.pdf");
    std::fs::write(&path, &fixture.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();

    let o = origin_at("Ada", 0);
    let mut a = Annotation::square(0, Rect::new(100.0, 100.0, 200.0, 160.0), &o);
    let values = [
        ("trade", Value::text("Electrical")),
        ("qty", Value::Number(12.34)),
        ("done", Value::Bool(true)),
        ("due", Value::day(2026, 12, 1)),
        ("ref", Value::text("Ø 25 — «north»")),
    ];
    for (id, v) in &values {
        assert_eq!(
            set_custom_value(&mut a, s.get(id).unwrap(), v.clone()),
            Ok(true)
        );
    }
    let untouched = Annotation::circle(0, Rect::new(300.0, 100.0, 340.0, 140.0), &o);
    let mut set = AnnotSet::new();
    let id = set.add(a).unwrap();
    let plain_id = set.add(untouched).unwrap();
    let plan = set.plan_save(file.document().unwrap()).unwrap();
    file.save_incremental(&plan.edits, &SaveOptions::default())
        .unwrap();

    let mut reopened = PdfFile::open(&path).unwrap();
    let loaded = AnnotSet::load(reopened.document().unwrap());
    let got = loaded.get(&id).unwrap();
    for (cid, v) in &values {
        assert_eq!(&custom_value(got, s.get(cid).unwrap()), v, "{cid}");
    }
    // Object level: one private key, holding one /Columns dictionary with five entries.
    let dict = got.original_dict().unwrap();
    let private = dict.get(PRIVATE_KEY.as_bytes()).unwrap().as_dict().unwrap();
    assert_eq!(private.len(), 1);
    let columns = private
        .get(COLUMNS_KEY.as_bytes())
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(columns.len(), 5);
    assert_eq!(columns.get(b"done").unwrap(), &Object::Boolean(true));
    let app_keys: Vec<String> = dict
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
        .filter(|k| k.starts_with("Pl") || k.contains("Column"))
        .collect();
    assert_eq!(app_keys, [PRIVATE_KEY]);
    assert!(!loaded
        .get(&plain_id)
        .unwrap()
        .original_dict()
        .unwrap()
        .has(PRIVATE_KEY.as_bytes()));

    // The list built from the loaded file shows them.
    let table = Table::build(
        loaded.iter(),
        &ListContext {
            schema: Some(&s),
            ..ListContext::default()
        },
    );
    let row = table.row_of(&id).unwrap();
    for (cid, v) in &values {
        assert_eq!(&table.value(row, &custom(cid)), v);
    }
    assert_eq!(
        table.value(table.row_of(&plain_id).unwrap(), &custom("qty")),
        Value::Empty
    );

    // Clearing a value and saving again rewrites the private key only.
    let mut edited = loaded.clone();
    let target = edited.get_mut(&id).unwrap();
    assert_eq!(
        set_custom_value(target, s.get("qty").unwrap(), Value::Empty),
        Ok(true)
    );
    let plan = edited.plan_save(reopened.document().unwrap()).unwrap();
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, [PRIVATE_KEY]);
}

/// A checkmark is the standard Marked state model, written as a hidden reply.
#[test]
fn a_checkmark_is_a_standard_marked_status_at_object_level() {
    let fixture = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marked.pdf");
    std::fs::write(&path, &fixture.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();
    let o = origin_at("Ada", 0);
    let mut set = AnnotSet::new();
    let m = set
        .add(Annotation::square(
            0,
            Rect::new(100.0, 100.0, 200.0, 160.0),
            &o,
        ))
        .unwrap();
    let parent = set.get(&m).unwrap().clone();
    let status = set
        .add(checkmark_status(&parent, true, &origin_at("Bo", 50)))
        .unwrap();
    let plan = set.plan_save(file.document().unwrap()).unwrap();
    file.save_incremental(&plan.edits, &SaveOptions::default())
        .unwrap();

    let mut reopened = PdfFile::open(&path).unwrap();
    let loaded = AnnotSet::load(reopened.document().unwrap());
    let parent_object = loaded.get(&m).unwrap().object_id().unwrap();
    let dict = loaded.get(&status).unwrap().original_dict().unwrap();
    assert_eq!(
        dict.get(b"Subtype").unwrap(),
        &Object::Name(b"Text".to_vec())
    );
    assert_eq!(dict.get(b"IRT").unwrap(), &Object::Reference(parent_object));
    let text = |key: &[u8]| match dict.get(key).unwrap() {
        Object::String(bytes, _) => pl_annot::decode_text(bytes),
        other => panic!("{other:?}"),
    };
    assert_eq!(text(b"StateModel"), MARKED_MODEL);
    assert_eq!(text(b"State"), "Marked");
    assert_eq!(text(b"T"), "Bo");
    let flags = dict.get(b"F").unwrap().as_i64().unwrap() as u32;
    assert!(Flags(flags).has(Flags::HIDDEN));

    let table = Table::build(loaded.iter(), &ListContext::default());
    assert_eq!(table.rows.len(), 1);
    assert!(table.rows[0].checked);
    // It does not disturb the review status.
    assert_eq!(table.rows[0].status, ReviewState::None);
    assert_eq!(loaded.review_state(&m), ReviewState::None);
}

/// 50,000 rows: build, then filter + three-key sort + two-level grouping, checked against the
/// reference and timed.
#[test]
fn fifty_thousand_rows_are_correct_and_within_budget() {
    const N: usize = 50_000;
    let s = schema();
    let annots = generate(N, &s);

    let t = Instant::now();
    let table = build(&annots, &s);
    let build_time = t.elapsed();
    assert_eq!(table.rows.len(), N);

    let sort = vec![
        SortKey::asc(Builtin::Subject),
        SortKey::desc(custom("qty")),
        SortKey::asc(Builtin::Author),
    ];
    let t = Instant::now();
    let sorted = View::build(&table, &Filter::default(), &sort, &[]);
    let sort_time = t.elapsed();
    let rows = sorted.rows();
    assert_eq!(rows.len(), N);
    assert_sorted(&table, &rows, &sort);

    let group = vec![SortKey::asc(custom("trade")), SortKey::asc(Builtin::Page)];
    let t = Instant::now();
    let grouped = View::build(&table, &Filter::default(), &[], &group);
    let group_time = t.elapsed();
    let top = groups(&grouped.body);
    assert_eq!(top.len(), 4);
    assert_eq!(top.iter().map(|g| g.count).sum::<usize>(), N);
    for g in top {
        let pages = groups(&g.body);
        assert_eq!(pages.len(), 40);
        let numbers: Vec<f64> = pages.iter().map(|p| p.key.as_number().unwrap()).collect();
        assert_eq!(numbers, (1..=40).map(f64::from).collect::<Vec<_>>());
        assert_eq!(pages.iter().map(|p| p.count).sum::<usize>(), g.count);
    }
    let total_qty: f64 = annots
        .iter()
        .filter_map(|a| custom_value(a, s.get("qty").unwrap()).as_number())
        .sum();
    assert_eq!(
        grouped
            .subtotals
            .iter()
            .find(|t| t.column == custom("qty"))
            .unwrap()
            .sum,
        total_qty
    );

    let filter = comment_search("level 2 item")
        .with(ColumnFilter::new(
            custom("qty"),
            Condition::Range {
                min: Some(Value::Number(5.0)),
                max: Some(Value::Number(40.0)),
            },
        ))
        .with(ColumnFilter::not(Builtin::Author, Condition::IsEmpty));
    let t = Instant::now();
    let filtered = View::build(&table, &filter, &[], &[]);
    let filter_time = t.elapsed();
    let want: Vec<usize> = (0..N)
        .filter(|i| {
            let a = &annots[*i];
            a.meta.contents.as_deref().is_some_and(|c| c.contains('2'))
                && a.meta.author.is_some()
                && custom_value(a, s.get("qty").unwrap())
                    .as_number()
                    .is_some_and(|q| (5.0..=40.0).contains(&q))
        })
        .collect();
    assert!(want.len() > 1000);
    assert_eq!(
        filtered
            .rows()
            .iter()
            .map(|r| table.rows[*r].source)
            .collect::<Vec<_>>(),
        want
    );

    let t = Instant::now();
    let everything = View::build(&table, &filter, &sort, &group);
    let combined_time = t.elapsed();
    assert_eq!(everything.count, want.len());
    let t = Instant::now();
    let lines = everything.lines(&table, true, &[]);
    let lines_time = t.elapsed();
    assert!(lines.len() > want.len());

    eprintln!(
        "50k rows: build {build_time:?}, sort(3 keys) {sort_time:?}, group(2 levels) {group_time:?}, \
         filter {filter_time:?}, filter+sort+group {combined_time:?}, lines {lines_time:?}"
    );
    // Generous: these run in well under a tenth of this on the development machine.
    let budget = std::time::Duration::from_secs(4);
    for (name, time) in [
        ("build", build_time),
        ("sort", sort_time),
        ("group", group_time),
        ("filter", filter_time),
        ("filter+sort+group", combined_time),
        ("lines", lines_time),
    ] {
        assert!(time < budget, "{name} took {time:?}");
    }
}
