//! Editing through [`Document`] against real PDFium and the pl-testkit fixtures: who draws an
//! annotation before and after it is edited, save and its failure modes, the crash journal,
//! copy and paste, and the tool adapter. Headless: nothing here opens a window, and the crash
//! journals live in temporary directories.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{
    AnnotId, AnnotKind, Annotation, Geometry, LineEnding, Origin, PdfDate, Quad, Rgb,
};
use pl_doc::pl_geom::{PageGeometry, Point, Rect, TileGrid, Vec2};
use pl_doc::pl_pdf::{FailPoint, PdfError, PdfFile, SaveOptions};
use pl_doc::pl_render::{Bitmap, PdfiumBackend, RenderBackend, RenderOptions, TileCache, TileKey};
use pl_doc::{
    Clipboard, Command, DocError, DocSnapshot, DocView, Document, DocumentId, PastePlacement,
    PendingJournal, Session, SessionOptions,
};
use pl_testkit::Fixture;

fn vendor_dll() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    std::path::absolute(p).unwrap()
}

fn backend() -> Arc<PdfiumBackend> {
    Arc::new(PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll"))
}

fn on_disk(fixture: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    (dir, path)
}

/// A session that settles everything at open, with its crash journals under `journal`.
fn session(backend: &Arc<PdfiumBackend>, journal: Option<&Path>) -> Session {
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    s.set_journal_dir(journal.map(Path::to_path_buf));
    s
}

fn open(s: &mut Session, path: &Path) -> DocumentId {
    s.open(path, None).unwrap()
}

fn origin() -> Origin {
    Origin::new(
        "Plumbline Tester",
        PdfDate::parse("D:20261124101500Z").unwrap(),
    )
}

fn model(doc: &mut Document) -> Vec<Annotation> {
    doc.annots().unwrap().model()
}

fn find(doc: &mut Document, id: &str) -> Annotation {
    doc.annots()
        .unwrap()
        .set()
        .get(&AnnotId::from(id))
        .unwrap_or_else(|| panic!("annotation {id}"))
        .clone()
}

fn moved(a: &Annotation, dx: f64, dy: f64) -> Annotation {
    let mut b = a.clone();
    b.translate(Vec2::new(dx, dy));
    b
}

// ---- bitmaps --------------------------------------------------------------------------------

fn one_way(a: &Bitmap, b: &Bitmap, radius: i32, tolerance: i32) -> usize {
    let (w, h) = (a.width as i32, a.height as i32);
    let mut bad = 0;
    for y in 0..h {
        for x in 0..w {
            let ia = ((y * w + x) * 4) as usize;
            if a.pixels[ia..ia + 3] == b.pixels[ia..ia + 3] {
                continue;
            }
            let mut lo = [255i32; 3];
            let mut hi = [0i32; 3];
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w || ny >= h {
                        continue;
                    }
                    let ib = ((ny * w + nx) * 4) as usize;
                    for c in 0..3 {
                        let v = b.pixels[ib + c] as i32;
                        lo[c] = lo[c].min(v);
                        hi[c] = hi[c].max(v);
                    }
                }
            }
            if (0..3).any(|c| {
                let v = a.pixels[ia + c] as i32;
                v < lo[c] - tolerance || v > hi[c] + tolerance
            }) {
                bad += 1;
            }
        }
    }
    bad
}

/// Pixels of either picture that have no counterpart within one pixel and 40 levels in the
/// other: the comparison pl-annot uses between a tiny-skia raster and a PDFium render.
fn mismatch(a: &Bitmap, b: &Bitmap) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    one_way(a, b, 1, 40) + one_way(b, a, 1, 40)
}

/// Pixels that differ at all, inside (`inside` true) or outside the given pixel boxes.
fn differing(a: &Bitmap, b: &Bitmap, boxes: &[[u32; 4]], inside: bool) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    let mut n = 0;
    for y in 0..a.height {
        for x in 0..a.width {
            let within = boxes
                .iter()
                .any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
            if within == inside && a.pixel(x, y) != b.pixel(x, y) {
                n += 1;
            }
        }
    }
    n
}

fn changed(a: &Bitmap, b: &Bitmap) -> usize {
    differing(a, b, &[], false)
}

/// The pixel box `[x0, y0, x1, y1]` of a page-space rectangle, grown by `grow` pixels.
fn pixel_box(geometry: &PageGeometry, scale: f64, rect: Rect, grow: f64) -> [u32; 4] {
    let t = pl_doc::pl_draw::raster::page_transform(geometry, scale);
    let r = t.transform_rect_bbox(rect).inflate(grow, grow);
    [
        r.x0.floor().max(0.0) as u32,
        r.y0.floor().max(0.0) as u32,
        r.x1.ceil().max(0.0) as u32,
        r.y1.ceil().max(0.0) as u32,
    ]
}

fn reddish(b: &Bitmap, within: Option<[u32; 4]>) -> usize {
    let mut n = 0;
    for y in 0..b.height {
        for x in 0..b.width {
            if within.is_some_and(|r| x < r[0] || x >= r[2] || y < r[1] || y >= r[3]) {
                continue;
            }
            let p = b.pixel(x, y);
            if p[0] > 180 && p[1] < 90 && p[2] < 90 {
                n += 1;
            }
        }
    }
    n
}

fn mirror(doc: &Document, page: u32, scale: f64) -> Bitmap {
    doc.backend()
        .render_page(doc.render_id(), page, scale)
        .unwrap()
}

fn page_only(doc: &Document, page: u32, scale: f64) -> Bitmap {
    doc.backend()
        .render_page_with(
            doc.render_id(),
            page,
            scale,
            RenderOptions { annotations: false },
        )
        .unwrap()
}

// ---- samples --------------------------------------------------------------------------------

/// A handful of markups of different kinds, placed from `o` (page space).
fn markups(page: usize, o: Vec2) -> Vec<Annotation> {
    let og = origin();
    let p = |x: f64, y: f64| Point::new(o.x + x, o.y + y);
    let r = |x0: f64, y0: f64, x1: f64, y1: f64| Rect::new(o.x + x0, o.y + y0, o.x + x1, o.y + y1);
    let mut square = Annotation::square(page, r(10.0, 10.0, 110.0, 70.0), &og);
    square.style.width = 3.0;
    square.style.fill = Some(Rgb::new(0.2, 0.6, 0.9));
    square.style.opacity = 0.5;
    let mut circle = Annotation::circle(page, r(130.0, 10.0, 210.0, 70.0), &og);
    circle.style.stroke = Some(Rgb::new(0.0, 0.5, 0.0));
    circle.style.width = 2.0;
    let mut line = Annotation::line(page, p(10.0, 90.0), p(200.0, 130.0), &og);
    line.style.width = 2.0;
    line.style.line_end = LineEnding::ClosedArrow;
    line.style.fill = Some(Rgb::new(0.8, 0.0, 0.0));
    let mut polygon = Annotation::polygon(
        page,
        vec![p(20.0, 150.0), p(100.0, 150.0), p(60.0, 210.0)],
        &og,
    );
    polygon.style.fill = Some(Rgb::new(1.0, 0.8, 0.2));
    let mut ink = Annotation::ink(
        page,
        vec![vec![
            p(130.0, 150.0),
            p(150.0, 190.0),
            p(170.0, 160.0),
            p(200.0, 205.0),
        ]],
        &og,
    );
    ink.style.width = 2.5;
    ink.style.stroke = Some(Rgb::new(0.3, 0.0, 0.6));
    let quad: Quad = [
        p(10.0, 225.0),
        p(180.0, 225.0),
        p(180.0, 240.0),
        p(10.0, 240.0),
    ];
    let highlight = Annotation::text_markup(AnnotKind::Highlight, page, vec![quad], &og);
    let mut note = Annotation::note(page, r(220.0, 220.0, 238.0, 238.0), "A note", &og);
    note.meta.subject = Some("Query".into());
    vec![square, circle, line, polygon, ink, highlight, note]
}

struct Target {
    name: &'static str,
    fixture: fn() -> Fixture,
    page: u32,
}

