//! Modify, delete, restack and move between pages; malformed input; private data; other
//! cross-reference styles and encrypted files.

mod common;

use common::{Work, TARGETS};
use pl_annot::{
    AnnotError, AnnotId, AnnotKind, AnnotSet, Annotation, DrawList, Flags, Geometry, LineEnding,
    PdfDate, Rgb, PRIVATE_KEY,
};
use pl_geom::{Point, Rect, Vec2};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, SaveOptions, StringFormat};

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn text(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

fn ints(v: &[i64]) -> Object {
    Object::Array(v.iter().map(|i| Object::Integer(*i)).collect())
}

fn dict_of(doc: &PdfDocument, id: ObjectId) -> &Dictionary {
    doc.object(id).unwrap().as_dict().unwrap()
}

/// Asserts that `new` is `old` with only `keys` set, changed or removed.
fn assert_only_keys_differ(old: &Dictionary, new: &Dictionary, keys: &[&str], label: &str) {
    let named = |k: &[u8]| keys.iter().any(|x| x.as_bytes() == k);
    for (key, value) in old.iter() {
        if !named(key) {
            assert_eq!(
                new.get(key).ok(),
                Some(value),
                "{label}: /{} changed",
                String::from_utf8_lossy(key)
            );
        }
    }
    for (key, _) in new.iter() {
        assert!(
            named(key) || old.has(key),
            "{label}: /{} appeared",
            String::from_utf8_lossy(key)
        );
    }
}

/// Each kind of edit, on a set of our own markups, writes exactly the keys it should.
#[test]
fn modify_writes_only_the_dirty_keys() {
    let mut work = Work::new(&pl_testkit::plain());
    let o = work.sample_origin(0);
    let mut set = AnnotSet::new();
    for a in common::one_of_each(0, o) {
        set.add(a).unwrap();
    }
    work.save(&set);

    let find = |set: &AnnotSet, kind: AnnotKind| -> AnnotId {
        set.iter().find(|a| a.kind == kind).unwrap().id.clone()
    };
    type Edit = (
        &'static str,
        AnnotKind,
        fn(&mut Annotation),
        &'static [&'static str],
    );
    let edits: [Edit; 14] = [
        (
            "contents",
            AnnotKind::Square,
            |a| a.meta.contents = Some("New text".into()),
            &["Contents"],
        ),
        (
            "author-subject",
            AnnotKind::Circle,
            |a| {
                a.meta.author = Some("Ren\u{e9}e".into());
                a.meta.subject = None;
            },
            &["Subj", "T"],
        ),
        (
            "touch",
            AnnotKind::Line,
            |a| a.touch(PdfDate::parse("D:20270203040506Z").unwrap()),
            &["M"],
        ),
        (
            "flags",
            AnnotKind::Ink,
            |a| a.meta.flags = a.meta.flags.with(Flags::LOCKED, true),
            &["F"],
        ),
        (
            "colour",
            AnnotKind::Square,
            |a| a.style.stroke = Some(Rgb::new(0.0, 0.0, 0.5)),
            &["AP", "C"],
        ),
        (
            "fill",
            AnnotKind::Circle,
            |a| a.style.fill = None,
            &["AP", "IC"],
        ),
        (
            "opacity",
            AnnotKind::Polygon,
            |a| a.style.opacity = 0.25,
            &["AP", "CA"],
        ),
        (
            "width",
            AnnotKind::Line,
            |a| a.style.width = 6.0,
            &["AP", "BS", "Rect"],
        ),
        (
            "dash",
            AnnotKind::Square,
            |a| a.style.dash = Some(vec![4.0, 2.0]),
            &["AP", "BS"],
        ),
        (
            "endings",
            AnnotKind::PolyLine,
            |a| a.style.line_end = LineEnding::ClosedArrow,
            &["AP", "LE", "Rect"],
        ),
        (
            "cloud",
            AnnotKind::Square,
            |a| a.style.cloud = Some(1.0),
            &["AP", "BE", "RD", "Rect"],
        ),
        (
            "blend",
            AnnotKind::Highlight,
            |a| a.style.blend = pl_annot::BlendMode::Normal,
            &["AP", "BM"],
        ),
        (
            "geometry",
            AnnotKind::Polygon,
            |a| {
                let Geometry::Points(p) = &mut a.geometry else {
                    panic!()
                };
                p[0] += Vec2::new(-10.0, -10.0);
            },
            &["AP", "Rect", "Vertices"],
        ),
        (
            "note-icon-and-move",
            AnnotKind::Text,
            |a| {
                a.icon = Some("Comment".into());
                a.translate(Vec2::new(4.0, 0.0));
            },
            &["AP", "Name", "Rect"],
        ),
    ];

    for (label, kind, edit, keys) in edits {
        let before = work.doc().clone();
        let mut set = AnnotSet::load(&before);
        assert!(!set.is_dirty(), "{label}: clean before the edit");
        let id = find(&set, kind);
        edit(set.get_mut(&id).unwrap());
        let expected = set.get(&id).unwrap().detached();
        let object = set.get(&id).unwrap().object_id().unwrap();
        assert!(set.is_dirty(), "{label}");

        let (plan, _) = work.save(&set);
        assert!(plan.added.is_empty() && plan.deleted.is_empty(), "{label}");
        assert_eq!(plan.modified.len(), 1, "{label}: one annotation is written");
        assert_eq!(plan.modified[0].id, id);
        assert_eq!(plan.modified[0].keys, keys, "{label}");
        assert_eq!(
            plan.modified[0].appearance.is_some(),
            keys.contains(&"AP"),
            "{label}"
        );

        let after = work.doc().clone();
        common::assert_untouched(&before, &after, &[object]);
        assert_only_keys_differ(
            dict_of(&before, object),
            dict_of(&after, object),
            keys,
            label,
        );
        for key in keys {
            if *key != "Subj" && *key != "IC" {
                assert_ne!(
                    dict_of(&before, object).get(key.as_bytes()).ok(),
                    dict_of(&after, object).get(key.as_bytes()).ok(),
                    "{label}: /{key} was written with a new value"
                );
            }
        }
        assert!(!dict_of(&after, object).has(b"Subj") || label != "author-subject");

        let loaded = AnnotSet::load(&after);
        assert_eq!(
            loaded.get(&id).unwrap().detached(),
            expected,
            "{label}: reads back as edited"
        );
        assert!(!loaded.is_dirty(), "{label}: clean after the save");
        assert_eq!(loaded.len(), set.len());
    }
}

