//! Measurements saved by Bluebeam Revu (`samples/external/cyantakeoff-revu`, local only and
//! never committed; every test that needs them skips when they are absent), and measurements
//! of ours in the form Revu writes.
//!
//! * Every Revu measurement of `measure.pdf` is read as a live measurement of the right kind,
//!   and the value worked out from its geometry and its own `/Measure`, in its own number
//!   formats, is the string Revu wrote as its `/Contents` (the golden test).
//! * The count files hold 24 and 13 PolygonCount markers, read as counts of one each, grouped
//!   as Revu grouped them.
//! * A save that does not touch them writes nothing; an edited one keeps every Revu key.
//! * What this crate writes reads back, by Revu's keys alone, as the same kinds and values.
//! * A count of an earlier version (one Ink annotation) is written Revu's way on the next save.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::Work;
use pl_annot::{
    AnnotId, AnnotSet, Annotation, CountSymbol, Geometry, MeasureFormat, MeasureKind, MeasureSpec,
    PRIVATE_KEY,
};
use pl_geom::{AreaFormat, AreaUnit, LengthFormat, Point, Scale, Vec2};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, SaveOptions, StringFormat};

fn sample(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("external")
        .join("cyantakeoff-revu")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("REVU SAMPLE SKIPPED: samples/external/cyantakeoff-revu/{name} is not present");
        None
    }
}

fn open(path: &Path) -> PdfDocument {
    PdfDocument::from_bytes(std::fs::read(path).unwrap(), None).unwrap()
}

fn kind(a: &Annotation) -> Option<MeasureKind> {
    a.measurement().map(|m| m.kind)
}

fn dict_of(doc: &PdfDocument, id: ObjectId) -> &Dictionary {
    doc.object(id).unwrap().as_dict().unwrap()
}

/// The Revu measurements of measure.pdf in file order, with the /Contents Revu wrote.
const MEASURE_PDF: [(MeasureKind, &str); 10] = [
    (MeasureKind::Length, "5'-5\""),
    (MeasureKind::Area, "56.37 sf"),
    (MeasureKind::Polylength, "18'-11 1/2\""),
    // Revu's Perimeter tool, left open: its vertices do not close, and its value is the
    // length along them. Read by the standard keys, it is a polylength.
    (MeasureKind::Polylength, "22'-4 1/2\""),
    (MeasureKind::Diameter, "4'-9\""),
    (MeasureKind::Radius, "3'-9\""),
    (MeasureKind::Radius, "2'-4\""),
    (MeasureKind::Angle, "96.69\u{b0}"),
    (MeasureKind::Volume, "0 cu ft"),
    (MeasureKind::Count, "1"),
];

#[test]
fn revu_values_are_worked_out_to_revus_own_strings() {
    let Some(path) = sample("measure.pdf") else {
        return;
    };
    let doc = open(&path);
    let set = AnnotSet::load(&doc);
    let got: Vec<&Annotation> = set.iter().collect();
    assert_eq!(got.len(), MEASURE_PDF.len());
    for (a, (want_kind, want)) in got.iter().zip(MEASURE_PDF) {
        assert_eq!(kind(a), Some(want_kind), "{want}");
        assert!(a.is_modelled(), "{want}");
        // Revu's string, as the file has it...
        assert_eq!(a.meta.contents.as_deref(), Some(want));
        // ...and as worked out here from the geometry and the annotation's own number
        // formats (a radius or diameter without the R or the diameter sign Plumbline's own
        // caption puts before it).
        assert_eq!(a.measure_value_text().as_deref(), Some(want), "{want_kind:?}");
        let m = a.measurement().unwrap();
        // Each carries its own scale: 0.25 in = 1 ft, 1/18 ft per point.
        let spec = m.spec.as_ref().expect("the annotation's own scale");
        assert!((spec.x_factor() - 0.05555556).abs() < 1e-9);
        assert!(spec.target_unit_conversion.is_some());
        let q = a.quantities().unwrap();
        match want_kind {
            MeasureKind::Volume => {
                assert_eq!(m.depth, Some(0.0));
                assert_eq!(q.volume, Some(0.0));
            }
            MeasureKind::Count => {
                assert_eq!(q.count, Some(1));
                assert_eq!(m.symbol, CountSymbol::Dot, "a filled circle");
                assert_eq!(a.count_points().len(), 1);
            }
            MeasureKind::Angle => {
                assert!((q.angle.unwrap().to_degrees() - 96.685).abs() < 0.001);
            }
            _ => assert!(q.length.is_some() || q.area.is_some()),
        }
    }
    // The radius is the distance from the middle vertex (the centre) to the first; the
    // diameter is the circle inside /Rect less /RD.
    let Geometry::Line { a: centre, b: edge } = got[5].geometry else {
        panic!("a radius is a line in the model");
    };
    assert_eq!(centre, Point::new(230.0505, 443.195));
    assert_eq!(edge, Point::new(288.3559, 409.1057));
    let Geometry::Line { a, b } = got[4].geometry else {
        panic!("a diameter is a line in the model");
    };
    assert!((a.distance(b) - 85.3643).abs() < 0.01, "{}", a.distance(b));
}

