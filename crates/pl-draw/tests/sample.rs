//! Real-file tests on the sample drawing set (`samples/site-development-plans.pdf`). They skip,
//! with a printed reason, when the file is not there. Nothing from the file is copied, embedded
//! or printed; only page sizes and pixel statistics are used.
//!
//! * A Multiply highlight composited by the rasteriser onto a real sheet, as the app does it
//!   against a tile: linework stays dark, paper takes the tint, nothing outside changes.
//! * The emitted appearance of a markup sized for a real sheet (large page, the sheet's own
//!   `/Rotate` and boxes), rendered by PDFium and compared with the rasteriser.

mod common;

use std::path::PathBuf;

use pdf_writer::types::AnnotationType;
use pl_draw::kurbo::{Point, Rect};
use pl_draw::*;
use pl_geom::PageGeometry;
use pl_render::{DocSource, RenderBackend};

fn sample() -> Option<PathBuf> {
    let path = std::path::absolute(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("samples")
            .join("site-development-plans.pdf"),
    )
    .ok()?;
    if path.is_file() {
        Some(path)
    } else {
        println!("SAMPLE SKIPPED: {} is not present", path.display());
        None
    }
}

/// A markup sized relative to the sheet: a highlight band across the middle, a cloud around
/// it, and an arrow pointing at it. Returns the list and the highlight's rectangle.
fn sheet_markup(geometry: &PageGeometry) -> (DrawList, Rect) {
    let c = geometry.crop_box();
    let unit = c.width().min(c.height()) / 100.0;
    let band = Rect::new(
        c.x0 + 0.25 * c.width(),
        c.y0 + 0.40 * c.height(),
        c.x0 + 0.75 * c.width(),
        c.y0 + 0.60 * c.height(),
    );
    let mut list = DrawList::new();
    list.push(highlight_op(
        &[rect_quad(band)],
        Rgb::new(1.0, 0.95, 0.2),
        1.0,
    ));
    list.push(DrawOp::Path(PathOp::stroked(
        cloud_rect_path(
            band.inflate(4.0 * unit, 4.0 * unit),
            &CloudStyle::new(6.0 * unit),
        ),
        Stroke::new(unit, Rgb::new(0.85, 0.1, 0.1)).with_join(LineJoin::Round),
    )));
    list.push_composite(
        line_ops(
            Point::new(c.x0 + 0.08 * c.width(), c.y0 + 0.9 * c.height()),
            Point::new(band.x0 - 6.0 * unit, band.y1 + 6.0 * unit),
            LineEnding::Circle,
            LineEnding::ClosedArrow,
            &Stroke::new(1.2 * unit, Rgb::new(0.1, 0.25, 0.8)),
            Some(Rgb::new(0.1, 0.25, 0.8)),
        ),
        0.6,
        BlendMode::Normal,
    );
    (list, band)
}

#[test]
fn a_highlight_over_a_real_sheet_keeps_the_linework() {
    let Some(path) = sample() else { return };
    let backend = common::backend();
    let info = backend
        .open(DocSource::file(&path), None)
        .expect("open the sample");
    assert!(info.page_count > 0);
    let mut sheets_with_linework = 0;
    for page in [0, info.page_count / 2, info.page_count - 1] {
        let geometry = backend.page_info(info.doc, page).unwrap().geometry();
        let size = geometry.display_size();
        let scale = 1400.0 / size.width.max(size.height);
        let backdrop = backend.render_page(info.doc, page, scale).unwrap();
        let (list, band) = sheet_markup(&geometry);
        let transform = page_transform(&geometry, scale);
        let out = render_over_rgba(
            &list,
            transform,
            &backdrop.pixels,
            backdrop.width,
            backdrop.height,
        )
        .unwrap();
        assert_eq!(out.len(), backdrop.pixels.len());

        let inside = transform.transform_rect_bbox(band).inflate(-2.0, -2.0);
        let touched = transform
            .transform_rect_bbox(list.bounds().unwrap())
            .inflate(2.0, 2.0);
        let (mut dark_before, mut dark_after, mut tinted, mut light) = (0u32, 0u32, 0u32, 0u32);
        for y in 0..backdrop.height {
            for x in 0..backdrop.width {
                let i = ((y * backdrop.width + x) * 4) as usize;
                let (b, o) = (&backdrop.pixels[i..i + 4], &out[i..i + 4]);
                let p = Point::new(x as f64 + 0.5, y as f64 + 0.5);
                if !touched.contains(p) {
                    assert_eq!(b, o, "page {page}: a pixel outside the markup changed");
                } else if inside.contains(p) {
                    // Multiply never lightens.
                    assert!(
                        (0..3).all(|c| o[c] <= b[c].saturating_add(1)),
                        "page {page}: the highlight lightened a pixel: {b:?} -> {o:?}"
                    );
                    if b[..3].iter().all(|v| *v < 60) {
                        dark_before += 1;
                        if o[..3].iter().all(|v| *v < 60) {
                            dark_after += 1;
                        }
                    }
                    if b[..3].iter().all(|v| *v > 235) {
                        light += 1;
                        // Paper takes the highlight colour: red and green kept, blue cut.
                        if o[0] > 225 && o[1] > 210 && o[2] < 70 {
                            tinted += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(
            dark_before, dark_after,
            "page {page}: dark pixels were lost"
        );
        assert!(
            light > 1000,
            "page {page}: no paper under the highlight ({light})"
        );
        assert_eq!(
            tinted, light,
            "page {page}: paper under the highlight is not tinted"
        );
        if dark_before > 200 {
            sheets_with_linework += 1;
        }
    }
    backend.close(info.doc).unwrap();
    assert!(
        sheets_with_linework > 0,
        "none of the sampled sheets has linework under the highlight; the test proves nothing"
    );
}

#[test]
fn a_markup_sized_for_a_real_sheet_matches_pdfium() {
    let Some(path) = sample() else { return };
    let backend = common::backend();
    let info = backend
        .open(DocSource::file(&path), None)
        .expect("open the sample");
    let mut seen = Vec::new();
    for page in 0..info.page_count {
        let page_info = backend.page_info(info.doc, page).unwrap();
        // One comparison per distinct sheet layout is enough.
        let key = format!("{page_info:?}");
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let geometry = page_info.geometry();
        let (m, c) = (geometry.media_box(), geometry.crop_box());
        // The same boxes and rotation on a synthetic page that carries our appearance stream
        // (this crate cannot write into the sample itself; that is pl-pdf's job).
        let variant = common::Variant {
            name: "sample-sheet",
            media: [m.x0, m.y0, m.x1, m.y1],
            crop: Some([c.x0, c.y0, c.x1, c.y1]),
            rotate: geometry.rotate().degrees(),
            user_unit: geometry.user_unit(),
        };
        let size = geometry.display_size();
        let scale = 1400.0 / size.width.max(size.height);
        let (list, _) = sheet_markup(&geometry);
        let r = common::render_both_at(&backend, &list, &variant, AnnotationType::Square, scale);
        assert_eq!(
            (r.pdfium.width, r.pdfium.height),
            pl_render::page_pixel_size(&geometry, scale)
        );
        let drawn = common::changed_pixels(&r.backdrop, &r.pdfium, 12);
        assert!(
            drawn > 5_000,
            "sheet {page}: the markup is barely drawn ({drawn} px)"
        );
        let d = common::diff(&r.ours, &r.pdfium, 1, 40);
        assert!(
            d.bad <= 3 && d.mean < 0.6,
            "sheet {page} ({page_info:?}): {d:?}"
        );
    }
    backend.close(info.doc).unwrap();
    assert!(!seen.is_empty());
}