/// Moving every kind on every page layout: the geometry key and the appearance are rewritten,
/// the model reads back moved, and PDFium draws it where the draw list says.
#[test]
fn moved_markups_round_trip_and_render_on_every_page_layout() {
    let backend = common::backend();
    for target in &TARGETS {
        let fixture = (target.fixture)();
        let mut work = Work::new(&fixture);
        let geometry = work.geometry(target.page);
        let o = work.sample_origin(target.page);
        let mut set = AnnotSet::new();
        for a in common::one_of_each(target.page, o) {
            set.add(a).unwrap();
        }
        work.save(&set);

        let before = work.doc().clone();
        let mut set = AnnotSet::load(&before);
        let by = Vec2::new(16.0, 8.5);
        let ids: Vec<AnnotId> = set.iter().map(|a| a.id.clone()).collect();
        for id in &ids {
            set.get_mut(id).unwrap().translate(by);
        }
        let expected: Vec<Annotation> = set.iter().map(Annotation::detached).collect();
        let (plan, _) = work.save(&set);
        assert_eq!(plan.modified.len(), ids.len(), "{}", target.name);
        for change in &plan.modified {
            let a = set.get(&change.id).unwrap();
            let geometry_key = match a.geometry {
                Geometry::Rect(_) => "Rect",
                Geometry::Line { .. } => "L",
                Geometry::Points(_) => "Vertices",
                Geometry::Ink(_) => "InkList",
                Geometry::Quads(_) => "QuadPoints",
            };
            let mut want = vec![geometry_key, "Rect"];
            if !matches!(a.kind, AnnotKind::Popup | AnnotKind::Link) {
                want.push("AP");
            }
            want.sort();
            want.dedup();
            assert_eq!(change.keys, want, "{}: {:?}", target.name, a.kind);
        }

        let after = work.doc().clone();
        let loaded = AnnotSet::load(&after);
        let got: Vec<Annotation> = loaded.iter().map(Annotation::detached).collect();
        assert_eq!(got, expected, "{}", target.name);
        assert!(!loaded.is_dirty());

        let mut list = DrawList::new();
        for a in loaded.iter() {
            list.extend(a.draw_list().ops);
        }
        let rendered = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
        let ours = common::raster_over(&list, &geometry, &rendered.backdrop);
        let d = common::diff(&ours, &rendered.pdfium, 1, 40);
        assert!(
            common::changed_pixels(&rendered.backdrop, &rendered.pdfium, 12) > 20_000,
            "{}",
            target.name
        );
        assert!(d.bad <= 6 && d.mean < 0.6, "{}: {d:?}", target.name);
    }
}

