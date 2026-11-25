//! Page operations on the structure fixtures: each operation's effect on pages, outlines,
//! links, named destinations, labels, fields, layers and Bluebeam columns, and the full-rewrite
//! save of the result.

mod structure_util;

use std::collections::BTreeSet;

use pl_pdf::{
    BlankPage, LinkAction, LinkDest, Object, OutlineTarget, PdfDocument, PdfError, PdfFile,
    ReplaceOptions, SaveKind, SaveOptions, SignaturePolicy,
};
use pl_testkit::{structure_main, structure_source, StructureTruth};
use structure_util::{content_streams, load, marker_in, markers, page_content, temp_dir};

fn main_doc() -> (PdfDocument, StructureTruth) {
    let (fx, truth) = structure_main();
    (load(&fx.bytes), truth)
}

fn source_doc() -> (PdfDocument, StructureTruth) {
    let (fx, truth) = structure_source();
    (load(&fx.bytes), truth)
}

fn marker_list(doc: &PdfDocument) -> Vec<String> {
    markers(doc)
        .into_iter()
        .map(|m| m.unwrap_or_else(|| "blank".into()))
        .collect()
}

fn titles(doc: &PdfDocument) -> Vec<String> {
    doc.outline()
        .flatten_with(doc)
        .into_iter()
        .map(|i| i.title)
        .collect()
}

/// Save `doc` as a new file and parse it back (the save verifies with lopdf and hayro-syntax).
fn round_trip(doc: PdfDocument, tag: &str) -> PdfDocument {
    let dir = temp_dir(tag);
    let path = dir.join("out.pdf");
    let pages = doc.page_count();
    let report = doc
        .save_new(&path, &SaveOptions::default(), SignaturePolicy::Refuse)
        .unwrap();
    assert_eq!(report.kind, SaveKind::Full);
    let v = report.verification.expect("verified");
    assert_eq!(v.page_count, pages);
    assert_eq!(v.second_parser, pl_pdf::SecondParser::Passed);
    let bytes = std::fs::read(&path).unwrap();
    let back = load(&bytes);
    // PDFium opens it with the same page count.
    let backend = structure_util::pdfium();
    let id = structure_util::open_bytes(&backend, bytes);
    assert_eq!(
        pl_render::RenderBackend::page_count(&backend, id).unwrap() as usize,
        pages
    );
    back
}

#[test]
fn fixture_reads_as_its_truth() {
    let (doc, truth) = main_doc();
    let m: Vec<String> = marker_list(&doc);
    assert_eq!(m, truth.markers);
    let labels: Vec<String> = doc
        .page_labels()
        .unwrap()
        .iter()
        .map(|l| l.text())
        .collect();
    assert_eq!(labels, truth.labels);
    let named: Vec<(String, usize)> = doc
        .named_destinations()
        .into_iter()
        .map(|n| (n.name_str(), n.dest.unwrap().page_index))
        .collect();
    assert_eq!(named, truth.named_dests);
    assert!(doc.signatures().signed_fields.is_empty());
    assert!(!doc.signatures().is_signed());
}

#[test]
fn inherited_attributes_are_materialised() {
    let (doc, _) = main_doc();
    let before = doc.pages().unwrap();
    let mut r = doc.restructure().unwrap();
    r.reorder(&[4, 5, 0, 1, 2, 3, 6, 7]).unwrap();
    let after = r.finish().unwrap();
    let pages = after.pages().unwrap();
    // The two pages that inherited from the intermediate node keep their geometry.
    for (new, old) in [(0, 4), (1, 5)] {
        assert_eq!(pages[new].media_box, before[old].media_box);
        assert_eq!(pages[new].rotate, before[old].rotate);
        assert_eq!(pages[new].geometry, before[old].geometry);
        let id = after.page_id(new).unwrap();
        let Some(Object::Dictionary(d)) = after.object(id) else {
            panic!()
        };
        for key in [b"MediaBox".as_slice(), b"Rotate", b"Resources"] {
            assert!(
                d.has(key),
                "page {new} owns {}",
                String::from_utf8_lossy(key)
            );
        }
    }
    let round = round_trip(after, "materialise");
    assert_eq!(round.pages().unwrap()[0].geometry, before[4].geometry);
    assert_eq!(marker_list(&round)[..2], ["PLM-A-05", "PLM-A-06"]);
}

