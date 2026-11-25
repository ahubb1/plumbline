//! Structural commands through [`Document`] against real PDFium and the pl-testkit structure
//! fixtures (milestone M8): page operations against a reference list model by page
//! fingerprint, on plain, rotated, UserUnit and CropBox pages; exact undo and redo; markups
//! that travel with their pages; the full-rewrite save; signatures; bookmarks and links that
//! resolve; flatten leaving the render unchanged; and journal replay after a structural step.
//! Headless: nothing opens a window, and every file lives in a temporary directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{AnnotId, Annotation, Origin, PdfDate};
use pl_doc::pl_geom::{Rect, Rotation};
use pl_doc::pl_pdf::{DictPatch, PdfDocument, PdfFile, SaveKind, SaveOptions, SignaturePolicy};
use pl_doc::pl_render::{Bitmap, PdfiumBackend};
use pl_doc::{
    BookmarkSpec, BookmarkTarget, Command, DestSpec, Document, DocumentId, FlattenScope, LinkData,
    LinkTarget, PageFingerprint, PageSource, Session, SessionOptions, StructCommand, StructReport,
};
use pl_testkit::{structure_main, structure_marker, structure_source, Fixture};

const KEEP: SignaturePolicy = SignaturePolicy::Refuse;

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

fn backend() -> Arc<PdfiumBackend> {
    Arc::new(PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll"))
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn session(backend: &Arc<PdfiumBackend>, journal: Option<&Path>) -> Session {
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    s.set_journal_dir(journal.map(Path::to_path_buf));
    s
}

fn origin() -> Origin {
    Origin::new(
        "Structure Tester",
        PdfDate::parse("D:20261124120000Z").unwrap(),
    )
}

fn open(s: &mut Session, path: &Path) -> DocumentId {
    let id = s.open(path, None).unwrap();
    s.document_mut(id).unwrap().load_annots().unwrap();
    id
}

/// What a page is in the reference model: its content, boxes, rotation and unit, and the
/// marker it draws (`None` for a blank page).
#[derive(Clone, Debug, PartialEq)]
struct Page {
    print: PageFingerprint,
    marker: Option<String>,
}

fn model_of(doc: &mut Document, markers: &[String]) -> Vec<Page> {
    doc.page_fingerprints()
        .unwrap()
        .into_iter()
        .zip(markers)
        .map(|(print, m)| Page {
            print,
            marker: Some(m.clone()),
        })
        .collect()
}

fn turned(r: Rotation, by: i32) -> Rotation {
    Rotation::from_degrees((r.degrees() + by).rem_euclid(360)).unwrap()
}

/// The document's pages agree with the model: by fingerprint (lopdf side), by geometry and by
/// the marker text PDFium extracts (display side).
fn check(doc: &mut Document, model: &[Page], what: &str) {
    let prints = doc.page_fingerprints().unwrap();
    assert_eq!(prints.len(), model.len(), "{what}: page count");
    assert_eq!(
        doc.page_count() as usize,
        model.len(),
        "{what}: display page count"
    );
    for (i, (got, want)) in prints.iter().zip(model).enumerate() {
        if want.marker.is_some() {
            assert_eq!(*got, want.print, "{what}: page {i} fingerprint");
        } else {
            // A blank page: its boxes are known, its content is not drawn.
            assert_eq!(
                got.media_box, want.print.media_box,
                "{what}: blank page {i}"
            );
            assert_eq!(got.rotate, want.print.rotate, "{what}: blank page {i}");
        }
        let geometry = *doc.page_geometry(i as u32).unwrap();
        assert_eq!(
            geometry.rotate(),
            want.print.rotate,
            "{what}: page {i} display rotation"
        );
        assert_eq!(
            geometry.user_unit(),
            want.print.user_unit,
            "{what}: page {i} unit"
        );
        let text = doc.page_text(i as u32).unwrap().text();
        match &want.marker {
            Some(m) => assert!(text.contains(m.as_str()), "{what}: page {i} shows {m}"),
            None => assert!(!text.contains("PLM-"), "{what}: page {i} is blank"),
        }
    }
}

/// Everything an undo has to give back: pages, annotations with their ties to the file, the
/// outline.
#[derive(Debug, PartialEq)]
struct State {
    prints: Vec<PageFingerprint>,
    annots: Vec<Annotation>,
    outline: Vec<(Vec<usize>, String, Option<usize>)>,
    pages: u32,
}

fn state(doc: &mut Document) -> State {
    State {
        prints: doc.page_fingerprints().unwrap(),
        annots: doc.annots().unwrap().set().iter().cloned().collect(),
        outline: doc
            .outline_rows()
            .unwrap()
            .into_iter()
            .map(|r| (r.path, r.title, r.dest.map(|d| d.page_index)))
            .collect(),
        pages: doc.page_count(),
    }
}

fn apply(doc: &mut Document, command: StructCommand) -> pl_doc::StructureOutcome {
    doc.apply_structure(command.clone(), KEEP)
        .unwrap_or_else(|e| panic!("{command:?}: {e}"))
}

#[test]
fn page_operations_match_the_reference_list_model_and_undo_exactly() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, truth) = structure_main();
    let (source, source_truth) = structure_source();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let source_path = write(dir.path(), "source.pdf", &source.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let source_id = s.open(&source_path, None).unwrap();
    let source_model = model_of(s.document_mut(source_id).unwrap(), &source_truth.markers);
    let doc = s.document_mut(id).unwrap();

    let mut model = model_of(doc, &truth.markers);
    // The fixture really mixes the geometries the convention is about.
    assert_eq!(model[1].print.rotate, Rotation::R90);
    assert_eq!(model[2].print.user_unit, 2.0);
    assert!(model[3].print.crop_box.is_some());
    check(doc, &model, "start");
    let mut history = vec![state(doc)];
    let before_render = doc.render_id();

    // Rotate the rotated, the UserUnit and the CropBox pages.
    apply(
        doc,
        StructCommand::RotatePages {
            pages: vec![1, 2, 3],
            degrees: 90,
        },
    );
    for p in [1, 2, 3] {
        model[p].print.rotate = turned(model[p].print.rotate, 90);
    }
    check(doc, &model, "rotate");
    assert_ne!(
        doc.render_id(),
        before_render,
        "the mirror shows the working copy"
    );
    let change = doc.take_structure_change().unwrap();
    assert!(change.page_map.is_identity());
    assert_eq!(change.old_render, before_render);
    history.push(state(doc));

    // Delete two pages.
    let out = apply(doc, StructCommand::DeletePages { pages: vec![0, 5] });
    assert!(matches!(out.report, StructReport::Pages(_)));
    assert_eq!(out.page_map.map(0), None);
    assert_eq!(out.page_map.map(1), Some(0));
    assert_eq!(out.page_map.map(6), Some(4));
    model.remove(5);
    model.remove(0);
    check(doc, &model, "delete");
    history.push(state(doc));

    // Move the last page to the front, then two pages to the end.
    apply(
        doc,
        StructCommand::MovePages {
            pages: vec![5],
            to: 0,
        },
    );
    let last = model.pop().unwrap();
    model.insert(0, last);
    check(doc, &model, "move");
    history.push(state(doc));
    apply(
        doc,
        StructCommand::MovePages {
            pages: vec![1, 3],
            to: 4,
        },
    );
    let moving = vec![model[1].clone(), model[3].clone()];
    model.remove(3);
    model.remove(1);
    model.splice(4..4, moving);
    check(doc, &model, "move two");
    history.push(state(doc));

    // Reorder: reverse.
    let n = model.len() as u32;
    apply(
        doc,
        StructCommand::ReorderPages {
            order: (0..n).rev().collect(),
        },
    );
    model.reverse();
    check(doc, &model, "reverse");
    history.push(state(doc));

    // Insert pages from another file, a duplicate of one of ours, and a blank page.
    apply(
        doc,
        StructCommand::InsertPages {
            at: 2,
            source: PageSource::File(source_path.clone()),
            pages: vec![3, 0],
        },
    );
    model.insert(2, source_model[0].clone());
    model.insert(2, source_model[3].clone());
    check(doc, &model, "insert from file");
    history.push(state(doc));
    apply(
        doc,
        StructCommand::InsertPages {
            at: 0,
            source: PageSource::This,
            pages: vec![4],
        },
    );
    model.insert(0, model[4].clone());
    check(doc, &model, "duplicate");
    history.push(state(doc));
    apply(
        doc,
        StructCommand::InsertBlank {
            at: 1,
            count: 1,
            media_box: Some([0.0, 0.0, 400.0, 300.0]),
            rotate: 270,
        },
    );
    let mut blank = model[0].clone();
    blank.marker = None;
    blank.print.media_box = Rect::new(0.0, 0.0, 400.0, 300.0);
    blank.print.crop_box = None;
    blank.print.rotate = Rotation::R270;
    blank.print.user_unit = 1.0;
    model.insert(1, blank);
    check(doc, &model, "blank");
    history.push(state(doc));

    // Replace one page with a source page.
    apply(
        doc,
        StructCommand::ReplacePages {
            at: 3,
            source: PageSource::File(source_path.clone()),
            pages: vec![1],
            keep_markups: false,
        },
    );
    model[3] = source_model[1].clone();
    check(doc, &model, "replace");
    history.push(state(doc));
    let end_model = model.clone();
    assert!(doc.is_modified());
    assert!(doc.has_structure_changes());

    // Undo every step: each state comes back exactly; then redo every step.
    for k in (0..history.len() - 1).rev() {
        assert!(doc.next_undo_is_structural());
        assert!(doc.undo().unwrap());
        assert_eq!(state(doc), history[k], "undo to step {k}");
    }
    assert!(!doc.can_undo());
    assert!(!doc.is_modified(), "back at the file");
    assert!(!doc.has_structure_changes());
    let undone = model_of(doc, &truth.markers);
    check(doc, &undone, "undone");
    for (k, want) in history.iter().enumerate().skip(1) {
        assert!(doc.redo().unwrap());
        assert_eq!(state(doc), *want, "redo to step {k}");
    }
    check(doc, &end_model, "redone");
    // The file was never touched.
    assert_eq!(std::fs::read(&path).unwrap(), main.bytes);
}

/// The same operations on each single-geometry fixture: the pages keep their geometry and
/// undo gives back the start.
#[test]
fn plain_rotated_user_unit_and_crop_box_fixtures_keep_their_geometry() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    for fixture in [
        pl_testkit::plain(),
        pl_testkit::rotated_pages(),
        pl_testkit::user_unit(),
        pl_testkit::crop_box(),
    ] {
        let name = fixture.manifest.file_name();
        let path = write(dir.path(), &name, &fixture.bytes);
        let mut s = session(&backend, None);
        let id = open(&mut s, &path);
        let doc = s.document_mut(id).unwrap();
        let start = state(doc);
        let geometries: Vec<_> = (0..doc.page_count())
            .map(|p| *doc.page_geometry(p).unwrap())
            .collect();
        let n = doc.page_count();
        // Duplicate every page at the end, rotate the copies, then move the copies first.
        apply(
            doc,
            StructCommand::InsertPages {
                at: n,
                source: PageSource::This,
                pages: (0..n).collect(),
            },
        );
        apply(
            doc,
            StructCommand::RotatePages {
                pages: (n..2 * n).collect(),
                degrees: 180,
            },
        );
        apply(
            doc,
            StructCommand::MovePages {
                pages: (n..2 * n).collect(),
                to: 0,
            },
        );
        for p in 0..n {
            let copy = *doc.page_geometry(p).unwrap();
            let orig = *doc.page_geometry(n + p).unwrap();
            assert_eq!(orig, geometries[p as usize], "{name}: page {p} unchanged");
            assert_eq!(copy.media_box(), orig.media_box(), "{name}: copy {p} boxes");
            assert_eq!(copy.crop_box(), orig.crop_box(), "{name}: copy {p} crop");
            assert_eq!(copy.user_unit(), orig.user_unit(), "{name}: copy {p} unit");
            assert_eq!(
                copy.rotate(),
                turned(orig.rotate(), 180),
                "{name}: copy {p} rotation"
            );
        }
        while doc.can_undo() {
            doc.undo().unwrap();
        }
        assert_eq!(state(doc), start, "{name}: undone");
        assert_eq!(
            (0..doc.page_count())
                .map(|p| *doc.page_geometry(p).unwrap())
                .collect::<Vec<_>>(),
            geometries,
            "{name}: geometry back"
        );
    }
}

