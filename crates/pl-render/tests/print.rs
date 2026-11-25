//! Banded print rendering through real PDFium, into the in-memory print device: stitched bands
//! against a direct render of the same page at the same resolution, the page-to-paper
//! transform against where the fixture's marks land, form-field and print-flag options, the
//! memory bound, and (when the real sample is present) the timing of one 34 x 22 in sheet.

mod common;

use std::path::{Path, PathBuf};
use std::time::Instant;

use common::*;
use pl_geom::{PageGeometry, Point};
use pl_render::{
    print_job, BandFormat, Bitmap, DocId, DocSource, MemorySink, PagePlacement, Paper,
    PdfiumBackend, PrintJob, PrintOrientation, PrintScaling, PrintableArea, RenderBackend,
    RenderOptions, WindowOptions, WindowRequest,
};
use pl_testkit::Fixture;

/// A sheet whose printable area is the page image at `dpi` plus `margin` px all round, with a
/// further 30 px unprintable border: the page lands at `(margin, margin)`, uncut.
fn paper_around(geometry: &PageGeometry, dpi: f64, margin: u32) -> Paper {
    let scale = dpi / 72.0 * geometry.user_unit();
    let (w, h) = pl_render::page_pixel_size(geometry, scale);
    Paper {
        dpi: [dpi, dpi],
        size: [w + 2 * margin + 60, h + 2 * margin + 60],
        printable: PrintableArea {
            x: 30,
            y: 30,
            width: w + 2 * margin,
            height: h + 2 * margin,
        },
    }
}

fn actual_size_job(page: u32, paper: Paper, rows: u32, annotations: bool) -> PrintJob {
    PrintJob {
        scaling: PrintScaling::ActualSize,
        orientation: PrintOrientation::AsIs,
        annotations,
        form_fields: true,
        print_flags: false,
        band_rows: rows,
        ..PrintJob::new(vec![page], paper)
    }
}

/// The band acceptance, as for stitched tiles (M1): within one grey level, except that PDFium's
/// anti-aliasing, which depends on where the destination bitmap starts, may put a few edge
/// pixels two levels apart (never more than two): at most one pixel in 5,000. The most seen is
/// 103 such pixels in 1,014,600, along the long anti-aliased diagonals of a `/Rotate 90` page;
/// the real sample at 75 dpi has 11 in 4,207,500.
fn assert_matches(a: &Bitmap, b: &Bitmap, what: &str) {
    let (worst, over) = grey_diff(a, b);
    let pixels = a.width as usize * a.height as usize;
    println!(
        "MATCH {what}: {} of {pixels} px differ, worst grey {worst}, {over} over 1, worst channel {}",
        diff_count(a, b),
        max_diff(a, b)
    );
    assert!(
        worst <= 2 && over * 5_000 <= pixels,
        "{what}: bands differ from the direct render by {worst} grey levels, {over} px over 1 of {pixels}"
    );
}

/// The printable area with `image` drawn at `offset`, white elsewhere.
fn canvas_with(paper: &Paper, image: &Bitmap, offset: [i64; 2]) -> Bitmap {
    let mut canvas = Bitmap::filled(
        paper.printable.width,
        paper.printable.height,
        [255, 255, 255, 255],
    );
    assert!(offset[0] >= 0 && offset[1] >= 0);
    canvas.blit(image, offset[0] as u32, offset[1] as u32);
    canvas
}

fn print_one(backend: &PdfiumBackend, doc: DocId, job: &PrintJob) -> (Bitmap, PagePlacement, u32) {
    let mut sink = MemorySink::new();
    let report = print_job(backend, doc, job, &mut sink).unwrap();
    assert_eq!(
        sink.calls.first().copied(),
        Some("begin_job"),
        "{:?}",
        sink.calls
    );
    assert_eq!(sink.calls.last().copied(), Some("end_job"));
    assert!(
        report.peak_band_bytes <= report.band_bound,
        "a band held {} bytes, over the bound {}",
        report.peak_band_bytes,
        report.band_bound
    );
    for b in &sink.bands {
        assert!(
            b.height <= job.band_rows,
            "band {b:?} is taller than {}",
            job.band_rows
        );
    }
    let sheet = sink.sheets.pop().unwrap();
    (sheet.image, sheet.placement, report.bands)
}