#[test]
fn reorder_keeps_untouched_content_bytes_identical() {
    let (doc, _) = main_doc();
    let original: Vec<Vec<Vec<u8>>> = (0..doc.page_count())
        .map(|i| {
            content_streams(&doc, i)
                .into_iter()
                .map(|s| s.content)
                .collect()
        })
        .collect();
    let order = [7, 6, 5, 4, 3, 2, 1, 0];
    let mut r = doc.restructure().unwrap();
    let report = r.reorder(&order).unwrap();
    assert_eq!(report.pages_affected, (0..8).collect::<Vec<_>>());
    let back = round_trip(r.finish().unwrap(), "reorder");
    for (new, old) in order.iter().enumerate() {
        let now: Vec<Vec<u8>> = content_streams(&back, new)
            .into_iter()
            .map(|s| s.content)
            .collect();
        assert_eq!(now, original[*old], "page {new} (was {old}) content bytes");
    }
    // Outline items and links still lead to the same pages (now at new indices).
    let outline = back.outline().flatten_with(&back);
    let cover = outline.iter().find(|i| i.title == "Cover").unwrap();
    assert_eq!(cover.dest.unwrap().page_index, 7);
    let link = back.links(7).unwrap();
    let goto = link
        .iter()
        .find_map(|l| match &l.action {
            LinkAction::GoTo(d @ LinkDest::Page(_)) => back.resolve_link_dest(d),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        goto.page_index, 2,
        "page 0's link to page 5 now leads to index 2"
    );
    assert_eq!((goto.left, goto.top), (Some(72.0), Some(700.0)));
    // Labels moved with their pages.
    let labels: Vec<String> = back
        .page_labels()
        .unwrap()
        .iter()
        .map(|l| l.text())
        .collect();
    assert_eq!(
        labels,
        ["A-6", "A-5", "A-4", "A-3", "A-2", "A-1", "ii", "i"]
    );
}

#[test]
fn delete_fixes_outline_links_names_fields_and_open_action() {
    let (doc, _) = main_doc();
    let mut r = doc.restructure().unwrap();
    // Page 1: the open action, "Contents", one widget of "project".
    // Page 3: "sheet-3" (named) -> "Sheets" keeps its children and loses its destination; the
    // link on page 0 through "sheet-3" goes.
    let report = r.delete(&[1, 3]).unwrap();
    assert_eq!(report.pages_removed, vec![1, 3]);
    assert!(report.open_action_removed);
    assert_eq!(report.outline_items_removed, vec!["Contents".to_string()]);
    assert_eq!(report.outline_items_cleared, vec!["Sheets".to_string()]);
    assert_eq!(report.named_dests_removed, vec!["sheet-3".to_string()]);
    assert_eq!(report.links_removed.len(), 1);
    assert_eq!(report.links_removed[0].page_index, 0);
    assert_eq!(report.links_removed[0].reason, "target page deleted");
    assert!(
        report.fields_removed.is_empty(),
        "project still has its widget on page 6"
    );
    assert!(report.labels_rewritten);
    let after = r.finish().unwrap();
    assert_eq!(
        marker_list(&after),
        ["PLM-A-01", "PLM-A-03", "PLM-A-05", "PLM-A-06", "PLM-A-07", "PLM-A-08"]
    );
    let t = titles(&after);
    assert_eq!(
        t,
        [
            "Front",
            "Cover",
            "Sheets",
            "Sheet 5",
            "Sheet 6",
            "Plan \u{00DC}bersicht \u{03A9}",
            "Web"
        ]
    );
    let sheets = after.outline().items[1].clone();
    assert_eq!(sheets.target, OutlineTarget::None);
    assert!(!sheets.open && sheets.bold && sheets.color == Some([1.0, 0.0, 0.0]));
    // Every remaining GoTo link resolves.
    for p in 0..after.page_count() {
        for l in after.links(p).unwrap() {
            if let LinkAction::GoTo(d) = &l.action {
                assert!(
                    after.resolve_link_dest(d).is_some(),
                    "link on page {p} resolves"
                );
            }
        }
    }
    let labels: Vec<String> = after
        .page_labels()
        .unwrap()
        .iter()
        .map(|l| l.text())
        .collect();
    assert_eq!(labels, ["i", "A-1", "A-3", "A-4", "A-5", "A-6"]);
    let back = round_trip(after, "delete");
    assert!(!back.catalog().unwrap().has(b"OpenAction"));
    assert_eq!(back.named_destinations().len(), 2);
}

#[test]
fn deleting_every_widget_removes_the_field_and_structure_refs() {
    let (doc, _) = main_doc();
    let mut r = doc.restructure().unwrap();
    let report = r.delete(&[1, 4, 5, 6]).unwrap();
    assert_eq!(report.fields_removed, vec!["project".to_string()]);
    // The structure element's /Pg pointed at page 4.
    assert!(report.refs_dropped >= 1, "{report:?}");
    assert_eq!(report.named_dests_removed, vec!["sheet-5".to_string()]);
    // "Sheet 5" and "Sheet 6" lead to deleted pages; the UTF-16 item to page 6.
    for gone in [
        "Sheet 5",
        "Sheet 6",
        "Plan \u{00DC}bersicht \u{03A9}",
        "Contents",
    ] {
        assert!(
            report.outline_items_removed.contains(&gone.to_string()),
            "{gone}"
        );
    }
    let after = round_trip(r.finish().unwrap(), "fields");
    let form = after.catalog().unwrap().get(b"AcroForm").unwrap();
    let form = after.resolve(form).unwrap().as_dict().unwrap();
    let fields = after
        .resolve(form.get(b"Fields").unwrap())
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(fields.len(), 1, "only 'sheet' is left");
}

#[test]
fn insert_from_another_file_merges_fields_layers_columns_and_links() {
    let (doc, _) = main_doc();
    let (src, _) = source_doc();
    let mut r = doc.restructure().unwrap();
    // Only pages 1 and 2 of the source: page 1's named link leads to page 3, which is copied
    // too; page 2's link leads to page 4, which is not.
    let report = r.insert_from(2, &src, &[0, 1]).unwrap();
    assert_eq!(report.pages_affected, vec![2, 3]);
    assert_eq!(
        report.fields_renamed,
        vec![("project".to_string(), "project_2".to_string())]
    );
    assert_eq!(report.links_removed.len(), 2, "{:?}", report.links_removed);
    assert!(report
        .links_removed
        .iter()
        .all(|l| l.reason == "target page not copied"));
    let after = r.finish().unwrap();
    assert_eq!(after.page_count(), 10);
    assert_eq!(marker_list(&after)[2..4], ["PLM-B-01", "PLM-B-02"]);
    // Labels: the source has none, so its pages take their new page numbers.
    let labels: Vec<String> = after
        .page_labels()
        .unwrap()
        .iter()
        .map(|l| l.text())
        .collect();
    assert_eq!(labels[..5], ["i", "ii", "3", "4", "A-1"]);
    let back = round_trip(after, "insert-from");
    // Annotations of the copies point at their pages.
    for p in [2, 3] {
        let id = back.page_id(p).unwrap();
        for a in back.annotations(p).unwrap() {
            assert_eq!(a.dict.get(b"P").unwrap(), &Object::Reference(id));
        }
    }
}

#[test]
fn merge_brings_outline_named_links_layers_and_columns() {
    let (doc, _) = main_doc();
    let (src, src_truth) = source_doc();
    let (merged, report) = doc.merge(&[&src]).unwrap();
    assert_eq!(merged.page_count(), 12);
    assert_eq!(report.outline_items_added, 2);
    assert_eq!(
        report.fields_renamed,
        vec![("project".to_string(), "project_2".to_string())]
    );
    assert_eq!(report.layers_merged, 1);
    assert_eq!(report.bluebeam_columns_added, vec!["Status".to_string()]);
    assert!(
        report.named_dests_inlined >= 2,
        "the link and the outline item through b-three"
    );
    assert!(report.links_removed.is_empty());
    let back = round_trip(merged, "merge");
    // The appended outline leads into the copied pages.
    let flat = back.outline().flatten_with(&back);
    let b_three = flat.iter().find(|i| i.title == "B three").unwrap();
    assert_eq!(b_three.dest.unwrap().page_index, 8 + 2);
    // The source's links resolve to the right copied pages.
    for t in &src_truth.links {
        let links = back.links(8 + t.page_index).unwrap();
        let l = links
            .iter()
            .find(|l| (l.rect.x0, l.rect.y0) == (t.rect[0], t.rect[1]))
            .unwrap();
        let LinkAction::GoTo(d) = &l.action else {
            panic!("{:?}", l.action)
        };
        let r = back.resolve_link_dest(d).unwrap();
        assert_eq!(r.page_index, 8 + t.target_page.unwrap());
        assert_eq!(
            marker_in(&page_content(&back, r.page_index)).unwrap(),
            src_truth.markers[t.target_page.unwrap()]
        );
    }
    // Layers: both groups listed, the source's OFF by default as in its file.
    let oc = back
        .resolve(back.catalog().unwrap().get(b"OCProperties").unwrap())
        .unwrap()
        .as_dict()
        .unwrap();
    let ocgs = back
        .resolve(oc.get(b"OCGs").unwrap())
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(ocgs.len(), 2);
    let d = back
        .resolve(oc.get(b"D").unwrap())
        .unwrap()
        .as_dict()
        .unwrap();
    let off = back
        .resolve(d.get(b"OFF").unwrap())
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(off.len(), 1);
    assert_eq!(&ocgs[1], &off[0]);
    // Bluebeam columns: an indirect array of Discipline, Qty, Status; the copied markup's
    // values (Qty 5, Status Open) moved to their columns.
    let cols = back.catalog().unwrap().get(b"BSIAnnotColumns").unwrap();
    assert!(matches!(cols, Object::Reference(_)));
    assert_eq!(back.resolve(cols).unwrap().as_array().unwrap().len(), 3);
    let square = back
        .annotations(11)
        .unwrap()
        .into_iter()
        .find(|a| a.subtype() == Some(b"Square"))
        .unwrap();
    let values: Vec<String> = square
        .dict
        .get(b"BSIColumnData")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| pl_pdf::decode_text(o).unwrap())
        .collect();
    assert_eq!(values, ["", "5", "Open"]);
    // The reply chain came along: popup's /Parent and the reply's /IRT point at the copy.
    let annots = back.annotations(11).unwrap();
    let square_id = square.id.unwrap();
    let reply = annots
        .iter()
        .find(|a| a.subtype() == Some(b"Text"))
        .unwrap();
    assert_eq!(
        reply.dict.get(b"IRT").unwrap(),
        &Object::Reference(square_id)
    );
    let popup = annots
        .iter()
        .find(|a| a.subtype() == Some(b"Popup"))
        .unwrap();
    assert_eq!(
        popup.dict.get(b"Parent").unwrap(),
        &Object::Reference(square_id)
    );
    // The target's own private data was kept.
    let private = back
        .resolve(back.catalog().unwrap().get(b"PlumblineData").unwrap())
        .unwrap();
    let fixture = private.as_dict().unwrap().get(b"Fixture").unwrap();
    assert_eq!(pl_pdf::decode_text(fixture).unwrap(), "structure_main");
}