#[test]
fn delete_removes_the_annotation_and_its_popup() {
    let backend = common::backend();
    for target in [&TARGETS[0], &TARGETS[7]] {
        let fixture = (target.fixture)();
        let mut work = Work::new(&fixture);
        let o = work.sample_origin(target.page);
        let og = common::origin();
        let mut note = Annotation::note(target.page, Rect::new(0.0, 0.0, 20.0, 20.0) + o, "n", &og);
        let popup = Annotation::popup_for(&mut note, Rect::new(30.0, 0.0, 150.0, 80.0) + o, &og);
        let mut reply =
            Annotation::note(target.page, Rect::new(0.0, 0.0, 20.0, 20.0) + o, "re", &og);
        reply.meta.reply_to = Some(note.id.clone());
        reply.meta.reply_type = Some("R".into());
        let square = Annotation::square(target.page, Rect::new(60.0, 60.0, 120.0, 100.0) + o, &og);
        let mut set = AnnotSet::new();
        let square_id = set.add(square).unwrap();
        let note_id = set.add(note).unwrap();
        let popup_id = set.add(popup).unwrap();
        let reply_id = set.add(reply).unwrap();
        work.save(&set);

        let before = work.doc().clone();
        assert_eq!(before.annotation_counts()[target.page], 4);
        let mut set = AnnotSet::load(&before);
        // The links were written as references and read back as ids.
        assert_eq!(set.get(&note_id).unwrap().popup.as_ref(), Some(&popup_id));
        assert_eq!(set.get(&popup_id).unwrap().parent.as_ref(), Some(&note_id));
        assert_eq!(
            set.get(&reply_id).unwrap().meta.reply_to.as_ref(),
            Some(&note_id)
        );
        let note_object = set.get(&note_id).unwrap().object_id().unwrap();
        let popup_object = set.get(&popup_id).unwrap().object_id().unwrap();
        assert_eq!(
            dict_of(&before, note_object).get(b"Popup").unwrap(),
            &Object::Reference(popup_object)
        );
        assert_eq!(
            dict_of(&before, popup_object).get(b"Parent").unwrap(),
            &Object::Reference(note_object)
        );

        // Delete then undo: nothing to save.
        let removed = set.remove(&note_id);
        assert_eq!(removed.items.len(), 3, "note, popup and reply");
        assert!(set.is_dirty());
        set.restore(removed);
        assert!(!set.is_dirty());
        assert!(set.plan_save(&before).unwrap().is_empty());

        // Delete for real.
        let removed = set.remove(&note_id);
        let gone: Vec<&AnnotId> = removed.ids().collect();
        assert!(gone.contains(&&note_id) && gone.contains(&&popup_id) && gone.contains(&&reply_id));
        let (plan, _) = work.save(&set);
        assert!(plan.added.is_empty() && plan.modified.is_empty());
        assert_eq!(plan.deleted.len(), 3);
        assert_eq!(plan.touched_pages(), [target.page]);

        let after = work.doc().clone();
        assert_eq!(
            after.annotation_counts()[target.page],
            1,
            "/Annots holds the square only"
        );
        let left = after.annotations(target.page).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, set.get(&square_id).unwrap().object_id());
        // The square was not rewritten by any of this.
        let page_id = before.page_id(target.page).unwrap();
        common::assert_untouched(&before, &after, &[page_id]);
        let loaded = AnnotSet::load(&after);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains(&square_id) && !loaded.contains(&popup_id));
        assert_eq!(
            common::pdfium_annotation_counts(
                &backend,
                pl_render::DocSource::Bytes(work.bytes()),
                &[target.page]
            ),
            [1]
        );
    }
}

