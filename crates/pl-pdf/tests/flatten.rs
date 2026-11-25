//! Flatten leaves the render unchanged (PDFium, before with annotations vs after without them),
//! on plain, rotated, UserUnit and CropBox pages; and a page rotation turns the page and its
//! annotations together.
//!
//! One known PDFium difference is handled on its own: PDFium 8125 ignores an annotation's
//! `/CA` when it draws an existing appearance stream, while the standard (ISO 32000-1 12.5.2,
//! and Acrobat) applies it. Flatten follows the standard. So the render-equality check runs on
//! the fixture with `/CA` taken off, and the opacity is checked separately against the
//! expected blend.

mod structure_util;

use std::collections::BTreeSet;

use pl_pdf::{
    DictPatch, FlattenOptions, FlattenSkip, FlattenTarget, PdfDocument, PdfFile, SaveOptions,
    SignaturePolicy,
};
use pl_render::{Bitmap, PdfiumBackend, RenderBackend, RenderOptions};
use pl_testkit::{flatten_set, structure_main};
use structure_util::{compare, inked, load, open_bytes, temp_dir};

const SCALE: f64 = 1.5;

fn render_all(
    backend: &PdfiumBackend,
    bytes: Vec<u8>,
    user_units: &[f64],
    annotations: bool,
) -> Vec<Bitmap> {
    let id = open_bytes(backend, bytes);
    for (i, u) in user_units.iter().enumerate() {
        if *u != 1.0 {
            backend.set_page_user_unit(id, i as u32, *u).unwrap();
        }
    }
    let n = backend.page_count(id).unwrap();
    let out = (0..n)
        .map(|p| {
            backend
                .render_page_with(id, p, SCALE, RenderOptions { annotations })
                .unwrap()
        })
        .collect();
    backend.close(id).unwrap();
    out
}

fn save_bytes(doc: PdfDocument, tag: &str) -> Vec<u8> {
    let path = temp_dir(tag).join("out.pdf");
    doc.save_new(&path, &SaveOptions::default(), SignaturePolicy::Refuse)
        .unwrap();
    std::fs::read(&path).unwrap()
}

