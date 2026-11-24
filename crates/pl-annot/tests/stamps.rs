//! Stamps: Plumbline's own (a label in a frame) and pictures, on a plain page and on rotated,
//! `/UserUnit` and offset-box pages. Each round-trips to an equal model and PDFium draws its
//! saved appearance like the tiny-skia raster of the draw list. A picture that is moved or
//! resized is not stored again. A foreign stamp keeps its own appearance.

mod common;

use std::sync::Arc;

use common::{Work, TARGETS};
use pl_annot::{
    AnnotId, AnnotKind, AnnotSet, Annotation, Geometry, RgbaImage, StampContent, StampScheme,
    StampSpec,
};
use pl_geom::{Rect, Vec2};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, SaveOptions};

fn dict_of(doc: &PdfDocument, id: ObjectId) -> &Dictionary {
    doc.object(id).unwrap().as_dict().unwrap()
}

/// A 12 x 8 gradient; with `alpha`, one corner fades out.
fn picture(alpha: bool) -> Arc<RgbaImage> {
    let (w, h) = (12u32, 8u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if alpha && x + y < 7 {
                40 + 20 * (x + y) as u8
            } else {
                255
            };
            px.extend_from_slice(&[(x * 21) as u8, (y * 32) as u8, 200 - (x * 12) as u8, a]);
        }
    }
    Arc::new(RgbaImage::new(w, h, px).unwrap())
}

/// A rectangle of `w` by `h` as displayed: sides swapped on a page turned a quarter.
fn placed(o: Vec2, rotate: i32, x: f64, y: f64, w: f64, h: f64) -> Rect {
    let (w, h) = if rotate % 180 == 90 { (h, w) } else { (w, h) };
    Rect::new(o.x + x, o.y + y, o.x + x + w, o.y + y + h)
}

fn stream_of(doc: &PdfDocument, id: ObjectId) -> &pl_pdf::Stream {
    match doc.object(id) {
        Some(Object::Stream(s)) => s,
        other => panic!("object {id:?} is not a stream: {other:?}"),
    }
}