#[test]
fn deleting_only_a_popup_unlinks_its_parent() {
    let mut work = Work::new(&pl_testkit::plain());
    let og = common::origin();
    let mut note = Annotation::note(0, Rect::new(100.0, 100.0, 120.0, 120.0), "n", &og);
    let popup = Annotation::popup_for(&mut note, Rect::new(130.0, 100.0, 250.0, 180.0), &og);
    let mut set = AnnotSet::new();
    let note_id = set.add(note).unwrap();
    let popup_id = set.add(popup).unwrap();
    work.save(&set);

    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let note_object = set.get(&note_id).unwrap().object_id().unwrap();
    set.remove(&popup_id);
    let (plan, _) = work.save(&set);
    assert_eq!(plan.deleted.len(), 1);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["Popup"]);
    let after = work.doc().clone();
    assert!(!dict_of(&after, note_object).has(b"Popup"));
    assert_only_keys_differ(
        dict_of(&before, note_object),
        dict_of(&after, note_object),
        &["Popup"],
        "note",
    );
    assert_eq!(after.annotation_counts(), [1]);
    assert_eq!(AnnotSet::load(&after).get(&note_id).unwrap().popup, None);
}

#[test]
fn restacking_rewrites_the_annots_array_and_nothing_else() {
    let mut work = Work::new(&pl_testkit::plain());
    let og = common::origin();
    let mut set = AnnotSet::new();
    let mut ids = Vec::new();
    for i in 0..4 {
        let x = 100.0 + 30.0 * i as f64;
        let mut a = Annotation::square(0, Rect::new(x, 300.0, x + 80.0, 380.0), &og);
        a.style.fill = Some(Rgb::new(0.25 * i as f32, 0.5, 1.0 - 0.25 * i as f32));
        ids.push(set.add(a).unwrap());
    }
    work.save(&set);
    let before = work.doc().clone();
    let page_id = before.page_id(0).unwrap();

    let mut set = AnnotSet::load(&before);
    let p = Point::new(195.0, 340.0);
    assert_eq!(set.hit_test(0, p, 0.0).unwrap().id, ids[3]);
    set.bring_to_front(&ids[1]);
    set.send_to_back(&ids[3]);
    assert!(set.is_dirty());
    assert_eq!(set.hit_test(0, p, 0.0).unwrap().id, ids[1]);
    // A new one, placed under everything.
    let mut under = Annotation::circle(0, Rect::new(90.0, 290.0, 300.0, 390.0), &og);
    under.style.fill = Some(Rgb::WHITE);
    let under_id = under.id.clone();
    set.insert(0, under).unwrap();

    let (plan, _) = work.save(&set);
    assert_eq!(plan.reordered_pages, [0]);
    assert!(plan.modified.is_empty() && plan.deleted.is_empty());
    assert_eq!(plan.added.len(), 1);
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[page_id]);

    let loaded = AnnotSet::load(&after);
    let order: Vec<AnnotId> = loaded.iter().map(|a| a.id.clone()).collect();
    let want = vec![
        under_id,
        ids[3].clone(),
        ids[0].clone(),
        ids[2].clone(),
        ids[1].clone(),
    ];
    assert_eq!(
        order, want,
        "the file's /Annots order is the stacking order"
    );
    assert!(!loaded.is_dirty());
    assert_eq!(after.annotation_counts(), [5]);
}

#[test]
fn an_annotation_can_move_to_another_page() {
    let mut work = Work::new(&pl_testkit::rotated_pages());
    let og = common::origin();
    let mut set = AnnotSet::new();
    let stay = set
        .add(Annotation::square(
            0,
            Rect::new(100.0, 100.0, 200.0, 200.0),
            &og,
        ))
        .unwrap();
    let mover = set
        .add(Annotation::line(
            0,
            Point::new(50.0, 50.0),
            Point::new(250.0, 150.0),
            &og,
        ))
        .unwrap();
    work.save(&set);
    let before = work.doc().clone();
    assert_eq!(before.annotation_counts(), [2, 0, 0, 0]);

    let mut set = AnnotSet::load(&before);
    set.get_mut(&mover).unwrap().page = 2;
    let object = set.get(&mover).unwrap().object_id().unwrap();
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["P"]);
    assert_eq!(plan.touched_pages(), [0, 2]);
    let after = work.doc().clone();
    assert_eq!(after.annotation_counts(), [1, 0, 1, 0]);
    assert_eq!(
        dict_of(&after, object).get(b"P").unwrap(),
        &Object::Reference(after.page_id(2).unwrap())
    );
    let loaded = AnnotSet::load(&after);
    assert_eq!(loaded.get(&mover).unwrap().page, 2);
    assert_eq!(loaded.get(&stay).unwrap().page, 0);
    assert_eq!(
        loaded.get(&mover).unwrap().object_id(),
        Some(object),
        "same object, new page"
    );
    assert!(!loaded.is_dirty());

    // A page that does not exist is refused.
    let mut set = loaded.clone();
    set.get_mut(&mover).unwrap().page = 9;
    assert!(matches!(
        set.plan_save(&after),
        Err(AnnotError::PageOutOfRange {
            page: 9,
            count: 4,
            ..
        })
    ));
}

