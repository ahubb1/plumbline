//! Cases found in review: foreign markups whose dictionaries say less, or more, than the model
//! holds, link targets that are cleared, and one annotation object listed twice.

mod common;

use common::Work;
use pl_annot::{AnnotId, AnnotKind, AnnotSet, Annotation, Geometry, LinkTarget, Rgb};
use pl_geom::{Point, Rect, Vec2};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, SaveOptions};

fn id(name: &str) -> AnnotId {
    AnnotId::from(name)
}

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn dict_of(doc: &PdfDocument, id: ObjectId) -> &Dictionary {
    doc.object(id).unwrap().as_dict().unwrap()
}

/// The annotations fixture after `change` has patched the annotation named `which`.
fn patched(which: &str, change: impl Fn(&mut pl_pdf::DictPatch)) -> Work {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let set = AnnotSet::load(&doc);
    let object = set.get(&id(which)).unwrap().object_id().unwrap();
    let mut edits = doc.edit();
    change(edits.patch(object));
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    work.original = work.bytes();
    work
}

fn appearance_stream(doc: &PdfDocument, annotation: ObjectId) -> (ObjectId, Vec<u8>) {
    let ap = dict_of(doc, annotation)
        .get(b"AP")
        .unwrap()
        .as_dict()
        .unwrap();
    let Object::Reference(n) = ap.get(b"N").unwrap() else {
        panic!("/AP /N is a reference");
    };
    let Some(Object::Stream(stream)) = doc.object(*n) else {
        panic!("/AP /N is a stream");
    };
    (*n, stream.content.clone())
}

#[test]
fn a_foreign_square_without_a_colour_key_stays_visible_when_it_is_moved() {
    let backend = common::backend();
    let mut work = patched("fixture-square-1", |p| {
        p.remove("C");
    });
    let geometry = work.geometry(0);
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let square_id = id("fixture-square-1");
    {
        let square = set.get(&square_id).unwrap();
        assert!(!square.original_dict().unwrap().has(b"C"));
        assert_eq!(
            square.style.stroke,
            Some(Rgb::BLACK),
            "no /C is the default colour, not transparency"
        );
        assert!(!square.draw_list().is_empty());
        assert!(!set.is_dirty());
    }
    let square = set.get_mut(&square_id).unwrap();
    square.translate(Vec2::new(5.0, 5.0));
    let object = square.object_id().unwrap();
    let frame = square.geometry.bounds();
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert!(plan.modified[0].keys.contains(&"AP".to_string()));
    assert!(
        !plan.modified[0].keys.contains(&"C".to_string()),
        "the colour did not change, so /C is not written"
    );

    let after = work.doc().clone();
    let (_, content) = appearance_stream(&after, object);
    assert!(
        !content.is_empty(),
        "the regenerated appearance paints something"
    );
    // On the page: a dark border where the square now is.
    let render = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    let scale = common::scale_for(&geometry);
    let on_border = (
        ((frame.x0 + 0.5) * scale) as u32,
        ((792.0 - frame.center().y) * scale) as u32,
    );
    let px = render.pdfium.pixel(on_border.0, on_border.1);
    assert!(
        px[0] < 90 && px[1] < 90 && px[2] < 90,
        "the border is black: {px:?}"
    );
    let loaded = AnnotSet::load(&after);
    assert_eq!(
        loaded.get(&square_id).unwrap().style.stroke,
        Some(Rgb::BLACK)
    );
    assert!(!loaded.is_dirty());
}

#[test]
fn a_missing_colour_reads_as_the_default_of_each_kind_and_an_empty_one_as_none() {
    for (which, want) in [
        ("fixture-line-1", Some(Rgb::BLACK)),
        ("fixture-highlight-1", Some(Rgb::new(1.0, 0.9, 0.2))),
    ] {
        let mut work = patched(which, |p| {
            p.remove("C");
        });
        let set = AnnotSet::load(work.doc());
        assert_eq!(set.get(&id(which)).unwrap().style.stroke, want, "{which}");
        let mut work = patched(which, |p| {
            p.set("C", Object::Array(Vec::new()));
        });
        let set = AnnotSet::load(work.doc());
        assert_eq!(set.get(&id(which)).unwrap().style.stroke, None, "{which}");
    }
}

