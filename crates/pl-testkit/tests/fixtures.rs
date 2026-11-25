//! Every fixture is re-read with lopdf (an independent parse of the written
//! bytes) and compared with its manifest.

use lopdf::content::Content;
use lopdf::xref::XrefType;
use lopdf::{Dictionary, Document, Object, ObjectId};
use pl_testkit::*;
use std::path::PathBuf;
use std::sync::OnceLock;

const EPS: f64 = 1e-3;

/// Default-size fixtures, generated once for the whole test binary.
fn fixtures() -> &'static [Fixture] {
    static ALL: OnceLock<Vec<Fixture>> = OnceLock::new();
    ALL.get_or_init(all_fixtures)
}

fn fixture(name: &str) -> &'static Fixture {
    fixtures()
        .iter()
        .find(|f| f.manifest.name == name)
        .unwrap_or_else(|| panic!("no fixture named {name}"))
}

fn load(fx: &Fixture) -> Document {
    Document::load_mem(&fx.bytes)
        .unwrap_or_else(|e| panic!("{} does not parse: {e}", fx.manifest.name))
}

fn page_ids(doc: &Document) -> Vec<ObjectId> {
    doc.get_pages().into_values().collect()
}

fn f(o: &Object) -> f64 {
    match o {
        Object::Integer(i) => *i as f64,
        Object::Real(r) => f64::from(*r),
        other => panic!("not a number: {other:?}"),
    }
}

fn floats(o: &Object) -> Vec<f64> {
    o.as_array().expect("array").iter().map(f).collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= EPS
}

fn all_close(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| close(*x, *y))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn string(d: &Dictionary, key: &[u8]) -> String {
    String::from_utf8(
        d.get(key)
            .and_then(Object::as_str)
            .expect("string entry")
            .to_vec(),
    )
    .unwrap()
}

fn name_of(d: &Dictionary, key: &[u8]) -> String {
    String::from_utf8(
        d.get(key)
            .and_then(Object::as_name)
            .expect("name entry")
            .to_vec(),
    )
    .unwrap()
}

#[derive(Debug)]
struct Drawn {
    segments: Vec<Segment>,
    /// (text, origin, font size)
    texts: Vec<(String, Point, f64)>,
    /// (marked-content property name, segments drawn inside)
    layer_segments: Vec<(String, usize)>,
    image_draws: usize,
}

/// Interpret the subset of operators the fixtures use, with lopdf's content parser.
fn interpret(doc: &Document, page: ObjectId) -> Drawn {
    let bytes = doc.get_page_content(page);
    let content = Content::decode(&bytes).expect("content stream parses");
    let mut out = Drawn {
        segments: Vec::new(),
        texts: Vec::new(),
        layer_segments: Vec::new(),
        image_draws: 0,
    };
    let (mut width, mut rgb) = (1.0, [0.0; 3]);
    let mut current: Option<Point> = None;
    let mut pending: Vec<(Point, Point)> = Vec::new();
    let (mut font_size, mut origin) = (0.0, [0.0, 0.0]);
    let mut layer: Option<usize> = None;
    for op in &content.operations {
        let n: Vec<f64> = op
            .operands
            .iter()
            .filter(|o| matches!(o, Object::Integer(_) | Object::Real(_)))
            .map(f)
            .collect();
        match op.operator.as_str() {
            "w" => width = n[0],
            "RG" => rgb = [n[0], n[1], n[2]],
            "m" => current = Some([n[0], n[1]]),
            "l" => {
                let a = current.expect("l without current point");
                pending.push((a, [n[0], n[1]]));
                current = Some([n[0], n[1]]);
            }
            "S" => {
                for (a, b) in pending.drain(..) {
                    out.segments.push(Segment { a, b, width, rgb });
                    if let Some(i) = layer {
                        out.layer_segments[i].1 += 1;
                    }
                }
                current = None;
            }
            "Tf" => font_size = n[0],
            "Td" => origin = [n[0], n[1]],
            "Tj" => {
                let s = String::from_utf8(op.operands[0].as_str().unwrap().to_vec()).unwrap();
                out.texts.push((s, origin, font_size));
            }
            "BDC" => {
                let prop = String::from_utf8(op.operands[1].as_name().unwrap().to_vec()).unwrap();
                out.layer_segments.push((prop, 0));
                layer = Some(out.layer_segments.len() - 1);
            }
            "EMC" => layer = None,
            "Do" => out.image_draws += 1,
            "BT" | "ET" | "g" | "q" | "Q" | "cm" => {}
            other => panic!("unexpected operator {other}"),
        }
    }
    assert!(pending.is_empty(), "unstroked path left over");
    out
}

fn segment_close(a: &Segment, b: &Segment) -> bool {
    all_close(&a.a, &b.a)
        && all_close(&a.b, &b.b)
        && close(a.width, b.width)
        && all_close(&a.rgb, &b.rgb)
}

fn inside(r: Rect, p: Point) -> bool {
    p[0] >= r[0] - EPS && p[0] <= r[2] + EPS && p[1] >= r[1] - EPS && p[1] <= r[3] + EPS
}

fn rect_inside(outer: Rect, inner: Rect) -> bool {
    inner[0] < inner[2]
        && inner[1] < inner[3]
        && inside(outer, [inner[0], inner[1]])
        && inside(outer, [inner[2], inner[3]])
}

// ---------------------------------------------------------------------------
// all fixtures
// ---------------------------------------------------------------------------

#[test]
fn expected_set_of_fixtures() {
    let names: Vec<&str> = fixtures()
        .iter()
        .map(|f| f.manifest.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "plain",
            "multi_page",
            "rotated_pages",
            "user_unit",
            "crop_box",
            "dense_sheet",
            "xref_classic",
            "xref_object_streams",
            "annotations",
            "layers",
            "acro_form",
            "raster_scan",
            "encrypted_rc4",
            "encrypted_aes256",
            "inherited_boxes",
            "user_unit_objstm",
            "linearized",
        ]
    );
}