#[test]
fn revu_count_files_hold_their_markers_and_groups() {
    for (name, markers, groups) in [
        ("markup-space.pdf", 24, vec![1, 1, 1, 1, 1, 4, 15]),
        ("markup-deep-spaces.pdf", 13, vec![1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3]),
    ] {
        let Some(path) = sample(name) else {
            continue;
        };
        let doc = open(&path);
        let set = AnnotSet::load(&doc);
        let counts: Vec<&Annotation> = set
            .iter()
            .filter(|a| kind(a) == Some(MeasureKind::Count))
            .collect();
        assert_eq!(counts.len(), markers, "{name}");
        let total: usize = counts
            .iter()
            .map(|a| a.quantities().unwrap().count.unwrap())
            .sum();
        assert_eq!(total, markers, "{name}");
        // Revu groups the markers of one count with /IRT and /RT /Group.
        let mut sizes: BTreeMap<AnnotId, usize> = BTreeMap::new();
        for a in &counts {
            *sizes.entry(a.count_group().unwrap()).or_default() += 1;
        }
        let mut sizes: Vec<usize> = sizes.into_values().collect();
        sizes.sort();
        assert_eq!(sizes, groups, "{name}");
        for a in &counts {
            assert!(a.is_modelled());
            assert_eq!(a.count_points().len(), 1);
        }
    }
}

#[test]
fn untouched_revu_files_save_nothing() {
    for name in ["measure.pdf", "markup-space.pdf", "markup-deep-spaces.pdf"] {
        let Some(path) = sample(name) else {
            continue;
        };
        let doc = open(&path);
        let set = AnnotSet::load(&doc);
        assert!(!set.is_dirty(), "{name}");
        assert!(set.plan_save(&doc).unwrap().is_empty(), "{name}");
    }
}

/// The keys Revu writes besides the standard ones; an edit of ours leaves each as it was.
const REVU_PRIVATE: [&str; 9] = [
    "MeasurementTypes",
    "DepthUnit",
    "SlopeType",
    "PitchRun",
    "AlignOnSegment",
    "RiseDrop",
    "Label",
    "FillOpacity",
    "DS",
];

