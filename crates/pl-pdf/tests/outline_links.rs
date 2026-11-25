//! Outlines (read, edit, write, both save paths) and links (read, resolve, create, edit,
//! delete), checked against the fixture truth and against PDFium.

mod structure_util;

use pl_pdf::{
    DestView, Destination, LinkAction, LinkDest, LinkSpec, Outline, OutlineItem, OutlineTarget,
    PdfFile, RemoteDest, SaveKind, SaveOptions, SignaturePolicy,
};
use pl_render::RenderBackend;
use pl_testkit::{structure_main, LinkKind};
use structure_util::{load, temp_dir};

#[test]
fn outline_reads_titles_targets_style_and_open_state() {
    let (fx, truth) = structure_main();
    let doc = load(&fx.bytes);
    let outline = doc.outline();
    assert_eq!(outline.len(), truth.outline.len());
    let flat = outline.flatten_with(&doc);
    for (item, t) in flat.iter().zip(&truth.outline) {
        assert_eq!(item.title, t.title);
        assert_eq!(item.level, t.level, "{}", t.title);
        assert_eq!(item.dest.map(|d| d.page_index), t.page, "{}", t.title);
        let model = outline.get(&item.path).unwrap();
        assert_eq!(model.color, t.color, "{}", t.title);
        assert_eq!(
            i64::from(model.italic) | (i64::from(model.bold) << 1),
            t.flags,
            "{}",
            t.title
        );
        assert_eq!(model.count_value().unwrap_or(0), t.count, "{}", t.title);
    }
    // The forms the file used are kept: /Dest array, GoTo action, name, URI action.
    assert!(matches!(
        outline.items[0].target,
        OutlineTarget::Page {
            as_action: false,
            ..
        }
    ));
    assert!(matches!(
        outline.items[0].children[1].target,
        OutlineTarget::Page {
            as_action: true,
            ..
        }
    ));
    assert!(
        matches!(&outline.items[1].target, OutlineTarget::Named { name, as_action: false } if name == b"sheet-3")
    );
    assert!(matches!(outline.items[3].target, OutlineTarget::Action(_)));
    assert_eq!(outline.visible_count(), 6);
}

