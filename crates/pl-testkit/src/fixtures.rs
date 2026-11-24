//! The fixture generators. Each returns the PDF bytes and its ground truth.

use crate::build::{
    array, form_xobject, group_under_node, name, number, save, text, Builder, Content, PageSpec, SaveKind,
};
use crate::{
    AnnotTruth, EncryptionTruth, Fixture, FormFieldTruth, GridTruth, ImageTruth, LayerTruth, Manifest,
    OutlineTruth, PageTruth, Rect, Segment, TextTruth, XrefKind, ARCH_D, BLACK, BLUE,
    DEFAULT_DENSE_SEGMENTS, DEFAULT_DENSE_STREAMS, DEFAULT_RASTER_PX, GREEN, LETTER, RED, YELLOW,
};
use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
use lopdf::{dictionary, Dictionary, Document, EncryptionState, EncryptionVersion, Object, Permissions, Stream};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

/// Pages in [`multi_page`].
pub const MULTI_PAGE_COUNT: usize = 24;
/// Pages per top-level bookmark in [`multi_page`].
pub const MULTI_PAGE_SECTION: usize = 6;
/// Owner password of the encrypted fixtures (the user password is empty).
pub const OWNER_PASSWORD: &str = "plumb-owner";
/// `/T` of the stand-in foreign markups in [`annotations`].
pub const FOREIGN_AUTHOR: &str = "Foreign Author";
/// `/M` of the stand-in foreign markups in [`annotations`].
pub const FOREIGN_DATE: &str = "D:20240102030405Z";

pub(crate) fn manifest(name: &str, version: &str, kind: SaveKind, pages: Vec<PageTruth>) -> Manifest {
    Manifest {
        name: name.to_string(),
        pdf_version: version.to_string(),
        xref: match kind {
            SaveKind::Classic => XrefKind::Table,
            SaveKind::Modern => XrefKind::Stream,
        },
        object_streams: matches!(kind, SaveKind::Modern),
        linearized: false,
        deterministic: true,
        pages,
        outline: Vec::new(),
        layers: Vec::new(),
        form_fields: Vec::new(),
        encryption: None,
    }
}

fn page_spec(truth: &PageTruth) -> PageSpec {
    PageSpec {
        media_box: truth.media_box,
        crop_box: truth.crop_box,
        rotate: truth.rotate,
        user_unit: truth.user_unit,
    }
}

/// Add a page that draws exactly the truth's segments, then its texts, in one content stream.
fn add_simple_page(b: &mut Builder, truth: &mut PageTruth) -> lopdf::ObjectId {
    let mut c = Content::default();
    c.segments(&truth.segments).texts(&truth.texts);
    truth.segment_count = truth.segments.len();
    truth.content_streams = 1;
    b.add_page(&page_spec(truth), vec![c.into_stream()], Dictionary::new())
}

// ---------------------------------------------------------------------------
// plain
// ---------------------------------------------------------------------------

/// Segments and texts of the [`plain`] page (also reused by the xref fixtures' first page).
pub(crate) fn plain_truth() -> PageTruth {
    let mut p = PageTruth::new(LETTER);
    p.segments = vec![
        // A 468 x 648 frame as four separate segments.
        Segment::new([72.0, 72.0], [540.0, 72.0], 1.0, BLACK),
        Segment::new([540.0, 72.0], [540.0, 720.0], 1.0, BLACK),
        Segment::new([540.0, 720.0], [72.0, 720.0], 1.0, BLACK),
        Segment::new([72.0, 720.0], [72.0, 72.0], 1.0, BLACK),
        // Two diagonals crossing at the page centre (306, 396).
        Segment::new([72.0, 72.0], [540.0, 720.0], 0.5, RED),
        Segment::new([72.0, 720.0], [540.0, 72.0], 0.5, RED),
        // A horizontal and a vertical through the centre, exactly 3 in and 4 in long.
        Segment::new([198.0, 396.0], [414.0, 396.0], 2.0, BLUE),
        Segment::new([306.0, 252.0], [306.0, 540.0], 2.0, BLUE),
        // A 3-4-5 triangle (legs 144 and 108, hypotenuse 180) as three segments.
        Segment::new([100.0, 100.0], [244.0, 100.0], 1.5, GREEN),
        Segment::new([244.0, 100.0], [244.0, 208.0], 1.5, GREEN),
        Segment::new([244.0, 208.0], [100.0, 100.0], 1.5, GREEN),
        // Short fractional-coordinate segment.
        Segment::new([400.25, 150.5], [450.75, 150.5], 0.25, BLACK),
    ];
    p.texts = vec![
        TextTruth::new("Plumbline fixture: plain page", [90.0, 690.0], 18.0),
        TextTruth::new("The quick brown fox jumps over the lazy dog", [90.0, 660.0], 12.0),
        TextTruth::new("0123456789", [90.0, 620.0], 24.0),
        TextTruth::new("Scale 1/4 in = 1 ft (A-101)", [320.0, 120.0], 10.0),
    ];
    p
}

/// One US Letter page with known vector segments and Helvetica text.
pub fn plain() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut page = plain_truth();
    add_simple_page(&mut b, &mut page);
    let bytes = save(b.finish("plain"), SaveKind::Classic);
    Fixture { manifest: manifest("plain", "1.4", SaveKind::Classic, vec![page]), bytes }
}

// ---------------------------------------------------------------------------
// multi-page with outline
// ---------------------------------------------------------------------------