#[test]
fn an_edited_revu_measurement_keeps_revus_keys_and_the_rest_is_untouched() {
    let Some(path) = sample("measure.pdf") else {
        return;
    };
    let mut work = Work::from_bytes("measure.pdf", std::fs::read(&path).unwrap());
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let ids: Vec<AnnotId> = set.iter().map(|a| a.id.clone()).collect();
    // Move the polylength, the area and the count marker by a quarter of an inch.
    let by = Vec2::new(18.0, 0.0);
    let moved = [&ids[1], &ids[2], &ids[9]];
    for id in moved {
        let a = set.get_mut(id).unwrap();
        a.geometry = a.geometry.map_points(|p| p + by);
        a.sync_measurement();
    }
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 3);
    assert!(plan.added.is_empty() && plan.deleted.is_empty());
    let after = work.doc().clone();
    let changed: Vec<ObjectId> = plan.modified.iter().map(|c| c.object).collect();
    common::assert_untouched(&before, &after, &changed);
    for change in &plan.modified {
        let old = dict_of(&before, change.object);
        let new = dict_of(&after, change.object);
        for key in REVU_PRIVATE.iter().chain(&["CountStyle", "NumCounts", "Version"]) {
            assert_eq!(
                old.get(key.as_bytes()).ok(),
                new.get(key.as_bytes()).ok(),
                "{key} of {:?}",
                change.id
            );
        }
        // The scale reads the same, so Revu's own measure dictionary (with its
        // /TargetUnitConversion) is kept as it was: the same reference.
        for key in ["Measure", "BBMeasure"] {
            assert_eq!(old.get(key.as_bytes()).ok(), new.get(key.as_bytes()).ok());
        }
        assert!(!change.keys.contains(&"Measure".to_string()));
    }
    // Read back: the same values (a translation changes none of them).
    let back = AnnotSet::load(&after);
    assert!(!back.is_dirty());
    for (id, (_, want)) in ids.iter().zip(MEASURE_PDF) {
        let a = back.get(id).unwrap();
        assert_eq!(a.measure_value_text().as_deref(), Some(want), "{id}");
    }
}

#[test]
fn an_edited_revu_radius_and_diameter_keep_revus_value_text_and_keys() {
    let Some(path) = sample("measure.pdf") else {
        return;
    };
    let mut work = Work::from_bytes("measure.pdf", std::fs::read(&path).unwrap());
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let ids: Vec<AnnotId> = set.iter().map(|a| a.id.clone()).collect();
    // The diameter and both radii, moved by one point.
    for id in &ids[4..7] {
        let a = set.get_mut(id).unwrap();
        a.geometry = a.geometry.map_points(|p| p + Vec2::new(1.0, 0.0));
        a.sync_measurement();
    }
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 3);
    let after = work.doc().clone();
    for (change, want) in plan.modified.iter().zip(&MEASURE_PDF[4..7]) {
        let old = dict_of(&before, change.object);
        let new = dict_of(&after, change.object);
        // Revu's bare value, and Revu's own rich text, which still says it.
        assert_eq!(new.get(b"Contents").unwrap(), old.get(b"Contents").unwrap(), "{want:?}");
        assert_eq!(new.get(b"RC").unwrap(), old.get(b"RC").unwrap(), "{want:?}");
        // Nothing Revu's radius and diameter do not have: no fill, no line endings, no
        // caption position.
        for key in ["IC", "LE", "CP"] {
            assert!(!old.has(key.as_bytes()));
            assert!(!new.has(key.as_bytes()), "{key} on {want:?}");
        }
    }
    // Read back, the same values and the same line endings.
    let back = AnnotSet::load(&after);
    for id in &ids[4..7] {
        let (a, b) = (set.get(id).unwrap(), back.get(id).unwrap());
        assert_eq!(a.measure_value_text(), b.measure_value_text());
        assert_eq!(
            (a.style.line_start, a.style.line_end),
            (b.style.line_start, b.style.line_end)
        );
    }
}

