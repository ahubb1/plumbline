//! Markups written by another application (the pl-testkit `annotations` fixture, with vendor
//! keys planted in it): they are read into the model, keep every key they came with, are never
//! rewritten by a save that does not concern them, and look the same before and after it.

mod common;

use common::Work;
use pl_annot::{AnnotId, AnnotKind, AnnotSet, Annotation, Geometry, PdfDate, Rgb, PRIVATE_KEY};
use pl_geom::{Point, Rect, Vec2};
use pl_pdf::{Dictionary, Object, ObjectId, SaveOptions, StringFormat};
use pl_render::Bitmap;
use pl_testkit::{FOREIGN_AUTHOR, FOREIGN_DATE};

fn text(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

/// The fixture with keys this crate knows nothing about added to every annotation, the way
/// vendor tools decorate theirs.
fn foreign_work() -> Work {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let mut edits = doc.edit();
    for entry in doc.annotations(0).unwrap() {
        let id = entry.id.unwrap();
        let mut nested = Dictionary::new();
        nested.set(
            "Columns",
            Object::Array(vec![text("Bay 3"), text("Level 2")]),
        );
        nested.set("Depth", Object::Real(2.5));
        let patch = edits.patch(id);
        patch.set("VNDRColumnData", Object::Dictionary(nested));
        patch.set("VNDRLayer", text("Structural"));
        patch.set("IT", Object::Name(b"VendorIntent".to_vec()));
        patch.set("Rotation", Object::Integer(0));
    }
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    // From here on this is "the original file" as far as the tests are concerned.
    work.original = work.bytes();
    work
}

const VENDOR_KEYS: [&str; 4] = ["VNDRColumnData", "VNDRLayer", "IT", "Rotation"];

fn id(name: &str) -> AnnotId {
    AnnotId::from(name)
}

/// True when the two bitmaps are identical outside `hole` (pixel coordinates).
fn same_outside(a: &Bitmap, b: &Bitmap, hole: Option<[u32; 4]>) -> bool {
    assert_eq!((a.width, a.height), (b.width, b.height));
    for y in 0..a.height {
        for x in 0..a.width {
            if hole.is_some_and(|h| x >= h[0] && x < h[2] && y >= h[1] && y < h[3]) {
                continue;
            }
            if a.pixel(x, y) != b.pixel(x, y) {
                return false;
            }
        }
    }
    true
}

/// A page-space rectangle of the (unrotated Letter) fixture page as pixel bounds, grown by a
/// margin.
fn px(rect: Rect, scale: f64, margin: f64) -> [u32; 4] {
    let r = rect.inflate(margin, margin);
    [
        (r.x0 * scale).floor().max(0.0) as u32,
        ((792.0 - r.y1) * scale).floor().max(0.0) as u32,
        (r.x1 * scale).ceil() as u32,
        ((792.0 - r.y0) * scale).ceil() as u32,
    ]
}

#[test]
fn foreign_markups_are_read_into_the_model() {
    let fixture = pl_testkit::annotations();
    let mut work = foreign_work();
    let set = AnnotSet::load(work.doc());
    let truths = &fixture.manifest.pages[0].annotations;
    assert_eq!(set.len(), truths.len());
    assert!(!set.is_dirty());
    assert!(set.plan_save(work.doc()).unwrap().is_empty());

    for (a, truth) in set.iter().zip(truths) {
        assert_eq!(a.id.as_str(), truth.name);
        assert_eq!(a.page, 0);
        assert_eq!(a.kind, AnnotKind::from_pdf_name(truth.subtype));
        assert_eq!(a.meta.author.as_deref(), Some(FOREIGN_AUTHOR));
        assert_eq!(a.meta.contents.as_deref(), Some(truth.contents.as_str()));
        assert_eq!(a.meta.subject.as_deref(), Some(truth.subtype));
        assert_eq!(a.meta.modified, PdfDate::parse(FOREIGN_DATE));
        assert_eq!(a.meta.created, PdfDate::parse(FOREIGN_DATE));
        assert!(a.meta.flags.has(pl_annot::Flags::PRINT));
        let [r, g, b] = truth.rgb;
        assert_eq!(a.style.stroke, Some(Rgb::new(r as f32, g as f32, b as f32)));
        assert_eq!(a.style.opacity, 1.0);
        let base = a.baseline().unwrap();
        assert!(base.has_appearance);
        assert_eq!(base.slot, set.position(&a.id).unwrap());
        // The original dictionary is kept whole, vendor keys included.
        let dict = a.original_dict().unwrap();
        for key in VENDOR_KEYS {
            assert!(dict.has(key.as_bytes()), "{}: /{key}", truth.name);
        }
        assert!(dict.has(b"AP") && dict.has(b"P"));
        assert!(a.app_data.is_none());
        assert!(!dict.has(PRIVATE_KEY.as_bytes()));
    }

    let square = set.get(&id("fixture-square-1")).unwrap();
    assert_eq!(
        square.geometry,
        Geometry::Rect(Rect::new(100.0, 400.0, 220.0, 480.0))
    );
    assert_eq!(square.style.width, 2.0);
    assert!(square.is_modelled());

    let line = set.get(&id("fixture-line-1")).unwrap();
    assert_eq!(
        line.geometry,
        Geometry::Line {
            a: Point::new(300.0, 420.0),
            b: Point::new(500.0, 470.0)
        }
    );
    assert_eq!(line.style.width, 3.0);

    let free = set.get(&id("fixture-freetext-1")).unwrap();
    assert_eq!(free.kind, AnnotKind::FreeText);
    assert!(!free.is_modelled(), "FreeText is read-only for now");
    assert_eq!(
        free.geometry,
        Geometry::Rect(Rect::new(100.0, 250.0, 340.0, 300.0))
    );
    assert!(
        free.draw_list().is_empty(),
        "it is shown from its own appearance"
    );
    assert!(free.handles().is_empty());
    assert!(free.original_dict().unwrap().has(b"DA"));

    let highlight = set.get(&id("fixture-highlight-1")).unwrap();
    let Geometry::Quads(quads) = &highlight.geometry else {
        panic!("a highlight has quads");
    };
    assert_eq!(quads.len(), 1);
    assert_eq!(
        quads[0][0],
        Point::new(72.0, 596.0),
        "ring starts at the bottom-left"
    );
    assert_eq!(quads[0][3], Point::new(72.0, 612.0));
    assert_eq!(highlight.style.blend, pl_annot::BlendMode::Multiply);

    // Hit-testing works on what was read.
    assert_eq!(
        set.hit_test(0, Point::new(400.0, 445.0), 2.0).unwrap().id,
        line.id
    );
    assert_eq!(
        set.hit_test(0, Point::new(200.0, 275.0), 0.0).unwrap().id,
        free.id
    );
    assert_eq!(
        set.hit_test(0, Point::new(101.0, 440.0), 1.0).unwrap().id,
        square.id
    );
    assert!(
        set.hit_test(0, Point::new(160.0, 440.0), 1.0).is_none(),
        "the square is hollow"
    );
}

#[test]
fn an_unrelated_save_leaves_foreign_markups_untouched_and_looking_the_same() {
    let backend = common::backend();
    let mut work = foreign_work();
    let geometry = work.geometry(0);
    let before = work.doc().clone();
    let page_id = before.page_id(0).unwrap();
    let render_before = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    assert!(common::changed_pixels(&render_before.backdrop, &render_before.pdfium, 12) > 5000);

    // Save 1: one foreign markup gets a new comment. Nothing about any appearance changes.
    let mut set = AnnotSet::load(&before);
    let line_id = id("fixture-line-1");
    set.get_mut(&line_id).unwrap().meta.contents = Some("Reviewed \u{2713} by Plumbline".into());
    let (plan, _) = work.save(&set);
    assert!(plan.added.is_empty() && plan.deleted.is_empty());
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["Contents"]);
    assert_eq!(plan.modified[0].appearance, None);
    let line_object = plan.modified[0].object;

    let mid = work.doc().clone();
    common::assert_untouched(&before, &mid, &[line_object]);
    // The rewritten dictionary differs from the original in that one key only.
    let (Some(Object::Dictionary(old)), Some(Object::Dictionary(new))) =
        (before.object(line_object), mid.object(line_object))
    else {
        panic!("annotation dictionaries");
    };
    assert_eq!(old.len(), new.len());
    for (key, value) in old.iter() {
        if key.as_slice() != b"Contents" {
            assert_eq!(
                new.get(key).unwrap(),
                value,
                "/{}",
                String::from_utf8_lossy(key)
            );
        }
    }
    let render_mid = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    assert!(
        same_outside(&render_before.pdfium, &render_mid.pdfium, None),
        "the page renders identically after a save that changed no appearance"
    );

    // Save 2: a new markup of ours in an empty corner of the page.
    let mut set = AnnotSet::load(&mid);
    assert_eq!(
        set.get(&line_id).unwrap().meta.contents.as_deref(),
        Some("Reviewed \u{2713} by Plumbline")
    );
    let corner = Rect::new(420.0, 120.0, 520.0, 200.0);
    let mut ours = Annotation::circle(0, corner, &common::origin());
    ours.style.fill = Some(Rgb::new(0.5, 0.75, 1.0));
    let ours_id = set.add(ours).unwrap();
    let (plan, _) = work.save(&set);
    assert_eq!(
        (plan.added.len(), plan.modified.len(), plan.deleted.len()),
        (1, 0, 0)
    );

    let after = work.doc().clone();
    // Only the page (its /Annots array is written in it) was rewritten.
    common::assert_untouched(&mid, &after, &[page_id]);
    let render_after = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    let hole = px(corner, common::scale_for(&geometry), 2.0);
    assert!(
        same_outside(&render_mid.pdfium, &render_after.pdfium, Some(hole)),
        "everything outside the new markup renders identically"
    );
    assert!(
        !same_outside(&render_mid.pdfium, &render_after.pdfium, None),
        "the new markup shows"
    );

    // Every foreign annotation still has every key it came with, and reads as the same model.
    let first = AnnotSet::load(&mid);
    let last = AnnotSet::load(&after);
    assert_eq!(last.len(), 5);
    for a in first.iter() {
        let again = last.get(&a.id).unwrap();
        assert_eq!(again, a, "{}: equal model, equal dictionary", a.id);
        for key in VENDOR_KEYS {
            assert!(again.original_dict().unwrap().has(key.as_bytes()));
        }
    }
    assert!(last.contains(&ours_id));
    assert_eq!(
        common::pdfium_annotation_counts(&backend, pl_render::DocSource::Bytes(work.bytes()), &[0]),
        [5]
    );
}