#[test]
fn unsaved_markups_are_committed_and_travel_with_their_page() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let base_count = doc.annots().unwrap().set().len();

    // A markup on the CropBox page (3), unsaved; and a scale on that page.
    let rect = Rect::new(200.0, 300.0, 260.0, 340.0);
    let square = Annotation::square(3, rect, &origin());
    let square_id = square.id.clone();
    doc.apply(Command::add(square)).unwrap();
    doc.select([square_id.clone()]).unwrap();
    let with_markup = state(doc);
    assert!(doc.overlay(3).iter().any(|o| o.id == square_id));

    let out = apply(
        doc,
        StructCommand::MovePages {
            pages: vec![3],
            to: 0,
        },
    );
    assert!(out.committed_markups);
    let a = doc
        .annotation(&square_id)
        .expect("the markup came along")
        .clone();
    assert_eq!(a.page, 0, "it moved with its page");
    assert_eq!(a.geometry.bounds(), rect, "page coordinates are unchanged");
    assert!(a.baseline().is_some(), "it is part of the structure now");
    assert!(doc.is_selected(&square_id), "the selection is kept");
    assert_eq!(doc.annots().unwrap().set().len(), base_count + 1);
    // PDFium draws it from the working copy now: it is no longer the overlay's.
    assert!(doc.overlay(0).iter().all(|o| o.id != square_id));
    assert!(doc
        .annotations(0)
        .unwrap()
        .iter()
        .any(|i| i.name.as_deref() == Some(square_id.as_str())));

    // An annotation command on the new structure, then undo it, the move, and the add.
    let mut moved = a.clone();
    moved.translate(pl_doc::pl_geom::Vec2::new(10.0, 0.0));
    doc.apply(Command::modify(moved)).unwrap();
    assert!(!doc.next_undo_is_structural());
    doc.undo().unwrap();
    assert!(doc.next_undo_is_structural());
    assert_eq!(doc.structure_undo_name(), Some("move pages"));
    doc.undo().unwrap();
    assert_eq!(
        state(doc),
        with_markup,
        "the move and its commit are undone"
    );
    let back = doc.annotation(&square_id).unwrap();
    assert_eq!(back.page, 3);
    assert!(back.baseline().is_none(), "unsaved again");
    assert!(doc.overlay(3).iter().any(|o| o.id == square_id));
    doc.undo().unwrap();
    assert!(doc.annotation(&square_id).is_none());
    assert!(!doc.is_modified());
    // Redo both: the move comes back with the markup on page 0.
    doc.redo().unwrap();
    doc.redo().unwrap();
    assert_eq!(doc.annotation(&square_id).unwrap().page, 0);
    // A new markup change drops what could be redone.
    doc.undo().unwrap();
    assert!(doc.can_redo());
    let other = Annotation::square(1, Rect::new(10.0, 10.0, 40.0, 40.0), &origin());
    doc.apply(Command::add(other)).unwrap();
    assert!(
        !doc.can_redo(),
        "the undone move cannot be redone after a new change"
    );
}

