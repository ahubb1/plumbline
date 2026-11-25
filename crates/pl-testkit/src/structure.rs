//! Fixtures for the document-structure layer (milestone M8): page operations, outlines, links,
//! named destinations, page labels, form fields, layers, Bluebeam columns, signatures and
//! flatten.
//!
//! Every page of [`structure_main`] and [`structure_source`] draws a unique marker string, so a
//! page can be recognised after any reordering by its text or by a hash of its content stream.

use crate::build::{
    array, form_xobject, group_under_node, name, number, save, text, Builder, Content, SaveKind,
};
use crate::fixtures::{manifest, page_spec};
use crate::{Fixture, PageTruth, Point, Rect, Segment, TextTruth, BLUE, LETTER, RED};
use lopdf::{dictionary, Dictionary, Object, ObjectId, StringFormat};

/// The marker string drawn on zero-based page `index` of a structure fixture with `prefix`.
pub fn structure_marker(prefix: &str, index: usize) -> String {
    format!("PLM-{prefix}-{:02}", index + 1)
}

/// What a link annotation in a structure fixture does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// GoTo with an explicit destination (`/Dest` array or `/A` GoTo).
    GoTo,
    /// GoTo through a named destination.
    GoToNamed,
    Uri,
    GoToR,
    Launch,
    /// A `/Named` action (`NextPage`).
    Named,
}

/// One link annotation of a structure fixture.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTruth {
    pub page_index: usize,
    pub rect: Rect,
    pub kind: LinkKind,
    /// For GoTo and GoToNamed: the page it leads to (for GoToR: the page in the other file).
    pub target_page: Option<usize>,
    /// The `/XYZ` left and top, when the destination has them.
    pub target_point: Option<Point>,
    /// The address, file or action name.
    pub value: String,
}

/// One outline item, in pre-order.
#[derive(Debug, Clone, PartialEq)]
pub struct StructOutlineTruth {
    pub title: String,
    pub level: usize,
    /// The page it leads to (directly, by GoTo action or by name); `None` for a URI item.
    pub page: Option<usize>,
    /// `/Count` as written: positive open, negative closed, 0 for a leaf.
    pub count: i64,
    pub color: Option<[f64; 3]>,
    /// `/F` flags (1 italic, 2 bold).
    pub flags: i64,
}

/// Ground truth of a structure fixture beyond the [`Manifest`](crate::Manifest).
#[derive(Debug, Clone, PartialEq)]
pub struct StructureTruth {
    /// The marker string of each page.
    pub markers: Vec<String>,
    /// Each page's label as a viewer shows it (empty when the file has no `/PageLabels`).
    pub labels: Vec<String>,
    /// Named destinations and the page each leads to.
    pub named_dests: Vec<(String, usize)>,
    pub links: Vec<LinkTruth>,
    /// Top-level form field names and the pages of their widgets.
    pub fields: Vec<(String, Vec<usize>)>,
    /// Optional-content group names.
    pub layers: Vec<String>,
    /// Bluebeam custom column names, in array order.
    pub bluebeam_columns: Vec<String>,
    pub outline: Vec<StructOutlineTruth>,
    /// The `/OpenAction` page, if any.
    pub open_action_page: Option<usize>,
}

fn rect_obj(r: Rect) -> Object {
    array(&r)
}

fn ref_(id: ObjectId) -> Object {
    Object::Reference(id)
}

/// A UTF-16BE text string with a byte-order mark.
fn utf16(s: &str) -> Object {
    let mut bytes = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        bytes.extend(u.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

fn xyz(page: ObjectId, left: f64, top: f64) -> Object {
    Object::Array(vec![
        ref_(page),
        name("XYZ"),
        number(left),
        number(top),
        Object::Null,
    ])
}

fn fit(page: ObjectId) -> Object {
    Object::Array(vec![ref_(page), name("Fit")])
}

fn goto(d: Object) -> Object {
    Object::Dictionary(dictionary! { "S" => name("GoTo"), "D" => d })
}

fn link(page: ObjectId, rect: Rect, key: &str, value: Object) -> Dictionary {
    let mut d = dictionary! {
        "Type" => name("Annot"),
        "Subtype" => name("Link"),
        "Rect" => rect_obj(rect),
        "Border" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)]),
        "F" => Object::Integer(4),
        "P" => ref_(page),
    };
    d.set(key, value);
    d
}

/// Geometry of the pages of [`structure_main`]: (media box, crop box, rotate, user unit).
fn main_geometry(i: usize) -> (Rect, Option<Rect>, i32, f64) {
    match i {
        1 => (LETTER, None, 90, 1.0),
        2 => ([0.0, 0.0, 306.0, 396.0], None, 0, 2.0),
        3 => (
            [100.0, 200.0, 712.0, 992.0],
            Some([150.0, 260.0, 650.0, 900.0]),
            0,
            1.0,
        ),
        // Pages 4 and 5 inherit their MediaBox, Rotate and Resources (see below).
        4 | 5 => ([0.0, 0.0, 1224.0, 792.0], None, 180, 1.0),
        _ => (LETTER, None, 0, 1.0),
    }
}