#[test]
fn outline_edits_write_a_consistent_tree_on_both_save_paths() {
    let (fx, _) = structure_main();
    let doc = load(&fx.bytes);
    let mut o = doc.outline();
    // Rename, add, move/reparent, delete, set destination.
    o.rename(&[2], "Plan overview").unwrap();
    let mut new = OutlineItem::to_page("Added", Destination::new(7, DestView::at(10.0, 20.0)));
    new.color = Some([0.0, 0.0, 1.0]);
    new.italic = true;
    o.insert(&[], 0, new).unwrap();
    o.move_item(&[3], &[1], 1).unwrap(); // "Plan overview" under "Front", second
    o.remove(&[2, 1]).unwrap(); // "Sheet 6"
    o.set_destination(
        &[2],
        Destination::new(4, DestView::FitH { top: Some(500.0) }),
    )
    .unwrap();
    o.items[1].open = false;
    let expected: Vec<(String, usize, Option<usize>)> = vec![
        ("Added".into(), 0, Some(7)),
        ("Front".into(), 0, Some(0)),
        ("Cover".into(), 1, Some(0)),
        ("Plan overview".into(), 1, Some(6)),
        ("Contents".into(), 1, Some(1)),
        ("Sheets".into(), 0, Some(4)),
        ("Sheet 5".into(), 1, Some(4)),
        ("Web".into(), 0, None),
    ];
    let check = |doc: &pl_pdf::PdfDocument| {
        let back = doc.outline();
        let flat: Vec<(String, usize, Option<usize>)> = back
            .flatten_with(doc)
            .into_iter()
            .map(|i| (i.title, i.level, i.dest.map(|d| d.page_index)))
            .collect();
        assert_eq!(flat, expected);
        assert_eq!(back.items[0].color, Some([0.0, 0.0, 1.0]));
        assert!(back.items[0].italic);
        assert!(!back.items[1].open);
        // Root /Count: the 4 top-level items only (Front and Sheets are closed).
        let root = doc
            .resolve(doc.catalog().unwrap().get(b"Outlines").unwrap())
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(root.get(b"Count").unwrap().as_i64().unwrap(), 4);
        // Every item's /Parent, /Prev and /Next agree with the tree.
        let mut prev = None;
        let mut next = root.get(b"First").unwrap().as_reference().ok();
        let mut seen = 0;
        while let Some(id) = next {
            let d = doc.object(id).unwrap().as_dict().unwrap();
            assert_eq!(
                d.get(b"Parent").unwrap().as_reference().unwrap(),
                root_id(doc)
            );
            assert_eq!(
                d.get(b"Prev").ok().and_then(|o| o.as_reference().ok()),
                prev
            );
            prev = Some(id);
            next = d.get(b"Next").ok().and_then(|o| o.as_reference().ok());
            seen += 1;
        }
        assert_eq!(seen, 4);
        assert_eq!(root.get(b"Last").unwrap().as_reference().ok(), prev);
        let sheets = back.items[2].clone();
        assert_eq!(sheets.count_value(), Some(-1));
        let OutlineTarget::Page { dest, .. } = sheets.target else {
            panic!()
        };
        assert_eq!(dest.view, DestView::FitH { top: Some(500.0) });
    };
    fn root_id(doc: &pl_pdf::PdfDocument) -> pl_pdf::ObjectId {
        doc.catalog()
            .unwrap()
            .get(b"Outlines")
            .unwrap()
            .as_reference()
            .unwrap()
    }

    // Full rewrite.
    let dir = temp_dir("outline");
    let full = dir.join("full.pdf");
    let rewritten = doc.with_outline(&o).unwrap();
    check(&rewritten);
    rewritten
        .save_new(&full, &SaveOptions::default(), SignaturePolicy::Refuse)
        .unwrap();
    let full_bytes = std::fs::read(&full).unwrap();
    check(&load(&full_bytes));

    // Incremental: the original bytes stay a prefix.
    let inc = dir.join("inc.pdf");
    std::fs::write(&inc, &fx.bytes).unwrap();
    let mut file = PdfFile::open(&inc).unwrap();
    let mut edits = file.edit().unwrap();
    file.document()
        .unwrap()
        .outline_edits(&mut edits, &o)
        .unwrap();
    let report = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    assert_eq!(report.kind, SaveKind::Incremental);
    let inc_bytes = std::fs::read(&inc).unwrap();
    assert!(inc_bytes.starts_with(&fx.bytes));
    check(&load(&inc_bytes));

    // PDFium reads the same tree.
    let backend = structure_util::pdfium();
    for bytes in [full_bytes, inc_bytes] {
        let id = structure_util::open_bytes(&backend, bytes);
        let items = backend.outline(id).unwrap();
        let got: Vec<(String, usize, Option<usize>)> = items
            .iter()
            .map(|i| {
                (
                    i.title.clone(),
                    i.level as usize,
                    i.page.map(|p| p as usize),
                )
            })
            .collect();
        assert_eq!(got, expected);
        let added = &items[0];
        assert_eq!((added.x, added.y), (Some(10.0), Some(20.0)));
    }

    // An empty outline removes /Outlines.
    let none = doc.with_outline(&Outline::default()).unwrap();
    assert!(!none.catalog().unwrap().has(b"Outlines"));
    assert!(none.outline().is_empty());
}