#[test]
fn a_structural_save_is_a_verified_full_rewrite_that_reopens_the_same() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, truth) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    apply(doc, StructCommand::DeletePages { pages: vec![7] });
    apply(
        doc,
        StructCommand::RotatePages {
            pages: vec![0],
            degrees: -90,
        },
    );
    let square = Annotation::square(2, Rect::new(20.0, 20.0, 80.0, 60.0), &origin());
    let square_id = square.id.clone();
    doc.apply(Command::add(square)).unwrap();
    apply(
        doc,
        StructCommand::AddBookmark {
            parent: vec![],
            index: 0,
            bookmark: BookmarkSpec::to_page("Start here", 2),
        },
    );
    let want = state(doc);
    let want_model = doc.annots().unwrap().model();

    // A plain incremental save of a caller's edit set is refused while page edits wait.
    let raw = PdfFile::open(&path).unwrap().edit().unwrap();
    assert!(matches!(
        doc.save_incremental(&raw, &[], &SaveOptions::default()),
        Err(pl_doc::DocError::UnsavedStructure)
    ));

    let saved = doc.save().unwrap();
    let outcome = saved.outcome.expect("written");
    assert_eq!(outcome.report.kind, SaveKind::Full);
    assert!(outcome.report.verification.is_some());
    assert!(!doc.is_modified());
    assert!(!doc.has_structure_changes());
    assert!(!doc.can_undo(), "structural steps are undoable until saved");
    assert_eq!(state(doc).prints, want.prints);
    assert_eq!(doc.annots().unwrap().model(), want_model);
    assert!(doc.annotation(&square_id).unwrap().baseline().is_some());
    let mut model: Vec<String> = truth.markers.clone();
    model.pop();
    for (i, m) in model.iter().enumerate() {
        assert!(doc.page_text(i as u32).unwrap().text().contains(m.as_str()));
    }

    // A fresh session reads the file as the document was.
    drop(s);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert_eq!(state(doc), want);
    assert_eq!(doc.annots().unwrap().model(), want_model);
    assert_eq!(doc.outline_rows().unwrap()[0].title, "Start here");
}

