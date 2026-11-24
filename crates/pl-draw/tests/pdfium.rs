//! The emitted appearance stream, rendered by PDFium, against the tiny-skia raster of the same
//! `DrawList`: every shape helper, dash pattern, line ending, opacity and Multiply case, on a
//! plain page and on rotated, `/UserUnit` and offset-CropBox pages.
//!
//! PDFium renders the page twice, without and with the annotation. The bare render is the
//! backdrop the rasteriser composites onto, exactly as the app composites a Multiply markup
//! onto a tile, so the two results are directly comparable.

mod cases;
mod common;

use cases::Case;
use common::{Rendered, Variant, CROP_ROTATED, PLAIN, VARIANTS};
use pdf_writer::types::AnnotationType;
use pl_draw::kurbo::Rect;
use pl_render::PdfiumBackend;

/// Neighbourhood radius and grey-level tolerance of the comparison (see `common::Diff`).
const RADIUS: i32 = 1;
const TOLERANCE: i32 = 40;
/// Upper bound on the mean absolute difference, in grey levels, over the whole page.
const MAX_MEAN: f64 = 0.6;

/// How many out-of-tolerance pixels (of the 345,600 on a test page) a case may have.
///
/// * Anything: 3, for single anti-aliased pixels on the edge of a composited group.
/// * Dashed strokes: 40. Where a dash wraps through the closing point of a closed outline
///   Skia joins it into one dash with a proper corner and PDFium butts two dashes together,
///   leaving a notch of a few pixels; and a dash that ends exactly on a vertex is joined
///   differently. Both are a handful of pixels at one or two corners.
/// * A rotated image: 600. PDFium does not anti-alias the slanted edge of an image and covers
///   up to a pixel more of it; the interior is identical.
fn allowance(name: &str) -> usize {
    if name.starts_with("dash-") || name == "le-dashed-line" {
        40
    } else if name == "image-rotated" {
        600
    } else {
        3
    }
}

/// The window all variants show; every case must stay inside it.
const WINDOW: Rect = Rect::new(30.0, 20.0, 230.0, 150.0);

fn check(backend: &PdfiumBackend, case: &Case, variant: &Variant, failures: &mut Vec<String>) {
    let bounds = case.list.bounds().expect("the case draws something");
    assert!(
        WINDOW.union(bounds) == WINDOW,
        "case {} leaves the test window: {bounds:?}",
        case.name
    );
    let Rendered {
        backdrop,
        pdfium,
        ours,
    } = common::render_both(backend, &case.list, variant, AnnotationType::Square);
    let label = format!("{}@{}", case.name, variant.name);

    // Guard against agreeing on nothing: the markup must be visible in both renders.
    let drawn_pdfium = common::changed_pixels(&backdrop, &pdfium, 12);
    let drawn_ours = common::changed_pixels(&backdrop, &ours, 12);
    let d = common::diff(&ours, &pdfium, RADIUS, TOLERANCE);
    if std::env::var_os("PL_DRAW_VERBOSE").is_some() {
        println!("{label:55} pdfium {drawn_pdfium:7} ours {drawn_ours:7} {d:?}");
        if d.bad != 0 {
            let at = |list: Vec<(i32, i32)>| -> Vec<String> {
                list.iter()
                    .take(6)
                    .map(|(x, y)| {
                        format!(
                            "({x},{y}) ours {:?} pdfium {:?}",
                            ours.pixel(*x as u32, *y as u32),
                            pdfium.pixel(*x as u32, *y as u32)
                        )
                    })
                    .collect()
            };
            println!(
                "    ours-not-in-pdfium {:?}",
                at(common::bad_pixels(&ours, &pdfium, RADIUS, TOLERANCE))
            );
            println!(
                "    pdfium-not-in-ours {:?}",
                at(common::bad_pixels(&pdfium, &ours, RADIUS, TOLERANCE))
            );
        }
    }
    let mut problems = Vec::new();
    if drawn_pdfium < 40 || drawn_ours < 40 {
        problems.push(format!(
            "barely drawn (pdfium {drawn_pdfium} px, ours {drawn_ours} px)"
        ));
    }
    if d.bad > allowance(&case.name) {
        problems.push(format!("{} pixels disagree beyond tolerance", d.bad));
    }
    // A rotated, thirty-times enlarged image: every slanted texel boundary falls a fraction of a
    // pixel differently in the two samplers, which adds up over the page.
    let max_mean = if case.name == "image-rotated" {
        2.0 * MAX_MEAN
    } else {
        MAX_MEAN
    };
    if d.mean > max_mean {
        problems.push(format!("mean difference {:.3}", d.mean));
    }
    if !problems.is_empty() {
        let p = common::dump(&pdfium, &format!("{label}-pdfium"));
        common::dump(&ours, &format!("{label}-ours"));
        failures.push(format!(
            "{label}: {} (images beside {})",
            problems.join("; "),
            p.display()
        ));
    }
}

