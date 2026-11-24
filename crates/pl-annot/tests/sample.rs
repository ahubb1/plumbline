//! The real-world sample (`samples/site-development-plans.pdf`, large civil drawing sheets):
//! one markup of each kind on page 1 and on page 2, saved to a temporary copy and re-opened
//! with lopdf, hayro-syntax and PDFium.
//!
//! The sample is not part of the repository. Without it the test prints a line and passes.
//! The sample itself is only ever read: the save goes to a temporary directory that is removed
//! when the test ends.

mod common;

use std::path::{Path, PathBuf};

use pl_annot::{AnnotSet, Annotation, DrawList};
use pl_geom::Vec2;
use pl_pdf::{PdfFile, SaveOptions, SecondParser};
use pl_render::{DocSource, RenderBackend, RenderOptions};

fn sample_path() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    path.is_file().then_some(path)
}

#[test]
fn every_kind_on_two_sheets_of_the_real_sample() {
    let Some(path) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let stamp_before = std::fs::metadata(&path)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("marked.pdf");

    let mut file = PdfFile::open(&path).unwrap();
    let before = file.document().unwrap().clone();
    assert!(before.page_count() >= 2);
    let counts_before = before.annotation_counts();

    let mut set = AnnotSet::load(&before);
    let had = set.len();
    let mut added: Vec<Annotation> = Vec::new();
    let mut per_page = [0usize; 2];
    for (page, count) in per_page.iter_mut().enumerate() {
        let crop = before.page(page).unwrap().geometry.crop_box();
        // A little way in from the lower-left corner of the sheet.
        let o = Vec2::new(
            crop.x0 + (crop.width() * 0.2).round(),
            crop.y0 + (crop.height() * 0.2).round(),
        );
        for a in common::one_of_each(page, o) {
            *count += 1;
            added.push(a.clone());
            set.add(a).unwrap();
        }
    }
    assert!(per_page[0] >= 13 && per_page[0] == per_page[1]);
    let kinds: std::collections::HashSet<_> = added.iter().map(|a| a.kind.clone()).collect();
    for kind in pl_annot::AnnotKind::MODELLED {
        assert!(kinds.contains(&kind), "{kind:?} is among the added markups");
    }

    let plan = set.plan_save(&before).unwrap();
    assert_eq!(plan.added.len(), added.len());
    assert!(plan.modified.is_empty() && plan.deleted.is_empty());
    assert_eq!(plan.touched_pages(), [0, 1]);
    let report = file
        .save_incremental_as(&out, &plan.edits, &SaveOptions::default())
        .expect("verified incremental save to a temporary copy");

    // lopdf and hayro-syntax re-opened it inside the save, with these counts.
    let mut expected = counts_before.clone();
    expected[0] += per_page[0];
    expected[1] += per_page[1];
    let verification = report.verification.expect("verification ran");
    assert_eq!(
        verification.second_parser,
        SecondParser::Passed,
        "hayro-syntax"
    );
    assert_eq!(verification.annotation_counts, expected);
    assert_eq!(verification.page_count, before.page_count());

    // The original bytes are an exact prefix, and the sample itself was not written to.
    let saved = std::fs::read(&out).unwrap();
    assert!(saved.len() > before.bytes().len());
    assert!(
        saved[..before.bytes().len()] == *before.bytes(),
        "exact prefix"
    );
    let stamp_after = std::fs::metadata(&path)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap();
    assert_eq!(stamp_before, stamp_after, "the sample is untouched");

    // Re-open with pl-pdf: no original object was rewritten except the two /Annots holders,
    // and the model reads back equal.
    let mut reopened = PdfFile::open(&out).unwrap();
    let after = reopened.document().unwrap().clone();
    assert_eq!(after.annotation_counts(), expected);
    let mut rewritten = 0;
    for id in before.object_ids() {
        if after.object_offset(id) != before.object_offset(id) {
            rewritten += 1;
        } else {
            assert!(
                after.object(id) == before.object(id),
                "object {id:?} changed"
            );
        }
    }
    assert!(
        rewritten <= 2,
        "{rewritten} original objects were rewritten"
    );
    let loaded = AnnotSet::load(&after);
    assert_eq!(loaded.len(), had + added.len());
    assert!(!loaded.is_dirty());
    for a in &added {
        assert_eq!(
            loaded.get(&a.id).map(Annotation::detached),
            Some(a.detached())
        );
    }

    // PDFium counts the same annotations and draws them where the draw lists say.
    let backend = common::backend();
    let doc = backend
        .open(DocSource::file(&out), None)
        .expect("PDFium opens the copy")
        .doc;
    assert_eq!(
        backend.page_count(doc).unwrap() as usize,
        before.page_count()
    );
    for (page, want) in expected.iter().enumerate().take(2) {
        let geometry = after.page(page).unwrap().geometry;
        assert_eq!(backend.annotations(doc, page as u32).unwrap().len(), *want);
        backend
            .set_page_user_unit(doc, page as u32, geometry.user_unit())
            .unwrap();
        // About 2,400 px on the long side: the markups are small on a sheet this size.
        let size = geometry.display_size();
        let scale = 2400.0 / size.width.max(size.height);
        let bare = backend
            .render_page_with(
                doc,
                page as u32,
                scale,
                RenderOptions { annotations: false },
            )
            .unwrap();
        let with = backend
            .render_page_with(doc, page as u32, scale, RenderOptions { annotations: true })
            .unwrap();
        let mut list = DrawList::new();
        for a in added.iter().filter(|a| a.page == page) {
            list.extend(a.draw_list().ops);
        }
        let pixels = pl_draw::render_over_rgba(
            &list,
            pl_draw::page_transform(&geometry, scale),
            &bare.pixels,
            bare.width,
            bare.height,
        )
        .unwrap();
        let ours = pl_render::Bitmap {
            width: bare.width,
            height: bare.height,
            pixels,
        };
        let drawn = common::changed_pixels(&bare, &with, 12);
        assert!(
            drawn > 300,
            "page {page}: the markups show in PDFium ({drawn} px)"
        );
        let d = common::diff(&ours, &with, 1, 40);
        assert!(d.bad <= 12 && d.mean < 0.6, "page {page}: {d:?}");
    }
    backend.close(doc).unwrap();
}