/// A plain page, the three rotated pages, the `/UserUnit` page and the three box-offset pages
/// (inset CropBox; MediaBox with a non-zero origin; both plus `/Rotate 90`).
const TARGETS: [Target; 8] = [
    Target {
        name: "plain",
        fixture: pl_testkit::plain,
        page: 0,
    },
    Target {
        name: "rotate90",
        fixture: pl_testkit::rotated_pages,
        page: 1,
    },
    Target {
        name: "rotate180",
        fixture: pl_testkit::rotated_pages,
        page: 2,
    },
    Target {
        name: "rotate270",
        fixture: pl_testkit::rotated_pages,
        page: 3,
    },
    Target {
        name: "userunit",
        fixture: pl_testkit::user_unit,
        page: 0,
    },
    Target {
        name: "cropbox",
        fixture: pl_testkit::crop_box,
        page: 0,
    },
    Target {
        name: "mediabox-offset",
        fixture: pl_testkit::crop_box,
        page: 1,
    },
    Target {
        name: "cropbox-rotate90",
        fixture: pl_testkit::crop_box,
        page: 2,
    },
];

fn scale_for(geometry: &PageGeometry) -> f64 {
    if geometry.user_unit() > 1.0 {
        4.0
    } else {
        2.0
    }
}

fn placed(geometry: &PageGeometry) -> Vec2 {
    let crop = geometry.crop_box();
    Vec2::new(crop.x0 + 24.0, crop.y0 + 24.0)
}

// ---- display and save on every page layout --------------------------------------------------

#[test]
fn new_markups_look_the_same_before_and_after_save_on_every_layout() {
    let backend = backend();
    for target in &TARGETS {
        let name = target.name;
        let fixture = (target.fixture)();
        let (_dir, path) = on_disk(&fixture);
        let mut s = session(&backend, None);
        let id = open(&mut s, &path);
        let doc = s.document_mut(id).unwrap();
        let page = target.page;
        let geometry = *doc.page_geometry(page).unwrap();
        let scale = scale_for(&geometry);
        let had = model(doc).len();
        let pdfium_had = doc.annotations(page).unwrap().len();
        let plain = mirror(doc, page, scale);

        let new = markups(page as usize, placed(&geometry));
        let n = new.len();
        let out = doc.apply(Command::AddAnnotations(new.clone())).unwrap();
        assert!(out.changed && out.touched.len() == n, "{name}");
        assert!(doc.is_modified() && doc.can_undo(), "{name}");

        // New markups never touch tiles or the mirror: they are the overlay's.
        assert!(doc.take_invalidations().is_empty(), "{name}");
        assert!(doc.hidden_annotations().is_empty(), "{name}");
        assert_eq!(doc.annotations(page).unwrap().len(), pdfium_had, "{name}");
        assert_eq!(
            changed(&mirror(doc, page, scale), &plain),
            0,
            "{name}: tiles"
        );
        let overlay = doc.overlay(page);
        assert_eq!(overlay.len(), n, "{name}");
        assert_eq!(
            overlay.iter().filter(|i| i.multiply).count(),
            1,
            "{name}: highlight"
        );
        assert!(overlay.iter().all(|i| !i.placeholder), "{name}");

        // What the user sees now ...
        let before_save = doc.render_page_composite(page, scale).unwrap();
        assert!(changed(&before_save, &plain) > 2000, "{name}: markups show");
        let unsaved = model(doc);

        // ... is what PDFium draws from the saved file.
        let saved = doc.save().unwrap();
        assert!(saved.wrote(), "{name}");
        assert_eq!(
            (saved.added, saved.modified, saved.deleted),
            (n, 0, 0),
            "{name}"
        );
        assert!(!doc.is_modified(), "{name}");
        assert!(
            doc.overlay(page).is_empty(),
            "{name}: nothing left to paint"
        );
        assert_eq!(
            doc.annotations(page).unwrap().len(),
            pdfium_had + n,
            "{name}"
        );
        let after_save = mirror(doc, page, scale);
        let bad = mismatch(&before_save, &after_save);
        assert!(
            bad <= 40,
            "{name}: {bad} pixels differ between overlay and saved file"
        );
        assert_eq!(
            doc.render_page_composite(page, scale).unwrap().pixels,
            after_save.pixels,
            "{name}: nothing is drawn twice after the save"
        );

        // The original bytes are an exact prefix, and the model reads back equal.
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.len() > fixture.bytes.len() && bytes.starts_with(&fixture.bytes),
            "{name}"
        );
        assert_eq!(model(doc), unsaved, "{name}: rebased model");
        assert!(!doc.annots().unwrap().set().is_dirty(), "{name}");

        // A second save has nothing to write and does not touch the file.
        let again = doc.save().unwrap();
        assert!(!again.wrote(), "{name}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}: second save");

        // A fresh session reads the same model.
        let mut s2 = session(&backend, None);
        let id2 = open(&mut s2, &path);
        let reread = model(s2.document_mut(id2).unwrap());
        assert_eq!(reread, unsaved, "{name}: reloaded model");
        assert_eq!(reread.len(), had + n, "{name}");

        // An edit of a markup that is now in the file: it is hidden in the mirror, the tiles
        // under its old place are named, and it is drawn once, at the new place.
        let doc = s.document_mut(id).unwrap();
        let first = unsaved[0].clone();
        doc.apply(Command::modify(moved(&first, 15.0, 10.0)))
            .unwrap();
        let hidden = doc.hidden_annotations();
        assert_eq!(hidden.len(), 1, "{name}");
        assert_eq!(
            (hidden[0].0.clone(), hidden[0].1.page),
            (first.id.clone(), page),
            "{name}"
        );
        let stale = doc.take_invalidations();
        assert_eq!(stale.len(), 1, "{name}");
        assert_eq!(
            (stale[0].render, stale[0].page),
            (doc.render_id(), page),
            "{name}"
        );
        assert!(
            stale[0].rect.contains_rect(first.bounds()),
            "{name}: {:?}",
            stale[0].rect
        );
        assert!(
            stale[0].rect.area() < first.bounds().inflate(4.0, 4.0).area(),
            "{name}"
        );
        let edited = doc.render_page_composite(page, scale).unwrap();
        assert!(
            changed(&edited, &after_save) > 200,
            "{name}: the move shows"
        );
        let saved = doc.save().unwrap();
        assert_eq!(
            (saved.added, saved.modified, saved.deleted),
            (0, 1, 0),
            "{name}"
        );
        let bad = mismatch(&edited, &mirror(doc, page, scale));
        assert!(
            bad <= 40,
            "{name}: {bad} pixels differ after the move was saved"
        );
        assert!(doc.undo().unwrap(), "{name}");
        assert_eq!(model(doc), unsaved, "{name}: the move undone");

        // Undo after the save takes the markups out again, and a save deletes them.
        assert!(doc.undo().unwrap(), "{name}");
        assert!(doc.is_modified(), "{name}");
        assert_eq!(model(doc).len(), had, "{name}");
        // They are in the file, so now they are hidden in the mirror instead of unpainted.
        assert_eq!(doc.hidden_annotations().len(), n, "{name}");
        let bad = mismatch(&doc.render_page_composite(page, scale).unwrap(), &plain);
        assert!(
            bad <= 40,
            "{name}: {bad} pixels of undone markups still show"
        );
        let saved = doc.save().unwrap();
        assert_eq!(
            (saved.added, saved.modified, saved.deleted),
            (0, 0, n),
            "{name}"
        );
        assert_eq!(doc.annotations(page).unwrap().len(), pdfium_had, "{name}");
        // Redo brings them back as new markups.
        assert!(doc.redo().unwrap(), "{name}");
        assert_eq!(model(doc), unsaved, "{name}: redone after a save");
        let saved = doc.save().unwrap();
        assert_eq!(
            (saved.added, saved.modified, saved.deleted),
            (n, 0, 0),
            "{name}"
        );
        let bad = mismatch(&mirror(doc, page, scale), &after_save);
        assert!(
            bad <= 40,
            "{name}: {bad} pixels differ after delete and re-add"
        );
    }
}