/// A page of annotations that are wrong in every way that came to mind. Loading, drawing,
/// hit-testing and planning must cope; none may panic.
#[test]
fn malformed_annotations_are_read_without_failing() {
    let mut work = Work::new(&pl_testkit::plain());
    let doc = work.doc().clone();
    let page_id = doc.page_id(0).unwrap();
    let mut edits = doc.edit();
    let mut make = |pairs: Vec<(&str, Object)>| -> Object {
        let mut d = Dictionary::new();
        for (k, v) in pairs {
            d.set(k, v);
        }
        Object::Reference(edits.add_object(Object::Dictionary(d)))
    };
    let good_rect = || ints(&[100, 100, 200, 200]);
    let mut entries = vec![
        // 0: an empty dictionary.
        make(vec![]),
        // 1: no /Rect, /Subtype not a name.
        make(vec![("Subtype", Object::Integer(7))]),
        // 2: a Square whose /Rect has three numbers and whose /C has five.
        make(vec![
            ("Subtype", name("Square")),
            ("Rect", ints(&[1, 2, 3])),
            ("C", ints(&[1, 0, 0, 1, 1])),
            ("BS", text("thick")),
            ("CA", text("half")),
            ("RD", ints(&[500, 500, 500, 500])),
        ]),
        // 3: a Line without /L, with a negative width and a wild /LE.
        make(vec![
            ("Subtype", name("Line")),
            ("Rect", good_rect()),
            ("BS", {
                let mut bs = Dictionary::new();
                bs.set("W", Object::Integer(-4));
                bs.set("S", name("D"));
                bs.set("D", ints(&[0, 0]));
                Object::Dictionary(bs)
            }),
            (
                "LE",
                Object::Array(vec![name("ROpenArrow"), Object::Integer(3), name("Circle")]),
            ),
        ]),
        // 4: a Polygon with an odd number of coordinates.
        make(vec![
            ("Subtype", name("Polygon")),
            ("Rect", good_rect()),
            ("Vertices", ints(&[1, 2, 3])),
        ]),
        // 5: an Ink whose /InkList holds things that are not point lists.
        make(vec![
            ("Subtype", name("Ink")),
            ("Rect", good_rect()),
            (
                "InkList",
                Object::Array(vec![ints(&[1, 2, 3]), text("scribble")]),
            ),
        ]),
        // 6: a Highlight with seven quad numbers, a reversed /Rect and a bad date.
        make(vec![
            ("Subtype", name("Highlight")),
            ("Rect", ints(&[200, 200, 100, 100])),
            ("QuadPoints", ints(&[1, 2, 3, 4, 5, 6, 7])),
            ("M", text("last Tuesday")),
            ("NM", text("")),
            ("F", Object::Integer(-1)),
        ]),
        // 7 and 8: two annotations with the same /NM; one links to things that are not there.
        make(vec![
            ("Subtype", name("Text")),
            ("Rect", good_rect()),
            ("NM", text("twin")),
            ("Popup", Object::Reference((9999, 0))),
            ("IRT", Object::Reference(page_id)),
            ("Name", text("not a name")),
        ]),
        make(vec![
            ("Subtype", name("Circle")),
            ("Rect", good_rect()),
            ("NM", text("twin")),
        ]),
        // 9: a Link with an action of no known kind and a destination that is not a page.
        make(vec![
            ("Subtype", name("Link")),
            ("Rect", good_rect()),
            ("A", {
                let mut a = Dictionary::new();
                a.set("S", name("Launch"));
                Object::Dictionary(a)
            }),
        ]),
        // 10: a PolyLine with huge coordinates and an empty /BE.
        make(vec![
            ("Subtype", name("PolyLine")),
            ("Rect", good_rect()),
            (
                "Vertices",
                Object::Array(vec![
                    Object::Real(1e30),
                    Object::Real(-1e30),
                    Object::Integer(0),
                    Object::Integer(0),
                ]),
            ),
            ("BE", Object::Dictionary(Dictionary::new())),
        ]),
    ];
    // 11: a dictionary written directly in the array; 12 and 13: entries that are not
    // annotations at all.
    let mut direct = Dictionary::new();
    direct.set("Subtype", name("Square"));
    direct.set("Rect", ints(&[300, 300, 360, 340]));
    direct.set("C", ints(&[0, 0, 1]));
    entries.push(Object::Dictionary(direct));
    entries.push(Object::Integer(42));
    entries.push(Object::Reference((8888, 0)));
    edits.patch(page_id).set("Annots", Object::Array(entries));
    // pl-pdf's verification counts /Annots through its own bookkeeping, which a hand-made
    // array bypasses; it is switched off for building this file.
    let options = SaveOptions {
        verify: false,
        ..SaveOptions::default()
    };
    work.file.save_incremental(&edits, &options).unwrap();

    let doc = work.doc().clone();
    let set = AnnotSet::load(&doc);
    assert_eq!(set.len(), 12, "the two non-annotation entries are skipped");
    assert!(!set.is_dirty());
    assert!(set.plan_save(&doc).unwrap().is_empty());
    let ids: std::collections::HashSet<&AnnotId> = set.iter().map(|a| &a.id).collect();
    assert_eq!(
        ids.len(),
        12,
        "ids are unique even where /NM is missing, empty or duplicated"
    );

    for a in set.iter() {
        // None of these panics, whatever was read.
        let list = a.draw_list();
        list.validate().unwrap();
        let _ = a.bounds();
        let _ = a.handles();
        let _ = a.hit_test(Point::new(150.0, 150.0), 2.0);
        let _ = a.placement();
        assert!(a.style.width.is_finite() && a.style.width >= 0.0);
        assert!((0.0..=1.0).contains(&a.style.opacity));
    }
    let at = |i: usize| set.iter().nth(i).unwrap();
    assert_eq!(at(0).kind, AnnotKind::Unknown(String::new()));
    assert_eq!(at(0).geometry, Geometry::Rect(Rect::ZERO));
    assert!(!at(0).is_modelled());
    assert_eq!(at(2).kind, AnnotKind::Square);
    assert_eq!(at(2).style.stroke, None);
    assert_eq!((at(2).style.width, at(2).style.opacity), (1.0, 1.0));
    assert!(!at(3).is_modelled(), "a Line without /L can only be moved");
    assert_eq!(
        at(3).geometry,
        Geometry::Rect(Rect::new(100.0, 100.0, 200.0, 200.0))
    );
    assert_eq!(at(3).style.width, 1.0);
    assert_eq!(at(3).style.dash, None, "an all-zero dash is no dash");
    assert_eq!(
        (at(3).style.line_start, at(3).style.line_end),
        (LineEnding::None, LineEnding::None)
    );
    assert!(!at(4).is_modelled() && !at(5).is_modelled() && !at(6).is_modelled());
    assert_eq!(
        at(6).geometry,
        Geometry::Rect(Rect::new(100.0, 100.0, 200.0, 200.0))
    );
    assert_eq!(at(6).meta.modified, None);
    assert!(
        at(6).id.as_str().starts_with("pl-"),
        "an empty /NM is no name"
    );
    assert_eq!(at(7).id.as_str(), "twin");
    assert_ne!(at(8).id.as_str(), "twin");
    assert_eq!(
        (
            at(7).popup.clone(),
            at(7).meta.reply_to.clone(),
            at(7).icon.clone()
        ),
        (None, None, None)
    );
    assert_eq!(at(9).link, Some(pl_annot::LinkTarget::Other));
    assert!(at(10).is_modelled());
    assert_eq!(at(10).style.cloud, None);
    assert_eq!(at(11).object_id(), None);
    assert_eq!(at(11).style.stroke, Some(Rgb::BLUE));

    // The same ids come out of a second load.
    let again = AnnotSet::load(&doc);
    assert_eq!(again, set);

    // A directly written annotation can be read but not changed or deleted this way.
    let direct_id = at(11).id.clone();
    let mut changed = set.clone();
    changed.get_mut(&direct_id).unwrap().style.width = 3.0;
    assert!(matches!(changed.plan_save(&doc), Err(AnnotError::NotEditable(id)) if id == direct_id));
    let mut removed = set.clone();
    removed.remove(&direct_id);
    assert!(matches!(removed.plan_save(&doc), Err(AnnotError::NotEditable(id)) if id == direct_id));

    // Malformed neighbours do not stop a normal edit: describe one, delete another, add one.
    let mut edited = set.clone();
    let nameless = at(6).id.clone();
    edited.get_mut(&nameless).unwrap().meta.contents = Some("Looked at".into());
    let twin = at(8).id.clone();
    edited.remove(&at(0).id.clone());
    edited
        .add(Annotation::square(
            0,
            Rect::new(400.0, 400.0, 450.0, 450.0),
            &common::origin(),
        ))
        .unwrap();
    let plan = edited.plan_save(&doc).unwrap();
    assert_eq!(
        (plan.added.len(), plan.modified.len(), plan.deleted.len()),
        (1, 1, 1)
    );
    // The annotation without a usable /NM gets one, so its id survives the save.
    assert_eq!(plan.modified[0].keys, ["Contents", "NM"]);
    work.file.save_incremental(&plan.edits, &options).unwrap();
    let doc = work.doc().clone();
    let loaded = AnnotSet::load(&doc);
    assert_eq!(loaded.len(), 12);
    assert_eq!(
        loaded.get(&nameless).unwrap().meta.contents.as_deref(),
        Some("Looked at")
    );
    assert!(
        loaded.contains(&twin),
        "the session id of a duplicated /NM is stable"
    );
    assert!(!loaded.is_dirty());
}