#[test]
fn save_as_writes_the_working_copy_elsewhere_and_leaves_the_file() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let other = dir.path().join("copy.pdf");
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    apply(
        doc,
        StructCommand::ReorderPages {
            order: vec![7, 6, 5, 4, 3, 2, 1, 0],
        },
    );
    let want = doc.page_fingerprints().unwrap();
    let saved = s.save_as(id, &other).unwrap();
    assert_eq!(saved.path, std::path::absolute(&other).unwrap());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        main.bytes,
        "the original is untouched"
    );
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.path(), Some(saved.path.as_path()));
    assert!(!doc.is_modified());
    let reread = PdfDocument::from_bytes(std::fs::read(&other).unwrap(), None).unwrap();
    assert_eq!(pl_doc::structure::fingerprints(&reread).unwrap(), want);

    // Extract writes a new file from the current state without changing the document.
    let part = dir.path().join("part.pdf");
    s.extract_pages(id, &[0, 7], &part, KEEP).unwrap();
    let part_doc = PdfDocument::from_bytes(std::fs::read(&part).unwrap(), None).unwrap();
    assert_eq!(
        pl_doc::structure::fingerprints(&part_doc).unwrap(),
        vec![want[0], want[7]]
    );
    assert!(matches!(
        s.extract_pages(id, &[0], &other, KEEP),
        Err(pl_doc::DocError::OpenElsewhere { .. })
    ));
}

#[test]
fn a_signed_document_needs_consent_before_a_structural_edit() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::signed_stub();
    let path = write(dir.path(), "signed.pdf", &fixture.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let fields = doc.structure_signatures().unwrap();
    assert!(!fields.is_empty());
    let start = state(doc);
    let render = doc.render_id();

    let rotate = StructCommand::RotatePages {
        pages: vec![0],
        degrees: 90,
    };
    let err = doc.apply_structure(rotate.clone(), KEEP).unwrap_err();
    let asked = err.signatures_to_confirm().expect("a signature warning");
    assert!(
        asked.iter().any(|f| f.contains(pl_testkit::SIGNED_FIELD)),
        "{asked:?}"
    );
    assert_eq!(state(doc), start, "nothing changed");
    assert_eq!(doc.render_id(), render);
    assert!(!doc.is_modified());

    let out = doc
        .apply_structure(rotate, SignaturePolicy::Invalidate)
        .unwrap();
    assert!(!out.signatures_invalidated.is_empty());
    // The agreement holds for the working copy: the next command and the save go ahead.
    apply(
        doc,
        StructCommand::RotatePages {
            pages: vec![0],
            degrees: 90,
        },
    );
    doc.save().unwrap();
    let reread = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
    assert_eq!(reread.pages().unwrap()[0].rotate, Rotation::R180);
}