// ---- the hidden set -------------------------------------------------------------------------

const SQUARE: &str = "fixture-square-1";
const LINE: &str = "fixture-line-1";
const FREETEXT: &str = "fixture-freetext-1";
const SQUARE_RECT: Rect = Rect::new(100.0, 400.0, 220.0, 480.0);

#[test]
fn an_edited_foreign_annotation_is_drawn_once_and_only_its_tiles_go() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let geometry = *doc.page_geometry(0).unwrap();
    let scale = 2.0;

    // Untouched: PDFium draws everything, the overlay is empty, loading changed nothing.
    let original = mirror(doc, 0, scale);
    let bare = page_only(doc, 0, scale);
    doc.load_annots().unwrap();
    assert!(doc.overlay(0).is_empty() && doc.hidden_annotations().is_empty());
    assert_eq!(changed(&mirror(doc, 0, scale), &original), 0);
    let old_box = pixel_box(&geometry, scale, SQUARE_RECT, 3.0);
    let red_before = reddish(&original, Some(old_box));
    assert!(red_before > 500, "the foreign square is red: {red_before}");

    // A tile cache with every tile of two levels, to see which ones an edit costs.
    let cache = TileCache::new(64 << 20);
    let render = doc.render_id();
    let mut keys = Vec::new();
    for level in [0i8, 1] {
        for tile in TileGrid::new(&geometry, level).all().iter() {
            let key = TileKey::new(render, 0, doc.page_revision(0), tile, 0);
            cache.insert(key, Arc::new(Bitmap::filled(8, 8, [1, 2, 3, 255])));
            keys.push(key);
        }
    }
    let other_page_key = TileKey::new(
        render,
        1,
        0,
        TileGrid::new(&geometry, 0).all().iter().next().unwrap(),
        0,
    );
    cache.insert(other_page_key, Arc::new(Bitmap::filled(8, 8, [0; 4])));

    // First edit: move the square up and to the right.
    let square = find(doc, SQUARE);
    let revision = doc.page_revision(0);
    let (dx, dy) = (250.0, 150.0);
    doc.apply(Command::modify(moved(&square, dx, dy))).unwrap();
    let new_rect = SQUARE_RECT + Vec2::new(dx, dy);
    let new_box = pixel_box(&geometry, scale, new_rect, 3.0);

    // It is hidden in the mirror at its place in /Annots ...
    let hidden = doc.hidden_annotations();
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].0, AnnotId::from(SQUARE));
    assert_eq!((hidden[0].1.page, hidden[0].1.index), (0, 0));
    assert_eq!(hidden[0].1.rect, SQUARE_RECT);
    let in_mirror = doc.annotations(0).unwrap();
    assert!(in_mirror[0].hidden && in_mirror[0].name.as_deref() == Some(SQUARE));
    assert!(in_mirror[1..].iter().all(|a| !a.hidden));

    // ... the page revision is unchanged and exactly the tiles under its old place go ...
    assert_eq!(doc.page_revision(0), revision);
    let removed = doc.apply_invalidations(&cache);
    let mut expected_gone = 0;
    for key in &keys {
        let grid = TileGrid::new(&geometry, key.level);
        let covers = grid
            .tiles_covering_page_rect(&geometry, SQUARE_RECT.inflate(2.0, 2.0))
            .contains(key.tile());
        assert_eq!(cache.contains(key), !covers, "tile {:?}", key.tile());
        expected_gone += usize::from(covers);
    }
    assert!(expected_gone > 0 && expected_gone < keys.len());
    assert_eq!(removed, expected_gone);
    assert!(cache.contains(&other_page_key));
    assert!(doc.take_invalidations().is_empty(), "taken once");

    // ... PDFium no longer draws it, and draws everything else exactly as before ...
    let hidden_render = mirror(doc, 0, scale);
    assert_eq!(differing(&hidden_render, &bare, &[old_box], true), 0);
    assert_eq!(differing(&hidden_render, &original, &[old_box], false), 0);

    // ... and the overlay draws it, once, at the new place.
    let overlay = doc.overlay(0);
    assert_eq!(overlay.len(), 1);
    assert_eq!(overlay[0].id, AnnotId::from(SQUARE));
    let shown = doc.render_page_composite(0, scale).unwrap();
    assert_eq!(
        differing(&shown, &bare, &[old_box], true),
        0,
        "nothing at the old place"
    );
    assert_eq!(differing(&shown, &original, &[old_box, new_box], false), 0);
    let red_after = reddish(&shown, Some(new_box));
    assert!(
        red_after.abs_diff(red_before) * 10 < red_before,
        "one square's worth of red at the new place: {red_after} against {red_before}"
    );
    assert_eq!(
        reddish(&shown, None),
        red_after + reddish(&original, None) - red_before
    );

    // A second edit of the same annotation costs no tiles and no backend change.
    doc.apply(Command::modify(moved(&square, dx, dy + 10.0)))
        .unwrap();
    assert!(doc.take_invalidations().is_empty());
    assert_eq!(doc.hidden_annotations().len(), 1);
    doc.undo().unwrap();

    // A reopened mirror (a layer toggle does this) gets the hidden set again.
    let old_render = doc.render_id();
    doc.reopen_render(Vec::new()).unwrap();
    assert_ne!(doc.render_id(), old_render);
    assert!(doc.annotations(0).unwrap()[0].hidden);
    assert_eq!(changed(&mirror(doc, 0, scale), &hidden_render), 0);
    assert_eq!(
        doc.render_page_composite(0, scale).unwrap().pixels,
        shown.pixels
    );

    // The picture on screen is the picture the saved file gives.
    let saved = doc.save().unwrap();
    assert_eq!((saved.added, saved.modified, saved.deleted), (0, 1, 0));
    assert!(doc.hidden_annotations().is_empty() && doc.overlay(0).is_empty());
    assert!(doc.annotations(0).unwrap().iter().all(|a| !a.hidden));
    let after_save = mirror(doc, 0, scale);
    let bad = mismatch(&shown, &after_save);
    assert!(
        bad <= 10,
        "{bad} pixels differ between the screen and the saved file"
    );

    // Undo of the move after the save: hidden again, drawn by the overlay at the old place.
    doc.undo().unwrap();
    assert_eq!(doc.hidden_annotations().len(), 1);
    let back = doc.render_page_composite(0, scale).unwrap();
    assert_eq!(differing(&back, &bare, &[new_box], true), 0);
    let red_back = reddish(&back, Some(old_box));
    assert!(red_back.abs_diff(red_before) * 10 < red_before);
}