#[test]
fn every_fixture_parses_with_expected_pages_and_boxes() {
    for fx in fixtures() {
        let m = &fx.manifest;
        let header = format!("%PDF-{}\n", m.pdf_version);
        assert!(
            fx.bytes.starts_with(header.as_bytes()),
            "{}: header",
            m.name
        );
        assert!(
            fx.bytes.ends_with(b"%%EOF") || fx.bytes.ends_with(b"%%EOF\n"),
            "{}: EOF marker",
            m.name
        );

        let doc = load(fx);
        assert_eq!(doc.version, m.pdf_version, "{}", m.name);
        let ids = page_ids(&doc);
        assert_eq!(ids.len(), m.page_count(), "{}: page count", m.name);
        assert!(m.page_count() >= 1);
        assert!(
            doc.xref_recovery.is_none(),
            "{}: xref needed repair",
            m.name
        );
        assert!(doc.trailer.has(b"ID"), "{}: trailer /ID", m.name);

        for (i, (id, truth)) in ids.iter().zip(&m.pages).enumerate() {
            let page = doc.get_dictionary(*id).unwrap();
            let ctx = format!("{} page {}", m.name, i + 1);
            // The manifest records effective boxes: the page's own, or the one it inherits.
            let inherited = |key: &[u8]| -> Option<&Object> {
                let mut node = page;
                for _ in 0..8 {
                    if let Ok(value) = node.get(key) {
                        return Some(value);
                    }
                    let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
                    node = doc.get_dictionary(parent).ok()?;
                }
                None
            };
            assert!(
                all_close(&floats(inherited(b"MediaBox").unwrap()), &truth.media_box),
                "{ctx}: MediaBox"
            );
            match truth.crop_box {
                Some(crop) => {
                    assert!(
                        all_close(&floats(inherited(b"CropBox").unwrap()), &crop),
                        "{ctx}: CropBox"
                    );
                    assert!(
                        rect_inside(truth.media_box, crop),
                        "{ctx}: CropBox inside MediaBox"
                    );
                }
                None => assert!(inherited(b"CropBox").is_none(), "{ctx}: unexpected CropBox"),
            }
            let rotate = page
                .get(b"Rotate")
                .map(|o| o.as_i64().unwrap())
                .unwrap_or(0);
            assert_eq!(rotate, i64::from(truth.rotate), "{ctx}: Rotate");
            let user_unit = page.get(b"UserUnit").map(f).unwrap_or(1.0);
            assert!(close(user_unit, truth.user_unit), "{ctx}: UserUnit");
            assert_eq!(
                doc.get_page_contents(*id).len(),
                truth.content_streams,
                "{ctx}: content streams"
            );
            let annots = doc.get_page_annotations(*id).unwrap();
            assert_eq!(
                annots.len(),
                truth.annotations.len(),
                "{ctx}: annotation count"
            );
        }
    }
}

#[test]
fn page_content_matches_manifest_segments_and_text() {
    for fx in fixtures() {
        let m = &fx.manifest;
        let doc = load(fx);
        for (i, (id, truth)) in page_ids(&doc).iter().zip(&m.pages).enumerate() {
            let ctx = format!("{} page {}", m.name, i + 1);
            let drawn = interpret(&doc, *id);
            assert_eq!(
                drawn.segments.len(),
                truth.segment_count,
                "{ctx}: segment count"
            );
            assert_eq!(drawn.image_draws, truth.images.len(), "{ctx}: image draws");
            if truth.grid.is_none() {
                assert_eq!(truth.segments.len(), truth.segment_count, "{ctx}");
                for (k, (got, want)) in drawn.segments.iter().zip(&truth.segments).enumerate() {
                    assert!(
                        segment_close(got, want),
                        "{ctx}: segment {k}: {got:?} vs {want:?}"
                    );
                }
            }
            assert_eq!(drawn.texts.len(), truth.texts.len(), "{ctx}: text count");
            for ((text, origin, size), want) in drawn.texts.iter().zip(&truth.texts) {
                assert_eq!(text, &want.text, "{ctx}");
                assert!(all_close(origin, &want.origin), "{ctx}: origin of {text:?}");
                assert!(close(*size, want.font_size), "{ctx}: size of {text:?}");
            }
            // The page font is the standard-14 Helvetica, not embedded.
            let fonts = doc.get_page_fonts(*id).unwrap();
            let font = fonts.get(b"F1".as_slice()).expect("font F1");
            assert_eq!(name_of(font, b"BaseFont"), "Helvetica", "{ctx}");
            assert_eq!(name_of(font, b"Subtype"), "Type1", "{ctx}");
            assert!(!font.has(b"FontDescriptor"), "{ctx}");
        }
    }
}

#[test]
fn ground_truth_is_self_consistent() {
    for fx in fixtures() {
        let m = &fx.manifest;
        for (i, p) in m.pages.iter().enumerate() {
            let ctx = format!("{} page {}", m.name, i + 1);
            let mb = p.media_box;
            assert!(mb[0] < mb[2] && mb[1] < mb[3], "{ctx}: media box");
            assert!([0, 90, 180, 270].contains(&p.rotate), "{ctx}: rotate");
            assert!(p.user_unit >= 1.0, "{ctx}");
            for s in &p.segments {
                assert!(
                    inside(mb, s.a) && inside(mb, s.b),
                    "{ctx}: segment outside MediaBox: {s:?}"
                );
                assert!(
                    s.length() > 0.0 && s.width > 0.0,
                    "{ctx}: degenerate segment"
                );
                assert!(s.rgb.iter().all(|c| (0.0..=1.0).contains(c)), "{ctx}");
            }
            for t in &p.texts {
                assert!(!t.text.is_empty(), "{ctx}");
                assert_eq!(
                    t.width,
                    helvetica_width(&t.text, t.font_size),
                    "{ctx}: width of {:?}",
                    t.text
                );
                assert!(
                    rect_inside(p.visible_box(), t.bbox()),
                    "{ctx}: text {:?} outside visible box",
                    t.text
                );
            }
            // No two texts on a page are identical, so a search hit is unambiguous.
            for (a, ta) in p.texts.iter().enumerate() {
                for tb in &p.texts[a + 1..] {
                    assert_ne!(ta.text, tb.text, "{ctx}: duplicate text");
                }
            }
            for a in &p.annotations {
                assert!(
                    rect_inside(p.visible_box(), a.rect),
                    "{ctx}: annotation {} outside page",
                    a.subtype
                );
                assert!(
                    a.ap_bbox[0] < a.ap_bbox[2] && a.ap_bbox[1] < a.ap_bbox[3],
                    "{ctx}"
                );
                // The appearance box has the same size as /Rect, so it maps without scaling.
                assert!(
                    close(a.ap_bbox[2] - a.ap_bbox[0], a.rect[2] - a.rect[0]),
                    "{ctx}: {} AP width",
                    a.subtype
                );
                assert!(
                    close(a.ap_bbox[3] - a.ap_bbox[1], a.rect[3] - a.rect[1]),
                    "{ctx}: {} AP height",
                    a.subtype
                );
            }
            let d = p.display_size();
            assert!(d[0] > 0.0 && d[1] > 0.0, "{ctx}");
        }
        assert_eq!(
            m.total_segments(),
            m.pages
                .iter()
                .map(|p| p.grid.as_ref().map_or(p.segments.len(), |g| g.count))
                .sum::<usize>(),
            "{}",
            m.name
        );
        assert_eq!(m.file_name(), format!("{}.pdf", m.name));
    }
}