#[test]
fn replace_repoints_references_and_can_keep_markups() {
    let (doc, _) = main_doc();
    let (src, _) = source_doc();
    let mut r = doc.restructure().unwrap();
    let report = r
        .replace(3, &src, &[2], ReplaceOptions { keep_markups: true })
        .unwrap();
    assert_eq!(report.pages_affected, vec![3]);
    assert_eq!(report.pages_removed, vec![3]);
    assert_eq!(report.markups_kept, 3, "square, popup and reply");
    assert_eq!(report.markups_dropped, 0);
    let mut dropping = doc.restructure().unwrap();
    let dropped = dropping
        .replace(
            3,
            &src,
            &[2],
            ReplaceOptions {
                keep_markups: false,
            },
        )
        .unwrap();
    assert_eq!((dropped.markups_kept, dropped.markups_dropped), (0, 3));
    assert!(
        report.refs_repointed >= 2,
        "the name tree entry and the outline: {report:?}"
    );
    assert!(report.outline_items_removed.is_empty() && report.named_dests_removed.is_empty());
    let after = round_trip(r.finish().unwrap(), "replace");
    assert_eq!(marker_list(&after)[3], "PLM-B-03");
    assert_eq!(after.page_count(), 8);
    // "sheet-3" now names the replacement; the link through it still works.
    assert_eq!(
        after
            .resolve_named_destination(b"sheet-3")
            .unwrap()
            .page_index,
        3
    );
    let subtypes: BTreeSet<String> = after
        .annotations(3)
        .unwrap()
        .iter()
        .map(|a| String::from_utf8_lossy(a.subtype().unwrap()).into_owned())
        .collect();
    assert!(subtypes.contains("Square") && subtypes.contains("Popup") && subtypes.contains("Text"));
    let labels: Vec<String> = after
        .page_labels()
        .unwrap()
        .iter()
        .map(|l| l.text())
        .collect();
    assert_eq!(
        labels[3], "A-2",
        "the replacement keeps the replaced page's label"
    );
}