#[test]
fn deleting_hides_and_kinds_we_cannot_draw_stay_with_pdfium() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let geometry = *doc.page_geometry(0).unwrap();
    let scale = 2.0;
    let original = mirror(doc, 0, scale);
    let bare = page_only(doc, 0, scale);

    // Moving the FreeText: we cannot draw it, so PDFium goes on drawing it from its own
    // appearance, at its new place (the mirror is given the new rectangle; the file is not
    // touched). Nothing is hidden and nothing is in the overlay.
    let free = find(doc, FREETEXT);
    assert!(!free.is_modelled());
    let free_rect = free.geometry.bounds();
    let by = Vec2::new(0.0, -80.0);
    doc.apply(Command::modify(moved(&free, by.x, by.y)))
        .unwrap();
    assert!(doc.hidden_annotations().is_empty());
    assert!(doc.overlay(0).is_empty(), "no stand-in outline is needed");
    let moved_now = doc.moved_annotations();
    assert_eq!(moved_now.len(), 1);
    assert_eq!(moved_now[0].0, AnnotId::from(FREETEXT));
    assert_eq!(moved_now[0].1.index, 2);
    assert_eq!(moved_now[0].1.file, free_rect);
    assert_eq!(moved_now[0].1.shown, free_rect + by);
    let inv = doc.take_invalidations();
    assert_eq!(inv.len(), 2, "the old place and the new one");
    assert!(inv[0].rect.contains_rect(free_rect) && inv[1].rect.contains_rect(free_rect + by));
    let old_box = pixel_box(&geometry, scale, free_rect, 3.0);
    let new_box = pixel_box(&geometry, scale, free_rect + by, 3.0);
    let shown = mirror(doc, 0, scale);
    assert_eq!(
        differing(&shown, &bare, &[old_box], true),
        0,
        "the old place shows the bare page"
    );
    assert!(
        differing(&shown, &original, &[new_box], true) > 100,
        "the text box is drawn at its new place"
    );
    assert_eq!(
        differing(&shown, &original, &[old_box, new_box], false),
        0,
        "nothing else changed"
    );
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
    // A second move goes from the place it is shown at, not from the file's.
    doc.apply(Command::modify(moved(&free, 30.0, -80.0)))
        .unwrap();
    let inv = doc.take_invalidations();
    assert_eq!(inv.len(), 2);
    assert!(inv[0].rect.contains_rect(free_rect + by));
    assert_eq!(
        doc.moved_annotations()[0].1.shown,
        free_rect + Vec2::new(30.0, -80.0)
    );
    doc.undo().unwrap();
    doc.undo().unwrap();
    assert!(doc.moved_annotations().is_empty());
    assert_eq!(doc.take_invalidations().len(), 4);
    assert!(doc.overlay(0).is_empty());
    assert_eq!(
        changed(&mirror(doc, 0, scale), &original),
        0,
        "undo puts it back exactly"
    );

    // Deleting it hides it; nothing paints it.
    doc.apply(Command::delete(free.id.clone())).unwrap();
    let hidden = doc.hidden_annotations();
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].1.index, 2);
    let inv = doc.take_invalidations();
    assert_eq!(inv.len(), 1);
    assert!(inv[0].rect.contains_rect(free_rect) && inv[0].page == 0);
    let free_box = pixel_box(&geometry, scale, free_rect, 3.0);
    let gone = doc.render_page_composite(0, scale).unwrap();
    assert_eq!(differing(&gone, &bare, &[free_box], true), 0);
    assert_eq!(differing(&gone, &original, &[free_box], false), 0);

    // Undo shows it again, from its own appearance.
    doc.undo().unwrap();
    assert!(doc.hidden_annotations().is_empty());
    assert_eq!(doc.take_invalidations().len(), 1);
    assert_eq!(
        changed(&doc.render_page_composite(0, scale).unwrap(), &original),
        0
    );

    // Deleting a markup we can draw, then undoing: it comes back in the overlay, once.
    let line = find(doc, LINE);
    doc.apply(Command::delete(line.id.clone())).unwrap();
    assert_eq!(doc.hidden_annotations().len(), 1);
    assert!(doc.overlay(0).is_empty());
    doc.undo().unwrap();
    assert_eq!(doc.hidden_annotations().len(), 1, "still ours to draw");
    assert_eq!(doc.overlay(0).len(), 1);
    let bad = mismatch(&doc.render_page_composite(0, scale).unwrap(), &original);
    assert!(
        bad <= 60,
        "{bad} pixels: our line against the foreign appearance"
    );
    // Nothing differs from the file, so nothing is written.
    assert!(!doc.save().unwrap().wrote());
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);

    // Restacking a page makes every markup on it ours, so the new order shows.
    let square = AnnotId::from(SQUARE);
    doc.apply(Command::Reorder {
        page: 0,
        order: vec![square.clone()],
    })
    .unwrap();
    let hidden: Vec<AnnotId> = doc.hidden_annotations().into_iter().map(|h| h.0).collect();
    assert!(hidden.contains(&square) && hidden.contains(&AnnotId::from(LINE)));
    assert!(!hidden.contains(&AnnotId::from(FREETEXT)));
    assert_eq!(doc.overlay(0).last().unwrap().id, square);

    // Discarding everything shows the file again.
    doc.discard_changes();
    assert!(!doc.annots_loaded() && doc.hidden_annotations().is_empty());
    assert!(!doc.take_invalidations().is_empty());
    assert_eq!(changed(&mirror(doc, 0, scale), &original), 0);
    assert_eq!(model(doc).len(), 4);
}

// ---- save failure modes ---------------------------------------------------------------------

#[test]
fn a_kill_during_save_leaves_the_original_and_the_changes_intact() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let journal = tempfile::tempdir().unwrap();
    for fail_point in [FailPoint::AfterTmpWrite, FailPoint::BeforeReplace] {
        let (_dir, path) = on_disk(&fixture);
        let mut s = session(&backend, Some(journal.path()));
        let id = open(&mut s, &path);
        let doc = s.document_mut(id).unwrap();
        let square = find(doc, SQUARE);
        doc.apply(Command::modify(moved(&square, 40.0, 40.0)))
            .unwrap();
        doc.apply(Command::AddAnnotations(markups(0, Vec2::new(300.0, 100.0))))
            .unwrap();
        doc.apply(Command::delete(AnnotId::from(LINE))).unwrap();
        let unsaved = model(doc);
        let shown = doc.render_page_composite(0, 1.0).unwrap();
        let hidden = doc.hidden_annotations();
        assert_eq!(hidden.len(), 2);

        // The process "dies" with the temporary file written (and, for the second point,
        // verified): a killed process cannot clean up, so the file is kept.
        let options = SaveOptions {
            fail_point: Some(fail_point),
            keep_tmp_on_failure: true,
            ..SaveOptions::default()
        };
        let err = doc.save_with(&options).unwrap_err();
        assert!(
            matches!(err, DocError::Pdf(PdfError::InjectedFailure(p)) if p == fail_point),
            "{err}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            fixture.bytes,
            "{fail_point:?}"
        );
        let tmp = pl_doc::pl_pdf::tmp_path_for(&path);
        assert!(
            tmp.exists(),
            "{fail_point:?}: the half-finished file is beside it"
        );

        // The document still has everything: changes, history, hidden set, picture, journal.
        assert!(doc.is_modified() && doc.can_undo());
        assert_eq!(model(doc), unsaved);
        assert_eq!(doc.hidden_annotations(), hidden);
        let in_mirror = doc.annotations(0).unwrap();
        assert!(in_mirror[0].hidden && in_mirror[1].hidden && !in_mirror[2].hidden);
        assert_eq!(
            doc.render_page_composite(0, 1.0).unwrap().pixels,
            shown.pixels,
            "{fail_point:?}"
        );
        assert_eq!(doc.pending_journal(), PendingJournal::None);
        let journals = std::fs::read_dir(journal.path()).unwrap().count();
        assert_eq!(
            journals, 1,
            "{fail_point:?}: the journal survives a failed save"
        );

        // The next save goes through (over the stale temporary file) and clears the journal.
        let saved = doc.save().unwrap();
        assert_eq!((saved.added, saved.modified, saved.deleted), (7, 1, 1));
        assert!(!tmp.exists());
        assert!(std::fs::read(&path).unwrap().starts_with(&fixture.bytes));
        assert_eq!(std::fs::read_dir(journal.path()).unwrap().count(), 0);
        assert_eq!(model(doc), unsaved);
    }
}

#[test]
fn a_file_changed_by_another_program_is_not_overwritten() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let square = find(doc, SQUARE);
    doc.apply(Command::modify(moved(&square, 40.0, 40.0)))
        .unwrap();

    // Another session saves a markup into the same file meanwhile.
    let mut other = session(&backend, None);
    let other_id = open(&mut other, &path);
    let other_doc = other.document_mut(other_id).unwrap();
    other_doc
        .apply(Command::AddAnnotations(markups(0, Vec2::new(300.0, 100.0))))
        .unwrap();
    assert!(other_doc.save().unwrap().wrote());
    drop(other);
    let theirs = std::fs::read(&path).unwrap();

    let err = doc.save().unwrap_err();
    assert!(
        matches!(err, DocError::Pdf(PdfError::ChangedOnDisk { .. })),
        "{err}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        theirs,
        "their save is intact"
    );
    assert!(doc.is_modified());

    // Saving under another name still works, and leaves their file alone too.
    let copy = path.with_file_name("mine.pdf");
    let err = doc.save_as(&copy).unwrap_err();
    assert!(
        matches!(err, DocError::Pdf(PdfError::ChangedOnDisk { .. })),
        "{err}"
    );
    assert!(!copy.exists());
    assert_eq!(std::fs::read(&path).unwrap(), theirs);
}

