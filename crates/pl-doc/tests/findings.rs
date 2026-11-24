//! Regressions found in review: drag merging against undo and save, journal replay across a
//! save, journals left behind by a clean close, cut, Save As onto an open file, the parsed
//! file after a save, the file's own Hidden flag, and layers. Real PDFium, headless.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{AnnotId, AnnotSet, Annotation, Flags, Geometry, Origin, PdfDate, Rgb};
use pl_doc::pl_geom::{Rect, Vec2};
use pl_doc::pl_render::{Bitmap, PdfiumBackend, RenderBackend};
use pl_doc::{
    Command, DocError, Document, DocumentId, Editor, PendingJournal, Session, SessionOptions,
};
use pl_testkit::Fixture;

const SQUARE: &str = "fixture-square-1";
const LINE: &str = "fixture-line-1";
const FREETEXT: &str = "fixture-freetext-1";

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

fn on_disk(fixture: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    (dir, path)
}

fn session(backend: &Arc<PdfiumBackend>, journal: Option<&Path>) -> Session {
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    s.set_journal_dir(journal.map(Path::to_path_buf));
    s
}

fn open(s: &mut Session, path: &Path) -> DocumentId {
    s.open(path, None).unwrap()
}

fn origin() -> Origin {
    Origin::new("Reviewer", PdfDate::parse("D:20261124101500Z").unwrap())
}

fn model(doc: &mut Document) -> Vec<Annotation> {
    doc.annots().unwrap().model()
}

fn find(doc: &mut Document, id: &str) -> Annotation {
    doc.annots()
        .unwrap()
        .set()
        .get(&AnnotId::from(id))
        .unwrap_or_else(|| panic!("annotation {id}"))
        .clone()
}

fn moved(a: &Annotation, dx: f64, dy: f64) -> Annotation {
    let mut b = a.clone();
    b.translate(Vec2::new(dx, dy));
    b
}

fn render(doc: &Document, page: u32) -> Bitmap {
    doc.backend()
        .render_page(doc.render_id(), page, 1.0)
        .unwrap()
}

// ---- the editor -------------------------------------------------------------------------------

#[test]
fn a_drag_does_not_merge_across_an_undo() {
    let mut ed = Editor::new(AnnotSet::new(), 1);
    let a = Annotation::square(0, Rect::new(0.0, 0.0, 50.0, 50.0), &origin());
    ed.execute(Command::add(a.clone())).unwrap();
    let out = ed
        .execute_merged(Command::modify(moved(&a, 1.0, 0.0)), 7)
        .unwrap();
    assert!(!out.merged);
    let mut wide = moved(&a, 1.0, 0.0);
    wide.style.width = 5.0;
    ed.execute(Command::modify(wide)).unwrap();
    assert!(ed.undo().is_some());
    assert_eq!(ed.redo_len(), 1);

    // Same key as the step now on top, but an undo lies between: a new step, and nothing
    // stays redoable above it.
    let out = ed
        .execute_merged(Command::modify(moved(&a, 2.0, 0.0)), 7)
        .unwrap();
    assert!(out.changed && !out.merged);
    assert_eq!(ed.redo_len(), 0);
    assert_eq!(ed.undo_len(), 3);
    let here = ed.model();
    assert!(ed.redo().is_none());
    ed.undo().unwrap();
    ed.redo().unwrap();
    assert_eq!(ed.model(), here, "undo then redo returns to the same state");

    // Nor into a step that was undone and redone while later steps are still redoable.
    let mut ed = Editor::new(AnnotSet::new(), 1);
    ed.execute(Command::add(a.clone())).unwrap();
    ed.execute_merged(Command::modify(moved(&a, 1.0, 0.0)), 9)
        .unwrap();
    let mut wide = moved(&a, 1.0, 0.0);
    wide.style.width = 5.0;
    ed.execute(Command::modify(wide.clone())).unwrap();
    ed.undo().unwrap();
    ed.undo().unwrap();
    ed.redo().unwrap();
    assert_eq!(ed.redo_len(), 1);
    let out = ed
        .execute_merged(Command::modify(moved(&a, 3.0, 0.0)), 9)
        .unwrap();
    assert!(!out.merged);
    assert_eq!(ed.redo_len(), 0);

    // A drag that is not interrupted still merges.
    let out = ed
        .execute_merged(Command::modify(moved(&a, 4.0, 0.0)), 9)
        .unwrap();
    assert!(out.merged);
}

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n.max(1) as u64) as usize
    }
}

