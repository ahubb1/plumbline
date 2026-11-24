//! Text boxes, stamps, pictures, text markups and the markups-list commands through
//! [`Document`] and the JSON command driver, against real PDFium and the pl-testkit fixtures:
//! what is written reads upright as the page is displayed on rotated, `/UserUnit` and CropBox
//! pages, text markups cover the characters PDFium reports, the journal replays the new
//! commands, and a saved file reopens with an equal model. Headless.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::markup_ops;
use pl_doc::pl_annot::{
    AnnotId, AnnotKind, Annotation, Flags, Geometry, Origin, PdfDate, ReviewState, RgbaImage,
    StampSpec,
};
use pl_doc::pl_geom::{PageGeometry, Point, Rect, Vec2};
use pl_doc::pl_pdf::{Object, PdfDocument};
use pl_doc::pl_render::{Bitmap, PdfiumBackend, RenderBackend};
use pl_doc::pl_takeoff::{ColumnType, Schema, Value};
use pl_doc::script::{self, LogOnly, Script};
use pl_doc::{Command, Document, Session, SessionOptions};
use pl_testkit::Fixture;

fn backend() -> Arc<PdfiumBackend> {
    let dll = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    let dll = std::path::absolute(dll).unwrap();
    Arc::new(PdfiumBackend::new(Some(&dll)).expect("load vendored pdfium.dll"))
}

fn on_disk(fixture: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    (dir, path)
}

fn session(backend: &Arc<PdfiumBackend>, journal: Option<&Path>) -> Session {
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    s.set_journal_dir(journal.map(Path::to_path_buf));
    s
}

fn origin(author: &str) -> Origin {
    Origin::new(author, PdfDate::parse("D:20261124101500Z").unwrap())
}

/// A picture whose left half is red and right half blue, with a noisy green channel so that
/// it does not compress to nothing.
fn flag(width: u32, height: u32) -> RgbaImage {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    let mut noise = 0x2545_f491u32;
    for _y in 0..height {
        for x in 0..width {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let g = (noise >> 24) as u8 % 40;
            if x < width / 2 {
                pixels.extend_from_slice(&[230, g, 20, 255]);
            } else {
                pixels.extend_from_slice(&[20, g, 230, 255]);
            }
        }
    }
    RgbaImage::new(width, height, pixels).unwrap()
}

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

/// The pixels inside `within` that `after` has and `before` has not, as display coordinates.
fn new_ink(before: &Bitmap, after: &Bitmap, within: [u32; 4]) -> Vec<(u32, u32)> {
    assert_eq!((before.width, before.height), (after.width, after.height));
    let mut out = Vec::new();
    for y in within[1]..within[3].min(after.height) {
        for x in within[0]..within[2].min(after.width) {
            let (a, b) = (after.pixel(x, y), before.pixel(x, y));
            let dark = u32::from(a[0]) + u32::from(a[1]) + u32::from(a[2]) < 450;
            if a != b && dark {
                out.push((x, y));
            }
        }
    }
    out
}

/// Whether ink has the shape of an upright capital L as displayed: the stem down the left,
/// the foot along the bottom. Any other quarter turn puts the stem or the foot elsewhere.
fn reads_as_upright_l(ink: &[(u32, u32)]) -> Result<(), String> {
    if ink.len() < 200 {
        return Err(format!("only {} pixels of ink", ink.len()));
    }
    let x0 = ink.iter().map(|p| p.0).min().unwrap();
    let x1 = ink.iter().map(|p| p.0).max().unwrap() + 1;
    let y0 = ink.iter().map(|p| p.1).min().unwrap();
    let y1 = ink.iter().map(|p| p.1).max().unwrap() + 1;
    let (w, h) = (x1 - x0, y1 - y0);
    let count = |f: &dyn Fn(u32, u32) -> bool| ink.iter().filter(|p| f(p.0, p.1)).count();
    let left = count(&|x, _| x < x0 + w / 3);
    let right = count(&|x, _| x >= x1 - w / 3);
    let top = count(&|_, y| y < y0 + h / 3);
    let bottom = count(&|_, y| y >= y1 - h / 3);
    if left > right * 2 && bottom > top * 3 / 2 && h > w {
        Ok(())
    } else {
        Err(format!(
            "ink {w} x {h} px: left {left}, right {right}, top {top}, bottom {bottom}"
        ))
    }
}