#[test]
fn bookmarks_and_links_are_undoable_and_resolve_to_their_pages() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, truth) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let rows = doc.outline_rows().unwrap();
    assert_eq!(rows.len(), truth.outline.len());
    let top = doc.outline().unwrap().items.len();
    let start = state(doc);

    // Bookmarks: add, rename, retarget, move, delete.
    apply(
        doc,
        StructCommand::AddBookmark {
            parent: vec![],
            index: top,
            bookmark: BookmarkSpec {
                title: "Details \u{2013} east".into(),
                target: BookmarkTarget::Page(DestSpec::at(5, 36.0, 700.0)),
                color: Some([0.0, 0.0, 1.0]),
                bold: true,
                italic: false,
                open: false,
            },
        },
    );
    let row = |doc: &mut Document, path: &[usize]| {
        doc.outline_rows()
            .unwrap()
            .into_iter()
            .find(|r| r.path == path)
            .unwrap()
    };
    let added = row(doc, &[top]);
    assert_eq!(added.title, "Details \u{2013} east");
    assert_eq!(added.dest.unwrap().page_index, 5);
    apply(
        doc,
        StructCommand::RenameBookmark {
            path: vec![top],
            title: "Details".into(),
        },
    );
    apply(
        doc,
        StructCommand::SetBookmarkTarget {
            path: vec![top],
            target: BookmarkTarget::Page(DestSpec::page(3)),
        },
    );
    assert_eq!(row(doc, &[top]).dest.unwrap().page_index, 3);
    apply(
        doc,
        StructCommand::MoveBookmark {
            from: vec![top],
            parent: vec![0],
            index: 0,
        },
    );
    let moved = row(doc, &[0, 0]);
    assert_eq!((moved.title.as_str(), moved.level), ("Details", 1));
    // A page operation carries the bookmark to the page's new place.
    apply(
        doc,
        StructCommand::MovePages {
            pages: vec![3],
            to: 7,
        },
    );
    assert_eq!(row(doc, &[0, 0]).dest.unwrap().page_index, 7);
    // The PDFium display has the same bookmark.
    let tree = doc.bookmarks().unwrap();
    assert!(tree.roots[0]
        .children
        .iter()
        .any(|c| c.title == "Details" && c.page == Some(7)));
    apply(doc, StructCommand::DeleteBookmark { path: vec![0, 0] });
    assert!(doc
        .outline_rows()
        .unwrap()
        .iter()
        .all(|r| r.title != "Details"));

    // Links: add one to the page that now shows marker 6, edit it, follow it.
    let target_page = 5u32;
    let mut link = LinkData::new(
        Rect::new(400.0, 400.0, 500.0, 430.0),
        LinkTarget::Page(DestSpec::at(target_page, 10.0, 500.0)),
    );
    apply(
        doc,
        StructCommand::AddLink {
            page: 0,
            link: link.clone(),
        },
    );
    let links = doc.page_links(0).unwrap();
    let ours = links
        .iter()
        .find(|l| l.link.rect == Rect::new(400.0, 400.0, 500.0, 430.0))
        .expect("the new link");
    assert_eq!(ours.resolved.unwrap().page_index, target_page as usize);
    let link_id = ours.id.clone().expect("the store names it").0;
    link.target = LinkTarget::Uri("https://example.invalid/sheet".into());
    apply(
        doc,
        StructCommand::EditLink {
            id: link_id.clone(),
            link,
        },
    );
    let edited = doc
        .page_links(0)
        .unwrap()
        .into_iter()
        .find(|l| l.id.as_ref().map(|i| i.0.as_str()) == Some(link_id.as_str()))
        .unwrap();
    assert!(
        matches!(edited.link.action, pl_doc::pl_pdf::LinkAction::Uri(ref u) if u.contains("sheet"))
    );
    assert!(
        edited.resolved.is_none(),
        "an address leads nowhere in the document"
    );
    // A command naming an annotation that is not a link is refused and changes nothing.
    let not_a_link = doc
        .annots()
        .unwrap()
        .set()
        .iter()
        .find(|a| {
            a.baseline().is_some_and(|b| {
                b.dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok())
                    == Some(b"Square".as_slice())
            })
        })
        .map(|a| a.id.0.clone())
        .unwrap();
    let before = state(doc);
    assert!(doc
        .apply_structure(StructCommand::DeleteLink { id: not_a_link }, KEEP)
        .is_err());
    assert_eq!(state(doc), before);
    apply(
        doc,
        StructCommand::DeleteLink {
            id: link_id.clone(),
        },
    );
    assert!(doc
        .page_links(0)
        .unwrap()
        .iter()
        .all(|l| l.id.as_ref().map(|i| i.0.as_str()) != Some(link_id.as_str())));

    // The fixture's own links resolve to the pages the truth names.
    while doc.can_undo() {
        doc.undo().unwrap();
    }
    assert_eq!(state(doc), start);
    for lt in truth.links.iter().filter(|l| l.target_page.is_some()) {
        if matches!(lt.kind, pl_testkit::LinkKind::GoToR) {
            continue;
        }
        let links = doc.page_links(lt.page_index as u32).unwrap();
        let found = links
            .iter()
            .find(|l| {
                (l.link.rect.x0 - lt.rect[0]).abs() < 1e-3
                    && (l.link.rect.y0 - lt.rect[1]).abs() < 1e-3
            })
            .unwrap_or_else(|| panic!("link {lt:?}"));
        assert_eq!(
            found.resolved.map(|r| r.page_index),
            lt.target_page,
            "link {lt:?}"
        );
    }
}

/// Pixels that differ by more than `tolerance` in any channel.
fn differing(a: &Bitmap, b: &Bitmap, tolerance: u8) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0)
        .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > tolerance))
        .count()
}

fn inked(b: &Bitmap) -> usize {
    b.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3].iter().any(|c| *c < 200))
        .count()
}

/// The flatten fixture without `/CA` (PDFium 8125 ignores it on existing appearances; see
/// pl-pdf's flatten tests).
fn flatten_fixture(dir: &Path) -> PathBuf {
    let (fx, _) = pl_testkit::flatten_set();
    let path = write(dir, "flatten.pdf", &fx.bytes);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    for p in 0..doc.page_count() {
        for a in doc.annotations(p).unwrap() {
            if a.dict.has(b"CA") {
                let mut patch = DictPatch::new();
                patch.remove("CA");
                edits.modify_annotation(a.id.unwrap(), patch);
            }
        }
    }
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    path
}