#[test]
fn deterministic_fixtures_reproduce_byte_for_byte() {
    let again = all_fixtures_with(&Options::small());
    let again2 = all_fixtures_with(&Options::small());
    assert_eq!(again.len(), fixtures().len());
    for ((a, b), full) in again.iter().zip(&again2).zip(fixtures()) {
        assert_eq!(a.manifest.name, full.manifest.name);
        if a.manifest.deterministic {
            assert!(
                a.bytes == b.bytes,
                "{} is not reproducible",
                a.manifest.name
            );
            assert_eq!(a.manifest, b.manifest);
            // Fixtures not affected by the size options also match the default-size run.
            if !["dense_sheet", "raster_scan"].contains(&a.manifest.name.as_str()) {
                assert!(
                    a.bytes == full.bytes,
                    "{} differs between runs",
                    a.manifest.name
                );
            }
        } else {
            assert!(
                a.bytes != b.bytes,
                "{} is flagged non-deterministic but reproduced",
                a.manifest.name
            );
        }
    }
    // The individual constructors agree with the collection.
    assert!(plain().bytes == fixture("plain").bytes);
    assert!(annotations().bytes == fixture("annotations").bytes);
}

// ---------------------------------------------------------------------------
// individual fixtures
// ---------------------------------------------------------------------------

#[test]
fn plain_page_known_geometry() {
    let p = &fixture("plain").manifest.pages[0];
    assert_eq!(p.media_box, LETTER);
    assert_eq!(p.segments.len(), 12);
    assert_eq!(p.texts.len(), 4);
    // The two diagonals cross at the page centre, where the blue cross also meets.
    assert_eq!(p.segments[4].midpoint(), [306.0, 396.0]);
    assert_eq!(p.segments[5].midpoint(), [306.0, 396.0]);
    assert_eq!(p.segments[6].midpoint(), [306.0, 396.0]);
    assert_eq!(p.segments[7].midpoint(), [306.0, 396.0]);
    assert_eq!(p.segments[6].length(), 216.0); // 3 in
    assert_eq!(p.segments[7].length(), 288.0); // 4 in
                                               // 3-4-5 triangle.
    assert_eq!(p.segments[8].length(), 144.0);
    assert_eq!(p.segments[9].length(), 108.0);
    assert_eq!(p.segments[10].length(), 180.0);
    assert_eq!(p.segments[11].a, [400.25, 150.5]);
    assert_eq!(p.display_size(), [612.0, 792.0]);
    assert!(p
        .texts
        .iter()
        .any(|t| t.text == "The quick brown fox jumps over the lazy dog"));
}

#[test]
fn multi_page_has_distinct_text_and_an_outline() {
    let fx = fixture("multi_page");
    let m = &fx.manifest;
    assert!(m.page_count() >= 20);
    assert_eq!(m.page_count(), MULTI_PAGE_COUNT);
    let mut tokens: Vec<String> = m.pages.iter().map(|p| p.texts[1].text.clone()).collect();
    for (i, t) in tokens.iter().enumerate() {
        assert_eq!(t, &multi_page_token(i));
    }
    tokens.sort();
    tokens.dedup();
    assert_eq!(tokens.len(), MULTI_PAGE_COUNT);

    // Walk the outline tree in the file in pre-order and compare.
    let doc = load(fx);
    let ids = page_ids(&doc);
    let catalog = doc.catalog().unwrap();
    let root = doc
        .get_dictionary(catalog.get(b"Outlines").unwrap().as_reference().unwrap())
        .unwrap();
    assert_eq!(
        root.get(b"Count").unwrap().as_i64().unwrap(),
        m.outline.len() as i64
    );
    fn walk(
        doc: &Document,
        ids: &[ObjectId],
        first: &Object,
        level: usize,
        out: &mut Vec<OutlineTruth>,
    ) {
        let mut next = Some(first.as_reference().unwrap());
        let mut prev = None;
        while let Some(id) = next {
            let d = doc.get_dictionary(id).unwrap();
            assert_eq!(
                d.get(b"Prev").ok().map(|o| o.as_reference().unwrap()),
                prev,
                "/Prev chain"
            );
            let dest = d.get(b"Dest").unwrap().as_array().unwrap();
            let page = dest[0].as_reference().unwrap();
            assert_eq!(dest[1].as_name().unwrap(), b"XYZ");
            out.push(OutlineTruth {
                title: string(d, b"Title"),
                level,
                page_index: ids
                    .iter()
                    .position(|p| *p == page)
                    .expect("dest page exists"),
                dest: [f(&dest[2]), f(&dest[3])],
            });
            if let Ok(child) = d.get(b"First") {
                walk(doc, ids, child, level + 1, out);
            }
            prev = Some(id);
            next = d.get(b"Next").ok().map(|o| o.as_reference().unwrap());
        }
    }
    let mut found = Vec::new();
    walk(&doc, &ids, root.get(b"First").unwrap(), 0, &mut found);
    assert_eq!(found, m.outline);
    assert_eq!(m.outline.len(), 28);
    assert_eq!(
        m.outline.iter().filter(|o| o.level == 0).count(),
        MULTI_PAGE_COUNT / MULTI_PAGE_SECTION
    );
    assert_eq!(m.outline[0].title, "Section 1");
    assert_eq!(m.outline[1].title, "Sheet 1");
    assert_eq!(m.outline.last().unwrap().page_index, MULTI_PAGE_COUNT - 1);
}