/// Mean position of the pixels inside `within` that `pick` accepts.
fn centroid(
    b: &Bitmap,
    within: [u32; 4],
    pick: impl Fn([u8; 4]) -> bool,
) -> Option<(f64, f64, usize)> {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0usize);
    for y in within[1]..within[3].min(b.height) {
        for x in within[0]..within[2].min(b.width) {
            if pick(b.pixel(x, y)) {
                sx += f64::from(x);
                sy += f64::from(y);
                n += 1;
            }
        }
    }
    (n > 0).then(|| (sx / n as f64, sy / n as f64, n))
}

/// Pixels of `a` with no counterpart in `b` within one pixel and 60 levels.
fn one_way(a: &Bitmap, b: &Bitmap) -> usize {
    let (w, h) = (a.width as i32, a.height as i32);
    let mut bad = 0;
    for y in 0..h {
        for x in 0..w {
            let pa = a.pixel(x as u32, y as u32);
            if pa == b.pixel(x as u32, y as u32) {
                continue;
            }
            let mut lo = [255i32; 3];
            let mut hi = [0i32; 3];
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w || ny >= h {
                        continue;
                    }
                    let pb = b.pixel(nx as u32, ny as u32);
                    for c in 0..3 {
                        lo[c] = lo[c].min(i32::from(pb[c]));
                        hi[c] = hi[c].max(i32::from(pb[c]));
                    }
                }
            }
            if (0..3).any(|c| i32::from(pa[c]) < lo[c] - 60 || i32::from(pa[c]) > hi[c] + 60) {
                bad += 1;
            }
        }
    }
    bad
}

/// Whether `read` (from a saved file) is the annotation `made` (in memory): every field
/// equal, coordinates to within what single precision in the file can hold.
fn assert_same(read: &Annotation, made: &Annotation, what: &str) {
    let points = |a: &Annotation| -> Vec<Point> {
        let mut out = Vec::new();
        match &a.geometry {
            Geometry::Rect(r) => out.extend([Point::new(r.x0, r.y0), Point::new(r.x1, r.y1)]),
            Geometry::Line { a, b } => out.extend([*a, *b]),
            Geometry::Points(p) => out.extend(p.iter().copied()),
            Geometry::Ink(strokes) => out.extend(strokes.iter().flatten().copied()),
            Geometry::Quads(quads) => out.extend(quads.iter().flatten().copied()),
        }
        if let Some(t) = &a.free_text {
            out.extend(t.callout.iter().copied());
        }
        out
    };
    let (p, q) = (points(read), points(made));
    assert_eq!(p.len(), q.len(), "{what}: {} geometry", made.id);
    for (a, b) in p.iter().zip(&q) {
        assert!(
            (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3,
            "{what}: {} moved from {b:?} to {a:?}",
            made.id
        );
    }
    let mut aligned = read.clone();
    aligned.geometry = made.geometry.clone();
    if let (Some(t), Some(m)) = (aligned.free_text.as_mut(), &made.free_text) {
        t.callout = m.callout.clone();
    }
    assert_eq!(
        &aligned, made,
        "{what}: {} reads back as it was written",
        made.id
    );
}

struct Placed {
    page: u32,
    rotate: i32,
    label: String,
    l_rect: Rect,
    stamp_rect: Rect,
    image_corner: Point,
}

/// Where the test puts its markups on each page of a fixture: clear of the fixture's own
/// marks, and for the stamp with its sides swapped on a page displayed sideways, so that it
/// is wide as displayed.
fn placements(fixture: &Fixture) -> Vec<Placed> {
    fixture
        .manifest
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let [x0, y0, x1, y1] = p.visible_box();
            // Whole units: exact in the single precision a saved file holds.
            let h = (y1 - y0).floor();
            let (up_l, up_i) = ((0.55 * h).floor(), (0.3 * h).floor());
            let sideways = p.rotate.rem_euclid(180) == 90;
            let (sw, sh) = if sideways { (36.0, 96.0) } else { (96.0, 36.0) };
            Placed {
                page: i as u32 + 1,
                rotate: p.rotate,
                label: p.texts[0].text.clone(),
                l_rect: Rect::new(x0 + 40.0, y0 + up_l, x0 + 104.0, y0 + up_l + 64.0),
                stamp_rect: Rect::new(x1 - 44.0 - sw, y0 + 44.0, x1 - 44.0, y0 + 44.0 + sh),
                image_corner: Point::new(x0 + 40.0, y0 + up_i),
            }
        })
        .collect()
}

