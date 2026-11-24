//! Bluebeam custom columns and statuses written back, against redcolumn's A3 test set
//! (`samples/external/redcolumn`, local only and never committed: the tests that need it skip
//! when it is absent), and Bluebeam columns added to a file that has none.
//!
//! * Round trip: one column value and one custom status edited, saved, read back; every other
//!   annotation and every other value keeps its bytes (the save is an incremental update after
//!   the untouched file), `/BSIAnnotColumns` is still an indirect array with the same entries.
//! * A column added from Plumbline is appended to `/BSIAnnotColumns` in Bluebeam's structure and
//!   every `/BSIColumnData` is padded; the whole is one undo step.
//! * Saves that do not touch the Bluebeam keys of Revu's own files write nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{AnnotId, Origin, PdfDate, Status};
use pl_doc::pl_pdf::{Object, ObjectId, PdfDocument};
use pl_doc::pl_render::PdfiumBackend;
use pl_doc::pl_takeoff::bluebeam::stored_text;
use pl_doc::pl_takeoff::{ColumnType, Value, ANNOT_COLUMNS_KEY, COLUMN_DATA_KEY};
use pl_doc::{Session, SessionOptions};

fn backend() -> Arc<PdfiumBackend> {
    let dll = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    let dll = std::path::absolute(dll).unwrap();
    Arc::new(PdfiumBackend::new(Some(&dll)).expect("load vendored pdfium.dll"))
}

fn sample(dir: &str, name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("external")
        .join(dir)
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("SAMPLE SKIPPED: samples/external/{dir}/{name} is not present");
        None
    }
}

/// A copy of `source` in a new temporary folder.
fn copy(source: &Path) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(source.file_name().unwrap());
    std::fs::copy(source, &path).unwrap();
    (dir, path)
}

fn session() -> Session {
    let mut s = Session::with_options(backend(), SessionOptions::complete());
    s.set_journal_dir(None);
    s
}

fn origin() -> Origin {
    Origin::new(
        "Plumbline Test",
        PdfDate::parse("D:20261124120000Z").unwrap(),
    )
}

fn read(path: &Path) -> PdfDocument {
    PdfDocument::from_bytes(std::fs::read(path).unwrap(), None).unwrap()
}

/// Every annotation object of a file: its id and dictionary, by `/NM` (a name the file gives
/// more than one annotation, as redcolumn's status replies can share one, is numbered by
/// occurrence: `name#2`).
fn annotations(doc: &PdfDocument) -> Vec<(String, Option<ObjectId>, pl_doc::pl_pdf::Dictionary)> {
    let mut out: Vec<(String, Option<ObjectId>, pl_doc::pl_pdf::Dictionary)> = Vec::new();
    for page in 0..doc.page_count() {
        for e in doc.annotations(page).unwrap() {
            let mut nm = match e.dict.get(b"NM") {
                Ok(Object::String(b, _)) => pl_doc::pl_annot::decode_text(b),
                _ => format!("#{page}-{}", e.slot),
            };
            let seen = out
                .iter()
                .filter(|(n, _, _)| n == &nm || n.starts_with(&format!("{nm}#")))
                .count();
            if seen > 0 {
                nm = format!("{nm}#{}", seen + 1);
            }
            out.push((nm, e.id, e.dict));
        }
    }
    out
}

/// The catalog's `/BSIAnnotColumns`: the object it refers to, and the entries.
fn columns_of(doc: &PdfDocument) -> (Option<ObjectId>, Vec<Object>) {
    let value = doc
        .catalog()
        .unwrap()
        .get(ANNOT_COLUMNS_KEY.as_bytes())
        .unwrap()
        .clone();
    let id = match value {
        Object::Reference(id) => Some(id),
        _ => None,
    };
    let entries = doc.resolve(&value).unwrap().as_array().unwrap().clone();
    (id, entries)
}

fn column_data(dict: &pl_doc::pl_pdf::Dictionary) -> Option<Vec<Object>> {
    dict.get(COLUMN_DATA_KEY.as_bytes())
        .ok()
        .and_then(|o| o.as_array().ok())
        .cloned()
}