#[test]
fn rotated_user_unit_and_crop_box_pages() {
    let rot = &fixture("rotated_pages").manifest;
    assert_eq!(
        rot.pages.iter().map(|p| p.rotate).collect::<Vec<_>>(),
        [0, 90, 180, 270]
    );
    assert_eq!(rot.pages[0].display_size(), [612.0, 792.0]);
    assert_eq!(rot.pages[1].display_size(), [792.0, 612.0]);
    assert_eq!(rot.pages[2].display_size(), [612.0, 792.0]);
    assert_eq!(rot.pages[3].display_size(), [792.0, 612.0]);
    // Same geometry in unrotated space on every page.
    for p in &rot.pages[1..] {
        assert_eq!(p.segments, rot.pages[0].segments);
        assert_eq!(p.texts[0].origin, rot.pages[0].texts[0].origin);
    }
    assert_eq!(rot.pages[1].texts[0].text, "ROTATE 90");

    let uu = &fixture("user_unit").manifest;
    assert_eq!(uu.pages[0].user_unit, 2.0);
    assert_eq!(uu.pages[0].media_box, [0.0, 0.0, 306.0, 396.0]);
    assert_eq!(uu.pages[0].display_size(), [612.0, 792.0]);
    assert!(uu.pdf_version.as_str() >= "1.6", "/UserUnit needs PDF 1.6");

    let crop = &fixture("crop_box").manifest;
    assert_eq!(crop.page_count(), 3);
    let p0 = &crop.pages[0];
    assert_eq!(p0.visible_box(), [36.0, 72.0, 540.0, 684.0]);
    assert_eq!(p0.display_size(), [504.0, 612.0]);
    // Corner ticks start exactly on the visible-box corners.
    assert_eq!(p0.segments[3].a, [36.0, 72.0]);
    assert_eq!(p0.segments[4].a, [540.0, 684.0]);
    // The last segment lies outside the CropBox.
    let hidden = p0.segments.last().unwrap();
    assert!(!inside(p0.visible_box(), hidden.a) && !inside(p0.visible_box(), hidden.b));
    let p1 = &crop.pages[1];
    assert_eq!(p1.crop_box, None);
    assert_eq!(p1.media_box[0], 100.0);
    assert_eq!(p1.display_size(), [612.0, 792.0]);
    let p2 = &crop.pages[2];
    assert_eq!(p2.rotate, 90);
    assert_eq!(p2.display_size(), [640.0, 500.0]);
    for p in &crop.pages[1..] {
        for s in &p.segments {
            assert!(inside(p.visible_box(), s.a) && inside(p.visible_box(), s.b));
        }
    }
}

#[test]
fn dense_sheet_has_exactly_the_default_segment_count() {
    let fx = fixture("dense_sheet");
    let p = &fx.manifest.pages[0];
    let g = p.grid.as_ref().unwrap();
    assert_eq!(p.media_box, ARCH_D);
    assert_eq!(p.display_size(), [36.0 * 72.0, 24.0 * 72.0]);
    assert_eq!(g.count, DEFAULT_DENSE_SEGMENTS);
    assert_eq!(g.count, 200_000);
    assert_eq!(p.segment_count, g.count);
    assert_eq!(g.streams, DEFAULT_DENSE_STREAMS);
    assert!(g.streams > 1, "several content streams");
    assert!(g.cols * g.rows >= g.count);
    assert!(p.segments.is_empty());

    // Independent parse of the decompressed streams: every stream is Flate
    // encoded, self-contained, and holds its share of the segments.
    let doc = load(fx);
    let page = page_ids(&doc)[0];
    let stream_ids = doc.get_page_contents(page);
    assert_eq!(stream_ids.len(), g.streams);
    let mut index = 0usize;
    for (k, id) in stream_ids.iter().enumerate() {
        let stream = doc.get_object(*id).unwrap().as_stream().unwrap();
        assert_eq!(name_of(&stream.dict, b"Filter"), "FlateDecode");
        let body = String::from_utf8(stream.decompressed_content().unwrap()).unwrap();
        let mut lines = body.lines();
        assert_eq!(
            lines.next(),
            Some("0.25 w 0 0 0 RG"),
            "stream {k} sets its own state"
        );
        for line in lines.filter(|l| l.ends_with(" l S")) {
            let tok: Vec<&str> = line.split(' ').collect();
            assert_eq!(tok.len(), 7, "{line}");
            assert_eq!((tok[2], tok[5], tok[6]), ("m", "l", "S"));
            // Text parses to exactly the manifest's f64 values.
            let want = g.segment(index);
            let got: Vec<f64> = [tok[0], tok[1], tok[3], tok[4]]
                .iter()
                .map(|t| t.parse().unwrap())
                .collect();
            assert_eq!(
                got,
                [want.a[0], want.a[1], want.b[0], want.b[1]],
                "segment {index}"
            );
            assert_eq!(g.stream_of(index), k, "segment {index} stream");
            index += 1;
        }
    }
    assert_eq!(index, g.count);

    // Geometry: inside the 0.5 in margin, non-degenerate, non-touching.
    let inner = [36.0, 36.0, ARCH_D[2] - 36.0, ARCH_D[3] - 36.0];
    let mut total = 0.0;
    for (i, s) in g.segments().enumerate() {
        assert!(
            inside(inner, s.a) && inside(inner, s.b),
            "segment {i} outside margin"
        );
        assert!(s.length() > 0.5, "segment {i} too short");
        total += s.length();
    }
    assert!(close(total, g.total_length()));
    assert!(
        g.inset_cpt[0] + g.extent_cpt[0] < g.pitch_cpt[0],
        "cells do not touch in x"
    );
    assert!(
        g.inset_cpt[1] + g.extent_cpt[1] < g.pitch_cpt[1],
        "cells do not touch in y"
    );
    assert_eq!(
        g.segment_cpt(0)[1],
        g.segment_cpt(0)[3],
        "segment 0 is horizontal"
    );
    assert_eq!(
        g.segment_cpt(1)[0],
        g.segment_cpt(1)[2],
        "segment 1 is vertical"
    );
    let d = g.segment_cpt(2);
    assert!(d[0] != d[2] && d[1] != d[3], "segment 2 is diagonal");
    // The file is large enough to be a meaningful benchmark but compressed.
    assert!(
        fx.bytes.len() > 500_000 && fx.bytes.len() < 8_000_000,
        "size {}",
        fx.bytes.len()
    );
}

#[test]
fn dense_sheet_is_parameterised() {
    for (count, streams) in [(0usize, 1usize), (1, 4), (7, 3), (999, 5), (10_000, 1)] {
        let fx = dense_sheet_with(count, streams);
        let p = &fx.manifest.pages[0];
        let g = p.grid.as_ref().unwrap();
        assert_eq!(
            (g.count, g.streams, p.content_streams),
            (count, streams, streams)
        );
        let doc = load(&fx);
        let page = page_ids(&doc)[0];
        assert_eq!(doc.get_page_contents(page).len(), streams);
        let drawn = interpret(&doc, page);
        assert_eq!(drawn.segments.len(), count);
        for (i, got) in drawn.segments.iter().enumerate() {
            assert!(
                segment_close(got, &g.segment(i)),
                "count {count} segment {i}"
            );
        }
        assert_eq!(drawn.texts.len(), 1);
    }
    assert_eq!(
        dense_sheet_with(5, 0).manifest.pages[0].content_streams,
        1,
        "0 streams is clamped to 1"
    );
}