fn steps_for(placed: &Placed, text: &pl_testkit::TextTruth) -> Vec<serde_json::Value> {
    let p = placed.page;
    let r = |r: Rect| serde_json::json!([r.x0, r.y0, r.x1, r.y1]);
    let mid = text.origin[1] + 0.3 * text.font_size;
    serde_json::json!([
        { "op": "add", "id": format!("L{p}"), "kind": "FreeText", "intent": "typewriter",
          "page": p, "rect": r(placed.l_rect), "text": "L",
          "text_style": { "font": "Helvetica-Bold", "size": 56, "color": [0, 0, 0],
                          "align": "centre", "v_align": "middle", "padding": 1 } },
        { "op": "add", "id": format!("S{p}"), "kind": "Stamp", "page": p,
          "rect": r(placed.stamp_rect), "stamp": "for_review" },
        { "op": "add", "id": format!("I{p}"), "kind": "Stamp", "page": p,
          "rect": [placed.image_corner.x, placed.image_corner.y, 0, 0], "image": "flag.png" },
        { "op": "markup_text", "id": format!("H{p}"), "kind": "Highlight", "page": p,
          "find": placed.label },
        { "op": "markup_text", "id": format!("U{p}"), "kind": "Underline", "page": p,
          "from": [text.origin[0] + 1.0, mid],
          "to": [text.origin[0] + text.width - 1.0, mid] },
        { "op": "status", "id": format!("S{p}"), "state": "Accepted", "author": "Bo" },
        { "op": "reply", "id": format!("L{p}"), "reply_id": format!("R{p}"),
          "text": "Agreed, see sheet 4.", "author": "Cy" },
        { "op": "check", "id": format!("H{p}") },
        { "op": "lock", "id": format!("S{p}") },
        { "op": "column", "id": format!("L{p}"), "column": "trade", "value": "Drainage" },
        { "op": "column", "id": format!("L{p}"), "column": "cost", "value": "1250,5" },
        { "op": "popup", "id": format!("U{p}"), "open": true },
        { "op": "expect_annot", "id": format!("L{p}"), "kind": "FreeText", "page": p,
          "contents": "L", "rotation": placed.rotate, "replies": 1, "checked": false,
          "columns": { "trade": "Drainage", "cost": "1250.5" } },
        { "op": "expect_annot", "id": format!("S{p}"), "kind": "Stamp", "status": "Accepted",
          "locked": true, "rotation": placed.rotate },
        { "op": "expect_annot", "id": format!("H{p}"), "kind": "Highlight", "quads": 1,
          "contents": placed.label, "checked": true },
        { "op": "expect_annot", "id": format!("U{p}"), "kind": "Underline", "quads": 1,
          "contents": placed.label, "popup": true, "open": true },
        { "op": "expect", "count": 9, "page": p }
    ])
    .as_array()
    .unwrap()
    .clone()
}

/// What the object of annotation `name` holds in the saved file.
fn dict_of(doc: &PdfDocument, name: &str) -> pl_doc::pl_pdf::Dictionary {
    for page in 0..doc.page_count() {
        for entry in doc.annotations(page).unwrap() {
            if let Ok(Object::String(nm, _)) = entry.dict.get(b"NM") {
                if nm.as_slice() == name.as_bytes() {
                    return entry.dict;
                }
            }
        }
    }
    panic!("no annotation {name} in the file");
}

fn object_of(doc: &PdfDocument, name: &str) -> pl_doc::pl_pdf::ObjectId {
    for page in 0..doc.page_count() {
        for entry in doc.annotations(page).unwrap() {
            if let Ok(Object::String(nm, _)) = entry.dict.get(b"NM") {
                if nm.as_slice() == name.as_bytes() {
                    return entry.id.expect("an indirect annotation");
                }
            }
        }
    }
    panic!("no annotation {name} in the file");
}

fn text_of(dict: &pl_doc::pl_pdf::Dictionary, key: &[u8]) -> String {
    match dict.get(key) {
        Ok(Object::String(bytes, _)) => pl_doc::pl_annot::decode_text(bytes),
        other => panic!("{}: {other:?}", String::from_utf8_lossy(key)),
    }
}