#[test]
fn a_value_and_a_custom_status_round_trip_and_nothing_else_moves() {
    let Some(source) = sample("redcolumn", "A3-07-columns-statuses-metadata.pdf") else {
        return;
    };
    let (_dir, path) = copy(&source);
    let before_bytes = std::fs::read(&path).unwrap();
    let before = read(&path);
    let before_annots = annotations(&before);
    let (before_columns_id, before_entries) = columns_of(&before);
    assert!(before_columns_id.is_some());

    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.annots().unwrap();
    assert!(doc.has_bluebeam_columns());
    let columns = doc.bluebeam_columns();
    assert_eq!(columns.len(), 11);
    let qty = columns.iter().find(|c| c.title == "Qty").unwrap().clone();
    let qty_index = qty.bluebeam.as_ref().unwrap().index;

    // The markup edited: the one with every column filled.
    let target: AnnotId = doc
        .annots()
        .unwrap()
        .set()
        .iter()
        .find(|a| a.meta.subject.as_deref() == Some("CC02 every column filled"))
        .unwrap()
        .id
        .clone();
    let status_target: AnnotId = doc
        .annots()
        .unwrap()
        .set()
        .iter()
        .find(|a| a.meta.subject.as_deref() == Some("SS Accepted"))
        .unwrap()
        .id
        .clone();
    let when = PdfDate::parse("D:20261124120000Z").unwrap();
    doc.set_column_text(&target, &qty, "42", when).unwrap();
    // Undo and redo the value edit.
    assert!(doc.undo().unwrap());
    assert_eq!(
        pl_doc::markup_ops::column_value(doc.annotation(&target).unwrap(), &qty),
        Value::Number(4.0)
    );
    assert!(doc.redo().unwrap());

    let sets = doc.status_sets();
    let custom = sets
        .iter()
        .find(|s| s.model == "BSI_A3TESTSET0001")
        .expect("the custom set is offered");
    assert!(custom.states.iter().any(|s| s == "Closed - No Action"));
    let closed = Status::new("BSI_A3TESTSET0001", "Closed - No Action");
    let reply = doc
        .set_status_named(&status_target, &closed, &origin())
        .unwrap();
    assert_eq!(doc.status(&status_target), Some(closed.clone()));
    let saved = doc.save().unwrap();
    assert!(saved.wrote());
    drop(s);

    // The file: an incremental update after the untouched bytes.
    let after_bytes = std::fs::read(&path).unwrap();
    assert!(after_bytes.starts_with(&before_bytes));
    let after = read(&path);
    let (after_columns_id, after_entries) = columns_of(&after);
    assert_eq!(
        after_columns_id, before_columns_id,
        "still an indirect array"
    );
    assert_eq!(after_entries, before_entries);

    let after_annots = annotations(&after);
    assert_eq!(
        after_annots.len(),
        before_annots.len() + 1,
        "one status reply more"
    );
    let mut changed = 0;
    for (nm, id, dict) in &before_annots {
        let (_, new_id, new_dict) = after_annots.iter().find(|(n, _, _)| n == nm).unwrap();
        assert_eq!(new_id, id, "{nm}");
        if nm.as_str() == target.as_str() {
            changed += 1;
            let old = column_data(dict).unwrap();
            let new = column_data(new_dict).unwrap();
            assert_eq!(old.len(), new.len());
            for (i, (o, n)) in old.iter().zip(&new).enumerate() {
                if i == qty_index {
                    assert_ne!(o, n);
                } else {
                    assert_eq!(o, n, "value {i} keeps its bytes");
                }
            }
        } else {
            assert_eq!(new_dict, dict, "{nm} is untouched");
        }
    }
    assert_eq!(changed, 1);
    let new_status = after_annots
        .iter()
        .find(|(n, _, _)| n == reply.as_str())
        .unwrap();
    assert_eq!(
        new_status.2.get(b"StateModel").unwrap(),
        &pl_doc::pl_annot::encode_text("BSI_A3TESTSET0001")
    );

    // Read back by a new session.
    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.annots().unwrap();
    let a = doc.annotation(&target).unwrap();
    assert_eq!(stored_text(a, qty_index).as_deref(), Some("42"));
    assert_eq!(doc.status(&status_target), Some(closed));
    let history = doc.annots().unwrap().set().status_history(&status_target);
    assert_eq!(history.len(), 2, "the earlier Accepted is kept");
    // Nothing more to write.
    assert!(!doc.save().unwrap().wrote());
}