/// A marked page: a unique marker string near the top left of the visible box and one segment
/// whose length depends on the index.
fn marked(
    prefix: &str,
    i: usize,
    media: Rect,
    crop: Option<Rect>,
    rotate: i32,
    unit: f64,
) -> PageTruth {
    let mut p = PageTruth::new(media);
    p.crop_box = crop;
    p.rotate = rotate;
    p.user_unit = unit;
    let vb = p.visible_box();
    let n = (i + 1) as f64;
    p.segments = vec![
        Segment::new(
            [vb[0] + 36.0, vb[1] + 36.0],
            [vb[0] + 36.0 + 12.0 * n, vb[1] + 36.0],
            2.0,
            BLUE,
        ),
        Segment::new(
            [vb[0] + 36.0, vb[1] + 40.0],
            [vb[0] + 36.0, vb[1] + 40.0 + 6.0 * n],
            1.0,
            RED,
        ),
    ];
    p.texts = vec![TextTruth::new(
        &structure_marker(prefix, i),
        [vb[0] + 36.0, vb[3] - 60.0],
        14.0,
    )];
    p.segment_count = p.segments.len();
    p
}

fn page_content(p: &PageTruth, extra: &str) -> lopdf::Stream {
    let mut c = Content::default();
    c.segments(&p.segments).texts(&p.texts);
    if !extra.is_empty() {
        c.raw(extra);
    }
    c.into_stream()
}

/// A square markup with an `/AP /N` (identity matrix, box at the origin), a closed popup and,
/// when `reply_author` is given, a reply (`/IRT`) Text note. Returns (markup, popup, reply).
fn square_with_reply(
    b: &mut Builder,
    page: ObjectId,
    rect: Rect,
    nm: &str,
    reply_author: Option<&str>,
) -> (ObjectId, ObjectId, Option<ObjectId>) {
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let ap = b.doc.add_object(form_xobject(
        [0.0, 0.0, w, h],
        Dictionary::new(),
        format!("2 w 1 0 0 RG 1 1 {} {} re S\n", w - 2.0, h - 2.0),
    ));
    let markup = b.doc.new_object_id();
    let popup = b.doc.add_object(dictionary! {
        "Type" => name("Annot"),
        "Subtype" => name("Popup"),
        "Rect" => rect_obj([rect[2], rect[1], rect[2] + 120.0, rect[1] + 60.0]),
        "Parent" => ref_(markup),
        "Open" => Object::Boolean(false),
        "P" => ref_(page),
    });
    b.doc.objects.insert(
        markup,
        Object::Dictionary(dictionary! {
            "Type" => name("Annot"),
            "Subtype" => name("Square"),
            "Rect" => rect_obj(rect),
            "NM" => text(nm),
            "T" => text("Fixture Author"),
            "Contents" => text("A square with a reply"),
            "F" => Object::Integer(4),
            "C" => array(&RED),
            "BS" => dictionary! { "W" => Object::Integer(2) },
            "P" => ref_(page),
            "Popup" => ref_(popup),
            "AP" => dictionary! { "N" => ref_(ap) },
            "BSIColumnData" => Object::Array(vec![text("Structural"), text("3")]),
        }),
    );
    let reply = reply_author.map(|author| {
        b.doc.add_object(dictionary! {
            "Type" => name("Annot"),
            "Subtype" => name("Text"),
            "Rect" => rect_obj([rect[0], rect[3], rect[0] + 20.0, rect[3] + 20.0]),
            "NM" => text(&format!("{nm}-reply")),
            "T" => text(author),
            "Contents" => text("Reply"),
            "IRT" => ref_(markup),
            "State" => text("Accepted"),
            "StateModel" => text("Review"),
            "F" => Object::Integer(4 | 2),
            "P" => ref_(page),
        })
    });
    (markup, popup, reply)
}

/// A text field widget (merged field and widget unless `parent` is given).
fn text_widget(
    b: &mut Builder,
    page: ObjectId,
    rect: Rect,
    field: Option<&str>,
    parent: Option<ObjectId>,
    value: &str,
) -> ObjectId {
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let helv = dictionary! { "Font" => dictionary! { "Helv" => ref_(b.font_id) } };
    let ap = b.doc.add_object(form_xobject(
        [0.0, 0.0, w, h],
        helv,
        format!("/Tx BMC q BT /Helv 10 Tf 0 g 2 5 Td ({value}) Tj ET Q EMC\n"),
    ));
    let mut d = dictionary! {
        "Type" => name("Annot"),
        "Subtype" => name("Widget"),
        "Rect" => rect_obj(rect),
        "F" => Object::Integer(4),
        "P" => ref_(page),
        "AP" => dictionary! { "N" => ref_(ap) },
    };
    if let Some(f) = field {
        d.set("T", text(f));
        d.set("FT", name("Tx"));
        d.set("V", text(value));
        d.set("DA", text("/Helv 10 Tf 0 g"));
    }
    if let Some(p) = parent {
        d.set("Parent", ref_(p));
    }
    b.doc.add_object(d)
}