#[test]
fn the_command_driver_marks_up_rotated_user_unit_and_crop_box_pages() {
    let backend = backend();
    let scale = 2.0;
    for fixture in [
        pl_testkit::rotated_pages(),
        pl_testkit::user_unit(),
        pl_testkit::crop_box(),
    ] {
        let name = fixture.manifest.name.clone();
        let (dir, path) = on_disk(&fixture);
        std::fs::write(
            dir.path().join("flag.png"),
            pl_doc::images::encode_png(&flag(80, 40)),
        )
        .unwrap();
        let placed = placements(&fixture);

        // The bare pages, before anything is added.
        let mut s = session(&backend, None);
        let id = s.open(&path, None).unwrap();
        let bare: Vec<Bitmap> = {
            let doc = s.document(id).unwrap();
            (0..doc.page_count())
                .map(|p| backend.render_page(doc.render_id(), p, scale).unwrap())
                .collect()
        };
        s.close(id).unwrap();

        let mut steps = vec![
            serde_json::json!({ "op": "open", "path": path }),
            serde_json::json!({ "op": "define_column", "title": "Trade", "kind": "Text" }),
            serde_json::json!({ "op": "define_column", "title": "Cost", "kind": "Number" }),
        ];
        for (placed, page) in placed.iter().zip(&fixture.manifest.pages) {
            steps.extend(steps_for(placed, &page.texts[0]));
        }
        let json = serde_json::json!({
            "author": "Ada", "date": "D:20261124101500Z", "steps": steps
        })
        .to_string();
        let script = Script::from_json(&json).unwrap();
        let mut host = LogOnly::default();
        let report = script::run(&mut s, &script, dir.path(), &mut host)
            .unwrap_or_else(|e| panic!("{name}: {e}\n{}", host.lines.join("\n")));
        let doc = s.document_mut(report.document.unwrap()).unwrap();

        // Our own drawing of every page, as the user sees it before the save.
        let ours: Vec<Bitmap> = (0..doc.page_count())
            .map(|p| doc.render_page_composite(p, scale).unwrap())
            .collect();
        let before = doc.annots().unwrap().model();
        let dest = dir.path().join("marked.pdf");
        let saved = doc.save_as(&dest).unwrap();
        assert!(saved.wrote());
        assert_eq!(saved.added, 9 * placed.len(), "{name}");
        assert!(doc.warnings().is_empty(), "{name}: {:?}", doc.warnings());
        drop(s);

        // A fresh session on the saved file: the same model, the right counts.
        let mut s = session(&backend, None);
        let id = s.open(&dest, None).unwrap();
        let doc = s.document_mut(id).unwrap();
        let after = doc.annots().unwrap().model();
        assert_eq!(after.len(), before.len(), "{name}");
        for (a, b) in after.iter().zip(&before) {
            assert_same(a, b, &name);
        }
        let parsed = PdfDocument::from_bytes(std::fs::read(&dest).unwrap(), None).unwrap();
        assert!(std::fs::read(&dest).unwrap().starts_with(&fixture.bytes));

        for placed in &placed {
            let p = placed.page;
            let page = p - 1;
            let what = format!("{name} page {p} (/Rotate {})", placed.rotate);
            let geometry = *doc.page_geometry(page).unwrap();
            assert_eq!(geometry.rotate().degrees(), placed.rotate, "{what}");
            let set = doc.annots().unwrap().set().clone();
            assert_eq!(set.on_page(page as usize).count(), 9, "{what}");
            assert_eq!(
                doc.annotations(page).unwrap().len(),
                9,
                "{what}: PDFium's count"
            );
            let get = |id: String| set.get(&AnnotId(id)).unwrap().clone();

            // PDFium draws the saved page from the appearances that were written.
            let shown = backend.render_page(doc.render_id(), page, scale).unwrap();

            // The text box: its letter stands upright as displayed, inside its rectangle.
            let l = get(format!("L{p}"));
            assert_eq!(i32::from(l.rotation()), placed.rotate, "{what}");
            assert!(l.is_modelled() && l.free_text.is_some());
            let l_box = pixel_box(&geometry, scale, placed.l_rect, 1.0);
            let ink = new_ink(&bare[page as usize], &shown, l_box);
            reads_as_upright_l(&ink).unwrap_or_else(|e| panic!("{what}: the text box: {e}"));
            let around = pixel_box(&geometry, scale, placed.l_rect.inflate(12.0, 12.0), 0.0);
            assert_eq!(
                new_ink(&bare[page as usize], &shown, around).len(),
                ink.len(),
                "{what}: the letter stays inside its box"
            );

            // The picture: red is on the left and blue on the right as displayed.
            let image = get(format!("I{p}"));
            let i_rect = image.geometry.bounds();
            let (dw, dh) = {
                let b = pixel_box(&geometry, scale, i_rect, 0.0);
                (b[2] - b[0], b[3] - b[1])
            };
            assert!(
                dw > dh,
                "{what}: the picture is wide as displayed ({dw} x {dh})"
            );
            let i_box = pixel_box(&geometry, scale, i_rect, 0.0);
            let red = centroid(&shown, i_box, |c| c[0] > 180 && c[2] < 90).expect("red");
            let blue = centroid(&shown, i_box, |c| c[2] > 180 && c[0] < 90).expect("blue");
            assert!(red.2 > 1000 && blue.2 > 1000, "{what}: {red:?} {blue:?}");
            assert!(
                red.0 + f64::from(dw) * 0.3 < blue.0 && (red.1 - blue.1).abs() < 3.0,
                "{what}: red {red:?}, blue {blue:?}"
            );

            // The stamp is drawn, wide as displayed, and what we painted before the save is
            // what PDFium paints after it.
            let stamp = get(format!("S{p}"));
            assert_eq!(i32::from(stamp.rotation()), placed.rotate);
            let s_box = pixel_box(&geometry, scale, placed.stamp_rect, 1.0);
            assert!(s_box[2] - s_box[0] > s_box[3] - s_box[1], "{what}");
            let stamp_ink = new_ink(&bare[page as usize], &shown, s_box).len();
            assert!(
                stamp_ink > 400,
                "{what}: the stamp has {stamp_ink} px of ink"
            );
            let total = (shown.width * shown.height) as usize;
            let differs =
                one_way(&ours[page as usize], &shown) + one_way(&shown, &ours[page as usize]);
            println!("{what}: {differs} of {total} px differ between our drawing and PDFium's");
            assert!(
                differs * 200 < total,
                "{what}: {differs} px differ between our drawing and PDFium's"
            );

            // The text markups cover exactly the characters PDFium reports for the label.
            let text = doc.page_text(page).unwrap();
            for id in [format!("H{p}"), format!("U{p}")] {
                let a = get(id.clone());
                assert_eq!(
                    a.meta.contents.as_deref(),
                    Some(placed.label.as_str()),
                    "{what}"
                );
                let Geometry::Quads(quads) = &a.geometry else {
                    panic!("{what}: {id} has no quads")
                };
                assert_eq!(quads.len(), 1, "{what}: one line");
                let q = quads[0];
                let (qx0, qx1) = (q[0].x.min(q[2].x), q[0].x.max(q[2].x));
                let (qy0, qy1) = (q[0].y.min(q[2].y), q[0].y.max(q[2].y));
                // The quad starts at the start of the baseline and runs along +x: the text
                // of these fixtures is drawn along the page's x axis whatever its /Rotate.
                assert!(
                    q[1].x > q[0].x && (q[1].y - q[0].y).abs() < 1e-6,
                    "{what}: {q:?}"
                );
                assert!(q[3].y > q[0].y, "{what}: {q:?}");
                let inside: Vec<&pl_doc::pl_render::TextChar> = text
                    .chars
                    .iter()
                    .filter(|c| c.has_box())
                    .filter(|c| {
                        let (cx, cy) =
                            ((c.bbox[0] + c.bbox[2]) / 2.0, (c.bbox[1] + c.bbox[3]) / 2.0);
                        cx > qx0 && cx < qx1 && cy > qy0 && cy < qy1
                    })
                    .collect();
                let under: String = inside.iter().map(|c| c.ch).collect();
                assert_eq!(
                    under.replace(' ', ""),
                    placed.label.replace(' ', ""),
                    "{what}: {id}"
                );
                // The quad is the union of those characters' boxes.
                let union = inside
                    .iter()
                    .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |u, c| {
                        [
                            u[0].min(c.bbox[0]),
                            u[1].min(c.bbox[1]),
                            u[2].max(c.bbox[2]),
                            u[3].max(c.bbox[3]),
                        ]
                    });
                for (got, want) in [qx0, qy0, qx1, qy1].into_iter().zip(union) {
                    assert!(
                        (got - want).abs() < 0.01,
                        "{what}: {id}: quad {q:?} vs {union:?}"
                    );
                }
            }

            // Status, reply and checkmark at object level, the way Acrobat writes them.
            assert_eq!(
                set.review_state(&AnnotId(format!("S{p}"))),
                ReviewState::Accepted
            );
            let status = set
                .review_history(&AnnotId(format!("S{p}")))
                .last()
                .map(|a| a.id.clone())
                .unwrap();
            let d = dict_of(&parsed, status.as_str());
            assert_eq!(d.get(b"Subtype").unwrap().as_name().unwrap(), b"Text");
            assert_eq!(
                d.get(b"IRT").unwrap().as_reference().unwrap(),
                object_of(&parsed, &format!("S{p}")),
                "{what}: /IRT refers to the stamp's object"
            );
            assert_eq!(text_of(&d, b"State"), "Accepted");
            assert_eq!(text_of(&d, b"StateModel"), "Review");
            assert_eq!(text_of(&d, b"T"), "Bo");
            let flags = d.get(b"F").unwrap().as_i64().unwrap() as u32;
            assert!(flags & Flags::HIDDEN != 0, "{what}: a status is hidden");
            assert!(d.has(b"AP"), "{what}: it has an appearance");

            let reply = dict_of(&parsed, &format!("R{p}"));
            assert_eq!(
                reply.get(b"IRT").unwrap().as_reference().unwrap(),
                object_of(&parsed, &format!("L{p}"))
            );
            assert_eq!(reply.get(b"RT").unwrap().as_name().unwrap(), b"R");
            assert_eq!(text_of(&reply, b"Contents"), "Agreed, see sheet 4.");
            assert_eq!(text_of(&reply, b"T"), "Cy");
            assert!(!reply.has(b"State"));

            assert!(markup_ops::is_checked(&set, &AnnotId(format!("H{p}"))));
            let mark = set
                .iter()
                .find(|a| {
                    a.meta.reply_to == Some(AnnotId(format!("H{p}")))
                        && a.meta.state_model.as_deref() == Some("Marked")
                })
                .unwrap();
            let d = dict_of(&parsed, mark.id.as_str());
            assert_eq!(text_of(&d, b"State"), "Marked");
            assert_eq!(text_of(&d, b"StateModel"), "Marked");

            // The lock is the standard flag; the columns are private data beside the
            // standard annotation; the popup is linked both ways.
            let d = dict_of(&parsed, &format!("S{p}"));
            let flags = d.get(b"F").unwrap().as_i64().unwrap() as u32;
            assert!(
                flags & Flags::LOCKED != 0 && flags & Flags::PRINT != 0,
                "{what}"
            );
            assert_eq!(d.get(b"Subtype").unwrap().as_name().unwrap(), b"Stamp");
            let d = dict_of(&parsed, &format!("L{p}"));
            assert!(
                d.has(b"PlumblineData") && d.has(b"AP") && d.has(b"DA"),
                "{what}"
            );
            assert_eq!(text_of(&d, b"Contents"), "L");
            let underline = dict_of(&parsed, &format!("U{p}"));
            let popup = underline.get(b"Popup").unwrap().as_reference().unwrap();
            let popup = parsed.object(popup).unwrap().as_dict().unwrap();
            assert_eq!(popup.get(b"Subtype").unwrap().as_name().unwrap(), b"Popup");
            assert_eq!(
                popup.get(b"Parent").unwrap().as_reference().unwrap(),
                object_of(&parsed, &format!("U{p}"))
            );
            assert!(popup.get(b"Open").unwrap().as_bool().unwrap());
        }
    }
}