/// Acceptance: for every page, at actual size, stitched bands equal the direct full-page
/// render at the same resolution within one grey level, with and without annotations.
fn check_bands_equal_direct(fixture: &Fixture, dpis: &[f64]) {
    let backend = backend();
    let doc = open(&backend, fixture);
    let mut multi_band = 0;
    for (index, _) in fixture.manifest.pages.iter().enumerate() {
        let page = index as u32;
        let geometry = backend.page_info(doc, page).unwrap().geometry();
        for &dpi in dpis {
            for annotations in [true, false] {
                let paper = paper_around(&geometry, dpi, 20);
                // An odd band height puts seams at varied places.
                let job = actual_size_job(page, paper, 61, annotations);
                let (sheet, placement, bands) = print_one(&backend, doc, &job);
                assert_eq!(placement.offset, [20, 20]);
                assert!(!placement.clipped);
                let scale = dpi / 72.0 * geometry.user_unit();
                let direct = backend
                    .render_page_with(doc, page, scale, RenderOptions { annotations })
                    .unwrap();
                assert_eq!(
                    (direct.width, direct.height),
                    (placement.size[0], placement.size[1]),
                    "the placed image is the page's pixel size at that scale"
                );
                let want = canvas_with(&paper, &direct, placement.offset);
                assert_matches(
                    &sheet,
                    &want,
                    &format!(
                        "{} page {index} at {dpi} dpi, annotations {annotations}, {bands} bands",
                        fixture.manifest.name
                    ),
                );
                if bands > 1 {
                    multi_band += 1;
                }
            }
        }
    }
    assert!(multi_band > 0);
    backend.close(doc).unwrap();
}

#[test]
fn bands_equal_direct_render_plain() {
    check_bands_equal_direct(&pl_testkit::plain(), &[72.0, 150.0]);
}

#[test]
fn bands_equal_direct_render_rotated() {
    check_bands_equal_direct(&pl_testkit::rotated_pages(), &[100.0]);
}

#[test]
fn bands_equal_direct_render_user_unit() {
    check_bands_equal_direct(&pl_testkit::user_unit(), &[100.0]);
}

#[test]
fn bands_equal_direct_render_crop_box() {
    check_bands_equal_direct(&pl_testkit::crop_box(), &[100.0]);
}

#[test]
fn bands_equal_direct_render_with_markups_on_every_geometry() {
    // Plain, /Rotate 90, /UserUnit 2 and an offset CropBox with /Rotate 270, each with nine
    // kinds of markup (opacity, Multiply, rotated and scaled appearances, hidden and no-view).
    let (fixture, _) = pl_testkit::flatten_set();
    check_bands_equal_direct(&fixture, &[100.0]);
}

#[test]
fn bands_equal_direct_render_inherited_boxes() {
    check_bands_equal_direct(&pl_testkit::inherited_boxes(), &[90.0]);
}