#[test]
fn a_column_added_here_is_appended_and_pads_every_markup() {
    let Some(source) = sample("redcolumn", "A3-07-columns-statuses-metadata.pdf") else {
        return;
    };
    let (_dir, path) = copy(&source);
    let before = read(&path);
    let (before_id, before_entries) = columns_of(&before);
    let before_annots = annotations(&before);
    let with_data = before_annots
        .iter()
        .filter(|(_, _, d)| column_data(d).is_some())
        .count();
    assert!(with_data > 50);

    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.annots().unwrap();
    let column = doc
        .add_bluebeam_column("Phase", ColumnType::Choice, &["Shell", "Fit-out"], false)
        .unwrap();
    assert_eq!(column.bluebeam.as_ref().unwrap().index, 11);
    assert_eq!(doc.bluebeam_columns().len(), 12);
    // One undo step takes back the definition and every padding.
    assert!(doc.undo().unwrap());
    assert_eq!(doc.bluebeam_columns().len(), 11);
    assert!(!doc.is_modified());
    assert!(doc.redo().unwrap());
    let target = doc
        .annots()
        .unwrap()
        .set()
        .iter()
        .find(|a| a.meta.subject.as_deref() == Some("CC01 no column values at all"))
        .unwrap()
        .id
        .clone();
    let when = PdfDate::parse("D:20261124120000Z").unwrap();
    doc.set_column_text(&target, &column, "fit-out", when)
        .unwrap();
    assert!(doc.save().unwrap().wrote());
    drop(s);

    let after = read(&path);
    let (after_id, after_entries) = columns_of(&after);
    assert_eq!(after_id, before_id, "the same indirect array");
    assert_eq!(after_entries.len(), 12);
    assert_eq!(&after_entries[..11], &before_entries[..]);
    let added = after_entries[11].as_dict().unwrap();
    assert_eq!(
        added.get(b"Subtype").unwrap(),
        &Object::Name(b"Choice".to_vec())
    );
    assert_eq!(added.get(b"DisplayOrder").unwrap(), &Object::Integer(11));
    let after_annots = annotations(&after);
    let mut padded = 0;
    for (nm, _, dict) in &before_annots {
        let (_, _, new_dict) = after_annots.iter().find(|(n, _, _)| n == nm).unwrap();
        match (column_data(dict), column_data(new_dict)) {
            (Some(old), Some(new)) => {
                padded += 1;
                assert_eq!(new.len(), 12, "{nm}");
                assert_eq!(&new[..11], &old[..], "{nm}");
                let last = match &new[11] {
                    Object::String(b, _) => pl_doc::pl_annot::decode_text(b),
                    other => panic!("{other:?}"),
                };
                let expected = if nm.as_str() == target.as_str() {
                    "Fit-out"
                } else {
                    ""
                };
                assert_eq!(last, expected, "{nm}");
                // Nothing but the padding changed.
                let mut a = new_dict.clone();
                let mut b = dict.clone();
                a.remove(COLUMN_DATA_KEY.as_bytes());
                b.remove(COLUMN_DATA_KEY.as_bytes());
                if nm.as_str() != target.as_str() {
                    assert_eq!(a, b, "{nm}");
                }
            }
            (None, None) => assert_eq!(new_dict, dict, "{nm}"),
            other => panic!("{nm}: {other:?}"),
        }
    }
    assert_eq!(padded, with_data);
}

#[test]
fn a_file_without_bluebeam_columns_gets_an_indirect_array() {
    let fixture = pl_testkit::rotated_pages();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();

    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.annots().unwrap();
    assert!(!doc.has_bluebeam_columns());
    let rect = pl_doc::pl_geom::Rect::new(50.0, 50.0, 120.0, 100.0);
    let square = pl_doc::pl_annot::Annotation::square(0, rect, &origin());
    let square_id = square.id.clone();
    doc.apply(pl_doc::Command::add(square)).unwrap();
    let column = doc
        .add_bluebeam_column("Notes", ColumnType::Text, &[], true)
        .unwrap();
    assert!(column.multiline);
    let when = PdfDate::parse("D:20261124120000Z").unwrap();
    doc.set_column(&square_id, &column, Value::text("two\nlines"), when)
        .unwrap();
    doc.save().unwrap();
    drop(s);

    let after = read(&path);
    let (array_id, entries) = columns_of(&after);
    assert!(array_id.is_some(), "written as an indirect array");
    assert_eq!(entries.len(), 1);
    let (_, _, dict) = annotations(&after)
        .into_iter()
        .find(|(nm, _, _)| nm.as_str() == square_id.as_str())
        .unwrap();
    let data = column_data(&dict).unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(
        data[0],
        Object::String(
            [
                0xFE, 0xFF, 0, b't', 0, b'w', 0, b'o', 0, b'\n', 0, b'l', 0, b'i', 0, b'n', 0,
                b'e', 0, b's'
            ]
            .to_vec(),
            pl_doc::pl_pdf::StringFormat::Hexadecimal
        )
    );
    // Nothing of it in Plumbline's private storage: one source of truth.
    assert!(!dict.has(pl_doc::pl_annot::PRIVATE_KEY.as_bytes()));
}