#[test]
fn a_marker_added_to_a_revu_count_is_an_item_like_revus_own() {
    let Some(path) = sample("measure.pdf") else {
        return;
    };
    let mut work = Work::from_bytes("measure.pdf", std::fs::read(&path).unwrap());
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let id = set.iter().map(|a| a.id.clone()).nth(9).unwrap();
    assert!(set.get_mut(&id).unwrap().add_count_point(p(500.0, 300.0)));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    let change = &plan.modified[0];
    assert_eq!(change.members.len(), 1);
    let after = work.doc().clone();
    let head = dict_of(&after, change.object);
    let item = dict_of(&after, change.members[0]);
    // The group's total on both.
    for d in [head, item] {
        assert_eq!(d.get(b"NumCounts").unwrap(), &Object::Integer(2));
        assert_eq!(d.get(b"Contents").unwrap().as_str().unwrap(), b"2");
    }
    assert_eq!(item.get(b"IRT").unwrap(), &Object::Reference(change.object));
    // Drawn the way the first is: Revu's filled circle, /IC set and no border.
    assert_eq!(head.get(b"IC").unwrap(), item.get(b"IC").unwrap());
    let w = |d: &Dictionary| d.get(b"BS").unwrap().as_dict().unwrap().get(b"W").unwrap().clone();
    assert_eq!(pl_pdf::number(&w(item)), Some(0.0));
    // Revu's keys of every item, and the same measure dictionary object.
    for key in ["Version", "DS", "Label", "DepthUnit", "CountStyle", "MeasurementTypes", "IT"] {
        assert_eq!(
            head.get(key.as_bytes()).ok(),
            item.get(key.as_bytes()).ok(),
            "{key}"
        );
    }
    assert!(matches!(head.get(b"BBMeasure").unwrap(), Object::Reference(_)));
    assert_eq!(head.get(b"BBMeasure").unwrap(), item.get(b"BBMeasure").unwrap());
    // Read back: one count of two markers.
    let back = AnnotSet::load(&after);
    assert!(!back.is_dirty());
    assert_eq!(back.get(&id).unwrap().count_points().len(), 2);
}

/// A PDF text string: UTF-16BE with its byte order mark, else (near enough here) Latin-1.
fn pdf_text(bytes: &[u8]) -> String {
    match bytes.strip_prefix(&[0xFE, 0xFF]) {
        Some(utf16) => String::from_utf16_lossy(
            &utf16
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect::<Vec<u16>>(),
        ),
        None => bytes.iter().map(|b| char::from(*b)).collect(),
    }
}

#[test]
fn a_marker_added_to_one_item_of_a_revu_group_updates_the_total_on_every_item() {
    let Some(path) = sample("markup-space.pdf") else {
        return;
    };
    let mut work = Work::from_bytes("markup-space.pdf", std::fs::read(&path).unwrap());
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let mut groups: BTreeMap<AnnotId, Vec<AnnotId>> = BTreeMap::new();
    for a in set.iter() {
        if let Some(g) = a.count_group() {
            groups.entry(g).or_default().push(a.id.clone());
        }
    }
    let (_, big) = groups.iter().find(|(_, items)| items.len() == 15).unwrap();
    let big = big.clone();
    // A marker added to the last item of the group of 15.
    let last = big.last().unwrap();
    let at = set.get(last).unwrap().count_points()[0] + Vec2::new(30.0, 0.0);
    assert!(set.get_mut(last).unwrap().add_count_point(at));
    let (plan, _) = work.save(&set);
    assert!(plan.added.is_empty() && plan.deleted.is_empty());
    // The edited item and the 14 others; no other markup.
    assert_eq!(plan.modified.len(), 15);
    let after = work.doc().clone();
    let mut written: Vec<ObjectId> = plan.modified.iter().map(|c| c.object).collect();
    for change in &plan.modified {
        assert!(big.contains(&change.id));
        written.extend(&change.members);
        let old = dict_of(&before, change.object);
        let new = dict_of(&after, change.object);
        assert_eq!(old.get(b"NumCounts").unwrap(), &Object::Integer(15));
        assert_eq!(new.get(b"NumCounts").unwrap(), &Object::Integer(16));
        // The label stays before the total.
        let text = |d: &Dictionary| pdf_text(d.get(b"Contents").unwrap().as_str().unwrap());
        let (was, now) = (text(old), text(new));
        assert!(was.ends_with("15") && now.ends_with("16"), "{was:?} {now:?}");
        assert_eq!(was.trim_end_matches("15"), now.trim_end_matches("16"));
        if change.id != *last {
            // Only the total is written on the others.
            assert!(change.keys.iter().all(|k| ["NumCounts", "Contents", "RC"].contains(&k.as_str())), "{:?}", change.keys);
        }
    }
    let member = dict_of(&after, plan.modified.iter().find(|c| c.id == *last).unwrap().members[0]);
    assert_eq!(member.get(b"NumCounts").unwrap(), &Object::Integer(16));
    // Every other object is as it was (the page's annotation array gains the new item).
    let page = before.page_id(0).unwrap();
    written.push(page);
    if let Ok(Object::Reference(annots)) = dict_of(&before, page).get(b"Annots") {
        written.push(*annots);
    }
    common::assert_untouched(&before, &after, &written);
    // Read back: the group is 16 markers, and the file is clean.
    let back = AnnotSet::load(&after);
    assert!(!back.is_dirty());
    assert!(back.plan_save(&after).unwrap().is_empty());
    let total: usize = back
        .iter()
        .filter(|a| a.count_group().as_ref() == Some(&big[0]))
        .map(|a| a.count_points().len())
        .sum();
    assert_eq!(total, 16);
}