#[test]
fn restyling_a_foreign_markup_rewrites_only_the_dirty_keys() {
    let backend = common::backend();
    let mut work = foreign_work();
    let geometry = work.geometry(0);
    let before = work.doc().clone();
    let render_before = common::render_pdf(&backend, work.bytes(), 0, &geometry);

    let mut set = AnnotSet::load(&before);
    let square_id = id("fixture-square-1");
    let square = set.get_mut(&square_id).unwrap();
    square.style.stroke = Some(Rgb::new(0.0, 0.5, 0.0));
    let expected = square.detached();
    let object = square.object_id().unwrap();
    let frame = square.geometry.bounds();
    assert!(square.is_dirty());

    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(
        plan.modified[0].keys,
        ["AP", "C"],
        "colour, and the appearance made from it"
    );
    let new_ap = plan.modified[0]
        .appearance
        .expect("a regenerated appearance");

    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[object]);
    let (Some(Object::Dictionary(old)), Some(Object::Dictionary(new))) =
        (before.object(object), after.object(object))
    else {
        panic!("annotation dictionaries");
    };
    assert_eq!(old.len(), new.len(), "no key added or removed");
    for (key, value) in old.iter() {
        match key.as_slice() {
            b"C" => assert_eq!(
                new.get(key).unwrap(),
                &Object::Array(vec![
                    Object::Integer(0),
                    Object::Real(0.5),
                    Object::Integer(0)
                ])
            ),
            b"AP" => {
                let ap = new.get(key).unwrap().as_dict().unwrap();
                assert_eq!(ap.get(b"N").unwrap(), &Object::Reference(new_ap));
                assert_ne!(new.get(key).unwrap(), value);
            }
            _ => assert_eq!(
                new.get(key).unwrap(),
                value,
                "/{}",
                String::from_utf8_lossy(key)
            ),
        }
    }
    for key in VENDOR_KEYS {
        assert!(new.has(key.as_bytes()));
    }

    // The model reads back as edited; nothing else became dirty.
    let loaded = AnnotSet::load(&after);
    assert_eq!(loaded.get(&square_id).unwrap().detached(), expected);
    assert!(!loaded.is_dirty());

    // On the page: the square is now green, everything else is pixel-identical.
    let render_after = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    let scale = common::scale_for(&geometry);
    let hole = px(frame, scale, 2.0);
    assert!(same_outside(
        &render_before.pdfium,
        &render_after.pdfium,
        Some(hole)
    ));
    let on_border = (
        ((frame.x0 + 1.0) * scale) as u32,
        ((792.0 - 440.0) * scale) as u32,
    );
    let was = render_before.pdfium.pixel(on_border.0, on_border.1);
    let is = render_after.pdfium.pixel(on_border.0, on_border.1);
    assert!(was[0] > 200 && was[1] < 60, "the border was red: {was:?}");
    assert!(
        is[0] < 60 && is[1] > 100 && is[2] < 60,
        "the border is green: {is:?}"
    );
    // And it is drawn from our draw list now.
    let ours = common::raster_over(
        &loaded.get(&square_id).unwrap().draw_list(),
        &geometry,
        &render_after.backdrop,
    );
    let crop = |b: &Bitmap| {
        let mut out = Bitmap::filled(hole[2] - hole[0], hole[3] - hole[1], [0, 0, 0, 255]);
        for y in hole[1]..hole[3] {
            for x in hole[0]..hole[2] {
                let i = ((y - hole[1]) * out.width + (x - hole[0])) as usize * 4;
                out.pixels[i..i + 4].copy_from_slice(&b.pixel(x, y));
            }
        }
        out
    };
    let d = common::diff(&crop(&ours), &crop(&render_after.pdfium), 1, 40);
    assert_eq!(d.bad, 0, "{d:?}");
}