/// Fit with the page turned onto the paper (and every other placement that is not a plain
/// window of the screen render): bands stitch into the same image rendered as one band, and
/// the fixture's marks are where `page_to_printable` says.
fn check_turned_pages(fixture: &Fixture) {
    let backend = backend();
    let doc = open(&backend, fixture);
    let mut turned = 0;
    for (index, truth) in fixture.manifest.pages.iter().enumerate() {
        let page = index as u32;
        let geometry = backend.page_info(doc, page).unwrap().geometry();
        for orientation in [
            PrintOrientation::Auto,
            PrintOrientation::Rotate { degrees: 90 },
            PrintOrientation::Rotate { degrees: 180 },
        ] {
            // Landscape paper: portrait pages turn under Auto, landscape ones stay.
            let paper = Paper::from_inches(11.0, 8.5, 96.0, 0.3);
            let banded = PrintJob {
                scaling: PrintScaling::Fit,
                orientation,
                print_flags: false,
                band_rows: 53,
                ..PrintJob::new(vec![page], paper)
            };
            let single = PrintJob {
                band_rows: paper.printable.height,
                ..banded.clone()
            };
            let (a, placement, bands) = print_one(&backend, doc, &banded);
            let (b, _, one) = print_one(&backend, doc, &single);
            assert!(bands > 1 && one == 1);
            assert_matches(
                &a,
                &b,
                &format!(
                    "{} page {index} {orientation:?} (turned {}, {:?} px), {bands} bands against one",
                    fixture.manifest.name, placement.rotation, placement.size
                ),
            );
            if placement.rotation != 0 {
                turned += 1;
            }
            let t = placement.page_to_printable(&geometry);
            let mut checked = 0;
            for s in &truth.segments {
                let mid = s.midpoint();
                if !inside_visible(truth, mid, 2.0) {
                    continue;
                }
                let p = t * Point::new(mid[0], mid[1]);
                assert!(
                    has_ink_near(&a, p.x, p.y, 2),
                    "{} page {index} {orientation:?}: no ink at segment midpoint {mid:?} -> ({:.1}, {:.1})",
                    fixture.manifest.name,
                    p.x,
                    p.y
                );
                checked += 1;
            }
            assert!(checked > 0 || truth.segments.is_empty());
        }
    }
    assert!(turned > 0);
    backend.close(doc).unwrap();
}

#[test]
fn turned_and_fitted_pages_plain_and_rotated() {
    check_turned_pages(&pl_testkit::plain());
    check_turned_pages(&pl_testkit::rotated_pages());
}

#[test]
fn turned_and_fitted_pages_user_unit_and_crop_box() {
    check_turned_pages(&pl_testkit::user_unit());
    check_turned_pages(&pl_testkit::crop_box());
}

/// A quarter turn on the paper is the screen render turned a quarter, give or take
/// anti-aliasing: PDFium draws the turned page itself, it is not a bitmap rotation.
#[test]
fn a_turned_page_is_the_screen_render_turned() {
    let fixture = pl_testkit::plain();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let direct = backend.render_page(doc, 0, 1.0).unwrap();
    for degrees in [90, 180, 270] {
        let size = if degrees == 180 {
            [direct.width, direct.height]
        } else {
            [direct.height, direct.width]
        };
        let reply = backend
            .render_window(WindowRequest {
                doc,
                page: 0,
                rotation: degrees,
                size,
                window: [0, 0, size[0], size[1]],
                options: WindowOptions::default(),
            })
            .unwrap();
        let turned = reply.bitmap;
        let (w, h) = (direct.width, direct.height);
        let mut expected = Bitmap::filled(size[0], size[1], [0; 4]);
        for y in 0..h {
            for x in 0..w {
                let (tx, ty) = match degrees {
                    90 => (h - 1 - y, x),
                    180 => (w - 1 - x, h - 1 - y),
                    _ => (y, w - 1 - x),
                };
                let i = ((ty * size[0] + tx) * 4) as usize;
                expected.pixels[i..i + 4].copy_from_slice(&direct.pixel(x, y));
            }
        }
        let differ = diff_count(&turned, &expected);
        let (worst, over) = grey_diff(&turned, &expected);
        println!("TURN {degrees}: {differ} px differ, worst grey {worst}, {over} over 1");
        // Ink in the same places: a small share of edge pixels may round differently.
        assert!(
            over * 50 < (w * h) as usize,
            "{degrees}: {over} of {} px differ by more than one grey level",
            w * h
        );
        // And the placement transform agrees with it.
        let placement = PagePlacement {
            page: 0,
            rotation: degrees,
            scale: [1.0, 1.0],
            size,
            offset: [0, 0],
            clipped: false,
        };
        let t = placement.page_to_printable(&geometry);
        for s in &fixture.manifest.pages[0].segments {
            let p = t * Point::new(s.midpoint()[0], s.midpoint()[1]);
            assert!(
                has_ink_near(&turned, p.x, p.y, 2),
                "{degrees}: no ink at {p:?}"
            );
        }
    }
    backend.close(doc).unwrap();
}