fn arch() -> MeasureSpec {
    let scale = Scale::parse("1/4\" = 1'-0\"").unwrap();
    MeasureSpec::new(
        &scale,
        1.0,
        &MeasureFormat::new(
            LengthFormat::architectural(16),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        ),
    )
}

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

/// One of each kind of ours, at 1/4" = 1'-0".
fn ours() -> Vec<Annotation> {
    let og = common::origin();
    let s = arch();
    let square = vec![p(300.0, 300.0), p(372.0, 300.0), p(372.0, 372.0), p(300.0, 372.0)];
    let mut count = Annotation::count(
        0,
        &[p(100.0, 500.0), p(130.0, 500.0), p(160.0, 520.0)],
        CountSymbol::Square,
        &og,
    );
    count.apply_scale(pl_annot::ScaleRef::Page, s.clone());
    vec![
        Annotation::measure_length(0, p(100.0, 100.0), p(244.0, 100.0), s.clone(), &og),
        Annotation::measure_polylength(
            0,
            vec![p(100.0, 150.0), p(172.0, 150.0), p(172.0, 222.0)],
            s.clone(),
            &og,
        ),
        Annotation::measure_perimeter(0, square.clone(), s.clone(), &og),
        Annotation::measure_area(0, square.clone(), Vec::new(), s.clone(), &og),
        Annotation::measure_volume(0, square, Vec::new(), 0.3048, s.clone(), &og),
        Annotation::measure_angle(
            0,
            p(400.0, 100.0),
            p(400.0, 160.0),
            p(460.0, 160.0),
            Some(s.clone()),
            &og,
        ),
        Annotation::measure_radius(0, p(450.0, 400.0), p(486.0, 400.0), s.clone(), &og),
        Annotation::measure_diameter(0, p(400.0, 600.0), p(436.0, 636.0), s, &og),
        count,
    ]
}