/// The unique search token on zero-based page `index` of [`multi_page`].
pub fn multi_page_token(index: usize) -> String {
    format!("PLUMB-PAGE-{:03}", index + 1)
}

/// 24 Letter pages, each with distinct text and one distinct segment, and a
/// two-level outline: 4 sections of 6 sheets (28 bookmarks).
pub fn multi_page() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut pages = Vec::new();
    let mut page_ids = Vec::new();
    for i in 0..MULTI_PAGE_COUNT {
        let n = (i + 1) as f64;
        let mut p = PageTruth::new(LETTER);
        p.segments = vec![
            Segment::new([72.0, 72.0], [540.0, 72.0], 1.0, BLACK),
            // Distinct per page: length grows by 10 pt per page.
            Segment::new([72.0, 144.0], [72.0 + 10.0 * n, 144.0 + 5.0 * n], 1.0, BLUE),
        ];
        p.texts = vec![
            TextTruth::new(&format!("Sheet {} of {}", i + 1, MULTI_PAGE_COUNT), [72.0, 720.0], 18.0),
            TextTruth::new(&multi_page_token(i), [72.0, 690.0], 12.0),
        ];
        page_ids.push(add_simple_page(&mut b, &mut p));
        pages.push(p);
    }

    // Outline: allocate ids first, then fill the dictionaries.
    let dest = [72.0, 738.0];
    let root_id = b.doc.new_object_id();
    let sections = MULTI_PAGE_COUNT / MULTI_PAGE_SECTION;
    let section_ids: Vec<_> = (0..sections).map(|_| b.doc.new_object_id()).collect();
    let sheet_ids: Vec<_> = (0..MULTI_PAGE_COUNT).map(|_| b.doc.new_object_id()).collect();
    let dest_obj = |page: lopdf::ObjectId| {
        Object::Array(vec![
            Object::Reference(page),
            name("XYZ"),
            number(dest[0]),
            number(dest[1]),
            Object::Integer(0),
        ])
    };
    let mut outline = Vec::new();
    for s in 0..sections {
        let first_page = s * MULTI_PAGE_SECTION;
        let title = format!("Section {}", s + 1);
        let mut d = dictionary! {
            "Title" => text(&title),
            "Parent" => Object::Reference(root_id),
            "Dest" => dest_obj(page_ids[first_page]),
            "First" => Object::Reference(sheet_ids[first_page]),
            "Last" => Object::Reference(sheet_ids[first_page + MULTI_PAGE_SECTION - 1]),
            "Count" => Object::Integer(MULTI_PAGE_SECTION as i64),
        };
        if s > 0 {
            d.set("Prev", Object::Reference(section_ids[s - 1]));
        }
        if s + 1 < sections {
            d.set("Next", Object::Reference(section_ids[s + 1]));
        }
        b.doc.objects.insert(section_ids[s], Object::Dictionary(d));
        outline.push(OutlineTruth { title, level: 0, page_index: first_page, dest });
        for k in 0..MULTI_PAGE_SECTION {
            let page = first_page + k;
            let title = format!("Sheet {}", page + 1);
            let mut d = dictionary! {
                "Title" => text(&title),
                "Parent" => Object::Reference(section_ids[s]),
                "Dest" => dest_obj(page_ids[page]),
            };
            if k > 0 {
                d.set("Prev", Object::Reference(sheet_ids[page - 1]));
            }
            if k + 1 < MULTI_PAGE_SECTION {
                d.set("Next", Object::Reference(sheet_ids[page + 1]));
            }
            b.doc.objects.insert(sheet_ids[page], Object::Dictionary(d));
            outline.push(OutlineTruth { title, level: 1, page_index: page, dest });
        }
    }
    b.doc.objects.insert(
        root_id,
        Object::Dictionary(dictionary! {
            "Type" => name("Outlines"),
            "First" => Object::Reference(section_ids[0]),
            "Last" => Object::Reference(section_ids[sections - 1]),
            "Count" => Object::Integer((sections + MULTI_PAGE_COUNT) as i64),
        }),
    );
    b.catalog.set("Outlines", Object::Reference(root_id));
    b.catalog.set("PageMode", name("UseOutlines"));

    let bytes = save(b.finish("multi_page"), SaveKind::Classic);
    let mut m = manifest("multi_page", "1.4", SaveKind::Classic, pages);
    m.outline = outline;
    Fixture { manifest: m, bytes }
}

// ---------------------------------------------------------------------------
// rotation, UserUnit, CropBox
// ---------------------------------------------------------------------------

/// Orientation marks relative to a visible box. They are asymmetric, so any
/// rotation or flip error shows: a long red horizontal and a short blue
/// vertical from the lower-left inset corner, a black diagonal towards the
/// upper-right, green ticks starting exactly on the lower-left and upper-right
/// box corners, and a label near the upper-left.
fn box_marks(label: &str, vb: Rect) -> (Vec<Segment>, Vec<TextTruth>) {
    let [x0, y0, x1, y1] = vb;
    let segments = vec![
        Segment::new([x0 + 36.0, y0 + 36.0], [x0 + 236.0, y0 + 36.0], 2.0, RED),
        Segment::new([x0 + 36.0, y0 + 36.0], [x0 + 36.0, y0 + 136.0], 2.0, BLUE),
        Segment::new([x0 + 36.0, y0 + 36.0], [x1 - 36.0, y1 - 36.0], 1.0, BLACK),
        Segment::new([x0, y0], [x0 + 18.0, y0 + 18.0], 1.0, GREEN),
        Segment::new([x1, y1], [x1 - 18.0, y1 - 18.0], 1.0, GREEN),
    ];
    let texts = vec![TextTruth::new(label, [x0 + 54.0, y1 - 72.0], 14.0)];
    (segments, texts)
}