/// Eight pages for the page-operation tests, each with a unique marker (`PLM-A-01`...), in
/// mixed geometry: page 2 has `/Rotate 90`, page 3 `/UserUnit 2`, page 4 an offset MediaBox and
/// CropBox, pages 5 and 6 inherit `/MediaBox`, `/Rotate 180` and `/Resources` from an
/// intermediate `/Pages` node (pages are one-based here, zero-based in the truth).
///
/// It also has: page labels (i, ii, then A-1...A-6); an outline with nested, closed, coloured,
/// bold, UTF-16 and named-destination items, a GoTo-action item and a URI item; named
/// destinations in the name tree and in the old `/Dests` dictionary; links of every kind; a
/// square with a popup and a reply; a text field with widgets on two pages and a single-widget
/// field; one optional-content layer; Bluebeam `/BSIAnnotColumns`; a catalog `/PlumblineData`;
/// an `/OpenAction`; and a small structure tree that refers to page 5.
pub fn structure_main() -> (Fixture, StructureTruth) {
    const PREFIX: &str = "A";
    const COUNT: usize = 8;
    let mut b = Builder::new("1.6");
    let ocg = b
        .doc
        .add_object(dictionary! { "Type" => name("OCG"), "Name" => text("Grid A") });
    let mut truths = Vec::new();
    let mut ids = Vec::new();
    for i in 0..COUNT {
        let (media, crop, rotate, unit) = main_geometry(i);
        let p = marked(PREFIX, i, media, crop, rotate, unit);
        let (extra, res) = if i == 6 {
            (
                "/OC /L0 BDC 0.5 w 0 0.5 0 RG 72 300 m 540 300 l S EMC".to_string(),
                dictionary! { "Properties" => dictionary! { "L0" => ref_(ocg) } },
            )
        } else {
            (String::new(), Dictionary::new())
        };
        let id = b.add_page(&page_spec(&p), vec![page_content(&p, &extra)], res);
        ids.push(id);
        truths.push(p);
    }

    // Annotations: links on pages 0, 6 and 7, a square with popup and reply on page 3,
    // form fields on pages 1, 2 and 6.
    let mut links = Vec::new();
    let mut annots: Vec<Vec<ObjectId>> = vec![Vec::new(); COUNT];
    let mut add_link =
        |b: &mut Builder, annots: &mut Vec<Vec<ObjectId>>, t: LinkTruth, key: &str, v: Object| {
            let id = b.doc.add_object(link(ids[t.page_index], t.rect, key, v));
            annots[t.page_index].push(id);
            links.push(t);
        };
    let l = |page_index, rect, kind, target_page, target_point, value: &str| LinkTruth {
        page_index,
        rect,
        kind,
        target_page,
        target_point,
        value: value.to_string(),
    };
    add_link(
        &mut b,
        &mut annots,
        l(
            0,
            [72.0, 600.0, 200.0, 620.0],
            LinkKind::GoTo,
            Some(5),
            Some([72.0, 700.0]),
            "",
        ),
        "Dest",
        xyz(ids[5], 72.0, 700.0),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            0,
            [72.0, 570.0, 200.0, 590.0],
            LinkKind::GoToNamed,
            Some(3),
            Some([150.0, 900.0]),
            "sheet-3",
        ),
        "Dest",
        text("sheet-3"),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            0,
            [72.0, 540.0, 200.0, 560.0],
            LinkKind::Uri,
            None,
            None,
            "https://example.com/plumbline",
        ),
        "A",
        Object::Dictionary(
            dictionary! { "S" => name("URI"), "URI" => text("https://example.com/plumbline") },
        ),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            0,
            [72.0, 510.0, 200.0, 530.0],
            LinkKind::GoToR,
            Some(2),
            None,
            "other.pdf",
        ),
        "A",
        Object::Dictionary(dictionary! {
            "S" => name("GoToR"),
            "F" => text("other.pdf"),
            "D" => Object::Array(vec![Object::Integer(2), name("Fit")]),
        }),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            0,
            [72.0, 480.0, 200.0, 500.0],
            LinkKind::Launch,
            None,
            None,
            "notes.txt",
        ),
        "A",
        Object::Dictionary(dictionary! { "S" => name("Launch"), "F" => text("notes.txt") }),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            0,
            [72.0, 450.0, 200.0, 470.0],
            LinkKind::Named,
            Some(1),
            None,
            "NextPage",
        ),
        "A",
        Object::Dictionary(dictionary! { "S" => name("Named"), "N" => name("NextPage") }),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            6,
            [72.0, 600.0, 200.0, 620.0],
            LinkKind::GoTo,
            Some(2),
            Some([10.0, 380.0]),
            "",
        ),
        "A",
        goto(xyz(ids[2], 10.0, 380.0)),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            7,
            [72.0, 600.0, 200.0, 620.0],
            LinkKind::GoTo,
            Some(0),
            None,
            "",
        ),
        "Dest",
        fit(ids[0]),
    );
    add_link(
        &mut b,
        &mut annots,
        l(
            1,
            [72.0, 100.0, 200.0, 120.0],
            LinkKind::GoToNamed,
            Some(7),
            None,
            "last",
        ),
        "A",
        goto(name("last")),
    );

    let (square, popup, reply) = square_with_reply(
        &mut b,
        ids[3],
        [200.0, 400.0, 320.0, 480.0],
        "struct-square-1",
        Some("Reviewer"),
    );
    annots[3].extend([square, popup, reply.expect("reply")]);

    let project = b.doc.add_object(dictionary! {
        "FT" => name("Tx"),
        "T" => text("project"),
        "V" => text("Tower"),
        "DA" => text("/Helv 10 Tf 0 g"),
    });
    let w1 = text_widget(
        &mut b,
        ids[1],
        [300.0, 700.0, 500.0, 720.0],
        None,
        Some(project),
        "Tower",
    );
    let w6 = text_widget(
        &mut b,
        ids[6],
        [300.0, 700.0, 500.0, 720.0],
        None,
        Some(project),
        "Tower",
    );
    b.doc
        .get_dictionary_mut(project)
        .expect("field")
        .set("Kids", Object::Array(vec![ref_(w1), ref_(w6)]));
    let sheet = text_widget(
        &mut b,
        ids[2],
        [100.0, 300.0, 250.0, 320.0],
        Some("sheet"),
        None,
        "A-101",
    );
    annots[1].push(w1);
    annots[6].push(w6);
    annots[2].push(sheet);
    for (i, a) in annots.iter().enumerate() {
        if !a.is_empty() {
            b.set_annots(ids[i], a);
        }
    }
    let helv = dictionary! { "Font" => dictionary! { "Helv" => ref_(b.font_id) } };
    b.catalog.set(
        "AcroForm",
        dictionary! {
            "Fields" => Object::Array(vec![ref_(project), ref_(sheet)]),
            "DA" => text("/Helv 10 Tf 0 g"),
            "DR" => helv,
        },
    );

    // Named destinations: the name tree and the old dictionary.
    let tree = b.doc.add_object(dictionary! {
        "Names" => Object::Array(vec![
            text("sheet-3"), xyz(ids[3], 150.0, 900.0),
            text("sheet-5"), Object::Dictionary(dictionary! { "D" => fit(ids[5]) }),
        ]),
    });
    b.catalog
        .set("Names", dictionary! { "Dests" => ref_(tree) });
    b.catalog
        .set("Dests", dictionary! { "last" => fit(ids[7]) });

    // Outline.
    let root = b.doc.new_object_id();
    let it: Vec<ObjectId> = (0..8).map(|_| b.doc.new_object_id()).collect();
    // 0 Front (open) -> [1 Cover, 2 Contents]; 3 Sheets (closed, red, bold) -> [4 Sheet 5, 5 Sheet 6];
    // 6 UTF-16 title; 7 Web (URI).
    let entries: Vec<(usize, Dictionary)> = vec![
        (
            0,
            dictionary! { "Title" => text("Front"), "Parent" => ref_(root), "Dest" => xyz(ids[0], 72.0, 740.0),
            "First" => ref_(it[1]), "Last" => ref_(it[2]), "Count" => Object::Integer(2), "Next" => ref_(it[3]) },
        ),
        (
            1,
            dictionary! { "Title" => text("Cover"), "Parent" => ref_(it[0]), "Dest" => fit(ids[0]), "Next" => ref_(it[2]) },
        ),
        (
            2,
            dictionary! { "Title" => text("Contents"), "Parent" => ref_(it[0]), "A" => goto(fit(ids[1])), "Prev" => ref_(it[1]) },
        ),
        (
            3,
            dictionary! { "Title" => text("Sheets"), "Parent" => ref_(root), "Dest" => text("sheet-3"),
            "First" => ref_(it[4]), "Last" => ref_(it[5]), "Count" => Object::Integer(-2),
            "C" => array(&RED), "F" => Object::Integer(2), "Prev" => ref_(it[0]), "Next" => ref_(it[6]) },
        ),
        (
            4,
            dictionary! { "Title" => text("Sheet 5"), "Parent" => ref_(it[3]), "Dest" => fit(ids[4]), "Next" => ref_(it[5]) },
        ),
        (
            5,
            dictionary! { "Title" => text("Sheet 6"), "Parent" => ref_(it[3]), "A" => goto(text("sheet-5")), "Prev" => ref_(it[4]) },
        ),
        (
            6,
            dictionary! { "Title" => utf16("Plan \u{00DC}bersicht \u{03A9}"), "Parent" => ref_(root), "Dest" => fit(ids[6]),
            "Prev" => ref_(it[3]), "Next" => ref_(it[7]), "F" => Object::Integer(1) },
        ),
        (
            7,
            dictionary! { "Title" => text("Web"), "Parent" => ref_(root),
            "A" => Object::Dictionary(dictionary! { "S" => name("URI"), "URI" => text("https://example.com") }),
            "Prev" => ref_(it[6]) },
        ),
    ];
    for (k, d) in entries {
        b.doc.objects.insert(it[k], Object::Dictionary(d));
    }
    b.doc.objects.insert(
        root,
        Object::Dictionary(dictionary! {
            "Type" => name("Outlines"), "First" => ref_(it[0]), "Last" => ref_(it[7]),
            "Count" => Object::Integer(6),
        }),
    );
    b.catalog.set("Outlines", ref_(root));
    let outline = vec![
        StructOutlineTruth {
            title: "Front".into(),
            level: 0,
            page: Some(0),
            count: 2,
            color: None,
            flags: 0,
        },
        StructOutlineTruth {
            title: "Cover".into(),
            level: 1,
            page: Some(0),
            count: 0,
            color: None,
            flags: 0,
        },
        StructOutlineTruth {
            title: "Contents".into(),
            level: 1,
            page: Some(1),
            count: 0,
            color: None,
            flags: 0,
        },
        StructOutlineTruth {
            title: "Sheets".into(),
            level: 0,
            page: Some(3),
            count: -2,
            color: Some(RED),
            flags: 2,
        },
        StructOutlineTruth {
            title: "Sheet 5".into(),
            level: 1,
            page: Some(4),
            count: 0,
            color: None,
            flags: 0,
        },
        StructOutlineTruth {
            title: "Sheet 6".into(),
            level: 1,
            page: Some(5),
            count: 0,
            color: None,
            flags: 0,
        },
        StructOutlineTruth {
            title: "Plan \u{00DC}bersicht \u{03A9}".into(),
            level: 0,
            page: Some(6),
            count: 0,
            color: None,
            flags: 1,
        },
        StructOutlineTruth {
            title: "Web".into(),
            level: 0,
            page: None,
            count: 0,
            color: None,
            flags: 0,
        },
    ];

    // Page labels: i, ii, then A-1 ...
    b.catalog.set(
        "PageLabels",
        dictionary! {
            "Nums" => Object::Array(vec![
                Object::Integer(0), Object::Dictionary(dictionary! { "S" => name("r") }),
                Object::Integer(2), Object::Dictionary(dictionary! { "S" => name("D"), "P" => text("A-") }),
            ]),
        },
    );
    let labels: Vec<String> = ["i", "ii"]
        .iter()
        .map(|s| s.to_string())
        .chain((1..=6).map(|n| format!("A-{n}")))
        .collect();

    // Layers, Bluebeam columns, private data, open action, structure tree.
    b.catalog.set(
        "OCProperties",
        dictionary! {
            "OCGs" => Object::Array(vec![ref_(ocg)]),
            "D" => dictionary! { "Name" => text("Default"), "ON" => Object::Array(vec![ref_(ocg)]), "Order" => Object::Array(vec![ref_(ocg)]) },
        },
    );
    let columns = b.doc.add_object(Object::Array(vec![
        Object::Dictionary(dictionary! { "Subtype" => name("Text"), "DisplayOrder" => Object::Integer(0), "Name" => text("Discipline") }),
        Object::Dictionary(dictionary! { "Subtype" => name("Number"), "DisplayOrder" => Object::Integer(1), "Name" => text("Qty"),
            "Format" => name("Normal"), "Precision" => Object::Integer(0) }),
    ]));
    b.catalog.set("BSIAnnotColumns", ref_(columns));
    b.catalog.set(
        "PlumblineData",
        dictionary! { "Fixture" => text("structure_main") },
    );
    b.catalog.set("OpenAction", fit(ids[1]));
    let elem = b.doc.add_object(
        dictionary! { "Type" => name("StructElem"), "S" => name("P"), "Pg" => ref_(ids[4]), "K" => Object::Integer(0) },
    );
    let struct_root = b
        .doc
        .add_object(dictionary! { "Type" => name("StructTreeRoot"), "K" => ref_(elem) });
    b.doc
        .get_dictionary_mut(elem)
        .expect("elem")
        .set("P", ref_(struct_root));
    b.catalog.set("StructTreeRoot", ref_(struct_root));

    let mut doc = b.finish("structure_main");
    let font = doc
        .get_dictionary(ids[4])
        .and_then(|d| d.get(b"Resources"))
        .cloned()
        .expect("resources");
    group_under_node(
        &mut doc,
        &ids[4..6],
        dictionary! { "MediaBox" => array(&[0.0, 0.0, 1224.0, 792.0]), "Rotate" => Object::Integer(180), "Resources" => font },
        &["MediaBox", "Rotate", "Resources"],
    );
    let bytes = save(doc, SaveKind::Classic);
    let truth = StructureTruth {
        markers: (0..COUNT).map(|i| structure_marker(PREFIX, i)).collect(),
        labels,
        named_dests: vec![
            ("sheet-3".into(), 3),
            ("sheet-5".into(), 5),
            ("last".into(), 7),
        ],
        links,
        fields: vec![("project".into(), vec![1, 6]), ("sheet".into(), vec![2])],
        layers: vec!["Grid A".into()],
        bluebeam_columns: vec!["Discipline".into(), "Qty".into()],
        outline,
        open_action_page: Some(1),
    };
    let m = manifest("structure_main", "1.6", SaveKind::Classic, truths);
    (Fixture { manifest: m, bytes }, truth)
}