#[test]
fn a_markup_that_paints_nothing_by_its_keys_keeps_its_own_appearance_when_moved() {
    // /C [] with an appearance that nevertheless draws: the model has nothing to paint, so
    // the author's stream must stay.
    let mut work = patched("fixture-square-1", |p| {
        p.set("C", Object::Array(Vec::new()));
    });
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let square_id = id("fixture-square-1");
    let square = set.get_mut(&square_id).unwrap();
    assert_eq!(square.style.stroke, None);
    assert!(square.draw_list().is_empty());
    let object = square.object_id().unwrap();
    let old_rect =
        pl_pdf::rect_from_object(dict_of(&before, object).get(b"Rect").unwrap()).unwrap();
    let (old_ap, old_content) = appearance_stream(&before, object);
    assert!(!old_content.is_empty());

    square.translate(Vec2::new(5.0, 7.0));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["Rect"]);
    assert_eq!(plan.modified[0].appearance, None);
    let after = work.doc().clone();
    let (new_ap, new_content) = appearance_stream(&after, object);
    assert_eq!(new_ap, old_ap, "the same appearance stream");
    assert_eq!(new_content, old_content);
    let new_rect = pl_pdf::rect_from_object(dict_of(&after, object).get(b"Rect").unwrap()).unwrap();
    assert_eq!(
        new_rect,
        Rect::new(
            old_rect.x0 + 5.0,
            old_rect.y0 + 7.0,
            old_rect.x1 + 5.0,
            old_rect.y1 + 7.0
        )
    );
    let loaded = AnnotSet::load(&after);
    assert!(!loaded.is_dirty());
    assert_eq!(
        loaded.get(&square_id).unwrap().geometry,
        set.get(&square_id).unwrap().geometry
    );
}

#[test]
fn a_colourless_shape_of_ours_round_trips_as_colourless() {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let mut set = AnnotSet::load(work.doc());
    let mut a = Annotation::square(0, Rect::new(300.0, 500.0, 360.0, 560.0), &common::origin());
    a.style.stroke = None;
    a.style.fill = Some(Rgb::BLUE);
    let new_id = set.add(a.clone()).unwrap();
    work.save(&set);
    let doc = work.doc().clone();
    let loaded = AnnotSet::load(&doc);
    let back = loaded.get(&new_id).unwrap();
    assert_eq!(back.style.stroke, None, "no border was asked for");
    assert_eq!(back.detached(), a);
    assert_eq!(
        back.original_dict().unwrap().get(b"C").unwrap(),
        &Object::Array(Vec::new()),
        "an empty /C says transparent; a missing one would not"
    );
    assert!(!loaded.is_dirty());
}

/// What an Acrobat-style dimension line carries besides /L.
fn dimension(p: &mut pl_pdf::DictPatch) {
    p.set("LL", Object::Integer(18));
    p.set("LLE", Object::Integer(6));
    p.set("Cap", Object::Boolean(true));
    p.set("CP", name("Top"));
    p.set("IT", name("LineDimension"));
    p.set(
        "LE",
        Object::Array(vec![name("OpenArrow"), name("OpenArrow")]),
    );
}