#[test]
fn xref_variants() {
    let classic = fixture("xref_classic");
    assert_eq!(classic.manifest.xref, XrefKind::Table);
    assert!(!classic.manifest.object_streams);
    assert!(contains(&classic.bytes, b"\nxref\n"));
    assert!(contains(&classic.bytes, b"trailer"));
    assert!(!contains(&classic.bytes, b"/ObjStm"));
    let doc = load(classic);
    assert!(matches!(
        doc.reference_table.cross_reference_type,
        XrefType::CrossReferenceTable
    ));
    assert_eq!(page_ids(&doc).len(), 3);

    let modern = fixture("xref_object_streams");
    assert_eq!(modern.manifest.xref, XrefKind::Stream);
    assert!(modern.manifest.object_streams);
    assert!(modern.manifest.pdf_version.as_str() >= "1.5");
    assert!(!contains(&modern.bytes, b"\nxref\n"), "no classic table");
    assert!(!contains(&modern.bytes, b"trailer"));
    assert!(contains(&modern.bytes, b"/ObjStm"));
    assert!(contains(&modern.bytes, b"/XRef"));
    // Page dictionaries live inside Flate-compressed object streams, so they
    // are not visible in the raw bytes (they are in the classic file).
    assert!(!contains(&modern.bytes, b"/Type/Page"));
    assert!(contains(&classic.bytes, b"/Type/Page"));
    assert!(contains(&modern.bytes, b"/Type/ObjStm") && contains(&modern.bytes, b"/FlateDecode"));
    let doc = load(modern);
    assert!(matches!(
        doc.reference_table.cross_reference_type,
        XrefType::CrossReferenceStream
    ));
    assert_eq!(page_ids(&doc).len(), 3);
    let compressed = doc
        .reference_table
        .entries
        .values()
        .filter(|e| matches!(e, lopdf::xref::XrefEntry::Compressed { .. }))
        .count();
    assert!(
        compressed >= 5,
        "only {compressed} objects are in object streams"
    );

    // Both files draw the same thing apart from the tag text.
    let (a, b) = (&classic.manifest.pages, &modern.manifest.pages);
    for (pa, pb) in a.iter().zip(b) {
        assert_eq!(pa.segments, pb.segments);
    }
    assert_eq!(a[0].texts.last().unwrap().text, "XREF-TABLE page 1");
    assert_eq!(b[2].texts.last().unwrap().text, "XREF-STREAM page 3");
}

