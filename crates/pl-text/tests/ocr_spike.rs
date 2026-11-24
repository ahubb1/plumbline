//! The M0 OCR spike: `Windows.Media.Ocr` called from an unpackaged exe (this test binary is
//! one), on a page rendered by PDFium from a pl-testkit fixture whose text is known.
//!
//! The plan's criterion is "passes or records the failure": Microsoft documents the engine
//! for packaged apps only, and it needs a language pack on the machine. So:
//!
//! * if the engine cannot be created, the test passes and prints `OCR SPIKE RECORDED
//!   FAILURE` with the typed reason (the owner then decides on a bundled engine);
//! * if the engine is created, it must really read the page: the known words have to come
//!   back, in about the right places. An engine that runs and reads nothing fails the test.
//!
//! Headless: no window is opened by PDFium or by the OCR engine.

use std::path::{Path, PathBuf};

use pl_render::{DocSource, PdfiumBackend, RenderBackend};
use pl_text::{OcrEngine, OcrError, OcrImage, WindowsOcr};

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

/// Lower-case letters and digits only, so "D-12" and "d12" compare equal.
fn key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[test]
fn windows_ocr_from_an_unpackaged_exe_reads_a_rendered_page_or_records_why_not() {
    println!(
        "OCR SPIKE installed OCR languages: {:?}",
        WindowsOcr::installed_languages()
    );
    let engine = match WindowsOcr::new() {
        Ok(engine) => engine,
        Err(reason @ (OcrError::Unavailable(_) | OcrError::NoLanguage)) => {
            println!("OCR SPIKE RECORDED FAILURE: the engine could not be created: {reason}");
            return;
        }
        Err(other) => panic!("unexpected error creating the engine: {other}"),
    };
    println!(
        "OCR SPIKE engine: {}, language {:?}, max image side {} px",
        engine.name(),
        engine.language(),
        engine.max_image_side()
    );
    assert!(engine.max_image_side() >= 1000);

    // The plain fixture at 200 dpi: its strings are 10 to 24 pt Helvetica.
    let fixture = pl_testkit::plain();
    let truth = &fixture.manifest.pages[0];
    assert!(truth.texts.len() >= 3);
    let backend = PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll");
    let doc = backend
        .open(DocSource::Bytes(fixture.bytes.clone()), None)
        .unwrap()
        .doc;
    let scale = 200.0 / 72.0;
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let bitmap = backend.render_page(doc, 0, scale).unwrap();
    backend.close(doc).unwrap();
    assert!(bitmap.width.max(bitmap.height) <= engine.max_image_side());

    let image = OcrImage::new(bitmap.width, bitmap.height, &bitmap.pixels).unwrap();
    let page = match engine.recognize(&image) {
        Ok(page) => page,
        Err(reason @ OcrError::Unavailable(_)) => {
            println!("OCR SPIKE RECORDED FAILURE: the engine refused to recognise: {reason}");
            return;
        }
        Err(other) => panic!("recognition failed: {other}"),
    };
    let recognised: Vec<String> = page.words().map(|w| key(&w.text)).collect();
    println!(
        "OCR SPIKE recognised {} words in {} lines, text angle {:?}:\n{}",
        recognised.len(),
        page.lines.len(),
        page.text_angle,
        page.text()
    );

    // Ground truth: every word of every string on the page (two letters or more: single
    // characters and punctuation are not a fair test of any OCR engine).
    let mut wanted = 0;
    let mut found = 0;
    let mut placed = 0;
    let to_px = geometry.page_to_display();
    for text in &truth.texts {
        let b = text.bbox();
        // The string's box in image pixels, grown by half a line for the engine's own boxes.
        let (p0, p1) = (
            to_px * pl_geom::Point::new(b[0], b[3]),
            to_px * pl_geom::Point::new(b[2], b[1]),
        );
        let grow = text.font_size * scale * 0.6;
        let area = [
            p0.x * scale - grow,
            p0.y * scale - grow,
            p1.x * scale + grow,
            p1.y * scale + grow,
        ];
        for word in text
            .text
            .split_whitespace()
            .map(key)
            .filter(|w| w.len() >= 2)
        {
            wanted += 1;
            let hits: Vec<_> = page.words().filter(|w| key(&w.text) == word).collect();
            if hits.is_empty() {
                println!("OCR SPIKE missed {word:?} (from {:?})", text.text);
                continue;
            }
            found += 1;
            let inside = hits.iter().any(|w| {
                let cx = (w.bbox[0] + w.bbox[2]) / 2.0;
                let cy = (w.bbox[1] + w.bbox[3]) / 2.0;
                cx >= area[0] && cx <= area[2] && cy >= area[1] && cy <= area[3]
            });
            if inside {
                placed += 1;
            } else {
                println!("OCR SPIKE {word:?} found, but not inside {area:?}");
            }
        }
    }
    println!(
        "OCR SPIKE PASSED: {found} of {wanted} known words recognised, {placed} of them at their place on the page"
    );
    assert!(wanted >= 5, "the fixture has too little text for a spike");
    // Clean vector text at 200 dpi: a working engine reads nearly all of it.
    assert!(
        found * 10 >= wanted * 8,
        "only {found} of {wanted} known words were recognised"
    );
    assert!(
        placed * 10 >= found * 9,
        "only {placed} of {found} recognised words are where the page has them"
    );
}

#[test]
fn an_image_the_engine_cannot_take_is_a_typed_error_not_a_crash() {
    let Ok(engine) = WindowsOcr::new() else {
        println!("OCR SPIKE RECORDED FAILURE: no engine, nothing to refuse an image");
        return;
    };
    let side = engine.max_image_side() + 1;
    // One row of pixels, too long.
    let pixels = vec![255u8; side as usize * 4];
    let image = OcrImage::new(side, 1, &pixels).unwrap();
    assert_eq!(
        engine.recognize(&image).unwrap_err(),
        OcrError::ImageTooLarge {
            side,
            max: engine.max_image_side()
        }
    );
    // A blank page is read as a page with no text.
    let blank = vec![255u8; 200 * 100 * 4];
    let page = engine
        .recognize(&OcrImage::new(200, 100, &blank).unwrap())
        .unwrap();
    assert_eq!(page.words().count(), 0);
    assert_eq!(page.text(), "");
}