/// The number of pixels that differ inside and outside a set of page-space rectangles.
fn diff_inside_outside(
    a: &Bitmap,
    b: &Bitmap,
    t: pl_geom::Affine,
    rects: &[[f64; 4]],
    slack: f64,
) -> (usize, usize) {
    let boxes: Vec<[f64; 4]> = rects
        .iter()
        .map(|r| {
            let p = t * Point::new(r[0], r[1]);
            let q = t * Point::new(r[2], r[3]);
            [
                p.x.min(q.x) - slack,
                p.y.min(q.y) - slack,
                p.x.max(q.x) + slack,
                p.y.max(q.y) + slack,
            ]
        })
        .collect();
    let (mut inside, mut outside) = (0, 0);
    for y in 0..a.height {
        for x in 0..a.width {
            if a.pixel(x, y) != b.pixel(x, y) {
                let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                if boxes
                    .iter()
                    .any(|r| fx >= r[0] && fx <= r[2] && fy >= r[1] && fy <= r[3])
                {
                    inside += 1;
                } else {
                    outside += 1;
                }
            }
        }
    }
    (inside, outside)
}

#[test]
fn form_fields_can_be_left_out_and_come_back() {
    let fixture = pl_testkit::acro_form();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let paper = paper_around(&geometry, 150.0, 10);
    let with = actual_size_job(0, paper, 70, true);
    let without = PrintJob {
        form_fields: false,
        ..with.clone()
    };
    let (a, placement, _) = print_one(&backend, doc, &with);
    let (b, _, _) = print_one(&backend, doc, &without);
    let rects: Vec<[f64; 4]> = fixture
        .manifest
        .form_fields
        .iter()
        .map(|f| f.rect)
        .collect();
    let t = placement.page_to_printable(&geometry);
    let (inside, outside) = diff_inside_outside(&a, &b, t, &rects, 2.0);
    assert!(
        inside > 50,
        "leaving form fields out changed only {inside} px in the fields"
    );
    assert_eq!(
        outside, 0,
        "leaving form fields out changed the page outside the fields"
    );
    // The values are gone: the field boxes print blank, and they printed with fields on.
    let ink_in = |bm: &Bitmap, r: [f64; 4]| {
        let p = t * Point::new(r[0], r[1]);
        let q = t * Point::new(r[2], r[3]);
        let mut n = 0;
        for y in p.y.min(q.y).ceil() as u32..p.y.max(q.y).floor() as u32 {
            for x in p.x.min(q.x).ceil() as u32..p.x.max(q.x).floor() as u32 {
                let [r, g, b, _] = bm.pixel(x, y);
                n += usize::from(r.min(g).min(b) < 180);
            }
        }
        n
    };
    for f in &fixture.manifest.form_fields {
        assert!(ink_in(&a, f.rect) > 20, "field {} did not print", f.name);
        assert_eq!(
            ink_in(&b, f.rect),
            0,
            "field {} printed when left out",
            f.name
        );
    }
    // The widgets were only hidden for the render: the mirror is as it was.
    let annots = backend.annotations(doc, 0).unwrap();
    assert!(annots.iter().all(|a| !a.hidden));
    let (c, _, _) = print_one(&backend, doc, &with);
    assert_eq!(
        diff_count(&a, &c),
        0,
        "printing with fields again gives the same sheet"
    );
    backend.close(doc).unwrap();
}