#[test]
fn annotations_have_appearance_streams() {
    let fx = fixture("annotations");
    let truth = &fx.manifest.pages[0].annotations;
    assert_eq!(
        truth.iter().map(|a| a.subtype).collect::<Vec<_>>(),
        ["Square", "Line", "FreeText", "Highlight"]
    );
    let doc = load(fx);
    let page = page_ids(&doc)[0];
    let annots = doc.get_page_annotations(page).unwrap();
    for (d, t) in annots.iter().zip(truth) {
        assert_eq!(name_of(d, b"Type"), "Annot");
        assert_eq!(name_of(d, b"Subtype"), t.subtype);
        assert!(
            all_close(&floats(d.get(b"Rect").unwrap()), &t.rect),
            "{}",
            t.subtype
        );
        assert_eq!(string(d, b"NM"), t.name);
        assert_eq!(string(d, b"T"), FOREIGN_AUTHOR);
        assert_eq!(string(d, b"Contents"), t.contents);
        assert_eq!(string(d, b"M"), FOREIGN_DATE);
        assert_eq!(d.get(b"F").unwrap().as_i64().unwrap(), 4, "print flag");
        assert!(all_close(&floats(d.get(b"C").unwrap()), &t.rgb));
        assert_eq!(d.get(b"P").unwrap().as_reference().unwrap(), page);
        let ap = d.get(b"AP").unwrap().as_dict().unwrap();
        let n = doc
            .get_object(ap.get(b"N").unwrap().as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap();
        assert_eq!(name_of(&n.dict, b"Subtype"), "Form");
        assert!(
            all_close(&floats(n.dict.get(b"BBox").unwrap()), &t.ap_bbox),
            "{}",
            t.subtype
        );
        assert!(!n.content.is_empty());
        Content::decode(&n.content).expect("appearance stream parses");
        match t.line {
            Some(l) => assert!(all_close(
                &floats(d.get(b"L").unwrap()),
                &[l[0][0], l[0][1], l[1][0], l[1][1]]
            )),
            None => assert!(!d.has(b"L")),
        }
        match t.quad_points {
            Some(q) => assert!(all_close(&floats(d.get(b"QuadPoints").unwrap()), &q)),
            None => assert!(!d.has(b"QuadPoints")),
        }
        if t.border_width > 0.0 {
            assert!(close(
                f(d.get(b"BS").unwrap().as_dict().unwrap().get(b"W").unwrap()),
                t.border_width
            ));
        }
    }
    // Line endpoints lie inside the line's rect; names are unique.
    let line = &truth[1];
    let l = line.line.unwrap();
    assert!(inside(line.rect, l[0]) && inside(line.rect, l[1]));
    let mut names: Vec<&str> = truth.iter().map(|a| a.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 4);
    // The highlight covers the advance box of the text it marks.
    let hl = &truth[3];
    let text = fx.manifest.pages[0]
        .texts
        .iter()
        .find(|t| t.text == "Highlighted words here")
        .unwrap();
    let q = hl.quad_points.unwrap();
    assert_eq!(
        [q[4], q[5], q[2], q[3]],
        hl.rect,
        "quad corners equal the rect"
    );
    assert!(hl.rect[0] <= text.origin[0] && hl.rect[2] >= text.origin[0] + text.width);
    assert!(hl.rect[1] < text.origin[1] && hl.rect[3] > text.origin[1] + 0.7 * text.font_size);
    assert!(contains(&fx.bytes, b"/Multiply"));
}

#[test]
fn layers_fixture_has_two_ocgs_with_distinct_marks() {
    let fx = fixture("layers");
    let m = &fx.manifest;
    assert_eq!(m.layers.len(), 2);
    let doc = load(fx);
    let catalog = doc.catalog().unwrap();
    let ocp = catalog.get(b"OCProperties").unwrap().as_dict().unwrap();
    let ocgs = ocp.get(b"OCGs").unwrap().as_array().unwrap();
    assert_eq!(ocgs.len(), 2);
    for (o, layer) in ocgs.iter().zip(&m.layers) {
        let d = doc.get_dictionary(o.as_reference().unwrap()).unwrap();
        assert_eq!(name_of(d, b"Type"), "OCG");
        assert_eq!(string(d, b"Name"), layer.name);
        assert!(layer.default_visible);
    }
    let default = ocp.get(b"D").unwrap().as_dict().unwrap();
    assert_eq!(default.get(b"ON").unwrap().as_array().unwrap(), ocgs);
    assert_eq!(default.get(b"Order").unwrap().as_array().unwrap(), ocgs);
    assert!(!default.has(b"OFF"));

    // The page's /Properties map the marked-content names to the two groups, in order.
    let page = page_ids(&doc)[0];
    let (resources, _) = doc.get_page_resources(page).unwrap();
    let props = resources
        .unwrap()
        .get(b"Properties")
        .unwrap()
        .as_dict()
        .unwrap();
    assert_eq!(props.get(b"LA").unwrap(), &ocgs[0]);
    assert_eq!(props.get(b"LB").unwrap(), &ocgs[1]);

    let drawn = interpret(&doc, page);
    assert_eq!(
        drawn.layer_segments,
        [
            ("LA".to_string(), m.layers[0].segments.len()),
            ("LB".to_string(), m.layers[1].segments.len())
        ]
    );
    let p = &m.pages[0];
    let layered: usize = m.layers.iter().map(|l| l.segments.len()).sum();
    assert_eq!(
        p.segments.len(),
        layered + 1,
        "one base segment plus the layer segments"
    );
    assert_eq!(p.texts.len(), 3);
    // Layer A is horizontal red, layer B vertical blue; nothing is shared.
    assert!(m.layers[0]
        .segments
        .iter()
        .all(|s| s.a[1] == s.b[1] && s.rgb == RED));
    assert!(m.layers[1]
        .segments
        .iter()
        .all(|s| s.a[0] == s.b[0] && s.rgb == BLUE));
    for a in &m.layers[0].segments {
        assert!(!m.layers[1].segments.contains(a));
        assert!(p.segments.contains(a));
    }
    assert_eq!(m.layers[0].texts[0].text, "LAYER-A");
    assert_eq!(m.layers[1].texts[0].text, "LAYER-B");
    assert!(
        m.pdf_version.as_str() >= "1.5",
        "optional content needs PDF 1.5"
    );
}

#[test]
fn acro_form_fields_are_filled_with_appearances() {
    let fx = fixture("acro_form");
    let m = &fx.manifest;
    assert_eq!(m.form_fields.len(), 2);
    let doc = load(fx);
    let page = page_ids(&doc)[0];
    let form = doc
        .catalog()
        .unwrap()
        .get(b"AcroForm")
        .unwrap()
        .as_dict()
        .unwrap();
    assert!(!form.get(b"NeedAppearances").unwrap().as_bool().unwrap());
    assert!(form
        .get(b"DR")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Font")
        .unwrap()
        .as_dict()
        .unwrap()
        .has(b"Helv"));
    let fields = form.get(b"Fields").unwrap().as_array().unwrap();
    assert_eq!(fields.len(), m.form_fields.len());
    let annots = doc
        .get_dictionary(page)
        .unwrap()
        .get(b"Annots")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(annots, fields, "merged field/widget objects");
    for ((o, t), a) in fields
        .iter()
        .zip(&m.form_fields)
        .zip(&m.pages[0].annotations)
    {
        let d = doc.get_dictionary(o.as_reference().unwrap()).unwrap();
        assert_eq!(name_of(d, b"Subtype"), "Widget");
        assert_eq!(name_of(d, b"FT"), "Tx");
        assert_eq!(string(d, b"T"), t.name);
        assert_eq!(string(d, b"V"), t.value);
        assert!(all_close(&floats(d.get(b"Rect").unwrap()), &t.rect));
        assert_eq!(
            (a.subtype, a.rect, &a.title, &a.contents),
            ("Widget", t.rect, &t.name, &t.value)
        );
        let ap = d.get(b"AP").unwrap().as_dict().unwrap();
        let n = doc
            .get_object(ap.get(b"N").unwrap().as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap();
        let body = String::from_utf8(n.content.clone()).unwrap();
        assert!(
            body.contains(&format!("({}) Tj", t.value)),
            "appearance shows the value"
        );
        assert!(body.starts_with("/Tx BMC"));
        // The value text origin is the widget origin plus the appearance's Td.
        assert!(body.contains("2 6.5 Td"));
        assert_eq!(t.text_origin, [t.rect[0] + 2.0, t.rect[1] + 6.5]);
        assert!(
            t.rect[2] - t.rect[0] > helvetica_width(&t.value, t.font_size) + 4.0,
            "value fits"
        );
        assert_eq!(t.page_index, 0);
    }
    assert_eq!(m.form_fields[0].value, "Plumbline Tower");
}

#[test]
fn raster_scan_image_matches_truth() {
    let fx = fixture("raster_scan");
    let p = &fx.manifest.pages[0];
    let img = &p.images[0];
    assert_eq!([img.width_px, img.height_px], DEFAULT_RASTER_PX);
    assert_eq!(img.placement, LETTER);
    assert!(p.segments.is_empty() && p.texts.is_empty());

    let doc = load(fx);
    let page = page_ids(&doc)[0];
    let (resources, _) = doc.get_page_resources(page).unwrap();
    let xobjects = resources
        .unwrap()
        .get(b"XObject")
        .unwrap()
        .as_dict()
        .unwrap();
    let stream = doc
        .get_object(xobjects.get(b"Im0").unwrap().as_reference().unwrap())
        .unwrap()
        .as_stream()
        .unwrap();
    assert_eq!(name_of(&stream.dict, b"Subtype"), "Image");
    assert_eq!(name_of(&stream.dict, b"ColorSpace"), img.color_space);
    assert_eq!(name_of(&stream.dict, b"Filter"), "FlateDecode");
    assert_eq!(
        stream.dict.get(b"Width").unwrap().as_i64().unwrap(),
        i64::from(img.width_px)
    );
    assert_eq!(
        stream.dict.get(b"Height").unwrap().as_i64().unwrap(),
        i64::from(img.height_px)
    );
    assert_eq!(
        stream
            .dict
            .get(b"BitsPerComponent")
            .unwrap()
            .as_i64()
            .unwrap(),
        i64::from(img.bits_per_component)
    );
    let pixels = stream.decompressed_content().unwrap();
    assert_eq!(pixels.len(), img.width_px as usize * img.height_px as usize);
    let at = |x: u32, y: u32| pixels[(y * img.width_px + x) as usize];

    assert_eq!(img.dark_rects.len(), 3);
    for (px, pt) in img.dark_rects_px.iter().zip(&img.dark_rects) {
        // Inside is dark, just outside is light.
        assert_eq!(at(px[0], px[1]), img.dark_level);
        assert_eq!(at(px[2] - 1, px[3] - 1), img.dark_level);
        assert_eq!(at((px[0] + px[2]) / 2, (px[1] + px[3]) / 2), img.dark_level);
        assert!(img.background_levels.contains(&at(px[0] - 1, px[1])));
        assert!(img.background_levels.contains(&at(px[2], px[3] - 1)));
        // Page-space rect is the pixel rect scaled by the placement, y flipped.
        assert!(rect_inside(img.placement, *pt));
        assert!(close(
            pt[0],
            f64::from(px[0]) * 612.0 / f64::from(img.width_px)
        ));
        assert!(close(
            pt[3],
            792.0 - f64::from(px[1]) * 792.0 / f64::from(img.height_px)
        ));
        assert!(close(
            (pt[2] - pt[0]) / (pt[3] - pt[1]),
            f64::from(px[2] - px[0]) / f64::from(px[3] - px[1])
        ));
    }
    // Every pixel equals the manifest's formula (sampled on a coarse lattice plus a full row).
    for y in (0..img.height_px).step_by(37) {
        for x in (0..img.width_px).step_by(41) {
            assert_eq!(at(x, y), img.pixel(x, y), "pixel {x},{y}");
        }
    }
    let dark = pixels.iter().filter(|v| **v == img.dark_level).count();
    let expected: usize = img
        .dark_rects_px
        .iter()
        .map(|r| ((r[2] - r[0]) * (r[3] - r[1])) as usize)
        .sum();
    assert_eq!(dark, expected, "dark pixel count");
    assert!(pixels
        .iter()
        .all(|v| *v == img.dark_level || img.background_levels.contains(v)));

    let small = raster_scan_with(40, 60);
    assert_eq!(page_ids(&load(&small)).len(), 1);
    assert_eq!(small.manifest.pages[0].images[0].width_px, 40);
}

#[test]
fn encrypted_fixtures_open_with_the_empty_user_password() {
    for (name, v, r, key_bits) in [
        ("encrypted_rc4", 2, 3, 128),
        ("encrypted_aes256", 5, 6, 256),
    ] {
        let fx = fixture(name);
        let truth = fx.manifest.encryption.as_ref().expect("encryption truth");
        assert_eq!(
            (truth.version, truth.revision, truth.key_bits),
            (v, r, key_bits),
            "{name}"
        );
        assert_eq!(truth.user_password, "");
        assert_eq!(truth.owner_password, OWNER_PASSWORD);

        // The plaintext is not in the file, and /Encrypt is.
        assert!(contains(&fx.bytes, b"/Encrypt"), "{name}");
        for i in 0..fx.manifest.page_count() {
            assert!(
                !contains(&fx.bytes, encrypted_token(i).as_bytes()),
                "{name}: plaintext token in file"
            );
        }
        assert!(!contains(&fx.bytes, b"Issued for construction"), "{name}");

        // lopdf opens it with no password given (empty user password) and decrypts.
        let doc = load(fx);
        assert!(doc.was_encrypted(), "{name}");
        let state = doc.encryption_state.as_ref().unwrap();
        assert_eq!((state.version(), state.revision()), (v, r), "{name}");
        let perms = state.permissions();
        assert_eq!(
            perms.contains(lopdf::Permissions::PRINTABLE),
            truth.can_print,
            "{name}"
        );
        assert_eq!(
            perms.contains(lopdf::Permissions::MODIFIABLE),
            truth.can_modify,
            "{name}"
        );
        assert_eq!(
            perms.contains(lopdf::Permissions::COPYABLE),
            truth.can_copy,
            "{name}"
        );
        assert_eq!(
            perms.contains(lopdf::Permissions::ANNOTABLE),
            truth.can_annotate,
            "{name}"
        );
        assert_eq!(
            perms.contains(lopdf::Permissions::ASSEMBLABLE),
            truth.can_assemble,
            "{name}"
        );
        let ids = page_ids(&doc);
        assert_eq!(ids.len(), 2, "{name}");
        for (i, id) in ids.iter().enumerate() {
            let content = doc.get_page_content(*id);
            assert!(
                contains(&content, encrypted_token(i).as_bytes()),
                "{name}: page {i} decrypts"
            );
        }

        // The owner password also opens it.
        let owner = Document::load_mem_with_options(
            &fx.bytes,
            lopdf::LoadOptions::with_password(OWNER_PASSWORD),
        )
        .unwrap_or_else(|e| panic!("{name}: owner password rejected: {e}"));
        assert!(
            contains(
                &owner.get_page_content(page_ids(&owner)[0]),
                encrypted_token(0).as_bytes()
            ),
            "{name}"
        );
    }
    assert!(fixture("encrypted_rc4").manifest.deterministic);
    assert!(!fixture("encrypted_aes256").manifest.deterministic);
}

// ---------------------------------------------------------------------------
// writing to disk
// ---------------------------------------------------------------------------

#[test]
fn write_all_writes_every_fixture_and_manifest() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pl-testkit-write-all");
    let _ = std::fs::remove_dir_all(&dir);
    let options = Options::small();
    let paths = write_all(&dir, &options).unwrap();
    let expected = all_fixtures_with(&options);
    assert_eq!(paths.len(), expected.len());
    for (path, fx) in paths.iter().zip(&expected) {
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            fx.manifest.file_name()
        );
        let on_disk = std::fs::read(path).unwrap();
        if fx.manifest.deterministic {
            assert!(on_disk == fx.bytes, "{}", fx.manifest.name);
        }
        let doc = Document::load(path).unwrap();
        assert_eq!(doc.get_pages().len(), fx.manifest.page_count());
        let manifest_text =
            std::fs::read_to_string(dir.join(format!("{}.manifest.txt", fx.manifest.name)))
                .unwrap();
        assert!(manifest_text.starts_with("Manifest {"));
        assert!(manifest_text.contains(&format!("name: \"{}\"", fx.manifest.name)));
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2 * expected.len());
    assert!(
        default_output_dir().ends_with("target/fixtures")
            || default_output_dir().ends_with("target\\fixtures")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fixture_generator_binary_writes_the_set() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pl-testkit-bin");
    let _ = std::fs::remove_dir_all(&dir);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pl-fixtures"))
        .arg(&dir)
        .args(["--small", "--dense", "321", "--raster", "100x120"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.lines().count(),
        all_fixtures_with(&Options::small()).len()
    );
    let dense = Document::load(dir.join("dense_sheet.pdf")).unwrap();
    assert_eq!(interpret(&dense, page_ids(&dense)[0]).segments.len(), 321);
    assert!(dir.join("plain.pdf").is_file() && dir.join("plain.manifest.txt").is_file());

    let bad = std::process::Command::new(env!("CARGO_BIN_EXE_pl-fixtures"))
        .arg("--nope")
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn inherited_boxes_are_not_in_the_page_dictionaries() {
    let fx = fixture("inherited_boxes");
    let doc = load(fx);
    let ids = page_ids(&doc);
    assert_eq!(ids.len(), 8);
    for (i, id) in ids.iter().enumerate() {
        let page = doc.get_dictionary(*id).unwrap();
        let parent = page.get(b"Parent").and_then(Object::as_reference).unwrap();
        let node = doc.get_dictionary(parent).unwrap();
        assert_eq!(name_of(node, b"Type"), "Pages");
        assert!(
            node.has(b"Parent"),
            "page {i} hangs off an intermediate node"
        );
        if i < 4 {
            assert!(!page.has(b"MediaBox") && !page.has(b"CropBox"), "page {i}");
            assert!(all_close(
                &floats(node.get(b"MediaBox").unwrap()),
                &INHERITED_MEDIA_BOX
            ));
        } else {
            assert!(page.has(b"MediaBox") && !page.has(b"CropBox"), "page {i}");
            assert!(all_close(
                &floats(node.get(b"CropBox").unwrap()),
                &INHERITED_CROP_BOX
            ));
        }
    }
    assert_eq!(
        fx.manifest
            .pages
            .iter()
            .map(|p| p.rotate)
            .collect::<Vec<_>>(),
        [0, 90, 180, 270, 0, 90, 180, 270]
    );
}

#[test]
fn the_object_stream_user_unit_is_invisible_in_the_raw_bytes() {
    let fx = fixture("user_unit_objstm");
    assert!(fx.manifest.object_streams);
    assert!(!contains(&fx.bytes, b"/UserUnit"));
    assert_eq!(fx.manifest.pages[0].user_unit, 2.0);
}

#[test]
fn the_linearized_fixture_has_the_layout_its_parameters_describe() {
    let fx = fixture("linearized");
    assert!(fx.manifest.linearized);
    assert!(fixtures().iter().filter(|f| f.manifest.linearized).count() == 1);
    let doc = load(fx);
    let lin = doc
        .linearization
        .as_ref()
        .expect("lopdf sees the parameter dictionary");
    assert_eq!(lin.file_length, fx.bytes.len() as u64, "/L");
    assert_eq!(lin.page_count, 2, "/N");
    assert_eq!(doc.xref_section_count, 2, "first-page and main sections");
    assert!(doc.xref_recovery.is_none());

    // The parameter dictionary is the first object and sits in the first 1024 bytes.
    let at = |needle: &[u8]| {
        fx.bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap()
    };
    assert!(at(b"/Linearized") < 200);
    // /O names the first page object, which lies before /E; page 2 lies after it.
    let ids = page_ids(&doc);
    assert_eq!(ids[0].0, lin.first_page_object);
    let first_page = at(format!("\n{} 0 obj\n", ids[0].0).as_bytes()) as u64;
    let second_page = at(format!("\n{} 0 obj\n", ids[1].0).as_bytes()) as u64;
    assert!(first_page < lin.first_page_end && second_page >= lin.first_page_end - 1);
    // /T is the first entry of the main table; the final startxref points at the first-page
    // table near the start, whose trailer chains to the main table with /Prev.
    let entries = &fx.bytes[lin.main_xref_offset as usize..];
    assert!(entries.starts_with(b"0000000000 65535 f "));
    assert!(
        doc.xref_start < lin.first_page_end as usize,
        "startxref points at the first-page table"
    );
    assert!(fx.bytes[doc.xref_start..].starts_with(b"xref\n"));
    // /H names an object that is really there.
    let (hint_at, hint_len) = lin.hint_streams[0];
    let hint = &fx.bytes[hint_at as usize..(hint_at + hint_len) as usize];
    assert!(hint.ends_with(b"endobj\n") && contains(hint, b"stream"));
}

#[test]
fn the_broken_page_tree_names_an_object_that_is_not_there() {
    let fx = broken_page_tree();
    assert_eq!(fx.manifest.page_count(), 3);
    let doc = Document::load_mem(&fx.bytes).unwrap();
    let catalog = doc.catalog().unwrap();
    let pages = doc
        .get_dictionary(catalog.get(b"Pages").unwrap().as_reference().unwrap())
        .unwrap();
    let kids = pages.get(b"Kids").unwrap().as_array().unwrap();
    assert_eq!(kids.len(), 3);
    let present: Vec<bool> = kids
        .iter()
        .map(|k| doc.objects.contains_key(&k.as_reference().unwrap()))
        .collect();
    assert_eq!(present, [true, false, true]);
    assert_eq!(BROKEN_PAGE_INDEX, 1);
}

#[test]
fn the_scan_set_is_streamed_to_the_asked_size_and_parses() {
    let dir = std::env::temp_dir().join(format!("pl-testkit-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("scan_set.pdf");
    let set = write_scan_set(&path, 3, [320, 400]).unwrap();
    assert_eq!(set.pages, 3);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, set.file_len);
    assert!(
        bytes.len() > 3 * 320 * 400,
        "the images are stored, not compressed away"
    );
    assert!(bytes.len() < 3 * 320 * 400 + 4096);
    let doc = Document::load_mem(&bytes).unwrap();
    assert!(
        doc.xref_recovery.is_none(),
        "the hand-written xref is read as written"
    );
    let ids = page_ids(&doc);
    assert_eq!(ids.len(), 3);
    for id in &ids {
        let images = doc.get_page_images(*id).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!((images[0].width, images[0].height), (320, 400));
        assert_eq!(images[0].content.len(), 320 * 400);
    }
    // Deterministic, and noise: the first image does not repeat a short pattern.
    let again = dir.join("again.pdf");
    write_scan_set(&again, 3, [320, 400]).unwrap();
    assert_eq!(std::fs::read(&again).unwrap(), bytes);
    let first = doc.get_page_images(ids[0]).unwrap()[0].content.to_vec();
    let distinct: std::collections::HashSet<u8> = first.iter().copied().collect();
    assert!(distinct.len() > 200);
    std::fs::remove_dir_all(&dir).unwrap();
}