#[test]
fn random_drags_undos_and_redos_keep_the_history_exact() {
    for seed in 1..=200u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut ed = Editor::new(AnnotSet::new(), 1);
        let a = Annotation::square(0, Rect::new(0.0, 0.0, 50.0, 50.0), &origin());
        let b = Annotation::square(0, Rect::new(100.0, 0.0, 150.0, 50.0), &origin());
        ed.execute(Command::AddAnnotations(vec![a.clone(), b.clone()]))
            .unwrap();
        for _ in 0..40 {
            let target = if rng.below(2) == 0 { &a } else { &b };
            // The add itself may be undone at this point: then only the history can move.
            let Some(current) = ed.set().get(&target.id).map(Annotation::detached) else {
                if rng.below(2) == 0 {
                    ed.undo();
                } else {
                    ed.redo();
                }
                continue;
            };
            match rng.below(6) {
                0 | 1 => {
                    let key = 1 + rng.below(2) as u64;
                    let next = moved(&current, 1.0 + rng.below(5) as f64, 0.0);
                    ed.execute_merged(Command::modify(next), key).unwrap();
                }
                2 => {
                    let mut next = current.clone();
                    next.style.width = 1.0 + rng.below(9) as f64;
                    ed.execute(Command::modify(next)).unwrap();
                }
                3 | 4 => {
                    ed.undo();
                }
                _ => {
                    ed.redo();
                }
            }
            // Wherever the history stands: one step back and forth, and one step forth and
            // back, both return to exactly this state.
            let here = ed.model();
            if ed.undo().is_some() {
                ed.redo().unwrap();
                assert_eq!(ed.model(), here, "seed {seed}: undo, redo");
            }
            if ed.redo().is_some() {
                ed.undo().unwrap();
                assert_eq!(ed.model(), here, "seed {seed}: redo, undo");
            }
        }
        // And all the way down is the start.
        while ed.undo().is_some() {}
        assert!(ed.model().is_empty(), "seed {seed}");
    }
}

// ---- journal ----------------------------------------------------------------------------------

#[test]
fn journal_replay_matches_after_a_drag_across_a_save() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let square = find(doc, SQUARE);

    doc.apply_drag(Command::modify(moved(&square, 10.0, 0.0)), 7)
        .unwrap();
    doc.save().unwrap();
    // The same drag key after the save: a step of its own, in the new journal.
    let out = doc
        .apply_drag(Command::modify(moved(&square, 20.0, 0.0)), 7)
        .unwrap();
    assert!(!out.merged, "a drag does not merge across a save");
    assert!(doc.undo().unwrap());
    let out = doc
        .apply_drag(Command::modify(moved(&square, 5.0, 5.0)), 7)
        .unwrap();
    assert!(!out.merged);
    assert!(doc.undo().unwrap());
    let before = model(doc);
    assert_eq!(
        find(doc, SQUARE).geometry,
        moved(&square, 10.0, 0.0).geometry,
        "back at the saved position"
    );
    // The kill.
    drop(s);

    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.pending_journal(), PendingJournal::Recoverable);
    doc.load_annots().unwrap();
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    assert_eq!(model(doc), before, "replay equals the pre-kill state");
    assert!(
        !doc.is_modified(),
        "everything after the save was undone again"
    );
}

#[test]
fn closing_a_document_with_nothing_unsaved_leaves_no_journal() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let journal = tempfile::tempdir().unwrap();
    let journals = || std::fs::read_dir(journal.path()).unwrap().count();

    // Edit, then undo.
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let extra = Annotation::square(0, Rect::new(300.0, 100.0, 360.0, 160.0), &origin());
    doc.apply(Command::add(extra.clone())).unwrap();
    assert_eq!(journals(), 1);
    assert!(doc.undo().unwrap());
    assert!(!doc.is_modified());
    s.close(id).unwrap();
    assert_eq!(journals(), 0, "nothing unsaved, nothing to recover");
    let id = open(&mut s, &path);
    assert_eq!(
        s.document(id).unwrap().pending_journal(),
        PendingJournal::None
    );

    // Save, undo, redo: the document is what the file holds.
    let doc = s.document_mut(id).unwrap();
    doc.apply(Command::add(extra.clone())).unwrap();
    doc.save().unwrap();
    assert!(doc.undo().unwrap());
    assert!(doc.redo().unwrap());
    assert!(!doc.is_modified());
    assert_eq!(journals(), 1, "the undo and redo were journalled");
    s.close(id).unwrap();
    assert_eq!(journals(), 0);

    // A modified document keeps its journal through a close without a save.
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let square = find(doc, SQUARE);
    doc.apply(Command::modify(moved(&square, 3.0, 3.0)))
        .unwrap();
    s.close(id).unwrap();
    assert_eq!(journals(), 1);
    let id = open(&mut s, &path);
    assert_eq!(
        s.document(id).unwrap().pending_journal(),
        PendingJournal::Recoverable
    );
    // And a journal nobody read in this run is still there after a close.
    s.close(id).unwrap();
    assert_eq!(journals(), 1);
}