fn marked_page(label: &str, media_box: Rect, crop_box: Option<Rect>, rotate: i32, user_unit: f64) -> PageTruth {
    let mut p = PageTruth::new(media_box);
    p.crop_box = crop_box;
    p.rotate = rotate;
    p.user_unit = user_unit;
    let (segments, texts) = box_marks(label, p.visible_box());
    p.segments = segments;
    p.texts = texts;
    p
}

/// Four Letter pages with `/Rotate` 0, 90, 180 and 270 and identical
/// content in unrotated space (apart from the label text).
pub fn rotated_pages() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut pages = Vec::new();
    for rotate in [0, 90, 180, 270] {
        let mut p = marked_page(&format!("ROTATE {rotate}"), LETTER, None, rotate, 1.0);
        add_simple_page(&mut b, &mut p);
        pages.push(p);
    }
    let bytes = save(b.finish("rotated_pages"), SaveKind::Classic);
    Fixture { manifest: manifest("rotated_pages", "1.4", SaveKind::Classic, pages), bytes }
}

/// One page with `/UserUnit 2`: MediaBox 306 x 396 units, so it displays as
/// US Letter (612 x 792 pt). Coordinates in the manifest are user-space units.
pub fn user_unit() -> Fixture {
    let mut b = Builder::new("1.6");
    let mut p = marked_page("USERUNIT 2", [0.0, 0.0, 306.0, 396.0], None, 0, 2.0);
    add_simple_page(&mut b, &mut p);
    let bytes = save(b.finish("user_unit"), SaveKind::Classic);
    Fixture { manifest: manifest("user_unit", "1.6", SaveKind::Classic, vec![p]), bytes }
}

/// Three pages exercising box offsets: (1) Letter MediaBox with an inset
/// CropBox `[36 72 540 684]`; (2) a MediaBox whose origin is not zero,
/// `[100 200 712 992]`, no CropBox; (3) both, with `/Rotate 90`.
/// Marks are placed relative to each page's visible box.
pub fn crop_box() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut pages = vec![
        marked_page("CROPBOX OFFSET", LETTER, Some([36.0, 72.0, 540.0, 684.0]), 0, 1.0),
        marked_page("MEDIABOX OFFSET", [100.0, 200.0, 712.0, 992.0], None, 0, 1.0),
        marked_page("CROP+ROTATE 90", [100.0, 200.0, 712.0, 992.0], Some([150.0, 260.0, 650.0, 900.0]), 90, 1.0),
    ];
    // Page 1 also has a segment wholly outside its CropBox (inside the MediaBox),
    // which a correct viewer clips away.
    pages[0].segments.push(Segment::new([4.0, 20.0], [30.0, 20.0], 4.0, RED));
    for p in &mut pages {
        add_simple_page(&mut b, p);
    }
    let bytes = save(b.finish("crop_box"), SaveKind::Classic);
    Fixture { manifest: manifest("crop_box", "1.4", SaveKind::Classic, pages), bytes }
}

/// The same page as [`user_unit`], saved with a cross-reference stream and the page
/// dictionary packed into a compressed object stream, so the name `/UserUnit` appears nowhere
/// in the raw bytes. A reader that only scans the file for the key misses it.
pub fn user_unit_in_object_stream() -> Fixture {
    let mut b = Builder::new("1.6");
    let mut p = marked_page("USERUNIT OBJSTM", [0.0, 0.0, 306.0, 396.0], None, 0, 2.0);
    add_simple_page(&mut b, &mut p);
    let bytes = save(b.finish("user_unit_objstm"), SaveKind::Modern);
    Fixture { manifest: manifest("user_unit_objstm", "1.6", SaveKind::Modern, vec![p]), bytes }
}

/// MediaBox every page of the first group of [`inherited_boxes`] inherits.
pub const INHERITED_MEDIA_BOX: Rect = [100.0, 100.0, 400.0, 500.0];
/// CropBox every page of the second group of [`inherited_boxes`] inherits.
pub const INHERITED_CROP_BOX: Rect = [100.0, 100.0, 400.0, 400.0];

/// Eight pages whose boxes come from an intermediate `/Pages` node, not from the page
/// dictionary: pages 1 to 4 inherit `/MediaBox [100 100 400 500]` (origin not zero) and have
/// no box of their own; pages 5 to 8 have their own `/MediaBox [0 0 600 800]` and inherit
/// `/CropBox [100 100 400 400]`. Each group has `/Rotate` 0, 90, 180 and 270. The manifest
/// records the effective boxes. Marks are placed relative to each page's visible box.
pub fn inherited_boxes() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut pages = Vec::new();
    let mut ids = Vec::new();
    for rotate in [0, 90, 180, 270] {
        let mut p = marked_page(&format!("INHERIT MEDIA {rotate}"), INHERITED_MEDIA_BOX, None, rotate, 1.0);
        ids.push(add_simple_page(&mut b, &mut p));
        pages.push(p);
    }
    for rotate in [0, 90, 180, 270] {
        let mut p = marked_page(
            &format!("INHERIT CROP {rotate}"),
            [0.0, 0.0, 600.0, 800.0],
            Some(INHERITED_CROP_BOX),
            rotate,
            1.0,
        );
        ids.push(add_simple_page(&mut b, &mut p));
        pages.push(p);
    }
    let mut doc = b.finish("inherited_boxes");
    group_under_node(
        &mut doc,
        &ids[..4],
        dictionary! { "MediaBox" => array(&INHERITED_MEDIA_BOX) },
        &["MediaBox"],
    );
    group_under_node(
        &mut doc,
        &ids[4..],
        dictionary! { "CropBox" => array(&INHERITED_CROP_BOX) },
        &["CropBox"],
    );
    let bytes = save(doc, SaveKind::Classic);
    Fixture { manifest: manifest("inherited_boxes", "1.4", SaveKind::Classic, pages), bytes }
}