#[test]
fn flatten_leaves_the_render_unchanged_and_is_undoable_until_saved() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let path = flatten_fixture(dir.path());
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let pages = doc.page_count();
    let render = |doc: &mut Document| -> Vec<Bitmap> {
        (0..pages)
            .map(|p| doc.render_page_composite(p, 1.5).unwrap())
            .collect()
    };
    let before = render(doc);
    let start = state(doc);
    let annots_before = start.annots.len();

    // One page first, then everything.
    let out = apply(
        doc,
        StructCommand::Flatten {
            scope: FlattenScope::Pages(vec![1]),
            print: false,
            include_form_fields: false,
        },
    );
    let StructReport::Flatten(report) = &out.report else {
        panic!("{:?}", out.report)
    };
    assert!(!report.flattened.is_empty());
    assert!(report.flattened.iter().all(|f| f.page_index == 1));
    apply(
        doc,
        StructCommand::Flatten {
            scope: FlattenScope::All,
            print: false,
            include_form_fields: false,
        },
    );
    let flat = state(doc);
    assert!(
        flat.annots.len() < annots_before,
        "annotations were burnt in"
    );
    assert_ne!(flat.prints, start.prints, "the page content changed");
    let after = render(doc);
    for (p, (a, b)) in before.iter().zip(&after).enumerate() {
        assert!(inked(a) > 1000, "page {p} draws something");
        let over = differing(a, b, 8);
        assert!(
            over <= 40,
            "page {p}: {over} pixels differ by more than 8 levels"
        );
    }

    // Undo gives the annotations back.
    doc.undo().unwrap();
    doc.undo().unwrap();
    assert_eq!(state(doc), start);
    let again = render(doc);
    for (p, (a, b)) in before.iter().zip(&again).enumerate() {
        assert_eq!(
            differing(a, b, 0),
            0,
            "page {p}: undo restores the exact render"
        );
    }

    // Flatten by selection: one annotation.
    let pick = doc
        .annots()
        .unwrap()
        .set()
        .iter()
        .find(|a| {
            // A circle that shows (not Hidden, not NoView). (The fixture's squares share a
            // name, so one of them would take the other's id once it is gone.)
            a.page == 0
                && a.baseline().is_some_and(|b| {
                    let subtype = b.dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok());
                    let flags = b
                        .dict
                        .get(b"F")
                        .ok()
                        .and_then(|o| o.as_i64().ok())
                        .unwrap_or(0);
                    subtype == Some(b"Circle".as_slice()) && flags & (2 | 32) == 0
                })
        })
        .map(|a| a.id.clone())
        .unwrap();
    let out = apply(
        doc,
        StructCommand::Flatten {
            scope: FlattenScope::Annotations(vec![pick.0.clone()]),
            print: false,
            include_form_fields: false,
        },
    );
    let StructReport::Flatten(report) = &out.report else {
        panic!("{:?}", out.report)
    };
    assert_eq!(report.flattened.len(), 1, "{report:?}");
    assert!(
        doc.annotation(&pick).is_none(),
        "{pick:?} {report:?} {:?}",
        doc.annots()
            .unwrap()
            .set()
            .iter()
            .map(|a| (a.id.clone(), a.page))
            .collect::<Vec<_>>()
    );
    assert_eq!(doc.annots().unwrap().set().len(), annots_before - 1);
    // Saved: the flatten is in the file and cannot be undone any more.
    doc.save().unwrap();
    assert!(!doc.can_undo());
    assert!(doc.annotation(&pick).is_none());
}

#[test]
fn the_journal_replays_structural_steps_after_a_kill() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let journal = tempfile::tempdir().unwrap();
    let (main, _) = structure_main();
    let (source, _) = structure_source();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let source_path = write(dir.path(), "source.pdf", &source.bytes);

    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let a = Annotation::square(4, Rect::new(30.0, 30.0, 90.0, 70.0), &origin());
    doc.apply(Command::add(a)).unwrap();
    apply(
        doc,
        StructCommand::MovePages {
            pages: vec![4],
            to: 0,
        },
    );
    let b = Annotation::circle(0, Rect::new(100.0, 100.0, 150.0, 150.0), &origin());
    let b_id = b.id.clone();
    doc.apply(Command::add(b)).unwrap();
    apply(
        doc,
        StructCommand::InsertPages {
            at: 1,
            source: PageSource::File(source_path.clone()),
            pages: vec![2],
        },
    );
    apply(doc, StructCommand::DeletePages { pages: vec![8] });
    doc.undo().unwrap(); // the delete
    apply(
        doc,
        StructCommand::RenameBookmark {
            path: vec![0],
            title: "Renamed".into(),
        },
    );
    doc.undo().unwrap(); // the rename
    doc.redo().unwrap(); // and again
    let c = Annotation::square(1, Rect::new(10.0, 10.0, 60.0, 60.0), &origin());
    doc.apply(Command::add(c)).unwrap();
    doc.undo().unwrap(); // c
    let before = state(doc);
    let (undo_depth, redo_depth) = (doc.can_undo(), doc.can_redo());
    assert!(doc.annotation(&b_id).is_some());
    // The kill.
    drop(s);
    assert_eq!(std::fs::read(&path).unwrap(), main.bytes);

    let mut s = session(&backend, Some(journal.path()));
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.pending_journal(), pl_doc::PendingJournal::Recoverable);
    doc.load_annots().unwrap();
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    assert!(doc.recovered_changes() >= 10);
    assert_eq!(
        state(doc),
        before,
        "the replay gives the state before the kill"
    );
    assert_eq!((doc.can_undo(), doc.can_redo()), (undo_depth, redo_depth));
    assert!(doc.has_structure_changes());
    // The history came back too: redo c, then undo everything to the file.
    assert!(doc.redo().unwrap());
    let mut steps = 0;
    while doc.can_undo() {
        doc.undo().unwrap();
        steps += 1;
    }
    assert!(steps >= 6, "{steps}");
    assert!(!doc.is_modified());
    let mut fresh = session(&backend, None);
    let fid = open(&mut fresh, &path);
    let file_state = state(fresh.document_mut(fid).unwrap());
    assert_eq!(state(s.document_mut(id).unwrap()), file_state);
}