#[test]
fn a_dimension_line_keeps_its_own_appearance() {
    let mut work = patched("fixture-line-1", dimension);
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let line_id = id("fixture-line-1");
    let line = set.get_mut(&line_id).unwrap();
    assert_eq!(line.kind, AnnotKind::Line);
    assert!(
        !line.is_modelled(),
        "leader lines and a caption are beyond the model"
    );
    assert!(line.draw_list().is_empty());
    let object = line.object_id().unwrap();
    let old = dict_of(&before, object).clone();
    let old_l = pl_pdf::rect_from_object(old.get(b"L").unwrap()).unwrap();

    // A restyle is ignored.
    line.style.stroke = Some(Rgb::BLUE);
    line.style.width = 7.0;
    assert!(!set.is_dirty());

    // A move carries /Rect and /L along and leaves the appearance alone.
    let line = set.get_mut(&line_id).unwrap();
    line.translate(Vec2::new(12.0, -8.0));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["L", "Rect"]);
    assert_eq!(plan.modified[0].appearance, None);
    let after = work.doc().clone();
    let new = dict_of(&after, object);
    assert_eq!(new.get(b"AP").unwrap(), old.get(b"AP").unwrap());
    for key in ["LL", "LLE", "Cap", "CP", "IT", "LE", "C"] {
        assert_eq!(
            new.get(key.as_bytes()).unwrap(),
            old.get(key.as_bytes()).unwrap(),
            "/{key}"
        );
    }
    let new_l = pl_pdf::rect_from_object(new.get(b"L").unwrap()).unwrap();
    assert!((new_l.x0 - old_l.x0 - 12.0).abs() < 1e-3 && (new_l.y1 - old_l.y1 + 8.0).abs() < 1e-3);
}

#[test]
fn lines_with_endings_or_measurements_the_model_lacks_are_not_regenerated() {
    type Change = fn(&mut pl_pdf::DictPatch);
    let beyond: [(&str, Change); 4] = [
        ("a reversed arrow", |p| {
            p.set("LE", Object::Array(vec![name("ROpenArrow"), name("None")]));
        }),
        ("a reversed closed arrow at the end", |p| {
            p.set(
                "LE",
                Object::Array(vec![name("None"), name("RClosedArrow")]),
            );
        }),
        ("a measurement", |p| {
            p.set("Measure", Object::Dictionary(Dictionary::new()));
        }),
        ("a leader line", |p| {
            p.set("LL", Object::Real(-9.0));
        }),
    ];
    for (what, change) in beyond {
        let mut work = patched("fixture-line-1", change);
        let set = AnnotSet::load(work.doc());
        assert!(
            !set.get(&id("fixture-line-1")).unwrap().is_modelled(),
            "{what}"
        );
    }
    // What the model does hold stays editable: plain endings, a zero leader, no caption.
    let mut work = patched("fixture-line-1", |p| {
        p.set(
            "LE",
            Object::Array(vec![name("OpenArrow"), name("Diamond")]),
        );
        p.set("LL", Object::Integer(0));
        p.set("Cap", Object::Boolean(false));
        p.set("IT", name("LineArrow"));
    });
    let set = AnnotSet::load(work.doc());
    let line = set.get(&id("fixture-line-1")).unwrap();
    assert!(line.is_modelled());
    assert!(matches!(line.geometry, Geometry::Line { .. }));
}

#[test]
fn dimension_polygons_and_polylines_are_not_regenerated() {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let mut set = AnnotSet::load(work.doc());
    let points = vec![
        Point::new(300.0, 500.0),
        Point::new(380.0, 500.0),
        Point::new(340.0, 560.0),
    ];
    let polygon = set
        .add(Annotation::polygon(0, points.clone(), &common::origin()))
        .unwrap();
    let polyline = set
        .add(Annotation::polyline(0, points, &common::origin()))
        .unwrap();
    work.save(&set);
    let doc = work.doc().clone();
    let loaded = AnnotSet::load(&doc);
    assert!(loaded.get(&polygon).unwrap().is_modelled());
    assert!(loaded.get(&polyline).unwrap().is_modelled());
    let mut edits = doc.edit();
    edits
        .patch(loaded.get(&polygon).unwrap().object_id().unwrap())
        .set("IT", name("PolygonDimension"));
    edits
        .patch(loaded.get(&polyline).unwrap().object_id().unwrap())
        .set("IT", name("PolyLineDimension"));
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let doc = work.doc().clone();
    let mut set = AnnotSet::load(&doc);
    for which in [&polygon, &polyline] {
        let a = set.get_mut(which).unwrap();
        assert!(!a.is_modelled());
        let object = a.object_id().unwrap();
        a.translate(Vec2::new(10.0, 10.0));
        let plan = set.plan_save(&doc).unwrap();
        let change = plan.modified.iter().find(|c| c.object == object).unwrap();
        assert_eq!(change.keys, ["Rect", "Vertices"]);
        assert_eq!(change.appearance, None);
    }
}