#[test]
fn a_callers_own_edit_set_is_refused_while_markups_are_unsaved() {
    let backend = backend();
    let fixture = pl_testkit::plain();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    doc.apply(Command::AddAnnotations(markups(0, Vec2::new(50.0, 50.0))))
        .unwrap();
    let edits = doc.pdf().unwrap().edit().unwrap();
    let err = doc
        .save_incremental(&edits, &[], &SaveOptions::default())
        .unwrap_err();
    assert!(matches!(err, DocError::UnsavedMarkups), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
    assert!(doc.save().unwrap().wrote());
}

// ---- save as, save optimised ----------------------------------------------------------------

#[test]
fn save_as_moves_the_document_and_leaves_the_original_alone() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();

    // With nothing changed, the new file is a copy.
    let copy = dir.path().join("copy.pdf");
    let saved = doc.save_as(&copy).unwrap();
    assert!(!saved.wrote() && saved.path == copy);
    assert_eq!(std::fs::read(&copy).unwrap(), fixture.bytes);
    assert_eq!(doc.path(), Some(copy.as_path()));

    // With changes: the original plus an incremental update, under the new name.
    let square = find(doc, SQUARE);
    doc.apply(Command::modify(moved(&square, 40.0, 40.0)))
        .unwrap();
    doc.apply(Command::AddAnnotations(markups(0, Vec2::new(300.0, 100.0))))
        .unwrap();
    doc.apply(Command::delete(AnnotId::from(LINE))).unwrap();
    assert_eq!(std::fs::read_dir(journal.path()).unwrap().count(), 1);
    let unsaved = model(doc);
    let shown = doc.render_page_composite(0, 2.0).unwrap();
    let marked = dir.path().join("marked.pdf");
    let saved = doc.save_as(&marked).unwrap();
    assert!(saved.wrote());
    assert_eq!((saved.added, saved.modified, saved.deleted), (7, 1, 1));
    assert_eq!(doc.path(), Some(marked.as_path()));
    assert_eq!(doc.title(), "marked.pdf");
    assert!(!doc.is_modified() && doc.hidden_annotations().is_empty());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        fixture.bytes,
        "the first file"
    );
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        fixture.bytes,
        "the file it was open from"
    );
    assert!(std::fs::read(&marked).unwrap().starts_with(&fixture.bytes));
    assert_eq!(
        std::fs::read_dir(journal.path()).unwrap().count(),
        0,
        "journal"
    );
    assert_eq!(model(doc), unsaved);
    // 4 foreign - 1 deleted + 7 new, as PDFium counts them in the new file.
    assert_eq!(doc.annotations(0).unwrap().len(), 10);
    let bad = mismatch(&mirror(doc, 0, 2.0), &shown);
    assert!(bad <= 40, "{bad}");

    // Later saves go to the new file.
    let note = find(doc, FREETEXT);
    doc.apply(Command::delete(note.id)).unwrap();
    let before = std::fs::read(&marked).unwrap();
    assert!(doc.save().unwrap().wrote());
    let after = std::fs::read(&marked).unwrap();
    assert!(after.len() > before.len() && after.starts_with(&before));
    assert_eq!(std::fs::read(&copy).unwrap(), fixture.bytes);

    // Saving "as" the document's own file is a plain save.
    assert!(!doc.save_as(&marked).unwrap().wrote());

    // The saved copy opens elsewhere with the same annotations.
    let expected = model(doc);
    let mut s2 = session(&backend, None);
    let id2 = open(&mut s2, &marked);
    assert_eq!(model(s2.document_mut(id2).unwrap()), expected);
}

#[test]
fn a_document_from_memory_is_saved_by_save_as() {
    let backend = backend();
    let fixture = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(&backend, None);
    let id = s
        .open_bytes("untitled.pdf", fixture.bytes.clone(), None)
        .unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.apply(Command::AddAnnotations(markups(0, Vec2::new(50.0, 50.0))))
        .unwrap();
    assert!(matches!(doc.save(), Err(DocError::NoFile)));
    let unsaved = model(doc);
    let dest = dir.path().join("from-memory.pdf");
    let saved = doc.save_as(&dest).unwrap();
    assert!(saved.wrote() && saved.added == 7 && saved.path == dest);
    assert_eq!(doc.path(), Some(dest.as_path()));
    assert!(std::fs::read(&dest).unwrap().starts_with(&fixture.bytes));
    assert_eq!(model(doc), unsaved);
    assert_eq!(doc.annotations(0).unwrap().len(), 7);
    assert!(!doc.save().unwrap().wrote());
}

#[test]
fn save_optimised_rewrites_one_revision_without_the_deleted_objects() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();

    // Two incremental saves first, so there is something to optimise away.
    doc.apply(Command::AddAnnotations(markups(0, Vec2::new(300.0, 100.0))))
        .unwrap();
    doc.save().unwrap();
    let line = find(doc, LINE);
    let line_object = line.object_id().unwrap();
    doc.apply(Command::delete(line.id.clone())).unwrap();
    let square = find(doc, SQUARE);
    doc.apply(Command::modify(moved(&square, 40.0, 40.0)))
        .unwrap();
    let unsaved = model(doc);
    let shown = doc.render_page_composite(0, 2.0).unwrap();
    let incremental_len = std::fs::metadata(&path).unwrap().len();

    let saved = doc.save_optimised().unwrap();
    assert!(saved.wrote());
    assert_eq!((saved.added, saved.modified, saved.deleted), (0, 1, 1));
    assert!(!doc.is_modified() && doc.hidden_annotations().is_empty());
    assert_eq!(model(doc), unsaved);
    let bad = mismatch(&mirror(doc, 0, 2.0), &shown);
    assert!(bad <= 40, "{bad}");

    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !bytes.starts_with(&fixture.bytes),
        "a rewrite, not an append"
    );
    let mut file = PdfFile::open(&path).unwrap();
    let parsed = file.document().unwrap();
    assert_eq!(parsed.revision_count(), 1);
    assert!(
        parsed.object(line_object).is_none(),
        "the deleted annotation is gone"
    );
    assert_eq!(parsed.annotation_counts()[0], 10);
    assert!(
        (bytes.len() as u64) < incremental_len + 4096,
        "{} against {incremental_len}",
        bytes.len()
    );

    // Nothing left to write; a second optimised save still rewrites, and changes nothing.
    assert!(!doc.save().unwrap().wrote());
    assert!(doc.save_optimised().unwrap().wrote());
    assert_eq!(model(doc), unsaved);
    let mut s2 = session(&backend, None);
    let id2 = open(&mut s2, &path);
    assert_eq!(model(s2.document_mut(id2).unwrap()), unsaved);

    // Without the store ever loaded it is a plain rewrite.
    let (_dir3, path3) = on_disk(&pl_testkit::multi_page());
    let mut s3 = session(&backend, None);
    let id3 = open(&mut s3, &path3);
    let doc3 = s3.document_mut(id3).unwrap();
    assert!(!doc3.save().unwrap().wrote());
    assert!(doc3.save_optimised().unwrap().wrote());
    assert!(!doc3.annots_loaded());
    assert_eq!(
        doc3.page_count() as usize,
        pl_testkit::multi_page().manifest.page_count()
    );
}

// ---- the crash journal ----------------------------------------------------------------------

/// A varied editing session on the annotations fixture: adds, a merged drag of a foreign
/// markup, a delete of another, undo and redo, a restack. Leaves one step on the redo stack.
fn busy_session(doc: &mut Document) {
    doc.apply(Command::AddAnnotations(markups(0, Vec2::new(300.0, 100.0))))
        .unwrap();
    let square = find(doc, SQUARE);
    for step in 1..=5 {
        let out = doc
            .apply_drag(Command::modify(moved(&square, 8.0 * step as f64, 0.25)), 42)
            .unwrap();
        assert_eq!(out.merged, step > 1);
    }
    doc.apply(Command::delete(AnnotId::from(LINE))).unwrap();
    assert!(doc.undo().unwrap());
    assert!(doc.redo().unwrap());
    let top = model(doc).last().unwrap().clone();
    doc.apply(Command::Batch(vec![
        Command::modify(moved(&top, -3.5, 7.25)),
        Command::Reorder {
            page: 0,
            order: vec![AnnotId::from(SQUARE)],
        },
    ]))
    .unwrap();
    doc.apply(Command::delete(AnnotId::from(FREETEXT))).unwrap();
    assert!(doc.undo().unwrap());
}