#[test]
fn discarding_goes_back_to_the_file() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, truth) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let start = state(doc);
    apply(
        doc,
        StructCommand::DeletePages {
            pages: vec![0, 1, 2],
        },
    );
    assert_eq!(doc.page_count(), 5);
    doc.discard_changes();
    assert!(!doc.is_modified() && !doc.can_undo() && !doc.can_redo());
    assert_eq!(doc.page_count(), 8);
    assert_eq!(state(doc), start);
    let discarded = model_of(doc, &truth.markers);
    check(doc, &discarded, "discarded");
    assert!(doc.take_structure_change().is_some());
}

#[test]
fn the_undo_budget_forgets_the_oldest_structural_steps() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    // Room for about two snapshots.
    doc.set_structure_undo_budget(main.bytes.len() * 5 / 2);
    for _ in 0..5 {
        apply(
            doc,
            StructCommand::RotatePages {
                pages: vec![0],
                degrees: 90,
            },
        );
    }
    assert!(doc.structure_undo_bytes() <= doc.structure_undo_budget());
    assert!(
        doc.structure_steps_dropped() >= 2,
        "{}",
        doc.structure_steps_dropped()
    );
    let mut undone = 0;
    while doc.can_undo() {
        doc.undo().unwrap();
        undone += 1;
    }
    assert_eq!(undone + doc.structure_steps_dropped(), 5);
    assert!(doc.is_modified(), "the oldest steps cannot be undone");
}

/// The real sample, when present: a copy is reordered, rotated, edited and saved; the sample
/// itself is never changed. Its content is not printed.
#[test]
fn the_sample_drawing_set_survives_page_operations() {
    let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    if !sample.is_file() {
        eprintln!("skipped: the sample is not present");
        return;
    }
    let original = std::fs::read(&sample).unwrap();
    let work = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("pl-doc-structure-sample");
    std::fs::create_dir_all(&work).unwrap();
    let path = work.join("sample-copy.pdf");
    std::fs::write(&path, &original).unwrap();
    let backend = backend();
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let start = doc.page_fingerprints().unwrap();
    let n = start.len() as u32;
    assert!(n >= 2);
    apply(
        doc,
        StructCommand::MovePages {
            pages: vec![n - 1],
            to: 0,
        },
    );
    apply(
        doc,
        StructCommand::RotatePages {
            pages: vec![1],
            degrees: 90,
        },
    );
    let mut want = start.clone();
    let last = want.pop().unwrap();
    want.insert(0, last);
    want[1].rotate = turned(want[1].rotate, 90);
    assert_eq!(doc.page_fingerprints().unwrap(), want);
    doc.save().unwrap();
    let reread = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
    assert_eq!(pl_doc::structure::fingerprints(&reread).unwrap(), want);
    drop(s);
    assert_eq!(
        std::fs::read(&sample).unwrap(),
        original,
        "the sample is untouched"
    );
    let _ = std::fs::remove_dir_all(&work);
}

/// A user interface's path: an interactive open (geometry arrives later), the change report,
/// and a background save of the working copy.
#[test]
fn an_interactive_document_saves_its_working_copy_in_the_background() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, truth) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let mut s = Session::new(backend.clone());
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let epoch = doc.geometry_epoch();
    let out = apply(
        doc,
        StructCommand::MovePages {
            pages: vec![2],
            to: 0,
        },
    );
    assert_eq!(out.page_map.map(2), Some(0));
    let change = doc.take_structure_change().unwrap();
    assert_eq!(change.page_map, out.page_map);
    assert_eq!(change.new_render, doc.render_id());
    assert!(doc.take_structure_change().is_none(), "reported once");
    assert!(doc.geometry_epoch() > epoch);
    // The UserUnit page moved first; its unit came along although no page is loaded yet.
    assert_eq!(doc.pages()[0].user_unit(), 2.0);
    assert_eq!(doc.load_geometry(0).unwrap().user_unit(), 2.0);
    // A late answer of the open's /UserUnit probe (about the file's order) changes nothing.
    doc.wait_background(std::time::Duration::from_secs(20));
    assert_eq!(doc.pages()[0].user_unit(), 2.0);
    assert_eq!(doc.pages()[2].user_unit(), 1.0);

    let ticket = doc.start_save().unwrap();
    assert!(doc.is_saving());
    assert!(matches!(
        doc.apply_structure(
            StructCommand::RotatePages {
                pages: vec![0],
                degrees: 90
            },
            KEEP
        ),
        Err(pl_doc::DocError::Saving)
    ));
    let saved = doc.finish_save(ticket).unwrap();
    assert_eq!(saved.outcome.unwrap().report.kind, SaveKind::Full);
    assert!(!doc.is_modified());
    assert!(doc.page_text(0).unwrap().text().contains(&truth.markers[2]));
    let reread = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
    assert_eq!(reread.pages().unwrap()[0].user_unit, 2.0);
    let _: Option<(&Fixture, AnnotId)> = None;
}

#[test]
fn structure_markers_are_what_the_fixtures_draw() {
    // Guards the reference model's assumption.
    let (_, truth) = structure_main();
    assert_eq!(truth.markers[0], structure_marker("A", 0));
}