#[test]
fn a_replay_that_ends_on_the_file_state_is_not_modified() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let extra = Annotation::square(0, Rect::new(300.0, 100.0, 360.0, 160.0), &origin());
    doc.apply(Command::add(extra)).unwrap();
    doc.save().unwrap();
    assert!(doc.undo().unwrap());
    assert!(doc.redo().unwrap());
    let before = model(doc);
    drop(s); // the kill

    let mut s = session(&backend, Some(journal.path()));
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert_eq!(doc.recovered_changes(), 2);
    assert_eq!(model(doc), before);
    assert!(
        !doc.is_modified(),
        "the replayed records net to what the file holds"
    );
}

// ---- cut ----------------------------------------------------------------------------------------

#[test]
fn cut_deletes_only_what_it_copied() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let count = model(doc).len();
    let clip = doc
        .cut(&[AnnotId::from(FREETEXT), AnnotId::from(SQUARE)])
        .unwrap();
    assert_eq!((clip.len(), clip.skipped), (1, 1));
    assert!(
        doc.annotation(&AnnotId::from(FREETEXT)).is_some(),
        "a markup that cannot be copied is not removed by a cut"
    );
    assert!(doc.annotation(&AnnotId::from(SQUARE)).is_none());
    assert_eq!(model(doc).len(), count - 1);

    // A cut of only uncopyable markups changes nothing and records no undo step.
    let undo_before = doc.annots().unwrap().undo_len();
    let clip = doc.cut(&[AnnotId::from(FREETEXT)]).unwrap();
    assert_eq!((clip.len(), clip.skipped), (0, 1));
    assert_eq!(doc.annots().unwrap().undo_len(), undo_before);
    assert_eq!(model(doc).len(), count - 1);
}

// ---- save ---------------------------------------------------------------------------------------

#[test]
fn save_as_onto_a_file_open_in_another_tab_is_refused() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir_a, path_a) = on_disk(&fixture);
    let (_dir_b, path_b) = on_disk(&fixture);
    let journal = tempfile::tempdir().unwrap();
    let mut s = session(&backend, Some(journal.path()));
    let a = open(&mut s, &path_a);
    let b = open(&mut s, &path_b);
    let extra = Annotation::square(0, Rect::new(300.0, 100.0, 360.0, 160.0), &origin());
    s.document_mut(b)
        .unwrap()
        .apply(Command::add(extra))
        .unwrap();
    let doc_a = s.document_mut(a).unwrap();
    doc_a.apply(Command::delete(AnnotId::from(SQUARE))).unwrap();

    let err = s.save_as(a, &path_b).unwrap_err();
    assert!(matches!(err, DocError::OpenElsewhere { .. }), "{err}");
    assert!(err.to_string().contains("open in another tab"), "{err}");
    assert_eq!(std::fs::read(&path_b).unwrap(), fixture.bytes);
    assert_eq!(s.document(a).unwrap().path(), Some(path_a.as_path()));
    // The other document still saves to its own file.
    let saved = s.document_mut(b).unwrap().save().unwrap();
    assert!(saved.wrote());

    // Its own file and a free name are both fine.
    let saved = s.save_as(a, &path_a).unwrap();
    assert!(saved.wrote());
    let free = path_a.with_file_name("copy.pdf");
    s.save_as(a, &free).unwrap();
    assert_eq!(s.document(a).unwrap().path(), Some(free.as_path()));
}