/// The flatten fixture with `/CA` removed from its annotations (an incremental save).
fn without_opacity() -> (Vec<u8>, Vec<f64>) {
    let (fx, _) = flatten_set();
    let units = fx.manifest.pages.iter().map(|p| p.user_unit).collect();
    let path = temp_dir("no-ca").join("no-ca.pdf");
    std::fs::write(&path, &fx.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    for p in 0..doc.page_count() {
        for a in doc.annotations(p).unwrap() {
            if a.dict.has(b"CA") {
                let mut patch = DictPatch::new();
                patch.remove("CA");
                edits.modify_annotation(a.id.unwrap(), patch);
            }
        }
    }
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    (std::fs::read(&path).unwrap(), units)
}

#[test]
fn flatten_leaves_the_render_unchanged() {
    let (_, truth) = flatten_set();
    let (bytes, units) = without_opacity();
    let doc = load(&bytes);
    let backend = structure_util::pdfium();
    let before = render_all(&backend, bytes.clone(), &units, true);
    let bare = render_all(&backend, bytes.clone(), &units, false);

    let (flat, report) = doc.flatten(&FlattenOptions::default()).unwrap();
    for (p, want) in truth.visible.iter().enumerate() {
        let n = report
            .flattened
            .iter()
            .filter(|f| f.page_index == p)
            .count();
        assert_eq!(n, *want, "page {p} flattened");
        let skipped: Vec<FlattenSkip> = report
            .skipped
            .iter()
            .filter(|s| s.0 == p)
            .map(|s| s.2)
            .collect();
        assert_eq!(
            skipped,
            [
                FlattenSkip::NotShown,
                FlattenSkip::NotShown,
                FlattenSkip::Link
            ],
            "page {p}"
        );
        // Hidden, no-view and the link stay as annotations; the popup went with its note.
        assert_eq!(flat.annotations(p).unwrap().len(), 3, "page {p}");
    }
    assert_eq!(report.popups_removed, truth.visible.len());
    let after = render_all(&backend, save_bytes(flat, "flatten"), &units, true);
    for (p, (a, b)) in before.iter().zip(&after).enumerate() {
        // The annotations do draw something, so the comparison means something.
        assert!(
            inked(a) > inked(&bare[p]) + 1000,
            "page {p}: annotations are visible before"
        );
        let (max, over) = compare(a, b, 8);
        assert!(
            over <= 20,
            "page {p}: {over} pixels differ by more than 8 levels (max {max})"
        );
    }
}

#[test]
fn flatten_applies_annotation_opacity_as_a_group() {
    let (fx, _) = flatten_set();
    let doc = load(&fx.bytes);
    let backend = structure_util::pdfium();
    let bare = render_all(&backend, fx.bytes.clone(), &[1.0], false);
    let (flat, _) = doc.flatten(&FlattenOptions::default()).unwrap();
    let after = render_all(&backend, save_bytes(flat, "opacity"), &[1.0], true);
    // Page 1 (Letter, unrotated): the /CA 0.5 square spans x 200..320, y 80..180; its inner
    // blue square is centred at (260, 130), over the grey band. Pixel row from the top.
    let (x, y) = ((260.0 * SCALE) as u32, ((792.0 - 130.0) * SCALE) as u32);
    let under = bare[0].pixel(x, y);
    let got = after[0].pixel(x, y);
    // The appearance paints red, then blue over its middle. As a group at 50 %, the middle is
    // half blue over the background; per-object opacity would leave a quarter of the red in it
    // (red channel 115 instead of 102 over the 204 grey).
    let want = [
        under[0] as f64 * 0.5,
        under[1] as f64 * 0.5,
        under[2] as f64 * 0.5 + 127.5,
    ];
    for c in 0..3 {
        assert!(
            (got[c] as f64 - want[c]).abs() <= 3.0,
            "channel {c}: {got:?} vs {want:?} over {under:?}"
        );
    }
}

#[test]
fn flatten_by_filter_and_for_print() {
    let (fx, _) = flatten_set();
    let doc = load(&fx.bytes);
    // Only page 2, only its first annotation.
    let first = doc.annotations(1).unwrap()[0].id.unwrap();
    let options = FlattenOptions {
        pages: Some(BTreeSet::from([1])),
        ids: Some(BTreeSet::from([first])),
        ..FlattenOptions::default()
    };
    let (flat, report) = doc.flatten(&options).unwrap();
    assert_eq!(report.flattened.len(), 1);
    assert_eq!(report.flattened[0].id, Some(first));
    assert_eq!(
        flat.annotations(1).unwrap().len(),
        doc.annotations(1).unwrap().len() - 1
    );
    assert_eq!(
        flat.annotations(0).unwrap().len(),
        doc.annotations(0).unwrap().len()
    );
    // Untouched pages keep their content streams as they were.
    assert_eq!(
        structure_util::page_content(&flat, 0),
        structure_util::page_content(&doc, 0)
    );

    // For print, the no-view square (it has the print flag) flattens too; the hidden one stays.
    let print = FlattenOptions {
        target: FlattenTarget::Print,
        ..FlattenOptions::default()
    };
    let (_, report) = doc.flatten(&print).unwrap();
    assert_eq!(
        report
            .flattened
            .iter()
            .filter(|f| f.page_index == 0)
            .count(),
        8
    );
}

#[test]
fn flatten_a_file_with_markups_and_form_fields() {
    let (fx, _) = structure_main();
    let doc = load(&fx.bytes);
    let units: Vec<f64> = fx.manifest.pages.iter().map(|p| p.user_unit).collect();
    let backend = structure_util::pdfium();
    let before = render_all(&backend, fx.bytes.clone(), &units, true);

    // Markups only: the square on the offset-CropBox page; the reply is hidden and stays,
    // with its /IRT to the flattened square dropped.
    let (flat, report) = doc.flatten(&FlattenOptions::default()).unwrap();
    let subtypes: Vec<&str> = report
        .flattened
        .iter()
        .map(|f| f.subtype.as_str())
        .collect();
    assert_eq!(subtypes, ["Square"]);
    assert_eq!(report.popups_removed, 1);
    assert_eq!(report.refs_dropped, 1);
    let after = render_all(&backend, save_bytes(flat, "flatten-main"), &units, true);
    for (p, (a, b)) in before.iter().zip(&after).enumerate() {
        let (max, over) = compare(a, b, 8);
        assert!(
            over <= 20,
            "page {p}: {over} pixels differ by more than 8 levels (max {max})"
        );
    }

    // With form fields: the three widgets go and their fields leave the form. PDFium draws
    // a live text field with its own form-fill renderer, not from the appearance stream, so
    // the glyphs of the field values differ slightly; only those few pixels may change.
    let options = FlattenOptions {
        include_form_fields: true,
        ..FlattenOptions::default()
    };
    let (flat, report) = doc.flatten(&options).unwrap();
    assert_eq!(
        report
            .flattened
            .iter()
            .filter(|f| f.subtype == "Widget")
            .count(),
        3
    );
    let mut removed = report.fields_removed.clone();
    removed.sort();
    assert_eq!(removed, ["project", "sheet"]);
    let back = load(&save_bytes(flat.clone(), "flatten-fields"));
    let form = back
        .resolve(back.catalog().unwrap().get(b"AcroForm").unwrap())
        .unwrap();
    let fields = back
        .resolve(form.as_dict().unwrap().get(b"Fields").unwrap())
        .unwrap();
    assert!(fields.as_array().unwrap().is_empty());
    let after = render_all(&backend, save_bytes(flat, "flatten-fields-2"), &units, true);
    for (p, (a, b)) in before.iter().zip(&after).enumerate() {
        let (max, over) = compare(a, b, 48);
        assert!(
            over <= 200,
            "page {p}: {over} pixels differ by more than 48 levels (max {max})"
        );
    }
}

/// The bitmap turned 90 degrees clockwise.
fn rotate_cw(b: &Bitmap) -> Bitmap {
    let (w, h) = (b.width, b.height);
    let mut out = Bitmap::filled(h, w, [0, 0, 0, 0]);
    for y in 0..h {
        for x in 0..w {
            let src = ((y * w + x) * 4) as usize;
            let (nx, ny) = (h - 1 - y, x);
            let dst = ((ny * h + nx) * 4) as usize;
            out.pixels[dst..dst + 4].copy_from_slice(&b.pixels[src..src + 4]);
        }
    }
    out
}

/// Pixels of `a` with no pixel within one pixel in `b` closer than `tolerance` levels (a
/// render of a rotated page lands on a pixel grid shifted by up to a pixel).
fn far_pixels(a: &Bitmap, b: &Bitmap, tolerance: u8) -> usize {
    let (w, h) = (a.width as i64, a.height as i64);
    let mut n = 0;
    for y in 0..h {
        for x in 0..w {
            let p = a.pixel(x as u32, y as u32);
            let near = (-1..=1).any(|dy| {
                (-1..=1).any(|dx| {
                    let (qx, qy) = (x + dx, y + dy);
                    qx >= 0 && qy >= 0 && qx < w && qy < h && {
                        let q = b.pixel(qx as u32, qy as u32);
                        (0..3).all(|c| p[c].abs_diff(q[c]) <= tolerance)
                    }
                })
            });
            if !near {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn rotating_a_page_turns_its_annotations_with_it() {
    let (bytes, _) = without_opacity();
    let doc = load(&bytes);
    let backend = structure_util::pdfium();
    let before = render_all(&backend, bytes, &[1.0], true);
    let mut r = doc.restructure().unwrap();
    r.rotate(&[0], 90).unwrap();
    let after = render_all(
        &backend,
        save_bytes(r.finish().unwrap(), "rotate-render"),
        &[1.0],
        true,
    );
    let want = rotate_cw(&before[0]);
    assert_eq!((after[0].width, after[0].height), (want.width, want.height));
    assert!(inked(&after[0]) > 5000);
    // Only glyph hinting of the page's marker text differs (fonts are hinted in device space).
    let far = far_pixels(&want, &after[0], 48);
    assert!(far <= 200, "{far} pixels have no match within a pixel");
}

#[test]
fn flatten_on_a_page_that_inherits_its_resources() {
    use pl_pdf::{Dictionary, Object};
    let (fx, _) = structure_main();
    let units: Vec<f64> = fx.manifest.pages.iter().map(|p| p.user_unit).collect();
    let path = temp_dir("inherit-flat").join("in.pdf");
    std::fs::write(&path, &fx.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    // Page 5 (index 4) inherits /Resources, /MediaBox and /Rotate 180 from a /Pages node.
    let id = doc.page_id(4).unwrap();
    assert!(!doc.object(id).unwrap().as_dict().unwrap().has(b"Resources"));
    let mut edits = doc.edit();
    let mut ap = Dictionary::new();
    ap.set("Type", Object::Name(b"XObject".to_vec()));
    ap.set("Subtype", Object::Name(b"Form".to_vec()));
    ap.set(
        "BBox",
        Object::Array([0, 0, 100, 50].map(Object::Integer).to_vec()),
    );
    let ap = edits.add_stream(ap, b"3 w 0 0 1 RG 2 2 96 46 re S".to_vec());
    let mut n = Dictionary::new();
    n.set("N", Object::Reference(ap));
    let mut annot = Dictionary::new();
    annot.set("Type", Object::Name(b"Annot".to_vec()));
    annot.set("Subtype", Object::Name(b"Square".to_vec()));
    annot.set(
        "Rect",
        Object::Array([600, 300, 700, 350].map(Object::Integer).to_vec()),
    );
    annot.set("F", Object::Integer(4));
    annot.set("AP", Object::Dictionary(n));
    edits.add_annotation(4, annot);
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let doc = load(&bytes);
    let backend = structure_util::pdfium();
    let before = render_all(&backend, bytes, &units, true);
    let (flat, report) = doc.flatten(&FlattenOptions::default()).unwrap();
    assert!(report.flattened.iter().any(|f| f.page_index == 4));
    let flat_bytes = save_bytes(flat, "inherit-flat-out");
    let after = render_all(&backend, flat_bytes.clone(), &units, true);
    let (max, over) = compare(&before[4], &after[4], 8);
    assert!(
        over <= 20,
        "{over} pixels differ by more than 8 levels (max {max})"
    );
    // The page still has its font: PDFium reads the marker text.
    let id = open_bytes(&backend, flat_bytes);
    assert!(backend
        .page_text(id, 4)
        .unwrap()
        .text()
        .contains("PLM-A-05"));
}