#[test]
fn print_flags_show_no_view_markups_and_keep_hidden_ones_out() {
    let (fixture, _) = pl_testkit::flatten_set();
    let backend = backend();
    let doc = open(&backend, &fixture);
    for page in 0..fixture.manifest.pages.len() as u32 {
        let geometry = backend.page_info(doc, page).unwrap().geometry();
        let paper = paper_around(&geometry, 100.0, 10);
        let screen = actual_size_job(page, paper, 64, true);
        let printed = PrintJob {
            print_flags: true,
            ..screen.clone()
        };
        let (a, placement, _) = print_one(&backend, doc, &screen);
        let (b, _, _) = print_one(&backend, doc, &printed);
        let annots = backend.annotations(doc, page).unwrap();
        let no_view: Vec<[f64; 4]> = annots
            .iter()
            .filter(|x| x.name.as_deref().is_some_and(|n| n.ends_with("-36")))
            .map(|x| x.rect)
            .collect();
        assert_eq!(
            no_view.len(),
            1,
            "page {page}: the fixture's no-view square"
        );
        let t = placement.page_to_printable(&geometry);
        let (inside, outside) = diff_inside_outside(&a, &b, t, &no_view, 2.0);
        assert!(
            inside > 20,
            "page {page}: the no-view square did not print ({inside} px)"
        );
        assert_eq!(
            outside, 0,
            "page {page}: print flags changed something else"
        );
        // The flag was put back: the screen does not show it.
        let after = backend.annotations(doc, page).unwrap();
        assert_eq!(after, annots);
        let (c, _, _) = print_one(&backend, doc, &screen);
        assert_eq!(diff_count(&a, &c), 0, "page {page}: the mirror changed");
    }
    backend.close(doc).unwrap();
}

#[test]
fn print_flags_leave_out_a_markup_without_the_print_flag() {
    // The foreign-annotations fixture with the first annotation's /F 4 changed to /F 0 (same
    // length, so the cross-reference offsets still hold).
    let mut fixture = pl_testkit::annotations();
    let needle = b"/F 4";
    let at = fixture
        .bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("an /F 4 entry");
    fixture.bytes[at + 3] = b'0';
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let paper = paper_around(&geometry, 100.0, 10);
    let screen = actual_size_job(0, paper, 64, true);
    let printed = PrintJob {
        print_flags: true,
        ..screen.clone()
    };
    let (a, placement, _) = print_one(&backend, doc, &screen);
    let (b, _, _) = print_one(&backend, doc, &printed);
    let t = placement.page_to_printable(&geometry);
    let rects: Vec<[f64; 4]> = fixture.manifest.pages[0]
        .annotations
        .iter()
        .map(|x| x.rect)
        .collect();
    let (inside, outside) = diff_inside_outside(&a, &b, t, &rects, 3.0);
    assert!(
        inside > 50,
        "the non-printing markup still printed ({inside} px changed)"
    );
    assert_eq!(outside, 0);
    // It is the only one: the printed sheet has fewer inked pixels than the screen one.
    let ink = |bm: &Bitmap| {
        bm.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0].min(p[1]).min(p[2]) < 200)
            .count()
    };
    assert!(ink(&b) < ink(&a));
    backend.close(doc).unwrap();
}

#[test]
fn memory_per_band_is_bounded_on_a_large_sheet() {
    // A Letter page at 600 dpi actual size is 5100 x 6600 px (about 135 MB as one bitmap);
    // printed in 64-row bands it never holds more than a band and its gutter.
    let fixture = pl_testkit::plain();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let paper = Paper::from_inches(8.5, 11.0, 600.0, 0.0);
    let job = PrintJob {
        scaling: PrintScaling::ActualSize,
        orientation: PrintOrientation::AsIs,
        band_rows: 64,
        ..PrintJob::new(vec![0], paper)
    };
    let mut sink = MemorySink::records_only();
    let report = print_job(&backend, doc, &job, &mut sink).unwrap();
    assert_eq!(report.placements[0].size, [5100, 6600]);
    assert_eq!(report.bands, 6600_u32.div_ceil(64));
    assert!(report.peak_band_bytes <= report.band_bound);
    let whole = 5100u64 * 6600 * 4;
    assert!(
        report.band_bound * 20 < whole,
        "the bound {} is not far below the whole sheet {whole}",
        report.band_bound
    );
    for b in &sink.bands {
        assert_eq!((b.width, b.x), (5100, 0));
        assert_eq!(b.bytes, 5100 * 4 * b.height as usize);
    }
    backend.close(doc).unwrap();
}