/// Four Letter pages (`PLM-B-01`...) to insert into [`structure_main`]: no page labels; a
/// `project` field (clashing with the main fixture's) on page 1 and a `notes` field on page 2;
/// a layer `Grid B` on page 3; Bluebeam columns `Qty`, `Status` (one shared with the main
/// fixture, at another position) and a markup with values on page 4; a link on page 1 to page 3
/// through the named destination `b-three` and one on page 2 to page 4; an outline of two items;
/// a square with popup and reply on page 4. Its pages are saved with a cross-reference stream
/// and object streams.
pub fn structure_source() -> (Fixture, StructureTruth) {
    const PREFIX: &str = "B";
    const COUNT: usize = 4;
    let mut b = Builder::new("1.6");
    let ocg = b
        .doc
        .add_object(dictionary! { "Type" => name("OCG"), "Name" => text("Grid B") });
    let mut truths = Vec::new();
    let mut ids = Vec::new();
    for i in 0..COUNT {
        let p = marked(PREFIX, i, LETTER, None, 0, 1.0);
        let (extra, res) = if i == 2 {
            (
                "/OC /L0 BDC 0.5 w 0 0 1 RG 72 300 m 540 300 l S EMC".to_string(),
                dictionary! { "Properties" => dictionary! { "L0" => ref_(ocg) } },
            )
        } else {
            (String::new(), Dictionary::new())
        };
        ids.push(b.add_page(&page_spec(&p), vec![page_content(&p, &extra)], res));
        truths.push(p);
    }
    let mut annots: Vec<Vec<ObjectId>> = vec![Vec::new(); COUNT];
    let l0 = b.doc.add_object(link(
        ids[0],
        [72.0, 600.0, 200.0, 620.0],
        "Dest",
        text("b-three"),
    ));
    let l1 = b.doc.add_object(link(
        ids[1],
        [72.0, 600.0, 200.0, 620.0],
        "A",
        goto(xyz(ids[3], 72.0, 500.0)),
    ));
    annots[0].push(l0);
    annots[1].push(l1);
    let project = text_widget(
        &mut b,
        ids[0],
        [300.0, 700.0, 500.0, 720.0],
        Some("project"),
        None,
        "Annex",
    );
    let notes = text_widget(
        &mut b,
        ids[1],
        [300.0, 650.0, 500.0, 670.0],
        Some("notes"),
        None,
        "None",
    );
    annots[0].push(project);
    annots[1].push(notes);
    let (square, popup, reply) = square_with_reply(
        &mut b,
        ids[3],
        [100.0, 100.0, 220.0, 160.0],
        "source-square-1",
        Some("Checker"),
    );
    // Its column values are by the source's positions: Qty, Status.
    b.doc.get_dictionary_mut(square).expect("square").set(
        "BSIColumnData",
        Object::Array(vec![text("5"), text("Open")]),
    );
    annots[3].extend([square, popup, reply.expect("reply")]);
    for (i, a) in annots.iter().enumerate() {
        if !a.is_empty() {
            b.set_annots(ids[i], a);
        }
    }
    let helv = dictionary! { "Font" => dictionary! { "Helv" => ref_(b.font_id) } };
    b.catalog.set(
        "AcroForm",
        dictionary! {
            "Fields" => Object::Array(vec![ref_(project), ref_(notes)]),
            "DA" => text("/Helv 10 Tf 0 g"),
            "DR" => helv,
        },
    );
    let tree = b
        .doc
        .add_object(dictionary! { "Names" => Object::Array(vec![text("b-three"), fit(ids[2])]) });
    b.catalog
        .set("Names", dictionary! { "Dests" => ref_(tree) });
    b.catalog.set(
        "OCProperties",
        dictionary! {
            "OCGs" => Object::Array(vec![ref_(ocg)]),
            "D" => dictionary! { "Name" => text("Default"), "OFF" => Object::Array(vec![ref_(ocg)]), "Order" => Object::Array(vec![ref_(ocg)]) },
        },
    );
    let columns = b.doc.add_object(Object::Array(vec![
        Object::Dictionary(
            dictionary! { "Subtype" => name("Number"), "DisplayOrder" => Object::Integer(0), "Name" => text("Qty") },
        ),
        Object::Dictionary(
            dictionary! { "Subtype" => name("Text"), "DisplayOrder" => Object::Integer(1), "Name" => text("Status") },
        ),
    ]));
    b.catalog.set("BSIAnnotColumns", ref_(columns));
    let root = b.doc.new_object_id();
    let a = b.doc.new_object_id();
    let c = b.doc.new_object_id();
    b.doc.objects.insert(a, Object::Dictionary(dictionary! { "Title" => text("B start"), "Parent" => ref_(root), "Dest" => fit(ids[0]), "Next" => ref_(c) }));
    b.doc.objects.insert(c, Object::Dictionary(dictionary! { "Title" => text("B three"), "Parent" => ref_(root), "Dest" => text("b-three"), "Prev" => ref_(a) }));
    b.doc.objects.insert(
        root,
        Object::Dictionary(dictionary! { "Type" => name("Outlines"), "First" => ref_(a), "Last" => ref_(c), "Count" => Object::Integer(2) }),
    );
    b.catalog.set("Outlines", ref_(root));
    let bytes = save(b.finish("structure_source"), SaveKind::Modern);
    let truth = StructureTruth {
        markers: (0..COUNT).map(|i| structure_marker(PREFIX, i)).collect(),
        labels: Vec::new(),
        named_dests: vec![("b-three".into(), 2)],
        links: vec![
            LinkTruth {
                page_index: 0,
                rect: [72.0, 600.0, 200.0, 620.0],
                kind: LinkKind::GoToNamed,
                target_page: Some(2),
                target_point: None,
                value: "b-three".into(),
            },
            LinkTruth {
                page_index: 1,
                rect: [72.0, 600.0, 200.0, 620.0],
                kind: LinkKind::GoTo,
                target_page: Some(3),
                target_point: Some([72.0, 500.0]),
                value: String::new(),
            },
        ],
        fields: vec![("project".into(), vec![0]), ("notes".into(), vec![1])],
        layers: vec!["Grid B".into()],
        bluebeam_columns: vec!["Qty".into(), "Status".into()],
        outline: vec![
            StructOutlineTruth {
                title: "B start".into(),
                level: 0,
                page: Some(0),
                count: 0,
                color: None,
                flags: 0,
            },
            StructOutlineTruth {
                title: "B three".into(),
                level: 0,
                page: Some(2),
                count: 0,
                color: None,
                flags: 0,
            },
        ],
        open_action_page: None,
    };
    let m = manifest("structure_source", "1.6", SaveKind::Modern, truths);
    (Fixture { manifest: m, bytes }, truth)
}