#[test]
fn clearing_a_link_target_is_saved() {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let mut set = AnnotSet::load(work.doc());
    let rect = Rect::new(300.0, 600.0, 400.0, 620.0);
    let web = set
        .add(Annotation::link(
            0,
            rect,
            LinkTarget::Uri("https://example.org/".into()),
            &common::origin(),
        ))
        .unwrap();
    let page = set
        .add(Annotation::link(
            0,
            rect,
            LinkTarget::Page { page: 0, at: None },
            &common::origin(),
        ))
        .unwrap();
    work.save(&set);
    let doc = work.doc().clone();
    let mut set = AnnotSet::load(&doc);
    assert!(set.get(&web).unwrap().original_dict().unwrap().has(b"A"));
    assert!(set
        .get(&page)
        .unwrap()
        .original_dict()
        .unwrap()
        .has(b"Dest"));

    // A target this crate cannot write is never written over the file's.
    set.get_mut(&web).unwrap().link = Some(LinkTarget::Other);
    assert!(!set.is_dirty());

    set.get_mut(&web).unwrap().link = None;
    set.get_mut(&page).unwrap().link = None;
    assert!(set.is_dirty(), "a cleared target is a change");
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 2);
    let doc = work.doc().clone();
    let loaded = AnnotSet::load(&doc);
    for which in [&web, &page] {
        let a = loaded.get(which).unwrap();
        assert_eq!(a.link, None);
        let dict = a.original_dict().unwrap();
        assert!(!dict.has(b"A") && !dict.has(b"Dest"));
    }
    assert!(!loaded.is_dirty());

    // And a page target can become a web address: /Dest goes, /A comes.
    let mut set = loaded;
    set.get_mut(&page).unwrap().link = Some(LinkTarget::Uri("https://example.org/b".into()));
    work.save(&set);
    let doc = work.doc().clone();
    let loaded = AnnotSet::load(&doc);
    assert_eq!(
        loaded.get(&page).unwrap().link,
        Some(LinkTarget::Uri("https://example.org/b".into()))
    );
}

#[test]
fn deleting_one_of_two_listings_of_the_same_object_removes_that_listing() {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let set = AnnotSet::load(&doc);
    let count = set.len();
    let object = set
        .get(&id("fixture-square-1"))
        .unwrap()
        .object_id()
        .unwrap();
    // The same annotation object listed a second time, as page-duplicating tools leave it.
    let mut edits = doc.edit();
    edits.attach_annotation(0, object);
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    work.original = work.bytes();

    let doc = work.doc().clone();
    let mut set = AnnotSet::load(&doc);
    assert_eq!(set.len(), count + 1);
    let twins: Vec<AnnotId> = set
        .iter()
        .filter(|a| a.object_id() == Some(object))
        .map(|a| a.id.clone())
        .collect();
    assert_eq!(twins.len(), 2);
    assert_ne!(twins[0], twins[1]);

    let removed = set.remove(&twins[1]);
    assert_eq!(removed.items.len(), 1);
    assert!(set.is_dirty());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.deleted.len(), 1, "the listing is taken out");
    assert_eq!(plan.deleted[0].object, object);
    let doc = work.doc().clone();
    assert_eq!(doc.annotation_counts(), [count]);
    let mut set = AnnotSet::load(&doc);
    assert_eq!(set.len(), count);
    assert!(set.contains(&twins[0]));

    // Removing the last listing removes the annotation from the page.
    set.remove(&twins[0]);
    let (plan, _) = work.save(&set);
    assert_eq!(plan.deleted.len(), 1);
    assert_eq!(work.doc().annotation_counts(), [count - 1]);
}