#[test]
fn the_bgr_format_reaches_the_sink_with_dib_rows() {
    let fixture = pl_testkit::plain();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let paper = paper_around(&geometry, 75.0, 5);
    let rgba = actual_size_job(0, paper, 40, true);
    let bgr = PrintJob {
        format: BandFormat::Bgr8,
        ..rgba.clone()
    };
    let (a, _, _) = print_one(&backend, doc, &rgba);
    let (b, _, _) = print_one(&backend, doc, &bgr);
    assert_eq!(diff_count(&a, &b), 0);
    backend.close(doc).unwrap();
}

fn sample_path() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    path.is_file().then_some(path)
}

/// One sheet of the real sample (34 x 22 in) printed at actual size on 34 x 22 in paper at
/// 300 dpi, into a sink that keeps no pixels; timed. At 75 dpi the stitched bands are also
/// compared with the direct render. Works on a copy under the target directory.
#[test]
fn real_sample_sheet_prints_in_bands_at_300_dpi() {
    let Some(source) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pl-render-print");
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join(format!("sample-{}.pdf", std::process::id()));
    std::fs::copy(&source, &copy).unwrap();
    let backend = backend();
    let doc = backend.open(DocSource::file(&copy), None).unwrap().doc;
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let points = geometry.display_size_points();
    println!(
        "SAMPLE sheet: {:.1} x {:.1} in",
        points.width / 72.0,
        points.height / 72.0
    );

    let paper = Paper::from_inches(34.0, 22.0, 300.0, 0.0);
    let job = PrintJob {
        scaling: PrintScaling::ActualSize,
        orientation: PrintOrientation::Auto,
        ..PrintJob::new(vec![0], paper)
    };
    // Warm the page handle (parsing the sheet's content once) so the timing is the banding.
    let warm = Instant::now();
    backend.render_page(doc, 0, 0.05).unwrap();
    let parse = warm.elapsed();
    let mut sink = MemorySink::records_only();
    let start = Instant::now();
    let report = print_job(&backend, doc, &job, &mut sink).unwrap();
    let elapsed = start.elapsed();
    let placement = report.placements[0];
    println!(
        "TIMING sample sheet at 300 dpi: {:?} px in {} bands of {} rows, {:.2} s (first parse {:.2} s), peak band memory {:.1} MB (bound {:.1} MB)",
        placement.size,
        report.bands,
        job.band_rows,
        elapsed.as_secs_f64(),
        parse.as_secs_f64(),
        report.peak_band_bytes as f64 / 1e6,
        report.band_bound as f64 / 1e6
    );
    assert!(report.peak_band_bytes <= report.band_bound);
    assert_eq!(sink.bands.len() as u32, report.bands);
    assert!(report.bands > 1);
    // Budget (docs/measurements.md, section 9): measured about 0.5 s.
    assert!(
        elapsed.as_secs_f64() < 15.0,
        "printing one sheet took {elapsed:?}"
    );

    // Equality at a lower resolution.
    let small = paper_around(&geometry, 75.0, 0);
    let check = actual_size_job(0, small, 97, true);
    let (sheet, placement, _) = print_one(&backend, doc, &check);
    let direct = backend
        .render_page_with(doc, 0, 75.0 / 72.0, RenderOptions { annotations: true })
        .unwrap();
    let want = canvas_with(&small, &direct, placement.offset);
    assert_matches(&sheet, &want, "sample sheet at 75 dpi");
    backend.close(doc).unwrap();
    let _ = std::fs::remove_file(&copy);
}