/// Zero-based index of the page of [`broken_page_tree`] that cannot be loaded.
pub const BROKEN_PAGE_INDEX: usize = 1;

/// A deliberately damaged file: three Letter pages in the page tree, but the middle `/Kids`
/// entry points at an object that does not exist. Readers open the file and show pages 1 and
/// 3; page 2 cannot be loaded. It is not part of [`crate::all_fixtures`] (it is not a valid
/// PDF). The manifest lists three pages; the middle entry is a blank Letter placeholder.
pub fn broken_page_tree() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut pages = Vec::new();
    let mut ids = Vec::new();
    for i in 0..3usize {
        let mut p = PageTruth::new(LETTER);
        p.segments = vec![Segment::new([72.0, 72.0], [540.0, 72.0], 1.0, BLACK)];
        p.texts.push(TextTruth::new(&format!("BROKEN-TREE page {}", i + 1), [72.0, 740.0], 12.0));
        ids.push(add_simple_page(&mut b, &mut p));
        pages.push(p);
    }
    let mut doc = b.finish("broken_page_tree");
    doc.objects.remove(&ids[BROKEN_PAGE_INDEX]);
    pages[BROKEN_PAGE_INDEX] = PageTruth::new(LETTER);
    pages[BROKEN_PAGE_INDEX].content_streams = 0;
    let bytes = save(doc, SaveKind::Classic);
    Fixture { manifest: manifest("broken_page_tree", "1.4", SaveKind::Classic, pages), bytes }
}

// ---------------------------------------------------------------------------
// dense large-format sheet
// ---------------------------------------------------------------------------

/// [`dense_sheet_with`] at the default 200,000 segments in 8 content streams.
pub fn dense_sheet() -> Fixture {
    dense_sheet_with(DEFAULT_DENSE_SEGMENTS, DEFAULT_DENSE_STREAMS)
}

/// A 36 x 24 in sheet with `count` short, non-touching line segments laid out
/// on a grid inside a 0.5 in margin, split over `streams` Flate-compressed
/// content streams. Each segment is its own stroked path. See [`GridTruth`].
///
/// # Panics
/// If `count` is so large that a cell is smaller than 0.05 pt (several million).
pub fn dense_sheet_with(count: usize, streams: usize) -> Fixture {
    let streams = streams.max(1);
    let origin = [3_600i64, 3_600];
    let usable = [252_000i64, 165_600];
    let cols = ((count as f64 * usable[0] as f64 / usable[1] as f64).sqrt().ceil() as usize).max(1);
    let rows = count.div_ceil(cols).max(1);
    let pitch = [usable[0] / cols as i64, usable[1] / rows as i64];
    let grid = GridTruth {
        count,
        cols,
        rows,
        streams,
        origin_cpt: origin,
        pitch_cpt: pitch,
        inset_cpt: [pitch[0] / 5, pitch[1] / 5],
        extent_cpt: [pitch[0] * 3 / 5, pitch[1] * 3 / 5],
        width: 0.25,
        rgb: BLACK,
    };
    assert!(grid.extent_cpt[0] >= 1 && grid.extent_cpt[1] >= 1, "too many segments for the sheet");

    let mut p = PageTruth::new(ARCH_D);
    p.texts = vec![TextTruth::new(&format!("DENSE SHEET {count} SEGMENTS"), [36.0, 1700.0], 14.0)];
    p.segment_count = count;
    p.content_streams = streams;

    // Every stream sets its own graphics state so each is valid on its own.
    let mut bodies: Vec<String> = (0..streams).map(|_| String::from("0.25 w 0 0 0 RG\n")).collect();
    for i in 0..count {
        let c = grid.segment_cpt(i);
        let body = &mut bodies[grid.stream_of(i)];
        let _ = writeln!(
            body,
            "{}.{:02} {}.{:02} m {}.{:02} {}.{:02} l S",
            c[0] / 100,
            c[0] % 100,
            c[1] / 100,
            c[1] % 100,
            c[2] / 100,
            c[2] % 100,
            c[3] / 100,
            c[3] % 100
        );
    }
    let mut title = Content::default();
    title.texts(&p.texts);
    bodies[0].push_str(&title.0);

    let contents: Vec<Stream> = bodies
        .into_iter()
        .map(|body| {
            let mut s = Stream::new(Dictionary::new(), body.into_bytes());
            s.compress().expect("flate compression");
            s
        })
        .collect();
    p.grid = Some(grid);

    let mut b = Builder::new("1.4");
    b.add_page(&page_spec(&p), contents, Dictionary::new());
    let bytes = save(b.finish("dense_sheet"), SaveKind::Classic);
    Fixture { manifest: manifest("dense_sheet", "1.4", SaveKind::Classic, vec![p]), bytes }
}

// ---------------------------------------------------------------------------
// xref variants
// ---------------------------------------------------------------------------