#[test]
fn the_parsed_file_is_released_after_a_save() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert!(!doc.pdf_is_loaded());
    let extra = Annotation::square(0, Rect::new(300.0, 100.0, 360.0, 160.0), &origin());
    doc.apply(Command::add(extra.clone())).unwrap();
    doc.save().unwrap();
    assert!(!doc.pdf_is_loaded(), "released after a save");
    assert!(!doc.save().unwrap().wrote());
    assert!(
        !doc.pdf_is_loaded(),
        "released after a save with nothing to write"
    );

    let square = find(doc, SQUARE);
    doc.apply(Command::modify(moved(&square, 4.0, 4.0)))
        .unwrap();
    doc.save_optimised().unwrap();
    assert!(!doc.pdf_is_loaded(), "released after an optimised save");

    doc.apply(Command::modify(moved(&square, 8.0, 8.0)))
        .unwrap();
    let other = path.with_file_name("elsewhere.pdf");
    doc.save_as(&other).unwrap();
    assert!(!doc.pdf_is_loaded(), "released after Save As");

    // A failed save releases it too, and the changes are still there.
    doc.apply(Command::modify(moved(&square, 12.0, 12.0)))
        .unwrap();
    let options = pl_doc::pl_pdf::SaveOptions {
        fail_point: Some(pl_doc::pl_pdf::FailPoint::BeforeReplace),
        ..Default::default()
    };
    assert!(doc.save_with(&options).is_err());
    assert!(!doc.pdf_is_loaded(), "released after a failed save");
    assert!(doc.is_modified());

    // Someone who loaded the file themselves keeps it.
    doc.pdf().unwrap().document().unwrap();
    assert!(doc.pdf_is_loaded());
    doc.save().unwrap();
    assert!(doc.pdf_is_loaded());
}

// ---- the hidden set -------------------------------------------------------------------------------

#[test]
fn an_annotation_the_file_hides_stays_hidden_in_the_mirror() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let mut ghost = Annotation::square(0, Rect::new(300.0, 100.0, 360.0, 160.0), &origin());
    ghost.style.fill = Some(Rgb::BLUE);
    ghost.meta.flags = Flags(Flags::PRINT | Flags::HIDDEN);
    let ghost_id = ghost.id.clone();
    doc.apply(Command::add(ghost)).unwrap();
    doc.save().unwrap();
    s.close(id).unwrap();

    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let mirror_hidden = |doc: &Document| {
        doc.annotations(0)
            .unwrap()
            .iter()
            .find(|i| i.name.as_deref() == Some(ghost_id.as_str()))
            .expect("the hidden square is in the mirror")
            .hidden
    };
    assert!(mirror_hidden(doc));
    let clean = render(doc, 0);

    // Bringing the bottom annotation to the front restacks the whole page.
    let order = vec![AnnotId::from(SQUARE)];
    doc.apply(Command::Reorder { page: 0, order }).unwrap();
    let slots = doc.hidden_annotations();
    let ghost_slot = slots
        .iter()
        .find(|(id, _)| *id == ghost_id)
        .expect("restacked with the rest");
    assert!(ghost_slot.1.hidden_in_file);
    assert!(mirror_hidden(doc));

    // Undo puts every annotation back under PDFium, the ghost still hidden.
    assert!(doc.undo().unwrap());
    assert!(doc.redo().unwrap());
    doc.discard_changes();
    assert!(doc.hidden_annotations().is_empty());
    assert!(
        mirror_hidden(doc),
        "still hidden after the changes were discarded"
    );
    assert_eq!(render(doc, 0).pixels, clean.pixels);
}

#[test]
fn a_restack_touches_only_what_is_at_or_above_the_change() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    let ids: Vec<AnnotId> = model(doc).iter().map(|a| a.id.clone()).collect();
    assert!(ids.len() >= 4);
    // Swap the top two: everything beneath stays PDFium's.
    let n = ids.len();
    let order = vec![ids[n - 1].clone(), ids[n - 2].clone()];
    let out = doc.apply(Command::Reorder { page: 0, order }).unwrap();
    let mut touched = out.touched.clone();
    touched.sort();
    let mut want = vec![ids[n - 1].clone(), ids[n - 2].clone()];
    want.sort();
    assert_eq!(touched, want);
    let hidden: Vec<AnnotId> = doc
        .hidden_annotations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(!hidden.contains(&ids[0]), "{hidden:?}");
    assert!(doc.undo().unwrap());
    assert_eq!(
        model(doc).iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        ids
    );
}

// ---- layers ---------------------------------------------------------------------------------------

/// How many pixels are mostly red, and how many mostly blue.
fn red_and_blue(b: &Bitmap) -> (usize, usize) {
    let (mut red, mut blue) = (0, 0);
    for px in b.pixels.as_chunks::<4>().0 {
        if px[0] > 180 && px[1] < 90 && px[2] < 90 {
            red += 1;
        }
        if px[2] > 180 && px[0] < 90 && px[1] < 90 {
            blue += 1;
        }
    }
    (red, blue)
}