/// Everything the journal has to bring back.
fn see(doc: &mut Document) -> (Vec<Annotation>, usize, usize, Vec<AnnotId>, Vec<AnnotId>) {
    let model = doc.annots().unwrap().model();
    let editor = doc.editor().unwrap();
    (
        model,
        editor.undo_len(),
        editor.redo_len(),
        doc.hidden_annotations().into_iter().map(|h| h.0).collect(),
        doc.overlay(0).into_iter().map(|i| i.id).collect(),
    )
}

#[test]
fn the_journal_replays_text_boxes_stamps_pictures_and_list_commands() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let journal_file = || -> PathBuf {
        std::fs::read_dir(journal.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .next()
            .expect("a journal")
    };
    let ada = origin("Ada");
    let bo = origin("Bo");
    let when = ada.when;
    let mut schema = Schema::new();
    let trade = schema.add("Trade", ColumnType::Text, &[]).unwrap();
    let cost = schema.add("Cost", ColumnType::Number, &[]).unwrap();
    let picture = Arc::new(flag(480, 320));
    let png_len = pl_doc::images::encode_png(&picture).len();

    let mut s = session(&backend, Some(journal.path()));
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();

    // Text boxes, a callout, typewriter text, a stamp and a picture.
    let mut note =
        Annotation::free_text(0, Rect::new(60.0, 620.0, 250.0, 690.0), "Check falls", &ada);
    note.set_rotation(90);
    let note_id = note.id.clone();
    let callout = Annotation::callout(
        0,
        Rect::new(330.0, 620.0, 470.0, 660.0),
        Point::new(290.0, 560.0),
        "Kerb here",
        &ada,
    );
    let callout_id = callout.id.clone();
    let typed = Annotation::typewriter(0, Rect::new(60.0, 560.0, 240.0, 580.0), "Typed", &ada);
    let stamp = Annotation::stamp(
        0,
        Rect::new(400.0, 80.0, 540.0, 130.0),
        StampSpec::revised().with_detail("Rev C"),
        &ada,
    );
    let stamp_id = stamp.id.clone();
    let image = Annotation::image_stamp(
        0,
        Rect::new(60.0, 80.0, 156.0, 144.0),
        picture.clone(),
        &ada,
    );
    let image_id = image.id.clone();
    doc.apply(Command::AddAnnotations(vec![note, callout, typed, stamp]))
        .unwrap();
    doc.apply(Command::add(image.clone())).unwrap();
    // A drag of the picture: many steps, one undo step, and the picture written once.
    let mut dragged = image.clone();
    for _ in 0..12 {
        dragged.translate(Vec2::new(4.0, 2.5));
        doc.apply_drag(Command::modify(dragged.clone()), 77)
            .unwrap();
    }
    // The list's commands.
    doc.set_text(&note_id, "Falls checked\non site.", when)
        .unwrap();
    let status = doc
        .set_status(&stamp_id, ReviewState::Rejected, &bo)
        .unwrap();
    doc.set_status(&stamp_id, ReviewState::Accepted, &ada)
        .unwrap();
    let reply = doc.add_reply(&callout_id, "Which kerb?", &bo).unwrap();
    let nested = doc.add_reply(&reply, "The dropped one.", &ada).unwrap();
    let doomed = doc.add_reply(&callout_id, "Ignore me", &bo).unwrap();
    doc.delete_reply(&doomed).unwrap();
    doc.delete_reply(&status).unwrap();
    assert!(doc.toggle_checked(&note_id, &bo).unwrap());
    assert!(doc.toggle_locked(&stamp_id, when).unwrap());
    doc.set_column(
        &note_id,
        schema.get(&trade).unwrap(),
        Value::text("Drainage"),
        when,
    )
    .unwrap();
    doc.set_column_text(&note_id, schema.get(&cost).unwrap(), "99.5", when)
        .unwrap();
    // The foreign text box: its words are changed, which makes it ours to draw.
    let foreign = AnnotId::from("fixture-freetext-1");
    assert!(!doc.annotation(&foreign).unwrap().is_modelled());
    doc.set_text(&foreign, "Rewritten", when).unwrap();
    assert!(doc.annotation(&foreign).unwrap().is_modelled());
    let popup = doc.ensure_popup(&callout_id, &ada).unwrap();
    doc.set_popup_open(&callout_id, true, when).unwrap();
    // Text that cannot be shown is refused with the reason and leaves no trace.
    let refused = doc
        .set_text(&note_id, "\u{4e2d}\u{6587}", when)
        .unwrap_err();
    assert!(refused.to_string().contains('\u{4e2d}'), "{refused}");
    // A little history: two steps undone, one redone.
    assert!(doc.undo().unwrap());
    assert!(doc.undo().unwrap());
    assert!(doc.redo().unwrap());

    let before = see(doc);
    assert_eq!(doc.review_state(&stamp_id), ReviewState::Accepted);
    assert!(doc.is_checked(&note_id));
    assert!(
        before.3.contains(&foreign),
        "the rewritten text box is hidden in the mirror"
    );
    assert_eq!(
        doc.annotation(&image_id).unwrap().geometry,
        dragged.geometry
    );
    let picture_before = doc.render_page_composite(0, 1.5).unwrap();
    // The journal holds the picture once, however many records mention it.
    let journal_len = std::fs::metadata(journal_file()).unwrap().len() as usize;
    let one_copy = png_len.div_ceil(3) * 4;
    println!("journal {journal_len} bytes; the picture is {one_copy} bytes as base64");
    assert!(
        one_copy > 40_000,
        "the picture is large enough to tell ({one_copy})"
    );
    assert!(
        journal_len > one_copy && journal_len < one_copy + one_copy / 2,
        "journal {journal_len} bytes, one base64 picture {one_copy}"
    );

    // The kill.
    drop(s);
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
    let mut s = session(&backend, Some(journal.path()));
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    assert!(doc.recovered_changes() > 30, "{}", doc.recovered_changes());
    let after = see(doc);
    assert_eq!(after.0.len(), before.0.len());
    for (a, b) in after.0.iter().zip(&before.0) {
        assert_eq!(a, b, "{}", b.id);
    }
    assert_eq!(
        (after.1, after.2, &after.3, &after.4),
        (before.1, before.2, &before.3, &before.4)
    );
    assert!(
        doc.render_page_composite(0, 1.5).unwrap() == picture_before,
        "the picture"
    );
    assert_eq!(doc.review_state(&stamp_id), ReviewState::Accepted);
    assert!(doc.is_checked(&note_id));
    let set = doc.annots().unwrap().set().clone();
    assert!(set.contains(&reply) && set.contains(&nested) && set.contains(&popup));
    assert!(!set.contains(&doomed) && !set.contains(&status));
    let note = set.get(&note_id).unwrap();
    assert_eq!(
        note.meta.contents.as_deref(),
        Some("Falls checked\non site.")
    );
    assert_eq!(note.rotation(), 90);
    assert_eq!(
        markup_ops::column_value(note, schema.get(&cost).unwrap()),
        Value::number(99.5)
    );
    // The replayed picture is one shared picture again, not one copy per record.
    let Some(pl_doc::pl_annot::Stamp {
        content: pl_doc::pl_annot::StampContent::Image(replayed),
        ..
    }) = &set.get(&image_id).unwrap().stamp
    else {
        panic!("the picture stamp lost its picture")
    };
    assert_eq!(**replayed, *picture);

    // The recovered history goes all the way back to the file and forward again.
    let mut undone = 0;
    while doc.undo().unwrap() {
        undone += 1;
    }
    assert_eq!(undone, before.1);
    assert!(!doc.is_modified());
    assert!(!doc.annots().unwrap().set().is_dirty());
    assert_eq!(doc.annots().unwrap().set().len(), 4);
    assert!(
        doc.hidden_annotations().len() <= 1,
        "only what was once ours stays ours"
    );
    for _ in 0..undone {
        assert!(doc.redo().unwrap());
    }
    let again = see(doc);
    assert_eq!(again.0, before.0);

    // Saved, and read back by a fresh session: an equal model, and nothing to recover.
    let saved = doc.save().unwrap();
    assert!(saved.wrote());
    assert!(std::fs::read_dir(journal.path()).unwrap().next().is_none());
    drop(s);
    let mut s = session(&backend, Some(journal.path()));
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let reread = doc.annots().unwrap().model();
    assert_eq!(doc.recovered_changes(), 0);
    assert_eq!(reread.len(), before.0.len());
    for (a, b) in reread.iter().zip(&before.0) {
        assert_same(a, b, "after save and reload");
    }
    assert_eq!(doc.review_state(&stamp_id), ReviewState::Accepted);
    assert!(doc.is_checked(&note_id));
    let kinds = |k: AnnotKind| reread.iter().filter(|a| a.kind == k).count();
    assert_eq!(kinds(AnnotKind::FreeText), 4);
    assert_eq!(kinds(AnnotKind::Stamp), 2);
    assert_eq!(kinds(AnnotKind::Popup), 1);
}