#[test]
fn rotate_changes_only_rotate() {
    let (doc, _) = main_doc();
    let before = doc.pages().unwrap();
    let annots_before = doc.annotations(3).unwrap();
    let mut r = doc.restructure().unwrap();
    r.rotate(&[0, 1, 3], 90).unwrap();
    r.rotate(&[3], -180).unwrap();
    assert!(r.rotate(&[0], 45).is_err());
    let after = round_trip(r.finish().unwrap(), "rotate");
    let pages = after.pages().unwrap();
    assert_eq!(pages[0].rotate.degrees(), 90);
    assert_eq!(pages[1].rotate.degrees(), 180);
    assert_eq!(pages[3].rotate.degrees(), 270);
    for i in [0, 1, 3] {
        assert_eq!(pages[i].media_box, before[i].media_box);
        assert_eq!(pages[i].crop_box, before[i].crop_box);
    }
    let rects = |a: &[pl_pdf::AnnotationEntry]| -> Vec<Object> {
        a.iter()
            .map(|e| e.dict.get(b"Rect").unwrap().clone())
            .collect()
    };
    assert_eq!(rects(&after.annotations(3).unwrap()), rects(&annots_before));
}

#[test]
fn blank_pages_extract_and_split() {
    let (doc, truth) = main_doc();
    let mut r = doc.restructure().unwrap();
    r.insert_blank(0, BlankPage::LikeNeighbour).unwrap();
    r.insert_blank(
        9,
        BlankPage::Size {
            media_box: pl_geom::Rect::new(0.0, 0.0, 842.0, 595.0),
            rotate: 90,
        },
    )
    .unwrap();
    let after = r.finish().unwrap();
    assert_eq!(after.page_count(), 10);
    let pages = after.pages().unwrap();
    assert_eq!(pages[0].media_box, pages[1].media_box);
    assert_eq!(pages[9].rotate.degrees(), 90);
    assert_eq!(pages[9].media_box.x1, 842.0);
    assert!(page_content(&after, 0)
        .iter()
        .all(|b| b.is_ascii_whitespace()));
    let labels: Vec<String> = after
        .page_labels()
        .unwrap()
        .iter()
        .map(|l| l.text())
        .collect();
    assert_eq!(labels[0], "1");
    assert_eq!(labels[1], "i");
    assert_eq!(labels[9], "10");

    let (part, report) = doc.extract_pages(&[5, 0]).unwrap();
    assert_eq!(marker_list(&part), ["PLM-A-06", "PLM-A-01"]);
    assert_eq!(report.pages_affected, vec![0, 1]);
    let flat = part.outline().flatten_with(&part);
    for item in &flat {
        if let Some(d) = item.dest {
            let want = truth
                .outline
                .iter()
                .find(|t| t.title == item.title)
                .unwrap()
                .page
                .unwrap();
            assert_eq!(
                marker_list(&part)[d.page_index],
                truth.markers[want],
                "{}",
                item.title
            );
        }
    }
    assert!(flat.iter().any(|i| i.title == "Cover"));
    assert!(!flat.iter().any(|i| i.title == "Contents"));
    let parts = doc.split(&[0..2, 2..8]).unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].0.page_count(), 2);
    assert_eq!(marker_list(&parts[1].0)[0], "PLM-A-03");
    let back = round_trip(parts[1].0.clone(), "split");
    assert_eq!(back.page_count(), 6);
    // Operations refuse bad input.
    let mut r = doc.restructure().unwrap();
    assert!(r.delete(&(0..8).collect::<Vec<_>>()).is_err());
    assert!(r.delete(&[1, 1]).is_err());
    assert!(matches!(
        r.delete(&[8]),
        Err(PdfError::PageOutOfRange { .. })
    ));
    assert!(r.reorder(&[0, 1]).is_err());
    assert!(r.move_pages(&[0], 8).is_err());
    assert!(doc.extract_pages(&[]).is_err());
}