#[test]
fn an_unmodelled_markup_can_be_moved_described_and_deleted_but_not_restyled() {
    let backend = common::backend();
    let mut work = foreign_work();
    let geometry = work.geometry(0);
    let before = work.doc().clone();
    let free_id = id("fixture-freetext-1");

    // Restyling is not possible: the change is ignored, nothing is written.
    let mut set = AnnotSet::load(&before);
    set.get_mut(&free_id).unwrap().style.stroke = Some(Rgb::BLUE);
    set.get_mut(&free_id).unwrap().style.width = 9.0;
    assert!(!set.is_dirty());
    assert!(set.plan_save(&before).unwrap().is_empty());

    // Moving it rewrites /Rect only; the appearance stream is the author's own, untouched.
    let by = Vec2::new(40.0, -60.0);
    let free = set.get_mut(&free_id).unwrap();
    free.translate(by);
    free.meta.contents = Some("Moved".into());
    free.touch(PdfDate::parse("D:20261124120000+01'00'").unwrap());
    let object = free.object_id().unwrap();
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["Contents", "M", "Rect"]);
    assert_eq!(plan.modified[0].appearance, None);

    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[object]);
    let dict = after.object(object).unwrap().as_dict().unwrap();
    let old = before.object(object).unwrap().as_dict().unwrap();
    assert_eq!(dict.get(b"AP").unwrap(), old.get(b"AP").unwrap());
    assert_eq!(dict.get(b"DA").unwrap(), old.get(b"DA").unwrap());
    assert_eq!(dict.get(b"NM").unwrap(), old.get(b"NM").unwrap());
    assert_eq!(
        pl_pdf::rect_from_object(dict.get(b"Rect").unwrap()),
        Some(Rect::new(140.0, 190.0, 380.0, 240.0))
    );
    assert_eq!(dict.get(b"M").unwrap(), &text("D:20261124120000+01'00'"));

    // PDFium shows the same picture, shifted.
    let old_render = common::render_pdf(&backend, before.bytes().to_vec(), 0, &geometry);
    let new_render = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    let scale = common::scale_for(&geometry);
    let (dx, dy) = ((by.x * scale) as i64, (-by.y * scale) as i64);
    let from = px(Rect::new(100.0, 250.0, 340.0, 300.0), scale, 0.0);
    let mut differing = 0;
    let mut inked = 0;
    for y in from[1]..from[3] {
        for x in from[0]..from[2] {
            let a = old_render.pdfium.pixel(x, y);
            let b = new_render
                .pdfium
                .pixel((x as i64 + dx) as u32, (y as i64 + dy) as u32);
            if a != b {
                differing += 1;
            }
            if a[0] < 128 {
                inked += 1;
            }
        }
    }
    assert!(inked > 500, "the FreeText has text and a border ({inked})");
    assert_eq!(differing, 0, "the moved appearance is the same picture");

    // Deleting it takes it off the page.
    let mut set = AnnotSet::load(&after);
    let removed = set.remove(&free_id);
    assert_eq!(removed.items.len(), 1);
    let (plan, _) = work.save(&set);
    assert_eq!(plan.deleted.len(), 1);
    assert_eq!(plan.deleted[0].object, object);
    let last = work.doc().clone();
    assert_eq!(last.annotation_counts(), [3]);
    let set = AnnotSet::load(&last);
    assert!(!set.contains(&free_id));
    assert_eq!(set.len(), 3);
    let ids: Vec<ObjectId> = set.iter().filter_map(Annotation::object_id).collect();
    assert!(!ids.contains(&object));
}
