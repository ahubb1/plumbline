//! Text markups made from a text selection: the quads of a Highlight, Underline, StrikeOut
//! and Squiggly are the boxes PDFium reports for the selected characters, merged per line, on
//! a plain page and on rotated, `/UserUnit` and offset-box pages.

mod common;

use common::{Work, TARGETS};
use pl_annot::{reading_direction, AnnotKind, AnnotSet, Annotation, Geometry};
use pl_geom::{Point, Rect};
use pl_pdf::Object;

/// The characters of the first lines of a page, as PDFium reports them: per line, the boxes
/// and origins of the characters that have a box.
fn lines_of(text: &pl_render::PageText, most: usize) -> Vec<Vec<(Rect, Point, char)>> {
    let mut lines: Vec<Vec<(Rect, Point, char)>> = vec![Vec::new()];
    for c in &text.chars {
        if c.ch == '\n' || c.ch == '\r' {
            if !lines.last().unwrap().is_empty() {
                lines.push(Vec::new());
            }
            continue;
        }
        if c.has_box() {
            lines.last_mut().unwrap().push((
                Rect::new(c.bbox[0], c.bbox[1], c.bbox[2], c.bbox[3]),
                Point::new(c.origin[0], c.origin[1]),
                c.ch,
            ));
        }
    }
    lines.retain(|l| l.len() >= 3);
    lines.truncate(most);
    lines
}