/// Name of the signed field in [`signed_stub`].
pub const SIGNED_FIELD: &str = "Approval";

/// One Letter page with a signature field that carries a (fake, unverifiable) signature value,
/// `/SigFlags 3`, and a catalog `/Perms /DocMDP`: a stand-in for a signed file. It is not a
/// valid signature; it only has the structure a reader looks for.
pub fn signed_stub() -> Fixture {
    let mut b = Builder::new("1.7");
    let p = marked("S", 0, LETTER, None, 0, 1.0);
    let page = b.add_page(
        &page_spec(&p),
        vec![page_content(&p, "")],
        Dictionary::new(),
    );
    let sig = b.doc.add_object(dictionary! {
        "Type" => name("Sig"),
        "Filter" => name("Adobe.PPKLite"),
        "SubFilter" => name("adbe.pkcs7.detached"),
        "ByteRange" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(0), Object::Integer(0)]),
        "Contents" => Object::String(vec![0; 64], StringFormat::Hexadecimal),
        "M" => text("D:20260101000000Z"),
    });
    let widget = b.doc.add_object(dictionary! {
        "Type" => name("Annot"),
        "Subtype" => name("Widget"),
        "FT" => name("Sig"),
        "T" => text(SIGNED_FIELD),
        "V" => ref_(sig),
        "Rect" => rect_obj([72.0, 72.0, 272.0, 122.0]),
        "F" => Object::Integer(4),
        "P" => ref_(page),
    });
    let empty = b.doc.add_object(dictionary! {
        "Type" => name("Annot"),
        "Subtype" => name("Widget"),
        "FT" => name("Sig"),
        "T" => text("Second signature"),
        "Rect" => rect_obj([300.0, 72.0, 500.0, 122.0]),
        "F" => Object::Integer(4),
        "P" => ref_(page),
    });
    b.set_annots(page, &[widget, empty]);
    b.catalog.set(
        "AcroForm",
        dictionary! { "Fields" => Object::Array(vec![ref_(widget), ref_(empty)]), "SigFlags" => Object::Integer(3) },
    );
    b.catalog
        .set("Perms", dictionary! { "DocMDP" => ref_(sig) });
    let bytes = save(b.finish("signed_stub"), SaveKind::Classic);
    Fixture {
        manifest: manifest("signed_stub", "1.7", SaveKind::Classic, vec![p]),
        bytes,
    }
}