#[test]
fn a_set_from_another_document_state_is_refused() {
    let mut work = Work::new(&pl_testkit::plain());
    let og = common::origin();
    let mut set = AnnotSet::new();
    let id = set
        .add(Annotation::square(
            0,
            Rect::new(100.0, 100.0, 200.0, 200.0),
            &og,
        ))
        .unwrap();
    work.save(&set);
    let first = work.doc().clone();
    let stale = AnnotSet::load(&first);

    let mut current = stale.clone();
    current.get_mut(&id).unwrap().meta.contents = Some("one".into());
    work.save(&current);
    let second = work.doc().clone();

    // The old set no longer describes the file: its baseline for this annotation is outdated.
    let mut late = stale.clone();
    late.get_mut(&id).unwrap().meta.contents = Some("two".into());
    assert!(matches!(late.plan_save(&second), Err(AnnotError::Stale(s)) if s == id));
    // Rebased, it is clean and holds what the file holds.
    late.rebase(&second);
    assert!(!late.is_dirty());
    assert_eq!(late.get(&id).unwrap().meta.contents.as_deref(), Some("one"));

    // A new annotation with an id that is already taken is refused too.
    let mut clash = late.clone();
    let mut twin = Annotation::circle(0, Rect::new(0.0, 0.0, 10.0, 10.0), &og);
    twin.id = id.clone();
    assert!(matches!(clash.add(twin), Err(AnnotError::DuplicateId(_))));
    // A coordinate that is not a number is refused at planning time.
    let mut broken = Annotation::line(0, Point::new(0.0, 0.0), Point::new(f64::NAN, 1.0), &og);
    broken.style.width = 2.0;
    let broken_id = clash.add(broken).unwrap();
    assert!(matches!(clash.plan_save(&second), Err(AnnotError::NotFinite(b)) if b == broken_id));
}