#[test]
fn links_read_and_resolve_as_the_truth_says() {
    let (fx, truth) = structure_main();
    let doc = load(&fx.bytes);
    for t in &truth.links {
        let links = doc.links(t.page_index).unwrap();
        let l = links
            .iter()
            .find(|l| {
                (l.rect.x0, l.rect.y0, l.rect.x1, l.rect.y1)
                    == (t.rect[0], t.rect[1], t.rect[2], t.rect[3])
            })
            .unwrap_or_else(|| panic!("{t:?}"));
        match (t.kind, &l.action) {
            (LinkKind::GoTo, LinkAction::GoTo(d @ LinkDest::Page(_)))
            | (LinkKind::GoToNamed, LinkAction::GoTo(d @ LinkDest::Named(_))) => {
                let r = doc.resolve_link_dest(d).unwrap();
                assert_eq!(Some(r.page_index), t.target_page, "{t:?}");
                if let Some([x, y]) = t.target_point {
                    assert_eq!((r.left, r.top), (Some(x), Some(y)), "{t:?}");
                }
                if let LinkDest::Named(n) = d {
                    assert_eq!(String::from_utf8_lossy(n), t.value);
                }
            }
            (LinkKind::Uri, LinkAction::Uri(u)) => assert_eq!(u, &t.value),
            (LinkKind::GoToR, LinkAction::GoToR { file, dest, .. }) => {
                assert_eq!(file, &t.value);
                assert!(matches!(
                    dest,
                    Some(RemoteDest::Page {
                        page_index: 2,
                        view: DestView::Fit
                    })
                ));
                assert!(
                    doc.resolve_link_action(&l.action, t.page_index).is_none(),
                    "never followed"
                );
            }
            (LinkKind::Launch, LinkAction::Launch { file, .. }) => {
                assert_eq!(file, &t.value);
                assert!(
                    doc.resolve_link_action(&l.action, t.page_index).is_none(),
                    "never run"
                );
            }
            (LinkKind::Named, LinkAction::Named(n)) => {
                assert_eq!(n, &t.value);
                let r = doc.resolve_link_action(&l.action, t.page_index).unwrap();
                assert_eq!(Some(r.page_index), t.target_page);
            }
            (kind, action) => panic!("{kind:?} read as {action:?}"),
        }
    }
}

#[test]
fn links_are_created_edited_and_deleted_incrementally() {
    let (fx, _) = structure_main();
    let dir = temp_dir("links");
    let path = dir.join("links.pdf");
    std::fs::write(&path, &fx.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let rect = pl_geom::Rect::new(300.0, 100.0, 400.0, 130.0);
    let specs = [
        LinkSpec::new(
            rect,
            LinkAction::GoTo(LinkDest::Page(Destination::new(
                6,
                DestView::at(50.0, 60.0),
            ))),
        ),
        LinkSpec::new(rect, LinkAction::GoTo(LinkDest::Named(b"last".to_vec()))),
        LinkSpec::new(rect, LinkAction::Uri("https://example.org/a?b=c".into())),
        LinkSpec::new(
            rect,
            LinkAction::GoToR {
                file: "Sheets\\E-201.pdf".into(),
                dest: Some(RemoteDest::Named(b"E-201".to_vec())),
                new_window: Some(true),
            },
        ),
        LinkSpec::new(
            rect,
            LinkAction::Launch {
                file: "report.xlsx".into(),
                new_window: None,
            },
        ),
        LinkSpec::new(rect, LinkAction::Named("LastPage".into())),
    ];
    let mut edits = file.edit().unwrap();
    let mut ids = Vec::new();
    for (k, spec) in specs.iter().enumerate() {
        let mut spec = spec.clone();
        spec.rect = pl_geom::Rect::new(
            rect.x0,
            rect.y0 + 40.0 * k as f64,
            rect.x1,
            rect.y1 + 40.0 * k as f64,
        );
        ids.push(doc.add_link(&mut edits, 7, &spec).unwrap());
    }
    // A link to a page that does not exist is refused.
    let bad = LinkSpec::new(
        rect,
        LinkAction::GoTo(LinkDest::Page(Destination::new(99, DestView::Fit))),
    );
    assert!(doc.link_dictionary(0, &bad).is_err());
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let doc = file.document().unwrap().clone();
    let links = doc.links(7).unwrap();
    assert_eq!(links.len(), 1 + specs.len());
    for (spec, l) in specs.iter().zip(&links[1..]) {
        assert_eq!(l.action, spec.action);
        assert_eq!(l.border_width, 0.0);
    }
    assert_eq!(
        doc.resolve_link_dest(&LinkDest::Named(b"last".to_vec()))
            .unwrap()
            .page_index,
        7
    );
    let last = doc.resolve_link_action(&links[6].action, 7).unwrap();
    assert_eq!(last.page_index, 7);
    let backend = structure_util::pdfium();
    let id = structure_util::open_bytes(&backend, std::fs::read(&path).unwrap());
    let annots = backend.annotations(id, 7).unwrap();
    assert_eq!(annots.len(), 1 + specs.len());

    // Edit one (new rectangle, now a URI) and delete another.
    let mut edits = file.edit().unwrap();
    let mut spec = LinkSpec::new(
        pl_geom::Rect::new(10.0, 10.0, 60.0, 30.0),
        LinkAction::Uri("https://example.net".into()),
    );
    spec.border_width = 1.0;
    spec.color = Some([0.0, 0.0, 1.0]);
    edits.modify_annotation(ids[0], doc.link_patch(&spec).unwrap());
    edits.delete_annotation(7, ids[5]);
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let doc = file.document().unwrap().clone();
    let links = doc.links(7).unwrap();
    assert_eq!(links.len(), specs.len());
    let edited = links.iter().find(|l| l.id == Some(ids[0])).unwrap();
    assert_eq!(edited.action, LinkAction::Uri("https://example.net".into()));
    assert_eq!((edited.rect.x0, edited.rect.y1), (10.0, 30.0));
    assert_eq!(edited.border_width, 1.0);
    assert_eq!(edited.color, Some([0.0, 0.0, 1.0]));
    let raw = doc.object(ids[0]).unwrap().as_dict().unwrap();
    assert!(!raw.has(b"Dest"), "the old destination is gone");
    assert!(!links.iter().any(|l| l.id == Some(ids[5])));
}

/// The `/Dest` (or GoTo `/D`) of the outline item titled `title` in `bytes`.
fn outline_dest_of(bytes: &[u8], title: &str) -> lopdf::Object {
    let raw = lopdf::Document::load_mem(bytes).unwrap();
    for object in raw.objects.values() {
        let Ok(d) = object.as_dict() else { continue };
        let Ok(t) = d.get(b"Title") else { continue };
        if lopdf::decode_text_string(t).ok().as_deref() != Some(title) {
            continue;
        }
        if let Ok(dest) = d.get(b"Dest") {
            return dest.clone();
        }
        return d
            .get(b"A")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"D")
            .unwrap()
            .clone();
    }
    panic!("no outline item {title:?}");
}