struct Seen {
    model: Vec<Annotation>,
    undo: usize,
    redo: usize,
    hidden: Vec<AnnotId>,
    picture: Vec<u8>,
}

fn see(doc: &mut Document) -> Seen {
    Seen {
        model: model(doc),
        undo: doc.editor().unwrap().undo_len(),
        redo: doc.editor().unwrap().redo_len(),
        hidden: doc.hidden_annotations().into_iter().map(|h| h.0).collect(),
        picture: doc.render_page_composite(0, 1.0).unwrap().pixels,
    }
}

fn assert_same(now: &Seen, then: &Seen, what: &str) {
    assert_eq!(now.model, then.model, "{what}: model");
    assert_eq!(
        (now.undo, now.redo),
        (then.undo, then.redo),
        "{what}: history"
    );
    assert_eq!(now.hidden, then.hidden, "{what}: hidden set");
    assert!(now.picture == then.picture, "{what}: picture");
}

#[test]
fn journal_replay_gives_the_state_before_the_kill() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let journal_files = || -> Vec<PathBuf> {
        std::fs::read_dir(journal.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    };

    // Before any edit there is no journal, loaded store or not.
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.pending_journal(), PendingJournal::None);
    let file_model = model(doc);
    assert!(journal_files().is_empty());
    assert_eq!(doc.recovered_changes(), 0);

    busy_session(doc);
    let before = see(doc);
    assert!(doc.is_modified());
    assert_eq!((before.undo, before.redo), (4, 1));
    assert_eq!(journal_files().len(), 1);
    // The kill: the session is dropped with nothing saved.
    drop(s);
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);

    // Reopened: the journal is offered, and loading the annotations replays it.
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.pending_journal(), PendingJournal::Recoverable);
    assert!(!doc.annots_loaded());
    doc.load_annots().unwrap();
    assert!(doc.recovered_changes() >= 12, "{}", doc.recovered_changes());
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    assert!(doc.is_modified());
    assert_same(&see(doc), &before, "after the first kill");
    assert_eq!(doc.pending_journal(), PendingJournal::None);

    // Work goes on in the same journal: redo the undone delete, add something, kill again.
    assert!(doc.redo().unwrap());
    doc.apply(Command::AddAnnotations(markups(0, Vec2::new(60.0, 60.0))))
        .unwrap();
    let later = see(doc);
    drop(s);
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert_same(&see(doc), &later, "after the second kill");

    // The recovered history undoes all the way back to the file, and forward again.
    while doc.undo().unwrap() {}
    assert_eq!(model(doc), file_model);
    assert!(!doc.is_modified());
    assert!(!doc.annots().unwrap().set().is_dirty());
    while doc.redo().unwrap() {}
    assert_eq!(model(doc), later.model);

    // A torn last record (the kill came in the middle of a write) costs that record only.
    let journal_file = journal_files().pop().unwrap();
    drop(s);
    let whole = std::fs::read(&journal_file).unwrap();
    std::fs::write(&journal_file, &whole[..whole.len() - 3]).unwrap();
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert_eq!(doc.warnings().len(), 1, "{:?}", doc.warnings());
    // The lost record was the last redo: one more redo gets there.
    assert_eq!(doc.editor().unwrap().redo_len(), 1);
    assert!(doc.redo().unwrap());
    assert_eq!(model(doc), later.model);

    // A successful save discards the journal; the next open has nothing to recover.
    let saved = doc.save().unwrap();
    assert!(saved.wrote());
    assert!(journal_files().is_empty());
    drop(s);
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.pending_journal(), PendingJournal::None);
    assert_eq!(model(doc), later.model);
    assert_eq!(doc.recovered_changes(), 0);
    assert!(!doc.is_modified() && !doc.can_undo());
}

#[test]
fn undo_across_a_save_is_journaled_as_what_it_did() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();

    // Three steps, saved; then two of them undone, one redone, and a new step: the journal
    // that starts after the save has no record of the commands being undone.
    let extra = markups(0, Vec2::new(300.0, 100.0));
    doc.apply(Command::AddAnnotations(extra.clone())).unwrap();
    doc.apply(Command::delete(AnnotId::from(LINE))).unwrap();
    let square = find(doc, SQUARE);
    doc.apply(Command::modify(moved(&square, 40.0, 40.0)))
        .unwrap();
    doc.save().unwrap();
    let saved_bytes = std::fs::read(&path).unwrap();
    assert!(doc.undo().unwrap()); // the move
    assert!(doc.undo().unwrap()); // the delete: the line comes back, as a new annotation
    assert!(doc.redo().unwrap()); // and goes again
    assert!(doc.undo().unwrap()); // and comes back
    doc.apply(Command::delete(extra[0].id.clone())).unwrap();
    let before = see(doc);
    assert_eq!(before.model.len(), 4 + 7 - 1);
    drop(s);
    assert_eq!(std::fs::read(&path).unwrap(), saved_bytes);

    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert_eq!(doc.recovered_changes(), 5);
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    let now = see(doc);
    assert_eq!(now.model, before.model);
    assert!(now.picture == before.picture);
    assert!(doc.is_modified());
    // Only the step made after the save is in the recovered history.
    assert_eq!((now.undo, now.redo), (1, 0));
    let saved = doc.save().unwrap();
    assert_eq!((saved.added, saved.modified, saved.deleted), (1, 1, 1));
    assert_eq!(model(doc), before.model);
}

#[test]
fn a_journal_for_another_state_of_the_file_is_set_aside() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    busy_session(s.document_mut(id).unwrap());
    drop(s);

    // The file is then saved by a session that knows nothing of the journal.
    let mut other = session(&backend, None);
    let other_id = open(&mut other, &path);
    let other_doc = other.document_mut(other_id).unwrap();
    assert_eq!(
        other_doc.pending_journal(),
        PendingJournal::None,
        "journaling is off here"
    );
    other_doc
        .apply(Command::delete(AnnotId::from(SQUARE)))
        .unwrap();
    other_doc.save().unwrap();
    let on_disk_model = model(other_doc);
    drop(other);

    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.pending_journal(), PendingJournal::Stale);
    doc.load_annots().unwrap();
    assert_eq!(doc.recovered_changes(), 0);
    assert_eq!(
        model(doc),
        on_disk_model,
        "nothing was replayed onto the wrong file"
    );
    assert!(!doc.is_modified());
    assert_eq!(doc.warnings().len(), 1);
    let names: Vec<String> = std::fs::read_dir(journal.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with(".plj.stale"), "{names:?}");

    // Editing starts a fresh journal beside it.
    doc.apply(Command::AddAnnotations(markups(0, Vec2::new(60.0, 60.0))))
        .unwrap();
    assert_eq!(std::fs::read_dir(journal.path()).unwrap().count(), 2);

    // Closing without saving: the journal goes, so nothing is offered next time.
    doc.discard_changes();
    assert_eq!(std::fs::read_dir(journal.path()).unwrap().count(), 1);
    drop(s);
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    assert_eq!(
        s.document_mut(id).unwrap().pending_journal(),
        PendingJournal::None
    );
}

// ---- clipboard ------------------------------------------------------------------------------