fn xref_pages(b: &mut Builder, tag: &str) -> Vec<PageTruth> {
    let mut pages = Vec::new();
    for i in 0..3usize {
        let mut p = if i == 0 { plain_truth() } else { PageTruth::new(LETTER) };
        if i > 0 {
            let n = i as f64;
            p.segments = vec![
                Segment::new([72.0, 72.0], [540.0, 72.0], 1.0, BLACK),
                Segment::new([72.0, 200.0], [72.0 + 100.0 * n, 200.0], 2.0, RED),
            ];
        }
        p.texts.push(TextTruth::new(&format!("{tag} page {}", i + 1), [72.0, 740.0], 12.0));
        add_simple_page(b, &mut p);
        pages.push(p);
    }
    pages
}

/// Three pages saved as PDF 1.4 with a classic `xref` table and `trailer`.
pub fn xref_classic() -> Fixture {
    let mut b = Builder::new("1.4");
    let pages = xref_pages(&mut b, "XREF-TABLE");
    let bytes = save(b.finish("xref_classic"), SaveKind::Classic);
    Fixture { manifest: manifest("xref_classic", "1.4", SaveKind::Classic, pages), bytes }
}

/// The same three pages saved as PDF 1.5 with a cross-reference stream and
/// with dictionaries packed into object streams.
pub fn xref_object_streams() -> Fixture {
    let mut b = Builder::new("1.5");
    let pages = xref_pages(&mut b, "XREF-STREAM");
    let bytes = save(b.finish("xref_object_streams"), SaveKind::Modern);
    Fixture { manifest: manifest("xref_object_streams", "1.5", SaveKind::Modern, pages), bytes }
}

// ---------------------------------------------------------------------------
// existing annotations
// ---------------------------------------------------------------------------

/// One Letter page carrying four markups written the way another application
/// would: Square, Line, FreeText and Highlight, each with `/NM`, `/T`, `/M`,
/// `/F 4`, `/Contents` and an `/AP /N` form XObject. Square and FreeText
/// appearances use a `[0 0 w h]` BBox; Line and Highlight use a BBox equal to
/// `/Rect` with content in page coordinates (both styles occur in the wild).
/// The Highlight appearance uses a Multiply blend ExtGState.
pub fn annotations() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut p = PageTruth::new(LETTER);
    let highlighted = TextTruth::new("Highlighted words here", [72.0, 600.0], 14.0);
    p.texts = vec![
        TextTruth::new("Plumbline fixture: foreign annotations", [72.0, 720.0], 16.0),
        highlighted.clone(),
    ];
    p.segments = vec![Segment::new([72.0, 72.0], [540.0, 72.0], 1.0, BLACK)];
    let page_id = add_simple_page(&mut b, &mut p);

    let hl_x1 = (highlighted.origin[0] + highlighted.width).ceil();
    let hl_rect: Rect = [72.0, 596.0, hl_x1, 612.0];
    let line = [[300.0, 420.0], [500.0, 470.0]];
    let truths = vec![
        AnnotTruth {
            subtype: "Square",
            rect: [100.0, 400.0, 220.0, 480.0],
            name: "fixture-square-1".into(),
            contents: "Foreign square".into(),
            title: FOREIGN_AUTHOR.into(),
            rgb: RED,
            border_width: 2.0,
            line: None,
            quad_points: None,
            ap_bbox: [0.0, 0.0, 120.0, 80.0],
        },
        AnnotTruth {
            subtype: "Line",
            rect: [295.0, 415.0, 505.0, 475.0],
            name: "fixture-line-1".into(),
            contents: "Foreign line".into(),
            title: FOREIGN_AUTHOR.into(),
            rgb: BLUE,
            border_width: 3.0,
            line: Some(line),
            quad_points: None,
            ap_bbox: [295.0, 415.0, 505.0, 475.0],
        },
        AnnotTruth {
            subtype: "FreeText",
            rect: [100.0, 250.0, 340.0, 300.0],
            name: "fixture-freetext-1".into(),
            contents: "Foreign FreeText note".into(),
            title: FOREIGN_AUTHOR.into(),
            rgb: [1.0, 1.0, 1.0],
            border_width: 1.0,
            line: None,
            quad_points: None,
            ap_bbox: [0.0, 0.0, 240.0, 50.0],
        },
        AnnotTruth {
            subtype: "Highlight",
            rect: hl_rect,
            name: "fixture-highlight-1".into(),
            contents: "Foreign highlight".into(),
            title: FOREIGN_AUTHOR.into(),
            rgb: YELLOW,
            border_width: 0.0,
            line: None,
            quad_points: Some([72.0, 612.0, hl_x1, 612.0, 72.0, 596.0, hl_x1, 596.0]),
            ap_bbox: hl_rect,
        },
    ];

    let helv = dictionary! { "Font" => dictionary! { "Helv" => Object::Reference(b.font_id) } };
    let multiply = dictionary! {
        "ExtGState" => dictionary! {
            "GS0" => dictionary! { "Type" => name("ExtGState"), "BM" => name("Multiply") },
        },
    };
    let appearances = vec![
        form_xobject(truths[0].ap_bbox, Dictionary::new(), "2 w 1 0 0 RG 1 1 118 78 re S\n".into()),
        form_xobject(truths[1].ap_bbox, Dictionary::new(), "3 w 0 0 1 RG 300 420 m 500 470 l S\n".into()),
        form_xobject(
            truths[2].ap_bbox,
            helv,
            "1 w 0 0 0 RG 1 1 1 rg 0.5 0.5 239 49 re B\nBT /Helv 12 Tf 0 g 6 30 Td (Foreign FreeText note) Tj ET\n"
                .into(),
        ),
        form_xobject(
            truths[3].ap_bbox,
            multiply,
            format!("/GS0 gs 1 1 0 rg 72 596 {} 16 re f\n", hl_x1 - 72.0),
        ),
    ];

    let mut ids = Vec::new();
    for (t, ap) in truths.iter().zip(appearances) {
        let ap_id = b.doc.add_object(ap);
        let mut d = dictionary! {
            "Type" => name("Annot"),
            "Subtype" => name(t.subtype),
            "Rect" => array(&t.rect),
            "NM" => text(&t.name),
            "T" => text(&t.title),
            "Contents" => text(&t.contents),
            "Subj" => text(t.subtype),
            "M" => text(FOREIGN_DATE),
            "CreationDate" => text(FOREIGN_DATE),
            "F" => Object::Integer(4),
            "C" => array(&t.rgb),
            "P" => Object::Reference(page_id),
            "AP" => dictionary! { "N" => Object::Reference(ap_id) },
        };
        if t.border_width > 0.0 {
            d.set("BS", dictionary! { "W" => number(t.border_width), "S" => name("S") });
        }
        if let Some(l) = t.line {
            d.set("L", array(&[l[0][0], l[0][1], l[1][0], l[1][1]]));
            d.set("LE", Object::Array(vec![name("None"), name("None")]));
        }
        if let Some(q) = t.quad_points {
            d.set("QuadPoints", array(&q));
        }
        if t.subtype == "FreeText" {
            d.set("DA", text("/Helv 12 Tf 0 g"));
        }
        ids.push(b.doc.add_object(d));
    }
    b.set_annots(page_id, &ids);
    p.annotations = truths;

    let bytes = save(b.finish("annotations"), SaveKind::Classic);
    Fixture { manifest: manifest("annotations", "1.4", SaveKind::Classic, vec![p]), bytes }
}