#[test]
fn named_destinations_keep_the_form_their_lookup_needs() {
    let (fx, _) = structure_main();
    let doc = load(&fx.bytes);
    let mut o = doc.outline();
    // "last" is only in the catalog's /Dests dictionary, "sheet-3" in the name tree.
    let named = |title: &str, name: &[u8], as_action| {
        OutlineItem::new(
            title,
            OutlineTarget::Named {
                name: name.to_vec(),
                as_action,
            },
        )
    };
    o.insert(&[], 0, named("To last", b"last", false)).unwrap();
    o.insert(&[], 0, named("To last (action)", b"last", true))
        .unwrap();
    o.insert(&[], 0, named("To sheet 3", b"sheet-3", false))
        .unwrap();
    let bytes = doc.with_outline(&o).unwrap().to_bytes().unwrap();
    assert_eq!(
        outline_dest_of(&bytes, "To last"),
        lopdf::Object::Name(b"last".to_vec())
    );
    assert_eq!(
        outline_dest_of(&bytes, "To last (action)"),
        lopdf::Object::Name(b"last".to_vec())
    );
    assert!(matches!(
        outline_dest_of(&bytes, "To sheet 3"),
        lopdf::Object::String(s, _) if s == b"sheet-3"
    ));
    let back = load(&bytes);
    let flat = back.outline().flatten_with(&back);
    let to_last = flat.iter().find(|i| i.title == "To last").unwrap();
    assert_eq!(to_last.dest.unwrap().page_index, 7);

    // A link to it is written the same way.
    let rect = pl_geom::Rect::new(300.0, 100.0, 400.0, 130.0);
    let mut edits = doc.edit();
    doc.add_link(
        &mut edits,
        0,
        &LinkSpec::new(rect, LinkAction::GoTo(LinkDest::Named(b"last".to_vec()))),
    )
    .unwrap();
    let bytes = doc.with_edits(&edits).unwrap().to_bytes().unwrap();
    let raw = lopdf::Document::load_mem(&bytes).unwrap();
    let page = raw.get_pages()[&1];
    let annots = raw
        .get_dictionary(page)
        .unwrap()
        .get(b"Annots")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    let found = annots.iter().any(|a| {
        let a = raw.dereference(a).unwrap().1.as_dict().unwrap();
        a.get(b"A")
            .ok()
            .and_then(|o| raw.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok())
            .and_then(|d| d.get(b"D").ok())
            == Some(&lopdf::Object::Name(b"last".to_vec()))
    });
    assert!(found, "the link's /D is the name object /last");
}