#[test]
fn private_data_and_non_latin_text_round_trip() {
    let mut work = Work::new(&pl_testkit::plain());
    let og = pl_annot::Origin::new(
        "\u{5efa}\u{7bc9} Ren\u{e9}e",
        PdfDate::parse("D:20261124101500+09'00'").unwrap(),
    );
    let mut a = Annotation::polygon(
        0,
        vec![
            Point::new(100.0, 100.0),
            Point::new(220.0, 100.0),
            Point::new(160.0, 190.0),
        ],
        &og,
    );
    a.meta.contents = Some("Fl\u{e4}che \u{2248} 5,4 m\u{b2}\nzweite Zeile".into());
    a.meta.subject = Some("Area (\u{9762}\u{7a4d})".into());
    let mut data = Dictionary::new();
    data.set("V", Object::Integer(1));
    data.set("Scale", Object::Real(0.25));
    data.set("Unit", text("m"));
    a.app_data = Some(data.clone());
    let mut set = AnnotSet::new();
    let id = set.add(a.clone()).unwrap();
    let (plan, _) = work.save(&set);
    assert!(plan.added[0].keys.iter().any(|k| k == PRIVATE_KEY));

    let doc = work.doc().clone();
    let loaded = AnnotSet::load(&doc);
    let got = loaded.get(&id).unwrap();
    assert_eq!(got.detached(), a.detached());
    assert_eq!(
        got.meta.author.as_deref(),
        Some("\u{5efa}\u{7bc9} Ren\u{e9}e")
    );
    let dict = got.original_dict().unwrap();
    assert_eq!(
        dict.get(PRIVATE_KEY.as_bytes()).unwrap(),
        &Object::Dictionary(data)
    );
    // The private key is the only non-standard key in the dictionary.
    let standard = [
        "Type",
        "Subtype",
        "Rect",
        "NM",
        "P",
        "F",
        "M",
        "CreationDate",
        "T",
        "Subj",
        "Contents",
        "C",
        "CA",
        "BS",
        "Vertices",
        "AP",
    ];
    for (key, _) in dict.iter() {
        let key = String::from_utf8_lossy(key);
        assert!(
            standard.contains(&key.as_ref()) || key == PRIVATE_KEY,
            "unexpected /{key}"
        );
    }

    // Changing the private data alone rewrites that key alone.
    let mut edited = loaded.clone();
    edited.get_mut(&id).unwrap().app_data = None;
    let (plan, _) = work.save(&edited);
    assert_eq!(plan.modified[0].keys, [PRIVATE_KEY]);
    let doc = work.doc().clone();
    assert_eq!(AnnotSet::load(&doc).get(&id).unwrap().app_data, None);
}