// ---------------------------------------------------------------------------
// optional content (layers)
// ---------------------------------------------------------------------------

/// One Letter page with two optional-content groups, both ON by default.
/// "Layer A" draws five red horizontal lines and the text `LAYER-A`;
/// "Layer B" draws five blue vertical lines and the text `LAYER-B`;
/// a black diagonal and the text `BASE` belong to no layer.
pub fn layers() -> Fixture {
    let mut b = Builder::new("1.5");
    let base_segments = vec![Segment::new([72.0, 72.0], [540.0, 720.0], 1.0, BLACK)];
    let base_texts = vec![TextTruth::new("BASE", [72.0, 740.0], 14.0)];
    let layer_a = LayerTruth {
        name: "Layer A".into(),
        default_visible: true,
        page_index: 0,
        segments: (0..5)
            .map(|k| {
                let y = 200.0 + 40.0 * f64::from(k);
                Segment::new([72.0, y], [540.0, y], 2.0, RED)
            })
            .collect(),
        texts: vec![TextTruth::new("LAYER-A", [72.0, 650.0], 14.0)],
    };
    let layer_b = LayerTruth {
        name: "Layer B".into(),
        default_visible: true,
        page_index: 0,
        segments: (0..5)
            .map(|k| {
                let x = 120.0 + 60.0 * f64::from(k);
                Segment::new([x, 100.0], [x, 500.0], 2.0, BLUE)
            })
            .collect(),
        texts: vec![TextTruth::new("LAYER-B", [300.0, 650.0], 14.0)],
    };

    let ocg = |b: &mut Builder, layer: &LayerTruth| {
        b.doc.add_object(dictionary! { "Type" => name("OCG"), "Name" => text(&layer.name) })
    };
    let ocg_a = ocg(&mut b, &layer_a);
    let ocg_b = ocg(&mut b, &layer_b);

    let mut c = Content::default();
    c.segments(&base_segments).texts(&base_texts);
    c.raw("/OC /LA BDC").segments(&layer_a.segments).texts(&layer_a.texts).raw("EMC");
    c.raw("/OC /LB BDC").segments(&layer_b.segments).texts(&layer_b.texts).raw("EMC");

    let mut p = PageTruth::new(LETTER);
    p.segments = [base_segments, layer_a.segments.clone(), layer_b.segments.clone()].concat();
    p.texts = [base_texts, layer_a.texts.clone(), layer_b.texts.clone()].concat();
    p.segment_count = p.segments.len();

    let resources = dictionary! {
        "Properties" => dictionary! {
            "LA" => Object::Reference(ocg_a),
            "LB" => Object::Reference(ocg_b),
        },
    };
    b.add_page(&page_spec(&p), vec![c.into_stream()], resources);

    let both = || Object::Array(vec![Object::Reference(ocg_a), Object::Reference(ocg_b)]);
    b.catalog.set(
        "OCProperties",
        dictionary! {
            "OCGs" => both(),
            "D" => dictionary! {
                "Name" => text("Default"),
                "BaseState" => name("ON"),
                "ON" => both(),
                "Order" => both(),
            },
        },
    );

    let bytes = save(b.finish("layers"), SaveKind::Classic);
    let mut m = manifest("layers", "1.5", SaveKind::Classic, vec![p]);
    m.layers = vec![layer_a, layer_b];
    Fixture { manifest: m, bytes }
}

// ---------------------------------------------------------------------------
// AcroForm
// ---------------------------------------------------------------------------