/// Revu's own files (no Bluebeam columns) save nothing when nothing is changed, and
/// `/BSIColumnData` never appears on them.
#[test]
fn revu_files_without_columns_are_left_alone() {
    for name in ["measure.pdf", "markup-space.pdf", "markup-deep-spaces.pdf"] {
        let Some(source) = sample("cyantakeoff-revu", name) else {
            continue;
        };
        let (_dir, path) = copy(&source);
        let mut s = session();
        let id = s.open(&path, None).unwrap();
        let doc = s.document_mut(id).unwrap();
        doc.annots().unwrap();
        assert!(!doc.has_bluebeam_columns(), "{name}");
        assert!(doc.bluebeam_columns().is_empty());
        assert!(!doc.save().unwrap().wrote(), "{name}");
    }
}

#[test]
fn a_count_group_takes_subject_lock_and_column_values_on_every_marker() {
    let Some(source) = sample("redcolumn", "A3-07-columns-statuses-metadata.pdf") else {
        return;
    };
    let (_dir, path) = copy(&source);
    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.annots().unwrap();
    let columns = doc.bluebeam_columns();
    let refno = columns.iter().find(|c| c.title == "Ref No.").unwrap().clone();
    let ref_index = refno.bluebeam.as_ref().unwrap().index;
    let head: AnnotId = doc
        .annots()
        .unwrap()
        .set()
        .iter()
        .find(|a| {
            a.meta.subject.as_deref() == Some("CT04 columns on a count")
                && a.count_group().as_ref() == Some(&a.id)
        })
        .unwrap()
        .id
        .clone();
    let group: Vec<AnnotId> = doc
        .annots()
        .unwrap()
        .set()
        .iter()
        .filter(|a| a.count_group().as_ref() == Some(&head))
        .map(|a| a.id.clone())
        .collect();
    assert!(group.len() > 1, "a count of several markers");
    let when = PdfDate::parse("D:20261124120000Z").unwrap();
    doc.set_subject(&head, "Renamed count", when).unwrap();
    doc.set_locked(&head, true, when).unwrap();
    doc.set_column_text(&head, &refno, "CT-count-2", when).unwrap();
    // One undo step each: undoing the column value puts it back on every marker.
    assert!(doc.undo().unwrap());
    for g in &group {
        assert_ne!(
            stored_text(doc.annotation(g).unwrap(), ref_index).as_deref(),
            Some("CT-count-2")
        );
    }
    assert!(doc.redo().unwrap());
    assert!(doc.save().unwrap().wrote());
    drop(s);

    let after = read(&path);
    let annots = annotations(&after);
    let mut seen = 0;
    for g in &group {
        let (_, _, dict) = annots.iter().find(|(n, _, _)| n == g.as_str()).unwrap();
        seen += 1;
        assert_eq!(
            dict.get(b"Subj").unwrap(),
            &pl_doc::pl_annot::encode_text("Renamed count"),
            "{g}"
        );
        let flags = dict.get(b"F").unwrap().as_i64().unwrap();
        assert_ne!(flags & 128, 0, "{g} locked");
        assert_ne!(flags & 4, 0, "{g} still printed");
    }
    assert_eq!(seen, group.len());

    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.annots().unwrap();
    for g in &group {
        let a = doc.annotation(g).unwrap();
        assert_eq!(stored_text(a, ref_index).as_deref(), Some("CT-count-2"), "{g}");
    }
}