#[test]
fn move_pages_puts_the_block_at_the_target() {
    let (doc, _) = main_doc();
    let mut r = doc.restructure().unwrap();
    r.move_pages(&[0, 2], 4).unwrap();
    let after = r.finish().unwrap();
    assert_eq!(
        marker_list(&after),
        [
            "PLM-A-02", "PLM-A-04", "PLM-A-05", "PLM-A-06", "PLM-A-01", "PLM-A-03", "PLM-A-07",
            "PLM-A-08"
        ]
    );
}

#[test]
fn restructured_save_in_place_replaces_the_file() {
    let (fx, _) = structure_main();
    let dir = temp_dir("in-place");
    let path = dir.join("main.pdf");
    std::fs::write(&path, &fx.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut r = doc.restructure().unwrap();
    r.delete(&[7]).unwrap();
    let report = file
        .save_restructured(
            r.finish().unwrap(),
            &SaveOptions::default(),
            SignaturePolicy::Refuse,
        )
        .unwrap();
    assert_eq!(report.kind, SaveKind::Full);
    assert_eq!(file.document().unwrap().page_count(), 7);
    let on_disk = load(&std::fs::read(&path).unwrap());
    assert_eq!(on_disk.page_count(), 7);
    // Unused objects are pruned: the deleted page is gone from the file.
    assert!(on_disk.object_count() < doc.object_count());
    assert!(!std::fs::read(&path)
        .unwrap()
        .windows(8)
        .any(|w| w == b"PLM-A-08"));
}

#[test]
fn a_failed_restructured_save_keeps_the_handle_and_the_file() {
    let (fx, _) = structure_main();
    let dir = temp_dir("failed");
    let path = dir.join("main.pdf");
    std::fs::write(&path, &fx.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let edits = file.edit().unwrap();
    let mut r = doc.restructure().unwrap();
    r.delete(&[0]).unwrap();
    let options = SaveOptions {
        fail_point: Some(pl_pdf::FailPoint::BeforeReplace),
        ..SaveOptions::default()
    };
    let err = file
        .save_restructured(r.finish().unwrap(), &options, SignaturePolicy::Refuse)
        .unwrap_err();
    assert!(matches!(err, PdfError::InjectedFailure(_)));
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);
    assert_eq!(file.document().unwrap().page_count(), 8);
    // An edit set from before is still valid against the handle.
    let mut edits = edits;
    let page = file.document().unwrap().page_id(0).unwrap();
    edits.patch(page).set("PlTest", Object::Integer(1));
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
}

fn layer_names(doc: &PdfDocument) -> Vec<String> {
    doc.layers().into_iter().map(|l| l.name).collect()
}

#[test]
fn duplicating_a_page_keeps_one_layer_per_name() {
    let (doc, truth) = main_doc();
    assert_eq!(layer_names(&doc), truth.layers);
    // Duplicate every page (the one drawing in "Grid A" among them) within the document.
    let mut r = doc.restructure().unwrap();
    let all: Vec<usize> = (0..doc.page_count()).collect();
    let report = r.insert_from(doc.page_count(), &doc, &all).unwrap();
    assert_eq!(
        report.layers_merged, 0,
        "the copies use the document's own groups"
    );
    let dup = round_trip(r.finish().unwrap(), "duplicate-layers");
    assert_eq!(layer_names(&dup), truth.layers);
    assert_eq!(dup.page_count(), 2 * doc.page_count());

    // Pages of the same file inserted twice share its group too, by name.
    let (src, src_truth) = source_doc();
    let mut r = doc.restructure().unwrap();
    let first = r.insert_document(r.page_count(), &src, false).unwrap();
    assert_eq!(first.layers_merged, 1);
    let once = r.finish().unwrap();
    let mut r = once.restructure().unwrap();
    let second = r.insert_document(r.page_count(), &src, false).unwrap();
    assert_eq!(second.layers_merged, 0);
    let twice = round_trip(r.finish().unwrap(), "insert-twice-layers");
    let mut want = truth.layers.clone();
    want.extend(src_truth.layers.clone());
    assert_eq!(layer_names(&twice), want);
}

#[test]
fn merged_layers_keep_the_targets_own_layers_listed() {
    let (fx, truth) = structure_main();
    // The target's /D has no /Order: every group of its own is listed.
    let mut raw = lopdf::Document::load_mem(&fx.bytes).unwrap();
    let oc = raw
        .catalog_mut()
        .unwrap()
        .get_mut(b"OCProperties")
        .unwrap()
        .as_dict_mut()
        .unwrap();
    oc.get_mut(b"D")
        .unwrap()
        .as_dict_mut()
        .unwrap()
        .remove(b"Order");
    let mut bytes = Vec::new();
    raw.save_to(&mut bytes).unwrap();
    let doc = load(&bytes);
    let (src, _) = source_doc();
    let (merged, report) = doc.merge(&[&src]).unwrap();
    assert_eq!(report.layers_merged, 1);
    let back = lopdf::Document::load_mem(&merged.to_bytes().unwrap()).unwrap();
    let oc = back
        .catalog()
        .unwrap()
        .get(b"OCProperties")
        .unwrap()
        .as_dict()
        .unwrap();
    let order = oc
        .get(b"D")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Order")
        .unwrap()
        .as_array()
        .unwrap();
    let ocgs = oc.get(b"OCGs").unwrap().as_array().unwrap();
    assert_eq!(order.len(), truth.layers.len() + 1);
    assert_eq!(order, ocgs, "the target's group first, then the new one");
}