/// One Letter page with two filled AcroForm text fields (`project_name`,
/// `sheet_number`), each a merged field/widget with `/V`, `/DA` and an
/// `/AP /N` stream showing the value. `/NeedAppearances` is false.
pub fn acro_form() -> Fixture {
    let mut b = Builder::new("1.4");
    let mut p = PageTruth::new(LETTER);
    p.texts = vec![
        TextTruth::new("Project name:", [72.0, 696.0], 12.0),
        TextTruth::new("Sheet number:", [72.0, 656.0], 12.0),
    ];
    let page_id = add_simple_page(&mut b, &mut p);

    let font_size = 12.0;
    let fields: Vec<FormFieldTruth> = [
        ("project_name", "Plumbline Tower", [160.0, 690.0, 410.0, 712.0]),
        ("sheet_number", "A-101", [160.0, 650.0, 260.0, 672.0]),
    ]
    .into_iter()
    .map(|(field, value, rect): (&str, &str, Rect)| FormFieldTruth {
        name: field.into(),
        value: value.into(),
        page_index: 0,
        rect,
        font_size,
        text_origin: [rect[0] + 2.0, rect[1] + 6.5],
    })
    .collect();

    let helv = dictionary! { "Font" => dictionary! { "Helv" => Object::Reference(b.font_id) } };
    let da = "/Helv 12 Tf 0 g";
    let mut ids = Vec::new();
    for f in &fields {
        let (w, h) = (f.rect[2] - f.rect[0], f.rect[3] - f.rect[1]);
        let ap_bbox = [0.0, 0.0, w, h];
        let content = format!(
            "/Tx BMC\nq\n1 1 {} {} re W n\nBT /Helv 12 Tf 0 g 2 6.5 Td ({}) Tj ET\nQ\nEMC\n",
            w - 2.0,
            h - 2.0,
            crate::build::escape(&f.value)
        );
        let ap_id = b.doc.add_object(form_xobject(ap_bbox, helv.clone(), content));
        ids.push(b.doc.add_object(dictionary! {
            "Type" => name("Annot"),
            "Subtype" => name("Widget"),
            "FT" => name("Tx"),
            "T" => text(&f.name),
            "V" => text(&f.value),
            "DV" => text(""),
            "Rect" => array(&f.rect),
            "F" => Object::Integer(4),
            "P" => Object::Reference(page_id),
            "DA" => text(da),
            "MK" => dictionary! { "BC" => array(&BLACK) },
            "AP" => dictionary! { "N" => Object::Reference(ap_id) },
        }));
        p.annotations.push(AnnotTruth {
            subtype: "Widget",
            rect: f.rect,
            name: String::new(),
            contents: f.value.clone(),
            title: f.name.clone(),
            rgb: BLACK,
            border_width: 0.0,
            line: None,
            quad_points: None,
            ap_bbox,
        });
    }
    b.set_annots(page_id, &ids);
    b.catalog.set(
        "AcroForm",
        dictionary! {
            "Fields" => Object::Array(ids.iter().map(|id| Object::Reference(*id)).collect()),
            "NeedAppearances" => Object::Boolean(false),
            "DA" => text(da),
            "DR" => helv,
        },
    );

    let bytes = save(b.finish("acro_form"), SaveKind::Classic);
    let mut m = manifest("acro_form", "1.4", SaveKind::Classic, vec![p]);
    m.form_fields = fields;
    Fixture { manifest: m, bytes }
}

// ---------------------------------------------------------------------------
// raster scan stand-in
// ---------------------------------------------------------------------------

/// [`raster_scan_with`] at 2550 x 3300 px (US Letter at 300 dpi).
pub fn raster_scan() -> Fixture {
    raster_scan_with(DEFAULT_RASTER_PX[0], DEFAULT_RASTER_PX[1])
}

/// One Letter page fully covered by a single 8-bit DeviceGray image
/// (Flate-compressed) and nothing else: a light two-tone checkerboard with
/// three solid dark rectangles at known places. See [`ImageTruth`].
///
/// # Panics
/// If either dimension is below 20 px.
pub fn raster_scan_with(width_px: u32, height_px: u32) -> Fixture {
    assert!(width_px >= 20 && height_px >= 20, "image must be at least 20 x 20 px");
    let (ux, uy) = (width_px / 20, height_px / 20);
    let dark_rects_px = vec![
        [2 * ux, 2 * uy, 8 * ux, 3 * uy],
        [2 * ux, 5 * uy, 3 * ux, 10 * uy],
        [10 * ux, 12 * uy, 17 * ux, 16 * uy],
    ];
    let placement = LETTER;
    let (sx, sy) = (612.0 / f64::from(width_px), 792.0 / f64::from(height_px));
    let dark_rects = dark_rects_px
        .iter()
        .map(|r| {
            [
                f64::from(r[0]) * sx,
                792.0 - f64::from(r[3]) * sy,
                f64::from(r[2]) * sx,
                792.0 - f64::from(r[1]) * sy,
            ]
        })
        .collect();
    let image = ImageTruth {
        width_px,
        height_px,
        bits_per_component: 8,
        color_space: "DeviceGray",
        placement,
        dark_rects,
        dark_rects_px,
        dark_level: 16,
        background_levels: [240, 248],
        checker_px: (width_px / 80).max(1),
    };

    let mut pixels = Vec::with_capacity(width_px as usize * height_px as usize);
    for y in 0..height_px {
        for x in 0..width_px {
            pixels.push(image.pixel(x, y));
        }
    }
    let mut stream = Stream::new(
        dictionary! {
            "Type" => name("XObject"),
            "Subtype" => name("Image"),
            "Width" => Object::Integer(i64::from(width_px)),
            "Height" => Object::Integer(i64::from(height_px)),
            "ColorSpace" => name("DeviceGray"),
            "BitsPerComponent" => Object::Integer(8),
        },
        pixels,
    );
    stream.compress().expect("flate compression");

    let mut b = Builder::new("1.4");
    let image_id = b.doc.add_object(stream);
    let mut c = Content::default();
    c.raw("q 612 0 0 792 0 0 cm /Im0 Do Q");
    let mut p = PageTruth::new(LETTER);
    p.images = vec![image];
    b.add_page(
        &page_spec(&p),
        vec![c.into_stream()],
        dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } },
    );
    let bytes = save(b.finish("raster_scan"), SaveKind::Classic);
    Fixture { manifest: manifest("raster_scan", "1.4", SaveKind::Classic, vec![p]), bytes }
}

