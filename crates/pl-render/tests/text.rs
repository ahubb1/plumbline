//! Text extraction, search and outline against fixture ground truth, through real PDFium.

mod common;

use common::*;
use pl_render::{quad_bounds, PageText, RenderBackend, SearchOptions};
use pl_testkit::{Fixture, TextTruth};

const CASE: SearchOptions = SearchOptions {
    match_case: true,
    whole_word: false,
};

/// The union of the boxes of the characters `[start, start + len)`.
fn chars_bounds(text: &PageText, start: u32, len: u32) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for c in &text.chars[start as usize..(start + len) as usize] {
        if c.has_box() {
            b[0] = b[0].min(c.bbox[0]);
            b[1] = b[1].min(c.bbox[1]);
            b[2] = b[2].max(c.bbox[2]);
            b[3] = b[3].max(c.bbox[3]);
        }
    }
    b
}

/// Checks that a box found by PDFium sits where the manifest says the string was drawn.
/// PDFium substitutes its own font for Helvetica and the manifest widths come from an AFM
/// table, so widths get a few percent of slack; the origin is exact.
fn assert_box_matches(found: [f64; 4], truth: &TextTruth, what: &str) {
    let want = truth.bbox();
    let size = truth.font_size;
    assert!(
        (found[0] - want[0]).abs() <= 0.1 * size,
        "{what}: left edge {} vs {}",
        found[0],
        want[0]
    );
    assert!(
        (found[2] - want[2]).abs() <= 0.05 * truth.width + 0.2 * size,
        "{what}: right edge {} vs {}",
        found[2],
        want[2]
    );
    // The box must contain the body of the glyphs: from the baseline to the x-height.
    assert!(
        found[1] <= truth.origin[1] + 0.05 * size && found[3] >= truth.origin[1] + 0.5 * size,
        "{what}: vertical extent {}..{} around baseline {}",
        found[1],
        found[3],
        truth.origin[1]
    );
    // And it must not be wildly tall.
    assert!(
        found[3] - found[1] <= 1.6 * size,
        "{what}: height {}",
        found[3] - found[1]
    );
}

/// Extracted text equals the manifest, and every string's character boxes are at the manifest
/// position in unrotated page space, whatever the page's rotation, crop box or user unit.
fn check_text(fixture: &Fixture) {
    let backend = backend();
    let doc = open(&backend, fixture);
    for (index, truth) in fixture.manifest.pages.iter().enumerate() {
        let page = index as u32;
        let text = backend.page_text(doc, page).unwrap();
        assert_eq!(text.page, page);

        let mut got = text.lines();
        let mut want: Vec<String> = truth.texts.iter().map(|t| t.text.clone()).collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{} page {index}", fixture.manifest.name);

        for t in &truth.texts {
            let what = format!("{} page {index} {:?}", fixture.manifest.name, t.text);
            let hits = backend.search(doc, page, &t.text, CASE).unwrap();
            assert_eq!(hits.len(), 1, "{what}");
            let hit = &hits[0];
            assert_eq!(hit.page, page);
            assert_eq!(hit.char_len as usize, t.text.chars().count(), "{what}");
            let matched: String = text.chars
                [hit.char_start as usize..(hit.char_start + hit.char_len) as usize]
                .iter()
                .map(|c| c.ch)
                .collect();
            assert_eq!(matched, t.text, "{what}");

            assert_box_matches(chars_bounds(&text, hit.char_start, hit.char_len), t, &what);
            assert_eq!(hit.quads.len(), 1, "{what}: one line, one quad");
            assert_box_matches(quad_bounds(&hit.quads[0]), t, &what);

            // The first character starts at the string's origin, on the baseline.
            let first = &text.chars[hit.char_start as usize];
            assert!(
                (first.origin[0] - t.origin[0]).abs() < 0.05,
                "{what}: origin x"
            );
            assert!(
                (first.origin[1] - t.origin[1]).abs() < 0.05,
                "{what}: origin y"
            );
            assert!(
                (first.font_size as f64 - t.font_size).abs() < 0.05,
                "{what}: font size"
            );
            assert!(!first.generated);
        }
    }
    backend.close(doc).unwrap();
}

#[test]
fn extracted_text_equals_ground_truth_plain() {
    check_text(&pl_testkit::plain());
}

#[test]
fn extracted_text_equals_ground_truth_rotated() {
    check_text(&pl_testkit::rotated_pages());
}

#[test]
fn extracted_text_equals_ground_truth_user_unit_and_crop_box() {
    check_text(&pl_testkit::user_unit());
    check_text(&pl_testkit::crop_box());
}

#[test]
fn extracted_text_equals_ground_truth_inherited_boxes() {
    // Character boxes are in page space; with an inherited MediaBox whose origin is not zero
    // they only line up with the page if the reported geometry has that origin too.
    check_text(&pl_testkit::inherited_boxes());
    check_text(&pl_testkit::linearized());
}