/// The command-file driver runs structural commands in the journal's form.
#[test]
fn a_command_file_runs_structural_steps() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, truth) = structure_main();
    write(dir.path(), "main.pdf", &main.bytes);
    let script = pl_doc::script::Script::from_json(
        r#"{ "steps": [
            { "op": "open", "path": "main.pdf" },
            { "op": "structure", "command": { "DeletePages": { "pages": [0, 1] } } },
            { "op": "expect_pages", "count": 6 },
            { "op": "structure", "command": { "RotatePages": { "pages": [0], "degrees": 90 } } },
            { "op": "undo" },
            { "op": "structure", "command": { "InsertBlank":
                { "at": 6, "count": 2, "media_box": null, "rotate": 0 } } },
            { "op": "expect_pages", "count": 8 },
            { "op": "save_as", "path": "out.pdf" }
        ] }"#,
    )
    .unwrap();
    let mut s = session(&backend, None);
    let mut host = pl_doc::script::LogOnly::default();
    let report = pl_doc::script::run(&mut s, &script, dir.path(), &mut host).unwrap();
    assert_eq!(report.steps, 8, "{:?}", host.lines);
    let out =
        PdfDocument::from_bytes(std::fs::read(dir.path().join("out.pdf")).unwrap(), None).unwrap();
    assert_eq!(out.page_count(), 8);
    assert_eq!(
        out.pages().unwrap()[0].rotate,
        Rotation::R0,
        "the rotation was undone"
    );
    // A failing expectation names its step.
    let bad = pl_doc::script::Script::from_json(
        r#"{ "steps": [ { "op": "open", "path": "out.pdf" },
                        { "op": "expect_pages", "count": 3 } ] }"#,
    )
    .unwrap();
    let err = pl_doc::script::run(&mut s, &bad, dir.path(), &mut host).unwrap_err();
    assert!(err.to_string().contains("expect_pages"), "{err}");
    let _ = truth;
}

#[test]
fn spaces_and_legends_follow_their_pages_through_page_operations() {
    use pl_doc::{LegendDef, SpaceDef};
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = structure_main();
    let path = write(dir.path(), "main.pdf", &main.bytes);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let pages = doc.page_count();
    let space = |id: &str, page: usize| SpaceDef {
        id: id.into(),
        name: id.to_uppercase(),
        page,
        outline: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]],
        parent: None,
    };
    doc.define_space(space("room", 0)).unwrap();
    doc.define_legend(LegendDef {
        id: "leg".into(),
        title: "Key".into(),
        page: pages as usize - 1,
        rect: [0.0, 0.0, 50.0, 50.0],
        subjects: Vec::new(),
        columns: Vec::new(),
    })
    .unwrap();

    // Page 0 goes to the end: the space goes with it; the old last page moves up one.
    let moved = apply(
        doc,
        StructCommand::MovePages {
            pages: vec![0],
            to: pages - 1,
        },
    );
    let room_page = moved.page_map.map(0).unwrap() as usize;
    let legend_page = moved.page_map.map(pages - 1).unwrap() as usize;
    assert_ne!(room_page, 0, "page 0 really moved");
    assert!(moved.settings_dropped.is_empty());
    let settings = doc.doc_settings().unwrap().clone();
    assert_eq!(settings.spaces[0].page, room_page);
    assert_eq!(settings.legends[0].page, legend_page);

    // Deleting the room's page drops the room, says so, and leaves nothing stale behind.
    let deleted = apply(
        doc,
        StructCommand::DeletePages {
            pages: vec![room_page as u32],
        },
    );
    assert_eq!(deleted.settings_dropped, vec!["space \"ROOM\"".to_string()]);
    let settings = doc.doc_settings().unwrap().clone();
    assert!(settings.spaces.is_empty());
    assert_eq!(
        settings.legends[0].page,
        deleted.page_map.map(legend_page as u32).unwrap() as usize
    );
    assert!(settings.validate(doc.page_count() as usize).is_ok());
    doc.define_space(space("room2", 0))
        .expect("later settings edits still work");

    // The saved file has the remapped settings.
    doc.save().unwrap();
    drop(s);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let settings = doc.doc_settings().unwrap().clone();
    assert_eq!(settings.spaces.len(), 1);
    assert_eq!(settings.spaces[0].id, "room2");
    assert_eq!(
        settings.legends[0].page,
        deleted.page_map.map(legend_page as u32).unwrap() as usize
    );
}

#[test]
fn page_operations_respect_the_files_permissions() {
    let backend = backend();
    let mut s = session(&backend, None);
    for fixture in [pl_testkit::encrypted_rc4(), pl_testkit::encrypted_aes256()] {
        let id = s.open_bytes("enc", fixture.bytes.clone(), None).unwrap();
        let doc = s.document_mut(id).unwrap();
        doc.load_annots().unwrap();
        let before = doc.page_count();
        let refused = doc.apply_structure(
            StructCommand::RotatePages {
                pages: vec![0],
                degrees: 90,
            },
            KEEP,
        );
        assert!(
            matches!(refused, Err(pl_doc::DocError::NotPermitted { .. })),
            "{refused:?}"
        );
        let refused = doc.apply_structure(
            StructCommand::Flatten {
                scope: FlattenScope::All,
                print: false,
                include_form_fields: false,
            },
            KEEP,
        );
        assert!(matches!(
            refused,
            Err(pl_doc::DocError::NotPermitted { .. })
        ));
        assert!(!doc.has_structure_changes());
        assert_eq!(doc.page_count(), before);
    }
}