#[test]
fn ours_read_back_by_revus_keys_alone_as_the_same_kinds_and_values() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let mut set = AnnotSet::load(&doc);
    let made = ours();
    for a in &made {
        set.add(a.clone()).unwrap();
    }
    let (plan, _) = work.save(&set);
    let saved = work.doc().clone();

    // Read back as ours: the same model, clean.
    let back = AnnotSet::load(&saved);
    assert!(!back.is_dirty());
    for a in &made {
        assert_eq!(back.get(&a.id).unwrap().detached(), a.detached(), "{:?}", kind(a));
    }

    // Without the private part, as Revu (or any reader of the standard and Revu keys)
    // sees them: the same kinds and the same values.
    let mut edits = saved.edit();
    for change in &plan.added {
        for object in std::iter::once(change.object).chain(change.members.iter().copied()) {
            edits.patch(object).remove(PRIVATE_KEY);
        }
    }
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let bare = work.doc().clone();
    let read = AnnotSet::load(&bare);
    let measured: Vec<&Annotation> = read.iter().filter(|a| kind(a).is_some()).collect();
    // The count is three markers there, grouped under the first.
    assert_eq!(measured.len(), made.len() + 2);
    for a in &made {
        let k = kind(a).unwrap();
        let got = read.get(&a.id).unwrap();
        assert_eq!(kind(got), Some(k), "{k:?}");
        if k == MeasureKind::Count {
            let group: Vec<&&Annotation> = measured
                .iter()
                .filter(|m| m.count_group() == Some(a.id.clone()))
                .collect();
            assert_eq!(group.len(), 3);
            continue;
        }
        if k == MeasureKind::Volume {
            // No key of Revu's is known to hold a depth (its volume in measure.pdf is 0 and
            // has none): by those keys alone the depth is 0, in the /V written for it.
            assert_eq!(got.measure_value_text().as_deref(), Some("0.00 cu yd"));
            continue;
        }
        assert_eq!(got.measure_value_text(), a.measure_value_text(), "{k:?}");

    }
}

#[test]
fn our_dot_count_is_revus_filled_circle_with_its_scale_on_every_item() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let mut set = AnnotSet::load(&doc);
    let mut count = Annotation::count(
        0,
        &[p(100.0, 500.0), p(130.0, 500.0)],
        CountSymbol::Dot,
        &common::origin(),
    );
    count.apply_scale(pl_annot::ScaleRef::Page, arch());
    let stroke = count.style.stroke.unwrap();
    let id = set.add(count).unwrap();
    let (plan, _) = work.save(&set);
    let saved = work.doc().clone();
    let change = &plan.added[0];
    for object in std::iter::once(change.object).chain(change.members.iter().copied()) {
        let d = dict_of(&saved, object);
        assert_eq!(d.get(b"CountStyle").unwrap(), &Object::Name(b"Circle".to_vec()));
        let ic: Vec<f32> = d
            .get(b"IC")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| pl_pdf::number(o).unwrap() as f32)
            .collect();
        assert_eq!(ic, [stroke.r, stroke.g, stroke.b]);
        assert!(d.has(b"BBMeasure"));
        assert_eq!(d.get(b"NumCounts").unwrap(), &Object::Integer(2));
    }
    let back = AnnotSet::load(&saved);
    assert!(!back.is_dirty());
    assert_eq!(back.get(&id).unwrap().measurement().unwrap().symbol, CountSymbol::Dot);
}

/// A count as an earlier version wrote it: one Ink annotation, a stroke per marker.
fn old_count(doc: &PdfDocument) -> (Dictionary, Vec<Point>) {
    let markers = vec![p(100.0, 100.0), p(140.0, 100.0), p(180.0, 120.0)];
    let text = |s: &str| Object::String(s.as_bytes().to_vec(), StringFormat::Literal);
    let real = |v: f64| Object::Real(v as f32);
    let mut model = Dictionary::new();
    model.set("Type", Object::Name(b"Measure".to_vec()));
    model.set("Kind", Object::Name(b"Count".to_vec()));
    model.set("Caption", Object::Boolean(false));
    model.set("FontSize", text("9"));
    model.set("Symbol", Object::Name(b"Ring".to_vec()));
    model.set("SymbolSize", text("10"));
    let mut private = Dictionary::new();
    private.set("PlModel", Object::Dictionary(model));
    let mut d = Dictionary::new();
    d.set("Type", Object::Name(b"Annot".to_vec()));
    d.set("Subtype", Object::Name(b"Ink".to_vec()));
    d.set(
        "Rect",
        Object::Array(vec![real(90.0), real(90.0), real(190.0), real(130.0)]),
    );
    d.set("NM", text("old-count"));
    d.set("P", Object::Reference(doc.page_id(0).unwrap()));
    d.set("F", Object::Integer(4));
    d.set("Contents", text("3"));
    d.set("Subj", text("Count"));
    d.set("C", Object::Array(vec![real(1.0), real(0.0), real(0.0)]));
    d.set(
        "InkList",
        Object::Array(
            markers
                .iter()
                .map(|m| Object::Array(vec![real(m.x), real(m.y), real(m.x), real(m.y)]))
                .collect(),
        ),
    );
    d.set(PRIVATE_KEY, Object::Dictionary(private));
    (d, markers)
}