// ---------------------------------------------------------------------------
// encryption
// ---------------------------------------------------------------------------

/// The unique plaintext token on zero-based page `index` of the encrypted fixtures.
pub fn encrypted_token(index: usize) -> String {
    format!("SECRET-SHEET-{:02}", index + 1)
}

fn encrypted_doc(fixture_name: &str, version: &str) -> (Document, Vec<PageTruth>) {
    let mut b = Builder::new(version);
    let mut pages = Vec::new();
    for i in 0..2usize {
        let mut p = PageTruth::new(LETTER);
        p.segments = vec![
            Segment::new([72.0, 72.0], [540.0, 72.0], 1.0, BLACK),
            Segment::new([72.0, 72.0], [72.0, 300.0 + 100.0 * i as f64], 2.0, RED),
        ];
        p.texts = vec![
            TextTruth::new("Issued for construction", [72.0, 720.0], 18.0),
            TextTruth::new(&encrypted_token(i), [72.0, 690.0], 12.0),
        ];
        add_simple_page(&mut b, &mut p);
        pages.push(p);
    }
    (b.finish(fixture_name), pages)
}

/// Permissions of the encrypted fixtures: printing, copying, commenting and
/// form filling allowed; content modification and page assembly forbidden.
fn issued_drawing_permissions() -> Permissions {
    Permissions::PRINTABLE
        | Permissions::PRINTABLE_IN_HIGH_QUALITY
        | Permissions::COPYABLE
        | Permissions::COPYABLE_FOR_ACCESSIBILITY
        | Permissions::ANNOTABLE
        | Permissions::FILLABLE
}

fn encryption_truth(algorithm: &'static str, key_bits: u32, version: i64, revision: i64) -> EncryptionTruth {
    EncryptionTruth {
        algorithm,
        key_bits,
        version,
        revision,
        user_password: String::new(),
        owner_password: OWNER_PASSWORD.into(),
        can_print: true,
        can_modify: false,
        can_copy: true,
        can_annotate: true,
        can_assemble: false,
    }
}

/// Two pages encrypted with the standard security handler, RC4 128-bit
/// (`/V 2 /R 3`), empty user password, owner password [`OWNER_PASSWORD`].
/// Deterministic. Opens without a prompt; modification is not permitted.
pub fn encrypted_rc4() -> Fixture {
    let (mut doc, pages) = encrypted_doc("encrypted_rc4", "1.4");
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &doc,
        owner_password: OWNER_PASSWORD,
        user_password: "",
        key_length: 128,
        permissions: issued_drawing_permissions(),
    })
    .expect("RC4 encryption state");
    doc.encrypt(&state).expect("encrypt");
    // lopdf fills the last 16 bytes of /U with random data. For revision 3
    // those bytes are arbitrary padding that readers ignore (ISO 32000-1,
    // Algorithm 5), so zero them to keep the file reproducible.
    let encrypt_id = doc.trailer.get(b"Encrypt").and_then(Object::as_reference).expect("/Encrypt reference");
    let user_value = doc
        .get_dictionary_mut(encrypt_id)
        .and_then(|d| d.get_mut(b"U"))
        .and_then(Object::as_str_mut)
        .expect("/U string");
    assert_eq!(user_value.len(), 32, "/U is 32 bytes");
    user_value[16..].fill(0);
    let bytes = save(doc, SaveKind::Classic);
    let mut m = manifest("encrypted_rc4", "1.4", SaveKind::Classic, pages);
    m.encryption = Some(encryption_truth("RC4", 128, 2, 3));
    Fixture { manifest: m, bytes }
}

/// The same two pages encrypted with AES-256 (`/V 5 /R 6`, PDF 2.0 handler),
/// empty user password, owner password [`OWNER_PASSWORD`]. **Not
/// byte-deterministic**: lopdf draws random IVs and salts on every call.
pub fn encrypted_aes256() -> Fixture {
    let (mut doc, pages) = encrypted_doc("encrypted_aes256", "2.0");
    // Fixed key; the output still varies because of random IVs and salts.
    let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(3));
    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let state = EncryptionState::try_from(EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: OWNER_PASSWORD,
        user_password: "",
        permissions: issued_drawing_permissions(),
    })
    .expect("AES-256 encryption state");
    doc.encrypt(&state).expect("encrypt");
    let bytes = save(doc, SaveKind::Classic);
    let mut m = manifest("encrypted_aes256", "2.0", SaveKind::Classic, pages);
    m.deterministic = false;
    m.encryption = Some(encryption_truth("AESV3", 256, 5, 6));
    Fixture { manifest: m, bytes }
}