#[test]
fn selection_quads_match_pdfium_character_boxes_on_every_page_layout() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for target in &TARGETS {
        let fixture = (target.fixture)();
        let truth = &fixture.manifest.pages[target.page];
        let mut probe = Work::new(&fixture);
        let geometry = probe.geometry(target.page);
        let text = common::page_text(&backend, fixture.bytes.clone(), target.page, &geometry);
        let lines = lines_of(&text, 2);
        assert!(!lines.is_empty(), "{}: the page has text", target.name);
        let boxes: Vec<Rect> = lines.iter().flatten().map(|c| c.0).collect();
        let origins: Vec<Point> = lines.iter().flatten().map(|c| c.1).collect();
        // The page's content is set normally whatever /Rotate says.
        let direction = reading_direction(&origins);
        assert_eq!(direction, 0, "{}", target.name);
        // 1 pt in the page's units.
        let tolerance = 1.0 / geometry.user_unit();

        for kind in [
            AnnotKind::Highlight,
            AnnotKind::Underline,
            AnnotKind::StrikeOut,
            AnnotKind::Squiggly,
        ] {
            let label = format!("{}@{}", kind.pdf_name(), target.name);
            let a = Annotation::text_markup_from_boxes(
                kind.clone(),
                target.page,
                &boxes,
                direction,
                &common::origin(),
            )
            .unwrap();
            assert_eq!(a.kind, kind);
            let Geometry::Quads(quads) = &a.geometry else {
                panic!("{label}: quads");
            };
            assert_eq!(quads.len(), lines.len(), "{label}: one quad per line");
            for (quad, line) in quads.iter().zip(&lines) {
                // Exactly the union of the line's character boxes.
                let union = line.iter().skip(1).fold(line[0].0, |r, c| r.union(c.0));
                let want = pl_annot::quad_for_box(union, 0);
                for (p, q) in quad.iter().zip(&want) {
                    assert!(
                        (*p - *q).hypot() < 1e-9,
                        "{label}: {quad:?} against {want:?}"
                    );
                }
                // And where the fixture says the text is: from the first glyph's origin over
                // the advance width, around the baseline.
                let string: String = line.iter().map(|c| c.2).collect();
                let known = truth
                    .texts
                    .iter()
                    .find(|t| t.text.replace(' ', "") == string.replace(' ', ""))
                    .unwrap_or_else(|| panic!("{label}: {string:?} is a text of the fixture"));
                assert!((quad[0].x - known.origin[0]).abs() < tolerance, "{label}");
                assert!(
                    (quad[1].x - (known.origin[0] + known.width)).abs() < tolerance,
                    "{label}: ends at {} against {}",
                    quad[1].x,
                    known.origin[0] + known.width
                );
                assert!(
                    quad[0].y < known.origin[1] && quad[3].y > known.origin[1],
                    "{label}: the baseline is inside the quad"
                );
                assert!(
                    quad[3].y - quad[0].y < 1.5 * known.font_size,
                    "{label}: about a line high"
                );
            }

            // Saved, the quads are in /QuadPoints (top-left, top-right, bottom-left,
            // bottom-right) and read back as the same quads.
            let mut work = Work::new(&fixture);
            let before = work.doc().clone();
            let mut set = AnnotSet::load(&before);
            set.add(a.clone()).unwrap();
            let (plan, _) = work.save(&set);
            let after = work.doc().clone();
            common::assert_untouched(&before, &after, &[before.page_id(target.page).unwrap()]);
            let dict = after
                .object(plan.added[0].object)
                .unwrap()
                .as_dict()
                .unwrap();
            assert_eq!(
                dict.get(b"Subtype").unwrap(),
                &Object::Name(kind.pdf_name().as_bytes().to_vec())
            );
            let numbers: Vec<f64> = dict
                .get(b"QuadPoints")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|n| pl_pdf::number(n).unwrap())
                .collect();
            assert_eq!(numbers.len(), 8 * lines.len(), "{label}");
            let q = &quads[0];
            for (got, want) in numbers[..8].iter().zip([
                q[3].x, q[3].y, q[2].x, q[2].y, q[0].x, q[0].y, q[1].x, q[1].y,
            ]) {
                assert!((got - want).abs() < 1e-3, "{label}: {numbers:?}");
            }
            let loaded = AnnotSet::load(&after);
            let got = loaded.get(&a.id).unwrap();
            assert!(!loaded.is_dirty() && got.is_modelled(), "{label}");
            let Geometry::Quads(read) = &got.geometry else {
                panic!("{label}: quads");
            };
            assert_eq!(read.len(), quads.len());
            for (r, w) in read.iter().flatten().zip(quads.iter().flatten()) {
                assert!((*r - *w).hypot() < 1e-3, "{label}: read back");
            }
            // Everything else is equal.
            let mut same = a.detached();
            same.geometry = got.geometry.clone();
            assert_eq!(got.detached(), same, "{label}");

            // PDFium draws the saved markup like the raster, over the text it marks.
            let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
            let ours = common::raster_over(&a.draw_list(), &geometry, &r.backdrop);
            let d = common::diff(&r.pdfium, &ours, 1, 40);
            if d.bad > 3 || d.mean > 0.6 {
                common::dump(&r.pdfium, &format!("selection-{label}-pdfium"));
                common::dump(&ours, &format!("selection-{label}-ours"));
                failures.push(format!("{label}: {d:?}"));
            }
            assert!(
                common::changed_pixels(&r.pdfium, &r.backdrop, 30) > 50,
                "{label}: the markup shows"
            );
            if kind == AnnotKind::Highlight {
                // Multiply: the text under the highlight is as dark as before.
                let (before, after) = (
                    common::dark_pixels(&r.backdrop, 90),
                    common::dark_pixels(&r.pdfium, 90),
                );
                assert!(
                    after as f64 >= 0.97 * before as f64,
                    "{label}: {after} dark pixels of {before}"
                );
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_selection_across_sizes_and_lines_gives_straight_bands() {
    // The plain page has four lines of different sizes; select from the middle of the first
    // to the middle of the third.
    let backend = common::backend();
    let fixture = pl_testkit::plain();
    let mut probe = Work::new(&fixture);
    let geometry = probe.geometry(0);
    let text = common::page_text(&backend, fixture.bytes.clone(), 0, &geometry);
    let lines = lines_of(&text, 3);
    assert_eq!(lines.len(), 3);
    let mut boxes: Vec<Rect> = Vec::new();
    boxes.extend(lines[0][10..].iter().map(|c| c.0));
    boxes.extend(lines[1].iter().map(|c| c.0));
    boxes.extend(lines[2][..5].iter().map(|c| c.0));
    let a =
        Annotation::text_markup_from_boxes(AnnotKind::StrikeOut, 0, &boxes, 0, &common::origin())
            .unwrap();
    let Geometry::Quads(quads) = &a.geometry else {
        panic!()
    };
    assert_eq!(quads.len(), 3);
    assert!((quads[0][0].x - lines[0][10].0.x0).abs() < 1e-9);
    assert!((quads[0][1].x - lines[0].last().unwrap().0.x1).abs() < 1e-9);
    assert!((quads[2][1].x - lines[2][4].0.x1).abs() < 1e-9);
    for q in quads {
        // Axis-aligned, anticlockwise from the bottom-left.
        assert!(q[0].y == q[1].y && q[2].y == q[3].y && q[0].x == q[3].x && q[1].x == q[2].x);
        assert!(q[1].x > q[0].x && q[3].y > q[0].y);
    }
    // The lines are 18, 12 and 24 units: each band is as tall as its own line.
    let heights: Vec<f64> = quads.iter().map(|q| q[3].y - q[0].y).collect();
    assert!(
        heights[2] > heights[0] && heights[0] > heights[1],
        "{heights:?}"
    );
}