#[test]
fn paste_across_pages_and_documents_gives_an_equal_model() {
    let backend = backend();
    let (_dir_a, path_a) = on_disk(&pl_testkit::rotated_pages());
    let (_dir_b, path_b) = on_disk(&pl_testkit::crop_box());
    let (_dir_c, path_c) = on_disk(&pl_testkit::annotations());
    let mut s = session(&backend, None);
    let a = open(&mut s, &path_a);
    let b = open(&mut s, &path_b);
    let c = open(&mut s, &path_c);
    let og = origin();

    // Source markups on page 1 of A.
    let sources = markups(0, Vec2::new(40.0, 40.0));
    let ids: Vec<AnnotId> = sources.iter().map(|m| m.id.clone()).collect();
    let doc = s.document_mut(a).unwrap();
    doc.apply(Command::AddAnnotations(sources.clone())).unwrap();

    // What a paste must give: the source, on the target page, moved, with a new id.
    let expect = |pasted: &[Annotation], page: usize, by: Vec2| {
        assert_eq!(pasted.len(), sources.len());
        for (p, source) in pasted.iter().zip(&sources) {
            let mut want = source.clone();
            want.id = p.id.clone();
            want.page = page;
            want.translate(by);
            assert_eq!(*p, want);
            assert!(!ids.contains(&p.id));
        }
    };
    let pasted_on = |doc: &mut Document, new: &[AnnotId]| -> Vec<Annotation> {
        let set = doc.annots().unwrap().set();
        new.iter()
            .map(|id| set.get(id).unwrap().detached())
            .collect()
    };

    // Another page of the same document (a rotated one), in place.
    let clip = doc.copy(&ids).unwrap();
    assert_eq!((clip.len(), clip.skipped), (7, 0));
    let new = doc.paste(&clip, 2, PastePlacement::InPlace, &og).unwrap();
    expect(&pasted_on(doc, &new), 2, Vec2::ZERO);
    assert_eq!(doc.selection(), new, "the pasted markups are selected");
    assert_eq!(
        doc.editor().unwrap().undo_len(),
        2,
        "a paste is one undo step"
    );

    // Another document, through the clipboard's JSON, with an offset.
    let json = clip.to_json();
    let doc_b = s.document_mut(b).unwrap();
    let clip_b = Clipboard::from_json(&json).unwrap();
    let by = Vec2::new(12.5, -7.25);
    let new_b = doc_b
        .paste(&clip_b, 1, PastePlacement::Offset(by), &og)
        .unwrap();
    expect(&pasted_on(doc_b, &new_b), 1, by);

    // Both documents save, and read back what was pasted.
    for (doc_id, new, page, by) in [(a, &new, 2usize, Vec2::ZERO), (b, &new_b, 1, by)] {
        let doc = s.document_mut(doc_id).unwrap();
        let unsaved = model(doc);
        assert!(doc.save().unwrap().wrote());
        assert_eq!(model(doc), unsaved);
        expect(&pasted_on(doc, new), page, by);
        let path = doc.path().unwrap().to_path_buf();
        let mut fresh = session(&backend, None);
        let fresh_id = open(&mut fresh, &path);
        let reread = fresh.document_mut(fresh_id).unwrap();
        assert_eq!(model(reread), unsaved);
        expect(&pasted_on(reread, new), page, by);
    }

    // Cut is copy and delete in one step; undo brings the markups back.
    let doc = s.document_mut(a).unwrap();
    let all = model(doc);
    let cut = doc.cut(&ids[..3]).unwrap();
    assert_eq!(cut.len(), 3);
    assert_eq!(model(doc).len(), all.len() - 3);
    doc.undo().unwrap();
    assert_eq!(model(doc), all);

    // Foreign markups copy too, except the kinds that cannot be created.
    let doc_c = s.document_mut(c).unwrap();
    let foreign: Vec<AnnotId> = model(doc_c).iter().map(|m| m.id.clone()).collect();
    let clip_c = doc_c.copy(&foreign).unwrap();
    assert_eq!(
        (clip_c.len(), clip_c.skipped),
        (3, 1),
        "the FreeText is left out"
    );
    let square = find(doc_c, SQUARE);
    let doc = s.document_mut(a).unwrap();
    let new_c = doc
        .paste(
            &clip_c,
            0,
            PastePlacement::CenteredAt(Point::new(300.0, 300.0)),
            &og,
        )
        .unwrap();
    let pasted = pasted_on(doc, &new_c);
    assert_eq!(pasted.len(), 3);
    let mut want = square.duplicate(&og);
    want.id = pasted[0].id.clone();
    want.geometry = pasted[0].geometry.clone();
    assert_eq!(pasted[0], want, "style, text and kind come across");
    assert_eq!(pasted[0].meta.author.as_deref(), Some("Plumbline Tester"));
    let bounds = pasted
        .iter()
        .map(|p| p.geometry.bounds())
        .reduce(|x, y| x.union(y))
        .unwrap();
    assert!((bounds.center() - Point::new(300.0, 300.0)).hypot() < 1e-9);
    assert!(doc.save().unwrap().wrote());
}

// ---- the tool adapter -----------------------------------------------------------------------

#[test]
fn the_doc_view_shows_tools_what_they_need() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();

    // Before the store is loaded the view is empty, and asking does not load it.
    {
        let view = DocSnapshot::new(doc);
        assert_eq!(view.page_count(), 1);
        assert!(view.annotations_on(0).is_empty() && view.selection().is_empty());
        assert!(view.hit(0, Point::new(160.0, 400.0), 2.0).is_none());
        assert!(view.page_geometry(0).is_some() && view.page_geometry(7).is_none());
    }
    assert!(!doc.annots_loaded());

    doc.load_annots().unwrap();
    let new = Annotation::square(0, Rect::new(90.0, 390.0, 230.0, 490.0), &origin());
    let new_id = new.id.clone();
    doc.apply(Command::add(new)).unwrap();
    doc.select([AnnotId::from(SQUARE)]).unwrap();
    doc.select_also(new_id.clone()).unwrap();
    let view = DocSnapshot::new(doc);
    assert_eq!(view.annotations_on(0).len(), 5);
    assert_eq!(view.annotation(&new_id).unwrap().kind, AnnotKind::Square);
    assert_eq!(view.selection(), [AnnotId::from(SQUARE), new_id.clone()]);
    assert!(view.is_selected(&new_id) && !view.is_selected(&AnnotId::from(LINE)));
    assert_eq!(view.selected().len(), 2);
    // On the lower edge of the foreign square only; the new one is drawn around it.
    let hits = view.hits(0, Point::new(160.0, 401.0), 2.0);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, AnnotId::from(SQUARE));
    assert_eq!(
        view.hit(0, Point::new(160.0, 390.0), 2.0).unwrap().id,
        new_id
    );
    // A corner handle of the annotation selected last wins.
    let (owner, handle) = view.handle_at(0, Point::new(90.0, 390.0), 3.0).unwrap();
    assert_eq!(owner.id, new_id);
    assert_eq!(handle.at, Point::new(90.0, 390.0));
    assert!(view.handle_at(0, Point::new(10.0, 10.0), 3.0).is_none());
    assert_eq!(
        doc.hit_annotation(0, Point::new(160.0, 390.0), 2.0)
            .unwrap()
            .id,
        new_id
    );

    // Selection changes are not edits.
    assert!(!doc.toggle_selected(&new_id).unwrap());
    doc.select_page(0).unwrap();
    assert_eq!(doc.selection().len(), 5);
    doc.clear_selection();
    assert_eq!(doc.editor().unwrap().undo_len(), 1);
    let revision = doc.edit_revision();
    doc.undo().unwrap();
    assert!(doc.edit_revision() > revision);
    let _ = Geometry::Rect(Rect::ZERO);
}

// ---- the command file -----------------------------------------------------------------------

struct Pictures(Vec<(PathBuf, u32, u32)>, Vec<String>);

impl pl_doc::script::ScriptHost for Pictures {
    fn write_png(&mut self, path: &Path, bitmap: &Bitmap) -> std::io::Result<()> {
        self.0
            .push((path.to_path_buf(), bitmap.width, bitmap.height));
        Ok(())
    }

    fn log(&mut self, line: &str) {
        self.1.push(line.to_string());
    }
}