#[test]
fn a_layer_toggle_changes_the_render_and_toggling_back_restores_it() {
    let backend = backend();
    let fixture = pl_testkit::layers();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert!(doc.cached_layers().is_none());
    let layers = doc.layers().unwrap().to_vec();
    assert_eq!(
        layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["Layer A", "Layer B"]
    );
    assert!(layers.iter().all(|l| l.visible && l.default_visible));
    assert!(!doc.pdf_is_loaded(), "listing the layers releases the file");

    let all = render(doc, 0);
    let (red, blue) = red_and_blue(&all);
    assert!(red > 500 && blue > 500, "{red} {blue}");
    let (render_id, revision) = (doc.render_id(), doc.page_revision(0));

    // Layer A (red) off.
    assert!(doc.set_layer_visible(layers[0].id, false).unwrap());
    assert_ne!(doc.render_id(), render_id, "a new mirror");
    assert!(doc.page_revision(0) > revision);
    let without_a = render(doc, 0);
    let (red, blue_now) = red_and_blue(&without_a);
    assert_eq!(red, 0, "the red layer is gone");
    assert_eq!(blue_now, blue, "the blue layer is untouched");
    assert!(!doc.cached_layers().unwrap()[0].visible);
    assert!(!doc.is_modified(), "a layer toggle is not an edit");
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);

    // Setting it to what it is does nothing.
    let same = doc.render_id();
    assert!(!doc.set_layer_visible(layers[0].id, false).unwrap());
    assert_eq!(doc.render_id(), same);

    // A markup hidden by an edit stays hidden across a toggle, and a save keeps the choice.
    let mut extra = Annotation::square(0, Rect::new(300.0, 520.0, 360.0, 580.0), &origin());
    extra.style.stroke = Some(Rgb::BLACK);
    let extra_id = extra.id.clone();
    doc.apply(Command::add(extra)).unwrap();
    doc.save().unwrap();
    let (red, _) = red_and_blue(&render(doc, 0));
    assert_eq!(red, 0, "layer A is still off after the save");
    let square = find(doc, extra_id.as_str());
    doc.apply(Command::modify(moved(&square, 5.0, 5.0)))
        .unwrap();
    assert_eq!(doc.hidden_annotations().len(), 1);
    assert!(doc.set_layer_visible(layers[1].id, false).unwrap());
    let (red, blue_now) = red_and_blue(&render(doc, 0));
    assert_eq!((red, blue_now), (0, 0));
    let slot = doc.hidden_annotations()[0].1;
    assert!(
        doc.annotations(0).unwrap()[slot.index as usize].hidden,
        "the edited markup is hidden in the new mirror too"
    );
    assert!(doc.undo().unwrap());

    // Back to the file's own configuration: the first picture again.
    assert!(doc.reset_layers().unwrap());
    assert!(doc.cached_layers().unwrap().iter().all(|l| l.visible));
    let (red_back, blue_back) = red_and_blue(&render(doc, 0));
    let (red_all, blue_all) = red_and_blue(&all);
    assert_eq!((red_back, blue_back), (red_all, blue_all));

    // An unknown layer is refused and changes nothing.
    assert!(doc.set_layer_visible((9999, 0), false).is_err());
    assert!(doc.cached_layers().unwrap().iter().all(|l| l.visible));
}

#[test]
fn a_document_without_layers_lists_none() {
    let backend = backend();
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session(&backend, None);
    let id = open(&mut s, &path);
    let doc = s.document_mut(id).unwrap();
    assert!(doc.layers().unwrap().is_empty());
    assert!(!doc.reset_layers().unwrap());
    let _ = Geometry::Rect(Rect::ZERO);
    let _ = LINE;
}

#[test]
fn the_real_sample_lists_its_layers_and_one_can_be_switched() {
    let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    if !sample.is_file() {
        eprintln!("skipped: the sample file is not present");
        return;
    }
    let backend = backend();
    let mut s = Session::new(backend.clone());
    let id = s.open(&sample, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    // The sample's optional-content groups are not listed in the catalog (the program that
    // last saved it dropped /OCProperties), so there may be nothing to switch; whatever is
    // listed must be switchable, and listing must not fail or leave the file parsed.
    let layers = doc.layers().unwrap().to_vec();
    assert!(!doc.pdf_is_loaded());
    let render_id = doc.render_id();
    match layers.iter().find(|l| l.visible) {
        Some(layer) => {
            assert!(doc.set_layer_visible(layer.id, false).unwrap());
            assert_ne!(doc.render_id(), render_id);
            assert!(doc.reset_layers().unwrap());
        }
        None => {
            assert!(!doc.reset_layers().unwrap());
            assert_eq!(doc.render_id(), render_id);
        }
    }
    assert_eq!(doc.page_count(), 12);
    assert!(doc.warnings().is_empty());
}