#[test]
fn an_earlier_versions_ink_count_is_written_revus_way_on_the_next_save() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let (dict, markers) = old_count(&doc);
    let mut edits = doc.edit();
    edits.add_annotation(0, dict);
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let old = work.doc().clone();
    let set = AnnotSet::load(&old);
    let id = AnnotId::from("old-count");
    let count = set.get(&id).unwrap();
    assert_eq!(kind(count), Some(MeasureKind::Count));
    assert_eq!(count.count_points(), markers);
    // Read as it is; written the new way on the next save.
    assert!(set.is_dirty());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].members.len(), 2);
    let saved = work.doc().clone();
    let head = dict_of(&saved, plan.modified[0].object);
    assert_eq!(head.get(b"Subtype").unwrap(), &Object::Name(b"Polygon".to_vec()));
    assert_eq!(
        head.get(b"IT").unwrap(),
        &Object::Name(b"PolygonCount".to_vec())
    );
    assert!(!head.has(b"InkList"));
    let back = AnnotSet::load(&saved);
    assert!(!back.is_dirty());
    let count = back.get(&id).unwrap();
    assert_eq!(count.count_points(), markers);
    assert_eq!(count.measurement().unwrap().symbol, CountSymbol::Ring);
    assert_eq!(count.meta.contents.as_deref(), Some("3"));
    // Changing it again replaces its further annotations; deleting it takes them all.
    let mut set = back;
    assert!(set.get_mut(&id).unwrap().add_count_point(p(220.0, 140.0)));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].members.len(), 3);
    let saved = work.doc().clone();
    let mut set = AnnotSet::load(&saved);
    assert_eq!(set.get(&id).unwrap().count_points().len(), 4);
    let page_had = saved.annotations(0).unwrap().len();
    set.remove(&id);
    work.save(&set);
    assert_eq!(work.doc().annotations(0).unwrap().len(), page_had - 4);
}

#[test]
fn a_count_patched_alone_patches_its_further_annotations_too() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let (dict, _) = old_count(&doc);
    let mut edits = doc.edit();
    edits.add_annotation(0, dict);
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let set = AnnotSet::load(&work.doc().clone());
    let (plan, _) = work.save(&set);
    let members = plan.modified[0].members.clone();
    assert_eq!(members.len(), 2);

    // Only the subject and the Locked flag change: the count is patched, not rewritten.
    let mut set = AnnotSet::load(&work.doc().clone());
    let id = AnnotId::from("old-count");
    let a = set.get_mut(&id).unwrap();
    a.meta.subject = Some("Renamed count".into());
    a.meta.flags = a.meta.flags.with(pl_annot::Flags::LOCKED, true);
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert!(plan.modified[0].members.is_empty(), "markers are not rewritten");
    let saved = work.doc().clone();
    for object in std::iter::once(plan.modified[0].object).chain(members.iter().copied()) {
        let d = dict_of(&saved, object);
        let subject = match d.get(b"Subj").unwrap() {
            Object::String(b, _) => pl_annot::decode_text(b),
            other => panic!("{other:?}"),
        };
        assert_eq!(subject, "Renamed count");
        let flags = d.get(b"F").unwrap().as_i64().unwrap();
        assert_ne!(flags & 128, 0, "every marker locked");
    }
    let back = AnnotSet::load(&saved);
    assert_eq!(back.get(&id).unwrap().count_points().len(), 3);
}