#[test]
fn our_stamps_round_trip_and_render_on_every_page_layout() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for target in &TARGETS {
        let fixture = (target.fixture)();
        let mut probe = Work::new(&fixture);
        let geometry = probe.geometry(target.page);
        let rotate = geometry.rotate().degrees();
        let o = probe.sample_origin(target.page);
        let scale = common::scale_for(&geometry);

        let specs = [
            StampSpec::approved().with_detail("P. Tester  24 Nov 2026"),
            StampSpec::void(),
            StampSpec::for_review(),
        ];
        for spec in specs {
            let label = format!("{}@{}", spec.label, target.name);
            let mut work = Work::new(&fixture);
            let (w, h) = spec.natural_size().unwrap();
            let mut a = Annotation::stamp(
                target.page,
                placed(o, rotate, 30.0, 40.0, w, h),
                spec.clone(),
                &common::origin(),
            );
            a.set_rotation(rotate);
            a.style.opacity = if spec.label == "VOID" { 0.75 } else { 1.0 };

            let before = work.doc().clone();
            let mut set = AnnotSet::load(&before);
            set.add(a.clone()).unwrap();
            let (plan, _) = work.save(&set);
            let after = work.doc().clone();
            common::assert_untouched(&before, &after, &[before.page_id(target.page).unwrap()]);
            let loaded = AnnotSet::load(&after);
            assert!(!loaded.is_dirty(), "{label}");
            let got = loaded.get(&a.id).unwrap();
            assert_eq!(got.detached(), a.detached(), "{label}: an equal model");
            assert!(got.is_modelled());
            assert!(matches!(
                &got.stamp.as_ref().unwrap().content,
                StampContent::Label(s) if *s == spec
            ));

            let dict = dict_of(&after, plan.added[0].object);
            assert_eq!(
                dict.get(b"Subtype").unwrap(),
                &Object::Name(b"Stamp".to_vec())
            );
            assert_eq!(
                dict.get(b"Name").unwrap(),
                &Object::Name(spec.name.as_bytes().to_vec()),
                "{label}"
            );
            assert_eq!(
                pl_pdf::rect_from_object(dict.get(b"Rect").unwrap()),
                Some(a.geometry.bounds())
            );
            for key in [
                "NM",
                "M",
                "CreationDate",
                "T",
                "Subj",
                "F",
                "CA",
                "P",
                "AP",
                "Contents",
            ] {
                assert!(dict.has(key.as_bytes()), "{label}: /{key}");
            }
            assert_eq!(dict.has(b"Rotate"), rotate != 0, "{label}");
            // The appearance is a full one: its own stream with the Helvetica-Bold resource.
            let ap = stream_of(&after, plan.added[0].appearance.unwrap());
            assert_eq!(
                pl_pdf::rect_from_object(ap.dict.get(b"BBox").unwrap()),
                Some(a.geometry.bounds())
            );
            // (A translucent stamp is one transparency group: its text is in a nested form.)
            if a.style.opacity >= 1.0 {
                let content = String::from_utf8_lossy(&ap.content).into_owned();
                assert!(
                    content.contains(&format!("({}) Tj", spec.label)),
                    "{label}: {content}"
                );
                let fonts = ap
                    .dict
                    .get(b"Resources")
                    .and_then(Object::as_dict)
                    .and_then(|r| r.get(b"Font"))
                    .and_then(Object::as_dict)
                    .unwrap();
                let bold = fonts.iter().any(|(_, f)| {
                    after
                        .resolve(f)
                        .and_then(|f| f.as_dict().ok())
                        .and_then(|f| f.get(b"BaseFont").ok())
                        == Some(&Object::Name(b"Helvetica-Bold".to_vec()))
                });
                assert!(bold, "{label}: the label is set in Helvetica-Bold");
            }

            let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
            let ours = common::raster_over(&a.draw_list(), &geometry, &r.backdrop);
            if let Some(why) = common::ink_mismatch(&r.pdfium, &ours, &r.backdrop, scale, 1.0) {
                common::dump(&r.pdfium, &format!("stamp-{label}-pdfium"));
                common::dump(&ours, &format!("stamp-{label}-ours"));
                failures.push(format!("{label}: {why}"));
            }
            // Upright as displayed: the label's ink is wider than it is tall on screen.
            let ([x0, y0, x1, y1], _) = common::ink(&r.pdfium, &r.backdrop).unwrap();
            assert!(
                x1 - x0 > y1 - y0,
                "{label}: the stamp lies along the screen"
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn image_stamps_round_trip_and_render_on_every_page_layout() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for target in &TARGETS {
        let fixture = (target.fixture)();
        let mut probe = Work::new(&fixture);
        let geometry = probe.geometry(target.page);
        let rotate = geometry.rotate().degrees();
        let o = probe.sample_origin(target.page);
        for (name, alpha, opacity) in [
            ("opaque", false, 1.0),
            ("alpha", true, 1.0),
            ("translucent", true, 0.5),
        ] {
            let label = format!("{name}@{}", target.name);
            let mut work = Work::new(&fixture);
            let image = picture(alpha);
            let mut a = Annotation::image_stamp(
                target.page,
                placed(o, rotate, 40.0, 30.0, 144.0, 96.0),
                image.clone(),
                &common::origin(),
            );
            a.set_rotation(rotate);
            a.style.opacity = opacity;

            let before = work.doc().clone();
            let mut set = AnnotSet::load(&before);
            set.add(a.clone()).unwrap();
            let (plan, _) = work.save(&set);
            let after = work.doc().clone();
            let loaded = AnnotSet::load(&after);
            assert!(!loaded.is_dirty(), "{label}");
            let got = loaded.get(&a.id).unwrap();
            assert_eq!(
                got.detached(),
                a.detached(),
                "{label}: an equal model, pixels included"
            );
            assert!(got.is_modelled());

            // Object level: the appearance holds one image XObject, with a soft mask exactly
            // when the picture has transparency.
            let dict = dict_of(&after, plan.added[0].object);
            assert_eq!(
                dict.get(b"Subtype").unwrap(),
                &Object::Name(b"Stamp".to_vec())
            );
            let ap = stream_of(&after, plan.added[0].appearance.unwrap());
            let xobjects = ap
                .dict
                .get(b"Resources")
                .and_then(Object::as_dict)
                .and_then(|r| r.get(b"XObject"))
                .and_then(Object::as_dict)
                .unwrap();
            // Opacity below 1 puts the image in a group of its own; find the image either way.
            let mut images = Vec::new();
            let mut stack: Vec<ObjectId> = xobjects
                .iter()
                .map(|(_, v)| v.as_reference().unwrap())
                .collect();
            while let Some(id) = stack.pop() {
                let s = stream_of(&after, id);
                match s.dict.get(b"Subtype").unwrap().as_name().unwrap() {
                    b"Image" => images.push(id),
                    _ => {
                        if let Ok(inner) = s
                            .dict
                            .get(b"Resources")
                            .and_then(Object::as_dict)
                            .and_then(|r| r.get(b"XObject"))
                            .and_then(Object::as_dict)
                        {
                            stack.extend(inner.iter().map(|(_, v)| v.as_reference().unwrap()));
                        }
                    }
                }
            }
            assert_eq!(images.len(), 1, "{label}: one image");
            let image_stream = stream_of(&after, images[0]);
            assert_eq!(
                image_stream.dict.get(b"Width").unwrap(),
                &Object::Integer(12)
            );
            assert_eq!(
                image_stream.dict.get(b"Height").unwrap(),
                &Object::Integer(8)
            );
            assert_eq!(
                image_stream.dict.get(b"ColorSpace").unwrap(),
                &Object::Name(b"DeviceRGB".to_vec())
            );
            assert_eq!(image_stream.dict.has(b"SMask"), alpha, "{label}");
            if alpha {
                let mask = stream_of(
                    &after,
                    image_stream
                        .dict
                        .get(b"SMask")
                        .unwrap()
                        .as_reference()
                        .unwrap(),
                );
                assert_eq!(
                    mask.dict.get(b"ColorSpace").unwrap(),
                    &Object::Name(b"DeviceGray".to_vec())
                );
                let samples = mask.get_plain_content().unwrap();
                assert_eq!(samples.len(), 96);
                assert_eq!(samples[0], 40, "{label}: the corner's alpha");
            }

            // PDFium draws it like the raster. A picture is not anti-aliased the same way
            // along its edge: allow the outline, require the inside to agree.
            let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
            let ours = common::raster_over(&a.draw_list(), &geometry, &r.backdrop);
            let d = common::diff(&r.pdfium, &ours, 1, 40);
            let scale = common::scale_for(&geometry);
            let outline = (2.0 * (144.0 + 96.0) * scale) as usize;
            if d.bad > outline || d.mean > 0.6 {
                common::dump(&r.pdfium, &format!("image-{label}-pdfium"));
                common::dump(&ours, &format!("image-{label}-ours"));
                failures.push(format!("{label}: {d:?} (allowed {outline})"));
            }
            let shown = common::changed_pixels(&r.pdfium, &r.backdrop, 30);
            assert!(
                shown as f64 > 0.5 * 144.0 * 96.0 * scale * scale * opacity,
                "{label}: the picture shows ({shown} px)"
            );
            // Upright as displayed: the picture's top-left pixel is dark blue (0, 0, 200) and
            // opaque without alpha; it must be at the top-left of the picture on screen.
            if !alpha {
                let ([x0, y0, _, _], _) = common::ink(&r.pdfium, &r.backdrop).unwrap();
                let p = r.pdfium.pixel(x0 + 6, y0 + 6);
                assert!(
                    p[2] > 150 && p[0] < 60 && p[1] < 60,
                    "{label}: the first pixel is at the top left on screen, found {p:?}"
                );
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_moved_or_resized_picture_is_not_stored_again() {
    let backend = common::backend();
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let geometry = work.geometry(0);
    let mut set = AnnotSet::load(work.doc());
    let a = Annotation::image_stamp(
        0,
        Rect::new(100.0, 300.0, 244.0, 396.0),
        picture(true),
        &common::origin(),
    );
    let id = set.add(a).unwrap();
    work.save(&set);

    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let object = set.get(&id).unwrap().object_id().unwrap();
    // Moved and made larger.
    set.get_mut(&id).unwrap().geometry = Geometry::Rect(Rect::new(150.0, 250.0, 366.0, 394.0));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["Rect"]);
    assert_eq!(plan.modified[0].appearance, None);
    assert_eq!(plan.edits.new_object_ids().count(), 0, "no new stream");
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[object]);
    assert_eq!(
        dict_of(&before, object).get(b"AP").unwrap(),
        dict_of(&after, object).get(b"AP").unwrap()
    );
    // The model reads back at the new place with the same picture, and PDFium stretches the
    // old appearance over the new rectangle exactly as the raster draws it.
    let loaded = AnnotSet::load(&after);
    let got = loaded.get(&id).unwrap();
    assert_eq!(got.detached(), set.get(&id).unwrap().detached());
    assert!(!loaded.is_dirty());
    let r = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    let ours = common::raster_over(&got.draw_list(), &geometry, &r.backdrop);
    let d = common::diff(&r.pdfium, &ours, 1, 40);
    assert!(d.bad < 2 * (216 + 144) * 2 && d.mean < 0.6, "{d:?}");

    // A new picture, or another opacity, does write a new appearance.
    let mut set = loaded.clone();
    set.get_mut(&id).unwrap().style.opacity = 0.5;
    let plan = set.plan_save(&after).unwrap();
    assert_eq!(plan.modified[0].keys, ["AP", "CA"]);
    let mut set = loaded;
    set.get_mut(&id).unwrap().stamp.as_mut().unwrap().content = StampContent::Image(picture(false));
    let plan = set.plan_save(&after).unwrap();
    assert_eq!(plan.modified[0].keys, ["AP"]);
}

#[test]
fn editing_a_stamp_rewrites_only_what_changed() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let mut set = AnnotSet::load(work.doc());
    let id = set
        .add(Annotation::stamp(
            0,
            Rect::new(100.0, 500.0, 260.0, 540.0),
            StampSpec::draft(),
            &common::origin(),
        ))
        .unwrap();
    work.save(&set);
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let a = set.get_mut(&id).unwrap();
    let Some(StampContent::Label(spec)) = a.stamp.as_mut().map(|s| &mut s.content) else {
        panic!("a label stamp");
    };
    spec.detail = Some("Rev C".into());
    spec.scheme = StampScheme::Amber;
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["AP", "PlumblineData"]);
    let loaded = AnnotSet::load(work.doc());
    let got = loaded.get(&id).unwrap();
    assert_eq!(got.detached(), set.get(&id).unwrap().detached());
    assert_eq!(got.icon.as_deref(), Some("Draft"));
    // Application data under the private key lives beside the stamp's own and is given back
    // without it.
    let mut set = loaded;
    let mut data = Dictionary::new();
    data.set("Tool", Object::Name(b"StampTool".to_vec()));
    set.get_mut(&id).unwrap().app_data = Some(data.clone());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["PlumblineData"]);
    let loaded = AnnotSet::load(work.doc());
    let got = loaded.get(&id).unwrap();
    assert_eq!(got.app_data, Some(data));
    assert!(got.is_modelled());
    let private = got
        .original_dict()
        .unwrap()
        .get(b"PlumblineData")
        .unwrap()
        .as_dict()
        .unwrap();
    assert!(private.has(b"PlModel") && private.has(b"Tool"));
}

#[test]
fn a_foreign_stamp_is_moved_and_deleted_with_its_own_appearance() {
    // The fixture's square, turned into a stamp the way another application would write it.
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let id = AnnotId::from("fixture-square-1");
    let object = AnnotSet::load(&doc).get(&id).unwrap().object_id().unwrap();
    let mut edits = doc.edit();
    edits
        .patch(object)
        .set("Subtype", Object::Name(b"Stamp".to_vec()))
        .set("Name", Object::Name(b"Approved".to_vec()))
        .set("VendorStamp", Object::Integer(7));
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    work.original = work.bytes();

    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let a = set.get(&id).unwrap();
    assert_eq!(a.kind, AnnotKind::Stamp);
    assert!(!a.is_modelled() && a.stamp.is_none());
    assert!(a.draw_list().is_empty());
    assert_eq!(a.icon.as_deref(), Some("Approved"));
    assert!(!set.is_dirty());

    set.get_mut(&id).unwrap().translate(Vec2::new(30.0, -40.0));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["Rect"]);
    let after = work.doc().clone();
    let (old, new) = (dict_of(&before, object), dict_of(&after, object));
    assert_eq!(old.get(b"AP").unwrap(), new.get(b"AP").unwrap());
    assert_eq!(new.get(b"VendorStamp").unwrap(), &Object::Integer(7));
    assert_eq!(
        pl_pdf::rect_from_object(new.get(b"Rect").unwrap()),
        Some(Rect::new(130.0, 360.0, 250.0, 440.0))
    );

    let mut set = AnnotSet::load(&after);
    assert_eq!(set.remove(&id).items.len(), 1);
    let (plan, _) = work.save(&set);
    assert_eq!(plan.deleted.len(), 1);
    assert_eq!(AnnotSet::load(work.doc()).len(), 3);
}