/// Text boxes, a callout, stamps, a picture, a popup, a reply and a status on the first sheet
/// of the sample, saved to a copy under `target\` (the sample never leaves the repository
/// folder) and read back; the text box's characters are where the layout put them.
#[test]
fn text_stamps_and_threads_on_a_sheet_of_the_real_sample() {
    use pl_annot::{ReviewState, RgbaImage, StampSpec};
    use pl_geom::{Point, Rect};

    let Some(path) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let stamp_before = std::fs::metadata(&path)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap();
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let out = dir.path().join("marked-text.pdf");

    let mut file = PdfFile::open(&path).unwrap();
    let before = file.document().unwrap().clone();
    let geometry = before.page(0).unwrap().geometry;
    let rotate = geometry.rotate().degrees();
    let crop = geometry.crop_box();
    let o = Vec2::new(
        crop.x0 + (crop.width() * 0.3).round(),
        crop.y0 + (crop.height() * 0.3).round(),
    );
    let sized = |x: f64, y: f64, w: f64, h: f64| {
        let (w, h) = if rotate % 180 == 90 { (h, w) } else { (w, h) };
        Rect::new(o.x + x, o.y + y, o.x + x + w, o.y + y + h)
    };
    let og = common::origin();
    let typed = "Confirm the invert level here before the pour.\nRefer to the survey.";

    let mut set = AnnotSet::load(&before);
    let had = set.len();
    let mut text_box = Annotation::free_text(0, sized(0.0, 0.0, 180.0, 80.0), typed, &og);
    text_box.set_rotation(rotate);
    let mut callout = Annotation::callout(
        0,
        sized(0.0, 120.0, 150.0, 50.0),
        Point::new(o.x - 60.0, o.y + 90.0),
        "See note 3",
        &og,
    );
    callout.set_rotation(rotate);
    let spec = StampSpec::for_review().with_detail("Plumbline Tester");
    let (w, h) = spec.natural_size().unwrap();
    let mut stamp = Annotation::stamp(0, sized(220.0, 0.0, w, h), spec, &og);
    stamp.set_rotation(rotate);
    let pixels: Vec<u8> = (0..6 * 4)
        .flat_map(|i| [(i * 10) as u8, 120, 255 - (i * 9) as u8, 255])
        .collect();
    let image = std::sync::Arc::new(RgbaImage::new(6, 4, pixels).unwrap());
    let mut picture = Annotation::image_stamp(0, sized(220.0, 90.0, 90.0, 60.0), image, &og);
    picture.set_rotation(rotate);

    let added = [text_box.clone(), callout, stamp, picture];
    for a in &added {
        set.add(a.clone()).unwrap();
    }
    let popup = set.ensure_popup(&text_box.id, crop, &og).unwrap();
    let reply = set.add_reply(&text_box.id, "Level confirmed", &og).unwrap();
    let status = set
        .set_review_state(&text_box.id, ReviewState::Accepted, &og)
        .unwrap();
    let expected: Vec<Annotation> = set.iter().skip(had).map(Annotation::detached).collect();
    assert_eq!(expected.len(), 7);

    let plan = set.plan_save(&before).unwrap();
    assert_eq!(plan.added.len(), 7);
    assert!(plan.modified.is_empty() && plan.deleted.is_empty());
    let report = file
        .save_incremental_as(&out, &plan.edits, &SaveOptions::default())
        .expect("verified incremental save to a copy under target");
    assert_eq!(
        report.verification.expect("verification ran").second_parser,
        SecondParser::Passed
    );
    let saved = std::fs::read(&out).unwrap();
    assert!(
        saved[..before.bytes().len()] == *before.bytes(),
        "exact prefix"
    );
    assert_eq!(
        stamp_before,
        std::fs::metadata(&path)
            .map(|m| (m.len(), m.modified().ok()))
            .unwrap(),
        "the sample is untouched"
    );

    let mut reopened = PdfFile::open(&out).unwrap();
    let after = reopened.document().unwrap().clone();
    let loaded = AnnotSet::load(&after);
    assert!(!loaded.is_dirty());
    for want in &expected {
        assert_eq!(&loaded.get(&want.id).unwrap().detached(), want);
    }
    assert_eq!(loaded.review_state(&text_box.id), ReviewState::Accepted);
    assert_eq!(loaded.replies(&text_box.id)[0].id, reply);
    assert_eq!(loaded.get(&text_box.id).unwrap().popup, Some(popup));
    assert!(loaded.contains(&status));

    // PDFium counts them and draws the four that show.
    let backend = common::backend();
    let doc = backend.open(DocSource::file(&out), None).unwrap().doc;
    assert_eq!(
        backend.annotations(doc, 0).unwrap().len(),
        before.annotation_counts()[0] + 7
    );
    backend
        .set_page_user_unit(doc, 0, geometry.user_unit())
        .unwrap();
    let size = geometry.display_size();
    let scale = 2400.0 / size.width.max(size.height);
    let bare = backend
        .render_page_with(doc, 0, scale, RenderOptions { annotations: false })
        .unwrap();
    let with = backend
        .render_page_with(doc, 0, scale, RenderOptions { annotations: true })
        .unwrap();
    backend.close(doc).unwrap();
    let to_px = pl_draw::page_transform(&geometry, scale);
    for a in &added {
        let px = to_px.transform_rect_bbox(a.bounds());
        let mut changed = 0;
        for y in px.y0.max(0.0) as u32..(px.y1.ceil() as u32).min(with.height) {
            for x in px.x0.max(0.0) as u32..(px.x1.ceil() as u32).min(with.width) {
                let (p, q) = (with.pixel(x, y), bare.pixel(x, y));
                if (0..3).any(|c| (p[c] as i32 - q[c] as i32).abs() > 30) {
                    changed += 1;
                }
            }
        }
        assert!(changed > 20, "{:?} shows in PDFium ({changed} px)", a.kind);
    }

    // The text box's appearance as page content: PDFium finds every typed character where
    // the layout put it, and extraction gives back the typed string.
    let ap = plan.added[0]
        .appearance
        .expect("the text box has an appearance");
    let Some(pl_pdf::Object::Stream(stream)) = after.object(ap) else {
        panic!("the appearance is a stream");
    };
    let mut edits = after.edit();
    edits
        .patch(after.page_id(0).unwrap())
        .set("Contents", pl_pdf::Object::Reference(ap))
        .set("Resources", stream.dict.get(b"Resources").unwrap().clone());
    reopened
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let text = common::page_text(&backend, std::fs::read(&out).unwrap(), 0, &geometry);
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words(&text.text()) == words(typed),
        "extraction returns the typed string"
    );
    let layout = text_box.text_layout().unwrap().unwrap();
    let (_, to) = text_box.text_frame().unwrap();
    let chars: Vec<char> = typed.chars().collect();
    let ours: Vec<Point> = (0..chars.len())
        .filter(|i| !chars[*i].is_whitespace())
        .map(|i| to * layout.carets[i].position)
        .collect();
    let theirs: Vec<Point> = text
        .chars
        .iter()
        .filter(|c| !c.generated && !c.ch.is_whitespace())
        .map(|c| Point::new(c.origin[0], c.origin[1]))
        .collect();
    assert_eq!(ours.len(), theirs.len());
    let worst = ours
        .iter()
        .zip(&theirs)
        .map(|(a, b)| (*a - *b).hypot())
        .fold(0.0, f64::max);
    assert!(
        worst < 1.0 / geometry.user_unit(),
        "characters are within 1 pt of the layout ({worst})"
    );
}
