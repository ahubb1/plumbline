//! Lazy load, page tree and annotation reading against the pl-testkit manifests.

mod common;

use common::{fixtures, write_fixture};
use pl_geom::{Rect, Rotation};
use pl_pdf::{number, rect_from_object, Object, PdfError, PdfFile, XrefStyle};
use pl_testkit::XrefKind;

fn rect(v: [f64; 4]) -> Rect {
    Rect::new(v[0], v[1], v[2], v[3])
}

#[test]
fn open_is_lazy_and_parses_on_first_use() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &pl_testkit::plain());
    let mut file = PdfFile::open(&path).unwrap();
    assert!(!file.is_loaded());
    assert!(file.load_stats().is_none());
    assert_eq!(file.page_count().unwrap(), 1);
    assert!(file.is_loaded());
    let stats = file.load_stats().unwrap();
    assert_eq!(stats.file_len, std::fs::metadata(&path).unwrap().len());
    assert!(stats.object_count > 0);
    file.unload();
    assert!(!file.is_loaded());
    assert_eq!(file.document().unwrap().page_count(), 1);
}

#[test]
fn open_rejects_missing_and_non_pdf_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.pdf");
    assert!(matches!(
        PdfFile::open(&missing),
        Err(PdfError::Io { op: "open", .. })
    ));

    let text = dir.path().join("notes.pdf");
    std::fs::write(&text, b"just some text, no header").unwrap();
    assert!(matches!(PdfFile::open(&text), Err(PdfError::NotPdf { .. })));

    // A header but no body passes the cheap check and fails at parse time, typed.
    let stub = dir.path().join("stub.pdf");
    std::fs::write(&stub, b"%PDF-1.7\nnothing else").unwrap();
    let mut file = PdfFile::open(&stub).unwrap();
    assert!(matches!(file.document(), Err(PdfError::Parse(_))));
}

#[test]
fn page_tree_matches_every_manifest() {
    let dir = tempfile::tempdir().unwrap();
    for fx in fixtures() {
        let m = &fx.manifest;
        let path = write_fixture(dir.path(), &fx);
        let mut file = PdfFile::open(&path).unwrap();
        let doc = file
            .document()
            .unwrap_or_else(|e| panic!("{}: {e}", m.name));
        assert_eq!(doc.page_count(), m.page_count(), "{}", m.name);
        assert_eq!(doc.bytes(), &fx.bytes[..], "{}", m.name);
        let want_style = match m.xref {
            XrefKind::Table => XrefStyle::Table,
            XrefKind::Stream => XrefStyle::Stream,
        };
        assert_eq!(doc.xref_style(), want_style, "{}", m.name);
        assert_eq!(doc.has_object_streams(), m.object_streams, "{}", m.name);
        assert!(!doc.xref_was_repaired(), "{}", m.name);
        // A linearised file has two cross-reference sections from the start.
        let sections = if m.linearized { 2 } else { 1 };
        assert_eq!(doc.revision_count(), sections, "{}", m.name);
        assert_eq!(doc.is_linearized(), m.linearized, "{}", m.name);

        let pages = doc.pages().unwrap();
        for (page, truth) in pages.iter().zip(&m.pages) {
            let ctx = format!("{} page {}", m.name, page.index);
            assert_eq!(doc.page_index_of(page.id), Some(page.index), "{ctx}");
            assert_eq!(page.media_box, rect(truth.media_box), "{ctx}: MediaBox");
            assert_eq!(page.crop_box, truth.crop_box.map(rect), "{ctx}: CropBox");
            assert_eq!(
                page.rotate,
                Rotation::from_degrees(truth.rotate).unwrap(),
                "{ctx}: Rotate"
            );
            assert_eq!(page.user_unit, truth.user_unit, "{ctx}: UserUnit");
            // The geometry pl-geom derives agrees with the manifest's own arithmetic.
            assert_eq!(
                page.geometry.crop_box(),
                rect(truth.visible_box()),
                "{ctx}: visible box"
            );
            let shown = page.geometry.display_size_points();
            let want = truth.display_size();
            assert!(
                (shown.width - want[0]).abs() < 1e-9 && (shown.height - want[1]).abs() < 1e-9,
                "{ctx}: display"
            );
            if m.name == "annotations" {
                assert_eq!(page.annotation_count, truth.annotations.len(), "{ctx}");
            }
        }
    }
}