#[test]
fn a_command_file_drives_a_whole_editing_session() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (dir, path) = on_disk(&fixture);
    let json = r#"{
      "author": "Script",
      "date": "D:20261124101500Z",
      "steps": [
        {"op": "open", "path": "annotations.pdf"},
        {"op": "expect", "count": 4, "modified": false},
        {"op": "add", "id": "box", "kind": "Square", "page": 1, "rect": [300, 100, 420, 180],
         "style": {"stroke": [0, 0, 1], "fill": [1, 1, 0], "width": 2, "opacity": 0.5},
         "contents": "Check this"},
        {"op": "add", "id": "arrow", "kind": "Line", "page": 1, "points": [[300, 200], [450, 240]],
         "style": {"line_end": "ClosedArrow", "width": 3}},
        {"op": "add", "kind": "Ink", "page": 1, "strokes": [[[310, 260], [330, 290], [360, 270]]]},
        {"op": "add", "kind": "Highlight", "page": 1, "quads": [[72, 596, 240, 612]]},
        {"op": "add", "kind": "Polygon", "page": 1, "points": [[460, 100], [520, 100], [490, 160]],
         "style": {"dash": [4, 2], "cloud": null}},
        {"op": "modify", "id": "box", "move": [10, -5], "style": {"width": 4, "fill": null}},
        {"op": "modify", "id": "fixture-square-1", "move": [0, 30]},
        {"op": "delete", "id": "fixture-line-1"},
        {"op": "undo"},
        {"op": "redo"},
        {"op": "copy", "ids": ["box", "arrow"]},
        {"op": "paste", "page": 1, "offset": [0, 250]},
        {"op": "expect", "count": 10, "page": 1, "modified": true},
        {"op": "render", "page": 1, "scale": 0.5, "output": "shown.png"},
        {"op": "save_as", "path": "out/marked.pdf"},
        {"op": "expect", "count": 10, "modified": false},
        {"op": "save"},
        {"op": "delete", "id": "arrow"},
        {"op": "save_optimised"},
        {"op": "expect", "count": 9}
      ]
    }"#;
    std::fs::create_dir(dir.path().join("out")).unwrap();
    let script = pl_doc::script::Script::from_json(json).unwrap();
    let mut s = session(&backend, None);
    let mut host = Pictures(Vec::new(), Vec::new());
    let report = pl_doc::script::run(&mut s, &script, dir.path(), &mut host).unwrap();
    assert_eq!(report.steps, 22);
    assert_eq!(host.1.len(), 22);
    assert_eq!(host.1[18], "19: save nothing to write");
    assert_eq!(host.0, [(dir.path().join("shown.png"), 306, 396)]);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        fixture.bytes,
        "save_as left the original"
    );

    let marked = dir.path().join("out").join("marked.pdf");
    let doc = s.document_mut(report.document.unwrap()).unwrap();
    assert_eq!(doc.path(), Some(marked.as_path()));
    let boxed = find(doc, "box");
    assert_eq!(
        boxed.geometry,
        Geometry::Rect(Rect::new(310.0, 95.0, 430.0, 175.0))
    );
    assert_eq!(boxed.style.width, 4.0);
    assert_eq!(boxed.style.fill, None);
    assert_eq!(boxed.style.stroke, Some(Rgb::new(0.0, 0.0, 1.0)));
    assert_eq!(boxed.meta.author.as_deref(), Some("Script"));
    assert_eq!(boxed.meta.contents.as_deref(), Some("Check this"));
    assert_eq!(
        find(doc, SQUARE).geometry,
        Geometry::Rect(SQUARE_RECT + Vec2::new(0.0, 30.0))
    );
    let mut fresh = session(&backend, None);
    let fresh_id = open(&mut fresh, &marked);
    let reread = fresh.document_mut(fresh_id).unwrap();
    assert_eq!(reread.annotations(0).unwrap().len(), 9);
    assert_eq!(model(reread), model(doc));

    // A failing step stops the run and is named.
    let bad = pl_doc::script::Script::from_json(
        r#"{"steps": [
            {"op": "open", "path": "annotations.pdf"},
            {"op": "add", "kind": "Square", "page": 3, "rect": [0, 0, 5, 5]}
        ]}"#,
    )
    .unwrap();
    let err = pl_doc::script::run(&mut s, &bad, dir.path(), &mut host).unwrap_err();
    assert!(
        err.to_string()
            .contains("step 2 (add): page 3 is out of range"),
        "{err}"
    );
    let nothing_open = pl_doc::script::Script::from_json(r#"{"steps": [{"op": "undo"}]}"#).unwrap();
    assert!(pl_doc::script::run(&mut s, &nothing_open, dir.path(), &mut host).is_err());
}

// ---- every fixture --------------------------------------------------------------------------

/// Classic and stream cross-references, object streams, RC4 and AES-256 encryption, the
/// linearised layout, layers, a form, inherited boxes, a broken page: markups go into each
/// by incremental save, come back equal, and survive a full rewrite.
#[test]
fn markups_save_into_every_fixture_and_read_back_equal() {
    let backend = backend();
    for fixture in pl_testkit::all_fixtures_with(&pl_testkit::Options::small()) {
        let name = fixture.manifest.name.clone();
        let (_dir, path) = on_disk(&fixture);
        let mut s = session(&backend, None);
        let id = open(&mut s, &path);
        let doc = s.document_mut(id).unwrap();
        // The last page that can be read (one fixture has a page that cannot).
        let page = (0..doc.page_count())
            .rev()
            .find(|p| doc.page_geometry(*p).is_ok())
            .unwrap_or_else(|| panic!("{name}: no readable page"));
        let geometry = *doc.page_geometry(page).unwrap();
        let had = model(doc);
        let pdfium_had = doc.annotations(page).unwrap().len();

        let new = markups(page as usize, placed(&geometry));
        let n = new.len();
        doc.apply(Command::AddAnnotations(new.clone()))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let saved = doc.save().unwrap_or_else(|e| panic!("{name}: save: {e}"));
        assert_eq!(
            (saved.added, saved.modified, saved.deleted),
            (n, 0, 0),
            "{name}"
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            bytes.starts_with(&fixture.bytes) && bytes.len() > fixture.bytes.len(),
            "{name}"
        );
        assert_eq!(
            doc.annotations(page).unwrap().len(),
            pdfium_had + n,
            "{name}"
        );
        let mut expected = had.clone();
        expected.extend(new.iter().cloned());
        // New markups go on top of their page; pages after it keep their place.
        expected.sort_by_key(|a| a.page);
        assert_eq!(model(doc), expected, "{name}: after the save");
        assert!(!doc.save().unwrap().wrote(), "{name}: second save");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}: second save");

        // An edit and a delete of saved markups, then a fresh session reads the same.
        doc.apply(Command::modify(moved(&new[0], 20.0, 12.0)))
            .unwrap();
        doc.apply(Command::delete(new[1].id.clone())).unwrap();
        let saved = doc
            .save()
            .unwrap_or_else(|e| panic!("{name}: second edit: {e}"));
        assert_eq!(
            (saved.added, saved.modified, saved.deleted),
            (0, 1, 1),
            "{name}"
        );
        let edited = model(doc);
        assert_eq!(edited.len(), had.len() + n - 1, "{name}");
        {
            let mut fresh = session(&backend, None);
            let fresh_id = open(&mut fresh, &path);
            assert_eq!(
                model(fresh.document_mut(fresh_id).unwrap()),
                edited,
                "{name}: reread"
            );
        }

        // The full rewrite keeps every annotation and the page count.
        let pages = doc.page_count();
        let saved = doc
            .save_optimised()
            .unwrap_or_else(|e| panic!("{name}: save optimised: {e}"));
        assert!(saved.wrote(), "{name}");
        assert_eq!(doc.page_count(), pages, "{name}");
        assert_eq!(model(doc), edited, "{name}: after the rewrite");
        assert_eq!(
            doc.annotations(page).unwrap().len(),
            pdfium_had + n - 1,
            "{name}"
        );
        let mut fresh = session(&backend, None);
        let fresh_id = open(&mut fresh, &path);
        assert_eq!(
            model(fresh.document_mut(fresh_id).unwrap()),
            edited,
            "{name}: reread after the rewrite"
        );
    }
}