fn run(cases: Vec<Case>, variants: &[Variant]) {
    let backend = common::backend();
    let mut failures = Vec::new();
    let mut count = 0;
    for case in &cases {
        for variant in variants {
            check(&backend, case, variant, &mut failures);
            count += 1;
        }
    }
    assert!(count > 0);
    assert!(
        failures.is_empty(),
        "{} of {count} comparisons failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn shapes_match_pdfium_on_every_page_variant() {
    run(cases::shape_cases(), &VARIANTS);
}

#[test]
fn caps_joins_and_miter_limits_match_pdfium_on_every_page_variant() {
    run(cases::stroke_style_cases(), &VARIANTS);
}

#[test]
fn dash_patterns_match_pdfium_on_every_page_variant() {
    run(cases::dash_cases(), &VARIANTS);
}

#[test]
fn line_endings_match_pdfium_on_every_page_variant() {
    run(cases::ending_cases(), &VARIANTS);
}

#[test]
fn opacity_matches_pdfium_on_every_page_variant() {
    run(cases::opacity_cases(), &VARIANTS);
}

#[test]
fn multiply_matches_pdfium_on_every_page_variant() {
    run(cases::multiply_cases(), &VARIANTS);
}

#[test]
fn groups_and_images_match_pdfium_on_every_page_variant() {
    run(cases::group_and_image_cases(), &VARIANTS);
}

#[test]
fn everything_matches_pdfium_on_a_rotated_page_with_an_offset_crop_box() {
    run(cases::all_geometry_cases(), &[CROP_ROTATED]);
}

/// A dashed cloud: PDFium and the rasteriser put the dashes on the same outline with the same
/// amount of ink, though not at the same places along it (see `cases::dashed_cloud_cases`).
#[test]
fn a_dashed_cloud_follows_the_same_outline_in_both_renders() {
    let backend = common::backend();
    let (dashed, solid) = cases::dashed_cloud_cases();
    for variant in VARIANTS.iter().chain([&CROP_ROTATED]) {
        // The solid border agrees pixel for pixel.
        let s = common::render_both(&backend, &solid.list, variant, AnnotationType::Square);
        let d = common::diff(&s.ours, &s.pdfium, RADIUS, TOLERANCE);
        assert!(d.bad <= 3, "{}: solid cloud {d:?}", variant.name);

        let r = common::render_both(&backend, &dashed.list, variant, AnnotationType::Square);
        let ink = |image: &pl_render::Bitmap, base: &pl_render::Bitmap, x: u32, y: u32| {
            let (a, b) = (image.pixel(x, y), base.pixel(x, y));
            (0..3).any(|c| (a[c] as i32 - b[c] as i32).abs() > 40)
        };
        let (w, h) = (r.backdrop.width, r.backdrop.height);
        let mut counts = [0usize; 3]; // pdfium dashed, ours dashed, solid
        for y in 0..h {
            for x in 0..w {
                let on_outline = (-1i32..=1).any(|dy| {
                    (-1i32..=1).any(|dx| {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        nx >= 0
                            && ny >= 0
                            && (nx as u32) < w
                            && (ny as u32) < h
                            && ink(&s.ours, &s.backdrop, nx as u32, ny as u32)
                    })
                });
                for (i, image) in [&r.pdfium, &r.ours].into_iter().enumerate() {
                    if ink(image, &r.backdrop, x, y) {
                        counts[i] += 1;
                        assert!(
                            on_outline,
                            "{}: dash ink off the outline at ({x}, {y}) in render {i}",
                            variant.name
                        );
                    }
                }
                if ink(&s.ours, &s.backdrop, x, y) {
                    counts[2] += 1;
                }
            }
        }
        let [pdfium, ours, full] = counts.map(|c| c as f64);
        // 5 on, 3 off: about five eighths of the solid border is inked, in both.
        assert!(
            (pdfium / ours - 1.0).abs() < 0.05,
            "{}: {counts:?}",
            variant.name
        );
        assert!(
            (0.5..0.8).contains(&(ours / full)),
            "{}: {counts:?}",
            variant.name
        );
        assert!(
            (0.5..0.8).contains(&(pdfium / full)),
            "{}: {counts:?}",
            variant.name
        );
    }
}

/// The comparison itself must be able to fail: a drawing that is shifted, recoloured or missing
/// is reported.
#[test]
fn the_comparison_detects_a_wrong_drawing() {
    use pl_draw::kurbo::Affine;
    let backend = common::backend();
    let case = cases::shape_cases()
        .into_iter()
        .find(|c| c.name == "rect-stroke")
        .unwrap();
    let r = common::render_both(&backend, &case.list, &PLAIN, AnnotationType::Square);
    assert_eq!(common::diff(&r.ours, &r.pdfium, RADIUS, TOLERANCE).bad, 0);

    let geometry = PLAIN.geometry();
    let raster = |list: &pl_draw::DrawList, extra: Affine| {
        let pixels = pl_draw::render_over_rgba(
            list,
            pl_draw::page_transform(&geometry, common::SCALE) * extra,
            &r.backdrop.pixels,
            r.backdrop.width,
            r.backdrop.height,
        )
        .unwrap();
        pl_render::Bitmap {
            width: r.backdrop.width,
            height: r.backdrop.height,
            pixels,
        }
    };
    // Shifted by one unit (3 px).
    let shifted = raster(&case.list, Affine::translate((1.0, 0.0)));
    assert!(common::diff(&shifted, &r.pdfium, RADIUS, TOLERANCE).bad > 100);
    // A different colour.
    let mut recoloured = case.list.clone();
    if let pl_draw::DrawOp::Path(p) = &mut recoloured.ops[0] {
        p.stroke.as_mut().unwrap().color = pl_draw::Rgb::new(0.1, 0.25, 0.8);
    }
    assert!(
        common::diff(
            &raster(&recoloured, Affine::IDENTITY),
            &r.pdfium,
            RADIUS,
            TOLERANCE
        )
        .bad > 100
    );
    // A stroke half as wide again.
    let mut wider = case.list.clone();
    if let pl_draw::DrawOp::Path(p) = &mut wider.ops[0] {
        p.stroke.as_mut().unwrap().width = 4.5;
    }
    assert!(
        common::diff(
            &raster(&wider, Affine::IDENTITY),
            &r.pdfium,
            RADIUS,
            TOLERANCE
        )
        .bad > 100
    );
    // Nothing at all.
    assert!(common::diff(&r.backdrop, &r.pdfium, RADIUS, TOLERANCE).bad > 100);
}

/// Text under a highlight keeps its dark pixels, in PDFium's render of the emitted stream and
/// in ours (plan, M2 acceptance).
#[test]
fn ink_under_a_multiply_highlight_stays_dark_in_both_renders() {
    let backend = common::backend();
    let case = cases::multiply_cases()
        .into_iter()
        .find(|c| c.name == "multiply-highlight")
        .unwrap();
    for subtype in [AnnotationType::Highlight, AnnotationType::Square] {
        for variant in VARIANTS.iter().chain([&CROP_ROTATED]) {
            let r = common::render_both(&backend, &case.list, variant, subtype);
            let mut dark = 0;
            let mut tinted = 0;
            for ((b, p), o) in r
                .backdrop
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .zip(r.pdfium.pixels.as_chunks::<4>().0)
                .zip(r.ours.pixels.as_chunks::<4>().0)
            {
                if b[..3] == [0, 0, 0] {
                    // Black ink is black in both, highlighted or not.
                    assert!(
                        p[..3].iter().all(|v| *v <= 4),
                        "PDFium lightened ink: {p:?}"
                    );
                    assert!(
                        o[..3].iter().all(|v| *v <= 4),
                        "raster lightened ink: {o:?}"
                    );
                    dark += 1;
                } else if b[..3] == [255, 255, 255] && p[..3] != [255, 255, 255] {
                    // White paper under the highlight takes the highlight colour in both.
                    if (0..3).all(|c| (p[c] as i32 - o[c] as i32).abs() <= 3) && p[2] < 100 {
                        tinted += 1;
                    }
                }
            }
            assert!(dark > 5_000, "{subtype:?} {}: {dark}", variant.name);
            assert!(tinted > 5_000, "{subtype:?} {}: {tinted}", variant.name);
        }
    }
}

/// Text runs: PDFium substitutes its own font for Helvetica, so glyph shapes differ. What must
/// agree is where the run is and how far it extends, which is what the metrics promise.
#[test]
fn text_runs_land_where_pdfium_draws_them() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for case in cases::text_cases() {
        for variant in VARIANTS.iter().chain([&CROP_ROTATED]) {
            let r = common::render_both(&backend, &case.list, variant, AnnotationType::Square);
            let label = format!("{}@{}", case.name, variant.name);
            let ink = |image: &pl_render::Bitmap| -> Option<(u32, u32, u32, u32, usize)> {
                let mut bx: Option<(u32, u32, u32, u32)> = None;
                let mut n = 0;
                for y in 0..image.height {
                    for x in 0..image.width {
                        let a = image.pixel(x, y);
                        let b = r.backdrop.pixel(x, y);
                        if (0..3).any(|c| (a[c] as i32 - b[c] as i32).abs() > 60) {
                            n += 1;
                            bx = Some(match bx {
                                None => (x, y, x, y),
                                Some((x0, y0, x1, y1)) => {
                                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                                }
                            });
                        }
                    }
                }
                bx.map(|(x0, y0, x1, y1)| (x0, y0, x1, y1, n))
            };
            let (Some(p), Some(o)) = (ink(&r.pdfium), ink(&r.ours)) else {
                failures.push(format!("{label}: text missing from a render"));
                continue;
            };
            if std::env::var_os("PL_DRAW_VERBOSE").is_some() {
                println!("{label:40} pdfium {p:?} ours {o:?}");
            }
            // Extents within 2 units (6 px) on every side; ink amount within a third.
            let tol = (2.0 * common::SCALE) as i64;
            let sides = [
                (p.0 as i64 - o.0 as i64).abs(),
                (p.1 as i64 - o.1 as i64).abs(),
                (p.2 as i64 - o.2 as i64).abs(),
                (p.3 as i64 - o.3 as i64).abs(),
            ];
            let ratio = o.4 as f64 / p.4 as f64;
            if sides.iter().any(|s| *s > tol) || !(0.67..=1.5).contains(&ratio) {
                common::dump(&r.pdfium, &format!("{label}-pdfium"));
                common::dump(&r.ours, &format!("{label}-ours"));
                failures.push(format!("{label}: pdfium {p:?} ours {o:?} ratio {ratio:.2}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