/// What [`flatten_set`] puts on each page.
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenTruth {
    /// Per page: annotations that show on screen and have an appearance (they flatten).
    pub visible: Vec<usize>,
    /// Per page: annotations that must stay (hidden, no-view, popup, link).
    pub stay: Vec<usize>,
}

/// Four pages for flatten tests: plain Letter, `/Rotate 90`, `/UserUnit 2`, and an offset
/// MediaBox and CropBox with `/Rotate 270`. Each has, placed relative to the visible box:
///
/// - a square whose appearance box starts at the origin (identity `/Matrix`);
/// - a line whose appearance box is in page coordinates;
/// - an ink mark whose appearance has a 90-degree `/Matrix` (rotated box onto the rect);
/// - a circle whose appearance box is half the rect's size (scaled up 2x);
/// - a filled square with `/CA 0.5`;
/// - a highlight with a Multiply blend in its appearance;
/// - a Text note with a closed popup;
/// - a `Hidden` square and a `NoView` square (both must stay unflattened and invisible);
/// - a link without an appearance.
pub fn flatten_set() -> (Fixture, FlattenTruth) {
    let geoms: [(Rect, Option<Rect>, i32, f64); 4] = [
        (LETTER, None, 0, 1.0),
        (LETTER, None, 90, 1.0),
        ([0.0, 0.0, 306.0, 396.0], None, 0, 2.0),
        (
            [100.0, 200.0, 712.0, 992.0],
            Some([150.0, 260.0, 650.0, 900.0]),
            270,
            1.0,
        ),
    ];
    let mut b = Builder::new("1.6");
    let mut truths = Vec::new();
    let mut visible = Vec::new();
    let mut stay = Vec::new();
    for (i, (media, crop, rotate, unit)) in geoms.into_iter().enumerate() {
        let p = marked("F", i, media, crop, rotate, unit);
        // A grey background band so the opacity and blend have something to show over.
        let vb = p.visible_box();
        let (ox, oy) = (vb[0], vb[1]);
        let s = if unit == 2.0 { 0.5 } else { 1.0 };
        let band = format!(
            "0.8 g {} {} {} {} re f",
            ox + 20.0 * s,
            oy + 100.0 * s,
            (vb[2] - vb[0]) - 40.0 * s,
            60.0 * s
        );
        let page = b.add_page(
            &page_spec(&p),
            vec![page_content(&p, &band)],
            Dictionary::new(),
        );
        let r = |x: f64, y: f64, w: f64, h: f64| -> Rect {
            [ox + x * s, oy + y * s, ox + (x + w) * s, oy + (y + h) * s]
        };
        let mut ids = Vec::new();
        let mut annot = |b: &mut Builder,
                         sub: &str,
                         rect: Rect,
                         flags: i64,
                         ap: Option<lopdf::Stream>,
                         extra: Dictionary| {
            let mut d = dictionary! {
                "Type" => name("Annot"),
                "Subtype" => name(sub),
                "Rect" => rect_obj(rect),
                "F" => Object::Integer(flags),
                "P" => ref_(page),
                "NM" => text(&format!("flat-{i}-{sub}-{flags}")),
            };
            if let Some(ap) = ap {
                let ap = b.doc.add_object(ap);
                d.set("AP", dictionary! { "N" => ref_(ap) });
            }
            for (k, v) in extra.iter() {
                d.set(k.clone(), v.clone());
            }
            let id = b.doc.add_object(d);
            ids.push(id);
            id
        };
        // Square, appearance box at the origin.
        let sq = r(60.0, 400.0, 100.0, 60.0);
        let w = sq[2] - sq[0];
        let h = sq[3] - sq[1];
        annot(
            &mut b,
            "Square",
            sq,
            4,
            Some(form_xobject(
                [0.0, 0.0, w, h],
                Dictionary::new(),
                format!("2 w 1 0 0 RG 1 1 {} {} re S\n", w - 2.0, h - 2.0),
            )),
            Dictionary::new(),
        );
        // Line, appearance box in page coordinates.
        let ln = r(200.0, 400.0, 150.0, 60.0);
        annot(
            &mut b,
            "Line",
            ln,
            4,
            Some(form_xobject(
                ln,
                Dictionary::new(),
                format!(
                    "3 w 0 0 1 RG {} {} m {} {} l S\n",
                    ln[0] + 2.0,
                    ln[1] + 2.0,
                    ln[2] - 2.0,
                    ln[3] - 2.0
                ),
            )),
            dictionary! { "L" => array(&[ln[0] + 2.0, ln[1] + 2.0, ln[2] - 2.0, ln[3] - 2.0]) },
        );
        // Ink, 90-degree matrix: the 100 x 40 box turns into 40 x 100 on the page.
        let ink = r(380.0, 380.0, 40.0, 100.0);
        let mut ink_ap = form_xobject(
            [0.0, 0.0, 100.0, 40.0],
            Dictionary::new(),
            "2 w 0 0.5 0 RG 2 2 m 98 2 l 98 38 l S 2 2 m 60 38 l S\n".into(),
        );
        ink_ap.dict.set(
            "Matrix",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(1),
                Object::Integer(-1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(0),
            ]),
        );
        annot(&mut b, "Ink", ink, 4, Some(ink_ap), Dictionary::new());
        // Circle, appearance at half size.
        let ci = r(60.0, 250.0, 100.0, 80.0);
        annot(
            &mut b,
            "Circle",
            ci,
            4,
            Some(form_xobject(
                [0.0, 0.0, 50.0, 40.0],
                Dictionary::new(),
                "1 w 0.6 0 0.6 RG 25 2 m 48 20 l 25 38 l 2 20 l h S\n".into(),
            )),
            Dictionary::new(),
        );
        // Half-transparent filled square over the grey band.
        let tr = r(200.0, 80.0, 120.0, 100.0);
        let (tw, th) = (tr[2] - tr[0], tr[3] - tr[1]);
        annot(
            &mut b,
            "Square",
            tr,
            4,
            Some(form_xobject(
                [0.0, 0.0, tw, th],
                Dictionary::new(),
                format!(
                    "1 0 0 rg 0 0 {tw} {th} re f 0 0 1 rg {} {} {} {} re f\n",
                    tw / 4.0,
                    th / 4.0,
                    tw / 2.0,
                    th / 2.0
                ),
            )),
            dictionary! { "CA" => Object::Real(0.5) },
        );
        // Highlight with Multiply.
        let hl = r(360.0, 90.0, 120.0, 40.0);
        let multiply = dictionary! { "ExtGState" => dictionary! { "G0" => dictionary! { "Type" => name("ExtGState"), "BM" => name("Multiply") } } };
        annot(
            &mut b,
            "Highlight",
            hl,
            4,
            Some(form_xobject(
                hl,
                multiply,
                format!(
                    "/G0 gs 1 1 0 rg {} {} {} {} re f\n",
                    hl[0],
                    hl[1],
                    hl[2] - hl[0],
                    hl[3] - hl[1]
                ),
            )),
            dictionary! { "QuadPoints" => array(&[hl[0], hl[3], hl[2], hl[3], hl[0], hl[1], hl[2], hl[1]]) },
        );
        // Text note with a popup.
        let tx = r(450.0, 600.0, 20.0, 20.0);
        let note = annot(
            &mut b,
            "Text",
            tx,
            4,
            Some(form_xobject(
                [0.0, 0.0, 20.0, 20.0],
                Dictionary::new(),
                "1 1 0 rg 0 0 0 RG 1 w 1 1 18 18 re B\n".into(),
            )),
            dictionary! { "Contents" => text("note") },
        );
        let popup = annot(
            &mut b,
            "Popup",
            r(470.0, 560.0, 100.0, 60.0),
            4,
            None,
            dictionary! { "Parent" => ref_(note), "Open" => Object::Boolean(false) },
        );
        b.doc
            .get_dictionary_mut(note)
            .expect("note")
            .set("Popup", ref_(popup));
        // Hidden and NoView squares: never shown.
        for flags in [4 | 2, 4 | 32] {
            let hr = r(250.0, 600.0, 60.0, 40.0);
            annot(
                &mut b,
                "Square",
                hr,
                flags,
                Some(form_xobject(
                    [0.0, 0.0, 60.0 * s, 40.0 * s],
                    Dictionary::new(),
                    "0 0 0 rg 0 0 1000 1000 re f\n".into(),
                )),
                Dictionary::new(),
            );
        }
        annot(
            &mut b,
            "Link",
            r(60.0, 700.0, 100.0, 20.0),
            4,
            None,
            dictionary! { "Dest" => fit(page), "Border" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)]) },
        );
        b.set_annots(page, &ids);
        visible.push(7);
        stay.push(4);
        truths.push(p);
    }
    let bytes = save(b.finish("flatten_set"), SaveKind::Classic);
    (
        Fixture {
            manifest: manifest("flatten_set", "1.6", SaveKind::Classic, truths),
            bytes,
        },
        FlattenTruth { visible, stay },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Document;

    #[test]
    fn structure_fixtures_parse() {
        for (fx, truth) in [structure_main(), structure_source()] {
            let doc = Document::load_mem(&fx.bytes).unwrap();
            assert_eq!(doc.get_pages().len(), truth.markers.len());
            for (i, page) in doc.page_iter().enumerate() {
                let content = doc.get_page_content(page);
                let marker = &truth.markers[i];
                assert!(
                    content
                        .windows(marker.len())
                        .any(|w| w == marker.as_bytes()),
                    "page {i} draws {marker}"
                );
            }
        }
        let doc = Document::load_mem(&signed_stub().bytes).unwrap();
        assert!(doc.catalog().unwrap().has(b"Perms"));
        let (fx, truth) = flatten_set();
        let doc = Document::load_mem(&fx.bytes).unwrap();
        for (i, page) in doc.page_iter().enumerate() {
            let annots = doc.get_page_annotations(page).unwrap();
            assert_eq!(annots.len(), truth.visible[i] + truth.stay[i]);
        }
    }

    #[test]
    fn main_fixture_inherits_on_pages_five_and_six() {
        let (fx, _) = structure_main();
        let doc = Document::load_mem(&fx.bytes).unwrap();
        let pages: Vec<_> = doc.page_iter().collect();
        for p in &pages[4..6] {
            let d = doc.get_dictionary(*p).unwrap();
            assert!(!d.has(b"MediaBox") && !d.has(b"Rotate") && !d.has(b"Resources"));
        }
    }
}