#[test]
fn extracted_text_equals_ground_truth_multi_page_and_object_streams() {
    check_text(&pl_testkit::multi_page());
    check_text(&pl_testkit::xref_classic());
    check_text(&pl_testkit::xref_object_streams());
}

#[test]
fn search_finds_each_page_token_on_its_own_page_only() {
    let fixture = pl_testkit::multi_page();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let pages = fixture.manifest.page_count();
    for target in [0usize, 7, 23] {
        let token = pl_testkit::multi_page_token(target);
        let truth = fixture.manifest.pages[target]
            .texts
            .iter()
            .find(|t| t.text.contains(&token))
            .expect("fixture text with the token");
        let offset = truth.text.find(&token).unwrap();
        for page in 0..pages {
            // Lower-case query, default options: the search is case-insensitive.
            let hits = backend
                .search(
                    doc,
                    page as u32,
                    &token.to_lowercase(),
                    SearchOptions::default(),
                )
                .unwrap();
            if page != target {
                assert!(hits.is_empty(), "{token} found on page {page}");
                continue;
            }
            assert_eq!(hits.len(), 1, "{token} on its own page");
            let b = quad_bounds(&hits[0].quads[0]);
            // The hit lies inside the string's box, at the token's offset along it.
            let whole = truth.bbox();
            let size = truth.font_size;
            assert!(
                b[0] >= whole[0] - 0.1 * size && b[2] <= whole[2] + 0.3 * size,
                "{b:?} in {whole:?}"
            );
            assert!(b[1] <= truth.origin[1] + 0.05 * size && b[3] >= truth.origin[1] + 0.5 * size);
            let expected_left =
                truth.origin[0] + pl_testkit::helvetica_width(&truth.text[..offset], size);
            assert!(
                (b[0] - expected_left).abs() <= 0.05 * truth.width + 0.2 * size,
                "hit starts at {} but the token starts near {expected_left}",
                b[0]
            );
            assert!(backend
                .search(doc, page as u32, &token.to_lowercase(), CASE)
                .unwrap()
                .is_empty());
        }
    }
    // Whole-word: "PLUMB" alone is only a prefix of the token.
    let whole_word = SearchOptions {
        match_case: false,
        whole_word: true,
    };
    let loose = backend
        .search(doc, 0, "PLUMB-PAGE", SearchOptions::default())
        .unwrap();
    assert_eq!(loose.len(), 1);
    assert!(backend
        .search(doc, 0, "PLUMB-PAG", whole_word)
        .unwrap()
        .is_empty());
    assert!(backend
        .search(doc, 0, "no such words here", SearchOptions::default())
        .unwrap()
        .is_empty());
    backend.close(doc).unwrap();
}

#[test]
fn search_quads_are_in_unrotated_page_space() {
    // Same string position on four pages with /Rotate 0, 90, 180, 270: the quads must agree
    // with the manifest (which is unrotated) on every page.
    let fixture = pl_testkit::rotated_pages();
    let backend = backend();
    let doc = open(&backend, &fixture);
    for (index, truth) in fixture.manifest.pages.iter().enumerate() {
        let label = &truth.texts[0];
        let hits = backend
            .search(doc, index as u32, "rotate", SearchOptions::default())
            .unwrap();
        assert_eq!(hits.len(), 1, "page {index}");
        let b = quad_bounds(&hits[0].quads[0]);
        assert!(
            (b[0] - label.origin[0]).abs() < 1.5,
            "page {index}: {b:?} vs {:?}",
            label.origin
        );
        assert!(b[1] <= label.origin[1] + 1.0 && b[3] >= label.origin[1] + 0.5 * label.font_size);
        // Quad corner order: bottom-left, bottom-right, top-right, top-left.
        let q = hits[0].quads[0];
        assert!(q[0][0] < q[1][0] && q[0][1] == q[1][1] && q[2][1] > q[1][1] && q[3][0] == q[0][0]);
    }
    backend.close(doc).unwrap();
}

#[test]
fn outline_matches_the_manifest() {
    let fixture = pl_testkit::multi_page();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let outline = backend.outline(doc).unwrap();
    let truth = &fixture.manifest.outline;
    assert_eq!(outline.len(), truth.len());
    assert_eq!(outline.len(), 28);
    for (got, want) in outline.iter().zip(truth) {
        assert_eq!(got.title, want.title);
        assert_eq!(got.level as usize, want.level, "{}", want.title);
        assert_eq!(got.page, Some(want.page_index as u32), "{}", want.title);
        assert_eq!(got.x, Some(want.dest[0]), "{}", want.title);
        assert_eq!(got.y, Some(want.dest[1]), "{}", want.title);
    }
    backend.close(doc).unwrap();

    // A document without an outline gives an empty list, not an error.
    let plain = open(&backend, &pl_testkit::plain());
    assert!(backend.outline(plain).unwrap().is_empty());
    backend.close(plain).unwrap();
}