/// Files with a cross-reference stream and object streams, and encrypted files: the model
/// goes through pl-pdf's save and comes back equal.
#[test]
fn other_file_structures_round_trip() {
    type Make = fn() -> pl_testkit::Fixture;
    let fixtures: [(&str, Make); 5] = [
        ("xref_classic", pl_testkit::xref_classic),
        ("xref_object_streams", pl_testkit::xref_object_streams),
        ("encrypted_rc4", pl_testkit::encrypted_rc4),
        ("encrypted_aes256", pl_testkit::encrypted_aes256),
        ("layers", pl_testkit::layers),
    ];
    for (label, make) in fixtures {
        let fixture = make();
        let mut work = Work::new(&fixture);
        let o = work.sample_origin(0);
        let before = work.doc().clone();
        let mut set = AnnotSet::load(&before);
        let had = set.len();
        let added = common::one_of_each(0, o);
        for a in &added {
            set.add(a.clone()).unwrap();
        }
        // On the layers fixture, put one markup on a layer.
        let layer = before.layers().first().map(|l| l.id);
        if let Some(layer) = layer {
            let first = added[0].id.clone();
            set.get_mut(&first).unwrap().meta.layer = Some(layer);
        }
        let before_save = work.bytes();
        let plan = set.plan_save(work.doc()).unwrap();
        let report = work
            .file
            .save_incremental(&plan.edits, &SaveOptions::default())
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(report.verification.is_some(), "{label}");
        assert!(
            work.bytes().starts_with(&before_save),
            "{label}: exact prefix"
        );

        let after = work.doc().clone();
        let loaded = AnnotSet::load(&after);
        assert_eq!(loaded.len(), had + added.len(), "{label}");
        assert!(!loaded.is_dirty(), "{label}");
        for a in &added {
            let got = loaded
                .get(&a.id)
                .unwrap_or_else(|| panic!("{label}: missing"));
            let mut want = a.detached();
            if a.id == added[0].id {
                want.meta.layer = layer;
                assert_eq!(got.original_dict().unwrap().has(b"OC"), layer.is_some());
            }
            assert_eq!(got.detached(), want, "{label}");
        }
    }
}