#[test]
fn annotation_dictionaries_are_returned_exactly_as_stored() {
    let fx = pl_testkit::annotations();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap();
    let truth = &fx.manifest.pages[0].annotations;
    let annots = doc.annotations(0).unwrap();
    assert_eq!(annots.len(), truth.len());
    assert_eq!(truth.len(), 4);
    for (slot, (entry, want)) in annots.iter().zip(truth).enumerate() {
        assert_eq!(entry.slot, slot);
        assert_eq!(entry.page_index, 0);
        let id = entry.id.expect("fixture annotations are indirect objects");
        // Identical to the stored object, key for key: nothing dropped, nothing added.
        assert_eq!(
            doc.object(id),
            Some(&Object::Dictionary(entry.dict.clone()))
        );
        assert_eq!(entry.subtype(), Some(want.subtype.as_bytes()));
        let r = rect_from_object(entry.dict.get(b"Rect").unwrap()).unwrap();
        assert_eq!(r, rect(want.rect));
        assert_eq!(
            entry.dict.get(b"Contents").unwrap().as_str().unwrap(),
            want.contents.as_bytes()
        );
        // The appearance stream is reachable through the re-exported object types.
        let ap = entry.dict.get(b"AP").unwrap().as_dict().unwrap();
        let n = doc
            .resolve(ap.get(b"N").unwrap())
            .unwrap()
            .as_stream()
            .unwrap();
        let bbox = rect_from_object(n.dict.get(b"BBox").unwrap()).unwrap();
        assert_eq!(bbox, rect(want.ap_bbox));
    }
    // A page without /Annots has none, and an out-of-range page is a typed error.
    let plain_path = write_fixture(dir.path(), &pl_testkit::plain());
    let mut plain = PdfFile::open(&plain_path).unwrap();
    assert!(plain.document().unwrap().annotations(0).unwrap().is_empty());
    assert!(matches!(
        plain.document().unwrap().annotations(7),
        Err(PdfError::PageOutOfRange { index: 7, count: 1 })
    ));
}

#[test]
fn numbers_keep_the_value_the_file_spelled() {
    assert_eq!(number(&Object::Integer(612)), Some(612.0));
    assert_eq!(number(&Object::Real(841.89)), Some(841.89));
    assert_eq!(number(&Object::Real(0.1)), Some(0.1));
    assert_eq!(number(&Object::Null), None);
}

#[test]
fn encrypted_fixtures_open_with_the_empty_user_password_and_report_permissions() {
    let dir = tempfile::tempdir().unwrap();
    for fx in [pl_testkit::encrypted_rc4(), pl_testkit::encrypted_aes256()] {
        let truth = fx.manifest.encryption.clone().unwrap();
        let path = write_fixture(dir.path(), &fx);
        for password in [None, Some(truth.owner_password.as_str())] {
            let mut file = match password {
                None => PdfFile::open(&path).unwrap(),
                Some(p) => PdfFile::open_with_password(&path, p).unwrap(),
            };
            let info = file
                .document()
                .unwrap()
                .encryption()
                .expect("encryption info");
            assert_eq!(info.version, truth.version, "{}", fx.manifest.name);
            assert_eq!(info.revision, truth.revision);
            assert_eq!(info.can_print, truth.can_print);
            assert_eq!(info.can_modify, truth.can_modify);
            assert_eq!(info.can_copy, truth.can_copy);
            assert_eq!(info.can_annotate, truth.can_annotate);
            assert_eq!(info.can_assemble, truth.can_assemble);
        }
    }
}

/// A file with a real user password: built here with lopdf because pl-testkit's encrypted
/// fixtures both use the empty user password.
#[test]
fn a_user_password_is_required_and_checked() {
    use lopdf::{Document, EncryptionState, EncryptionVersion, Permissions};
    let mut doc = Document::load_mem(&pl_testkit::plain().bytes).unwrap();
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner",
        user_password: "secret",
        key_length: 128,
        permissions: Permissions::all(),
    })
    .unwrap();
    doc.encrypt(&state).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.pdf");
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    std::fs::write(&path, &bytes).unwrap();

    let mut none = PdfFile::open(&path).unwrap();
    assert!(
        matches!(none.document(), Err(PdfError::PasswordRequired)),
        "no password"
    );
    let mut wrong = PdfFile::open_with_password(&path, "guess").unwrap();
    assert!(
        matches!(wrong.document(), Err(PdfError::WrongPassword)),
        "wrong password"
    );
    // After a prompt the same handle can be given the password.
    wrong.set_password(Some("secret"));
    assert_eq!(wrong.page_count().unwrap(), 1);
    let mut owner = PdfFile::open_with_password(&path, "owner").unwrap();
    assert_eq!(owner.page_count().unwrap(), 1);
}
