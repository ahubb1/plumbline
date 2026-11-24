//! Incremental save, full save, verification and failure behaviour on the pl-testkit fixtures.

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{
    add_square, contains, fixtures, write_fixture, SQUARE_AP, SQUARE_CONTENTS, SQUARE_RECT,
};
use hayro_syntax::object::{Array, Dict, Stream as HayroStream};
use hayro_syntax::Pdf;
use pl_pdf::{
    rect_from_object, tmp_path_for, verify_bytes, verify_file, DictPatch, Dictionary, Expected,
    FailPoint, Object, ObjectId, PdfDocument, PdfError, PdfFile, ReplaceMethod, SaveKind,
    SaveOptions, SecondParser, StringFormat, VerifyError, XrefStyle,
};
use pl_testkit::Fixture;

fn text(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

/// Annotation counts and the last annotation's appearance content, read by hayro-syntax alone.
fn hayro_view(bytes: &[u8], page_index: usize) -> (usize, Vec<usize>, Option<Vec<u8>>) {
    let pdf = Pdf::new(bytes.to_vec()).expect("hayro-syntax opens the file");
    let pages = pdf.pages();
    let counts: Vec<usize> = pages
        .iter()
        .map(|p| {
            p.raw()
                .get::<Array<'_>>(b"Annots")
                .map_or(0, |a| a.raw_iter().count())
        })
        .collect();
    let last_ap = pages[page_index]
        .raw()
        .get::<Array<'_>>(b"Annots")
        .and_then(|a| a.iter::<Dict<'_>>().last())
        .and_then(|annot| annot.get::<Dict<'_>>(b"AP"))
        .and_then(|ap| ap.get::<HayroStream<'_>>(b"N"))
        .and_then(|n| n.decoded().ok().map(|c| c.into_owned()));
    (pages.len(), counts, last_ap)
}

/// Everything the M0/M2 acceptance asks of one incremental save.
fn check_incremental(fx: &Fixture, dir: &Path) {
    let name = &fx.manifest.name;
    let path = write_fixture(dir, fx);
    let mut file = PdfFile::open(&path).unwrap();
    let before = file.document().unwrap().clone();
    let page_index = before.page_count() - 1;
    let counts_before = before.annotation_counts();

    let mut edits = before.edit();
    let (annot_id, ap_id) = add_square(&mut edits, &before, page_index);
    let report = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap_or_else(|e| panic!("{name}: {e}"));

    // --- the file on disk
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(report.kind, SaveKind::Incremental, "{name}");
    assert_eq!(report.original_len, fx.bytes.len() as u64, "{name}");
    assert_eq!(report.new_len, saved.len() as u64, "{name}");
    assert!(saved.len() > fx.bytes.len(), "{name}");
    assert_eq!(
        &saved[..fx.bytes.len()],
        &fx.bytes[..],
        "{name}: original bytes must be an exact prefix"
    );
    assert!(!tmp_path_for(&path).exists(), "{name}: tmp left behind");
    if cfg!(windows) {
        assert_eq!(report.replace, ReplaceMethod::ReplaceFile, "{name}");
    }

    // --- the appended section uses the previous revision's xref style
    let tail = &saved[fx.bytes.len()..];
    match before.xref_style() {
        XrefStyle::Table => {
            assert!(
                contains(tail, b"\nxref\n") && contains(tail, b"trailer"),
                "{name}: classic section expected"
            );
            assert!(!contains(tail, b"/XRef"), "{name}: no xref stream expected");
        }
        XrefStyle::Stream => {
            assert!(contains(tail, b"/XRef"), "{name}: xref stream expected");
            assert!(
                !contains(tail, b"trailer"),
                "{name}: no classic trailer expected"
            );
        }
    }
    assert!(
        contains(tail, format!("/Prev {}", before.xref_start()).as_bytes()),
        "{name}: /Prev"
    );
    assert!(tail.ends_with(b"%%EOF"), "{name}");
    // No second header in the appended region.
    assert!(!contains(tail, b"%PDF-"), "{name}");

    // --- verification report (lopdf + hayro-syntax, run inside the save)
    let v = report
        .verification
        .as_ref()
        .expect("verification on by default");
    assert!(v.prefix_checked, "{name}");
    assert_eq!(v.second_parser, SecondParser::Passed, "{name}");
    assert_eq!(v.page_count, fx.manifest.page_count(), "{name}");
    let mut want_counts = counts_before.clone();
    want_counts[page_index] += 1;
    assert_eq!(v.annotation_counts, want_counts, "{name}");

    // --- reopen from disk with lopdf (a fresh handle, nothing cached)
    let mut reopened = PdfFile::open(&path).unwrap();
    let after = reopened.document().unwrap();
    assert_eq!(after.page_count(), before.page_count(), "{name}");
    assert_eq!(after.annotation_counts(), want_counts, "{name}");
    assert_eq!(after.xref_style(), before.xref_style(), "{name}");
    assert_eq!(
        after.revision_count(),
        before.revision_count() + 1,
        "{name}"
    );
    assert_eq!(after.encryption(), before.encryption(), "{name}");
    assert_eq!(after.pages().unwrap(), {
        let mut pages = before.pages().unwrap();
        pages[page_index].annotation_count += 1;
        pages
    });
    // The handle that saved holds the same state without another parse from disk.
    assert!(file.is_loaded(), "{name}");
    assert_eq!(file.document().unwrap().bytes(), &saved[..], "{name}");

    // --- the Square and its hand-built appearance show up on reload
    let annots = after.annotations(page_index).unwrap();
    let square = annots.last().unwrap();
    assert_eq!(square.id, Some(annot_id), "{name}");
    assert_eq!(square.subtype(), Some(&b"Square"[..]), "{name}");
    assert_eq!(
        square.dict.get(b"Contents").unwrap().as_str().unwrap(),
        SQUARE_CONTENTS.as_bytes(),
        "{name}"
    );
    let r = rect_from_object(square.dict.get(b"Rect").unwrap()).unwrap();
    assert_eq!(
        [r.x0, r.y0, r.x1, r.y1],
        SQUARE_RECT.map(|v| v as f64),
        "{name}"
    );
    let n = square
        .dict
        .get(b"AP")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"N")
        .unwrap();
    assert_eq!(n, &Object::Reference(ap_id), "{name}");
    let stream = after.resolve(n).unwrap().as_stream().unwrap();
    assert_eq!(stream.content, SQUARE_AP, "{name}");
    assert_eq!(
        stream.dict.get(b"Subtype").unwrap().as_name().unwrap(),
        b"Form",
        "{name}"
    );

    // --- untouched objects: same value, and still served from the same bytes of the prefix
    let written = written_ids(&before, &edits, page_index);
    for id in before.object_ids() {
        if written.contains(&id) {
            continue;
        }
        assert_eq!(
            after.object(id),
            before.object(id),
            "{name}: object {id:?} changed"
        );
        assert_eq!(
            after.object_offset(id),
            before.object_offset(id),
            "{name}: object {id:?} moved"
        );
    }
    // Exactly the new objects plus the one /Annots holder were appended.
    assert_eq!(report.objects_written, written.len(), "{name}");
    // A cross-reference stream is itself an object, so that style adds one more.
    let xref_object = usize::from(before.xref_style() == XrefStyle::Stream);
    assert_eq!(
        after.object_count(),
        before.object_count() + 2 + xref_object,
        "{name}"
    );

    // --- hayro-syntax, independently of the save's own check
    let (pages, counts, last_ap) = hayro_view(&saved, page_index);
    assert_eq!(pages, fx.manifest.page_count(), "{name}");
    assert_eq!(counts, want_counts, "{name}");
    assert_eq!(
        last_ap.as_deref(),
        Some(SQUARE_AP),
        "{name}: hayro reads the new appearance stream"
    );

    // --- encrypted files: the appended objects are ciphertext, and /Encrypt is carried over
    if fx.manifest.encryption.is_some() {
        assert!(
            !contains(tail, SQUARE_CONTENTS.as_bytes()),
            "{name}: /Contents appended in the clear"
        );
        assert!(
            !contains(tail, SQUARE_AP),
            "{name}: appearance stream appended in the clear"
        );
        assert!(contains(tail, b"/Encrypt"), "{name}: trailer lost /Encrypt");
    } else {
        assert!(contains(tail, SQUARE_CONTENTS.as_bytes()), "{name}");
    }
}

/// Ids an "add one annotation to `page_index`" save has to write.
fn written_ids(
    before: &PdfDocument,
    edits: &pl_pdf::EditSet,
    page_index: usize,
) -> BTreeSet<ObjectId> {
    let mut ids: BTreeSet<ObjectId> = edits.new_object_ids().collect();
    let page_id = before.page_id(page_index).unwrap();
    let page = before.object(page_id).unwrap().as_dict().unwrap();
    match page.get(b"Annots") {
        Ok(Object::Reference(array_id)) => ids.insert(*array_id),
        _ => ids.insert(page_id),
    };
    ids
}

#[test]
fn incremental_save_on_every_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let all = fixtures();
    assert!(all.len() >= 14);
    for fx in &all {
        check_incremental(fx, dir.path());
    }
}

#[test]
fn classic_and_xref_stream_fixtures_keep_their_own_style() {
    let dir = tempfile::tempdir().unwrap();
    let classic = pl_testkit::xref_classic();
    let streams = pl_testkit::xref_object_streams();
    check_incremental(&classic, dir.path());
    check_incremental(&streams, dir.path());

    // In the object-stream fixture the page lives inside a compressed object stream; the
    // new version of it is written as an ordinary object that overrides the compressed one.
    let path = dir.path().join(streams.manifest.file_name());
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap();
    assert_eq!(doc.xref_style(), XrefStyle::Stream);
    assert!(doc.has_object_streams());
    let last = doc.page_count() - 1;
    assert_eq!(doc.annotations(last).unwrap().len(), 1);
}

#[test]
fn encrypted_incremental_save_reopens_with_both_passwords() {
    let dir = tempfile::tempdir().unwrap();
    for fx in [pl_testkit::encrypted_rc4(), pl_testkit::encrypted_aes256()] {
        let truth = fx.manifest.encryption.clone().unwrap();
        // Opened with the owner password instead of the empty user password.
        let path = write_fixture(dir.path(), &fx);
        let mut file = PdfFile::open_with_password(&path, &truth.owner_password).unwrap();
        let doc = file.document().unwrap().clone();
        let mut edits = doc.edit();
        add_square(&mut edits, &doc, 0);
        let report = file
            .save_incremental(&edits, &SaveOptions::default())
            .unwrap();
        assert_eq!(
            report.verification.unwrap().second_parser,
            SecondParser::Passed
        );

        let saved = std::fs::read(&path).unwrap();
        assert_eq!(&saved[..fx.bytes.len()], &fx.bytes[..]);
        // Page content of the original revision still decrypts, and so does the new annotation.
        for password in [None, Some(truth.owner_password.as_str())] {
            let doc = PdfDocument::from_bytes(saved.clone(), password).unwrap();
            let square = doc.annotations(0).unwrap().pop().unwrap();
            assert_eq!(
                square.dict.get(b"Contents").unwrap().as_str().unwrap(),
                SQUARE_CONTENTS.as_bytes()
            );
            assert_eq!(doc.encryption().unwrap().can_modify, truth.can_modify);
        }
        let lo = lopdf::Document::load_mem(&saved).unwrap();
        let first_page = lo.page_iter().next().unwrap();
        let content = lo.get_page_content(first_page);
        assert!(
            contains(&content, pl_testkit::encrypted_token(0).as_bytes()),
            "{}",
            fx.manifest.name
        );
    }
}

#[test]
fn modify_changes_only_the_dirty_keys_and_keeps_unknown_keys() {
    let fx = pl_testkit::annotations();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();

    // Revision 2: plant keys this crate knows nothing about on annotation 0.
    let before = file.document().unwrap().clone();
    let annots = before.annotations(0).unwrap();
    let target = annots[0].id.unwrap();
    let mut private = Dictionary::new();
    private.set("Foo", Object::Array(vec![1.into(), 2.into(), 3.into()]));
    private.set("Bar", text("opaque"));
    let mut edits = before.edit();
    edits
        .patch(target)
        .set("XVendorPrivate", Object::Dictionary(private.clone()))
        .set("XFlag", true);
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let rev2 = std::fs::read(&path).unwrap();

    // Revision 3: an ordinary edit of two standard keys, plus one removal.
    let mid = file.document().unwrap().clone();
    let original = mid.annotations(0).unwrap()[0].dict.clone();
    assert_eq!(
        original.get(b"XVendorPrivate").unwrap(),
        &Object::Dictionary(private.clone())
    );
    let mut patch = DictPatch::new();
    patch
        .set("Contents", text("edited"))
        .set("C", Object::Array(vec![0.into(), 0.into(), 1.into()]))
        .remove("XFlag");
    assert_eq!(patch.dirty_keys().count(), 3);
    let mut edits = mid.edit();
    edits.modify_annotation(target, patch);
    let report = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    assert_eq!(
        report.objects_written, 1,
        "only the modified annotation is rewritten"
    );

    let rev3 = std::fs::read(&path).unwrap();
    assert_eq!(&rev3[..rev2.len()], &rev2[..]);
    assert_eq!(&rev3[..fx.bytes.len()], &fx.bytes[..]);

    let after = PdfDocument::from_bytes(rev3, None).unwrap();
    assert_eq!(after.revision_count(), 3);
    let edited = after.annotations(0).unwrap()[0].dict.clone();
    assert_eq!(
        edited.get(b"Contents").unwrap().as_str().unwrap(),
        b"edited"
    );
    assert!(!edited.has(b"XFlag"));
    assert_eq!(
        edited.get(b"XVendorPrivate").unwrap(),
        &Object::Dictionary(private)
    );
    // Every other key is exactly what the file had.
    for (key, value) in original.iter() {
        if [&b"Contents"[..], b"C", b"XFlag"].contains(&key.as_slice()) {
            continue;
        }
        assert_eq!(
            edited.get(key).unwrap(),
            value,
            "key /{}",
            String::from_utf8_lossy(key)
        );
    }
    assert_eq!(edited.len(), original.len() - 1);
    // Every other object is untouched, including the three other annotations.
    for id in mid.object_ids().filter(|id| *id != target) {
        assert_eq!(after.object(id), mid.object(id));
        assert_eq!(after.object_offset(id), mid.object_offset(id));
    }
    assert_eq!(after.annotation_counts(), vec![4]);
}

#[test]
fn delete_removes_the_annotation_from_the_page_only() {
    let fx = pl_testkit::annotations();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let before = file.document().unwrap().clone();
    let ids: Vec<ObjectId> = before
        .annotations(0)
        .unwrap()
        .iter()
        .map(|a| a.id.unwrap())
        .collect();

    let mut edits = before.edit();
    edits.delete_annotation(0, ids[1]);
    let report = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    assert_eq!(
        report.objects_written, 1,
        "only the /Annots holder is rewritten"
    );

    let saved = std::fs::read(&path).unwrap();
    assert_eq!(&saved[..fx.bytes.len()], &fx.bytes[..]);
    let after = PdfDocument::from_bytes(saved, None).unwrap();
    let left: Vec<ObjectId> = after
        .annotations(0)
        .unwrap()
        .iter()
        .map(|a| a.id.unwrap())
        .collect();
    assert_eq!(
        left,
        vec![ids[0], ids[2], ids[3]],
        "order of the survivors is kept"
    );
    // Incremental: the object is still in the file, just unreferenced.
    assert_eq!(after.object(ids[1]), before.object(ids[1]));

    // A full save drops it.
    let mut file = PdfFile::open(&path).unwrap();
    let empty = file.edit().unwrap();
    let report = file.save_full(&empty, &SaveOptions::default()).unwrap();
    assert_eq!(report.kind, SaveKind::Full);
    let doc = file.document().unwrap();
    assert!(doc.object(ids[1]).is_none());
    assert_eq!(doc.annotation_counts(), vec![3]);
    assert_eq!(doc.revision_count(), 1);
}

#[test]
fn add_and_delete_in_one_set_cancel_out() {
    let fx = pl_testkit::plain();
    let doc = PdfDocument::from_bytes(fx.bytes.clone(), None).unwrap();
    let mut edits = doc.edit();
    let (id, _) = add_square(&mut edits, &doc, 0);
    edits.delete_annotation(0, id);
    assert_eq!(
        edits.new_object_ids().count(),
        1,
        "only the orphaned appearance stream is left"
    );
}

#[test]
fn reserved_ids_allow_mutual_references() {
    let fx = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let mut edits = file.edit().unwrap();

    let markup = edits.reserve_id();
    let popup = edits.reserve_id();
    // Saving with an unfilled reservation is refused before anything is written.
    edits.attach_annotation(0, markup);
    let err = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap_err();
    assert!(matches!(err, PdfError::ReservedIdUnset(_)), "{err:?}");
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);

    let mut m = Dictionary::new();
    m.set("Type", Object::Name(b"Annot".to_vec()));
    m.set("Subtype", Object::Name(b"Text".to_vec()));
    m.set(
        "Rect",
        Object::Array(vec![10.into(), 10.into(), 30.into(), 30.into()]),
    );
    m.set("Popup", Object::Reference(popup));
    let mut p = Dictionary::new();
    p.set("Type", Object::Name(b"Annot".to_vec()));
    p.set("Subtype", Object::Name(b"Popup".to_vec()));
    p.set(
        "Rect",
        Object::Array(vec![40.into(), 40.into(), 140.into(), 100.into()]),
    );
    p.set("Parent", Object::Reference(markup));
    edits.set_object(markup, Object::Dictionary(m));
    edits.set_object(popup, Object::Dictionary(p));
    edits.attach_annotation(0, popup);
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();

    let doc = file.document().unwrap();
    let annots = doc.annotations(0).unwrap();
    assert_eq!(annots.len(), 2);
    assert_eq!(
        annots[0].dict.get(b"Popup").unwrap(),
        &Object::Reference(annots[1].id.unwrap())
    );
    assert_eq!(
        annots[1].dict.get(b"Parent").unwrap(),
        &Object::Reference(annots[0].id.unwrap())
    );
}

#[test]
fn full_save_on_every_fixture() {
    let dir = tempfile::tempdir().unwrap();
    for fx in fixtures() {
        let name = &fx.manifest.name;
        let path = write_fixture(dir.path(), &fx);
        let mut file = PdfFile::open(&path).unwrap();
        let before = file.document().unwrap().clone();
        let mut edits = before.edit();
        let (annot_id, _) = add_square(&mut edits, &before, 0);
        let report = file
            .save_full(&edits, &SaveOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let v = report.verification.unwrap();
        assert!(!v.prefix_checked, "{name}");
        assert_eq!(v.second_parser, SecondParser::Passed, "{name}");
        assert!(!tmp_path_for(&path).exists(), "{name}");

        let saved = std::fs::read(&path).unwrap();
        let mut reopened = PdfFile::open(&path).unwrap();
        let after = reopened.document().unwrap();
        let mut want = before.annotation_counts();
        want[0] += 1;
        assert_eq!(after.page_count(), before.page_count(), "{name}");
        assert_eq!(after.annotation_counts(), want, "{name}");
        assert_eq!(
            after.revision_count(),
            1,
            "{name}: a full save is one revision"
        );
        assert_eq!(after.xref_style(), before.xref_style(), "{name}");
        assert_eq!(
            after.has_object_streams(),
            before.has_object_streams(),
            "{name}"
        );
        // Page geometry is unchanged.
        let geometry = |d: &PdfDocument| {
            d.pages()
                .unwrap()
                .into_iter()
                .map(|p| p.geometry)
                .collect::<Vec<_>>()
        };
        assert_eq!(geometry(after), geometry(&before), "{name}");
        let square = after.annotations(0).unwrap().pop().unwrap();
        assert_eq!(square.id, Some(annot_id), "{name}: ids are not renumbered");
        assert_eq!(
            square.dict.get(b"Contents").unwrap().as_str().unwrap(),
            SQUARE_CONTENTS.as_bytes(),
            "{name}"
        );
        let (pages, counts, last_ap) = hayro_view(&saved, 0);
        assert_eq!((pages, counts), (before.page_count(), want), "{name}");
        assert_eq!(last_ap.as_deref(), Some(SQUARE_AP), "{name}");

        // Protection is kept, not stripped.
        assert_eq!(after.encryption(), before.encryption(), "{name}");
        if fx.manifest.encryption.is_some() {
            assert!(contains(&saved, b"/Encrypt"), "{name}");
            assert!(
                !contains(&saved, SQUARE_CONTENTS.as_bytes()),
                "{name}: written in the clear"
            );
        }
    }
}

#[test]
fn a_second_incremental_save_stacks_on_the_first() {
    let fx = pl_testkit::xref_object_streams();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();

    let doc = file.document().unwrap().clone();
    let mut first = doc.edit();
    add_square(&mut first, &doc, 0);
    file.save_incremental(&first, &SaveOptions::default())
        .unwrap();
    let rev2 = std::fs::read(&path).unwrap();

    // The first edit set belongs to the old state and is refused, with the file untouched.
    let err = file
        .save_incremental(&first, &SaveOptions::default())
        .unwrap_err();
    assert!(matches!(err, PdfError::StaleEditSet), "{err:?}");
    assert_eq!(std::fs::read(&path).unwrap(), rev2);

    let doc = file.document().unwrap().clone();
    let mut second = doc.edit();
    add_square(&mut second, &doc, 0);
    add_square(&mut second, &doc, 2);
    file.save_incremental(&second, &SaveOptions::default())
        .unwrap();
    let rev3 = std::fs::read(&path).unwrap();
    assert_eq!(&rev3[..rev2.len()], &rev2[..]);
    assert_eq!(&rev3[..fx.bytes.len()], &fx.bytes[..]);

    let after = PdfDocument::from_bytes(rev3.clone(), None).unwrap();
    assert_eq!(after.revision_count(), 3);
    assert_eq!(after.annotation_counts(), vec![2, 0, 1]);
    assert_eq!(hayro_view(&rev3, 0).1, vec![2, 0, 1]);
}

#[test]
fn an_empty_edit_set_is_not_appended() {
    let fx = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let edits = file.edit().unwrap();
    assert!(edits.is_empty());
    let err = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap_err();
    assert!(matches!(err, PdfError::NothingToSave), "{err:?}");
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);
    assert!(!tmp_path_for(&path).exists());
}

#[test]
fn invalid_edits_are_refused_before_anything_is_written() {
    let fx = pl_testkit::annotations();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let catalog_ref = doc.trailer().get(b"Root").unwrap().as_reference().unwrap();

    let mut wrong_page = doc.edit();
    add_square(&mut wrong_page, &doc, 0);
    wrong_page.attach_annotation(9, (1, 0));
    let mut not_on_page = doc.edit();
    not_on_page.delete_annotation(0, catalog_ref);
    let mut missing = doc.edit();
    missing.patch((9_999, 0)).set("Contents", text("x"));
    let mut not_dict = doc.edit();
    let number = not_dict.add_object(Object::Integer(7));
    not_dict.patch(number).set("K", 1);

    for (edits, what) in [
        (wrong_page, "page"),
        (not_on_page, "annot"),
        (missing, "object"),
        (not_dict, "dict"),
    ] {
        let err = file
            .save_incremental(&edits, &SaveOptions::default())
            .unwrap_err();
        let ok = match what {
            "page" => matches!(err, PdfError::PageOutOfRange { index: 9, .. }),
            "annot" => matches!(err, PdfError::AnnotationNotOnPage { page_index: 0, .. }),
            "object" => matches!(err, PdfError::ObjectNotFound((9_999, 0))),
            _ => matches!(err, PdfError::NotADictionary(_)),
        };
        assert!(ok, "{what}: {err:?}");
        assert_eq!(std::fs::read(&path).unwrap(), fx.bytes, "{what}");
        assert!(!tmp_path_for(&path).exists(), "{what}");
        // The parsed document was not consumed by the refused save.
        assert!(file.is_loaded(), "{what}");
    }
}

#[test]
fn a_failure_between_tmp_write_and_replace_leaves_the_original_untouched() {
    let dir = tempfile::tempdir().unwrap();
    for fx in [
        pl_testkit::annotations(),
        pl_testkit::xref_object_streams(),
        pl_testkit::encrypted_rc4(),
    ] {
        let name = &fx.manifest.name;
        let path = write_fixture(dir.path(), &fx);
        let tmp = tmp_path_for(&path);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();

        for (kind, point, keep) in [
            (SaveKind::Incremental, FailPoint::AfterTmpWrite, false),
            (SaveKind::Incremental, FailPoint::BeforeReplace, false),
            (SaveKind::Incremental, FailPoint::BeforeReplace, true),
            (SaveKind::Full, FailPoint::AfterTmpWrite, false),
            (SaveKind::Full, FailPoint::BeforeReplace, true),
        ] {
            let mut file = PdfFile::open(&path).unwrap();
            let doc = file.document().unwrap().clone();
            let mut edits = doc.edit();
            add_square(&mut edits, &doc, 0);
            let options = SaveOptions {
                fail_point: Some(point),
                keep_tmp_on_failure: keep,
                ..SaveOptions::default()
            };
            let err = match kind {
                SaveKind::Incremental => file.save_incremental(&edits, &options),
                SaveKind::Full => file.save_full(&edits, &options),
            }
            .unwrap_err();
            assert!(
                matches!(err, PdfError::InjectedFailure(p) if p == point),
                "{name}: {err:?}"
            );

            // The original: same bytes, same timestamp.
            assert_eq!(
                std::fs::read(&path).unwrap(),
                fx.bytes,
                "{name} {kind:?} {point:?}"
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().modified().unwrap(),
                modified,
                "{name}"
            );
            // The tmp is what a killed process would leave: complete, and removed unless kept.
            assert_eq!(tmp.exists(), keep, "{name} {kind:?} {point:?}");
            if keep {
                let left = std::fs::read(&tmp).unwrap();
                assert!(left.len() > 100 && left.starts_with(b"%PDF-"), "{name}");
            }

            // The handle recovers by re-reading the intact original, and the same edits then save
            // (a stale tmp from the failed attempt is simply overwritten).
            assert!(!file.is_loaded(), "{name}");
            assert_eq!(file.document().unwrap().bytes(), &fx.bytes[..], "{name}");
            let retry_target = dir.path().join(format!("{name}-retry.pdf"));
            file.save_incremental_as(&retry_target, &edits, &SaveOptions::default())
                .unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                fx.bytes,
                "{name}: save-as must not touch the source"
            );
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

#[cfg(windows)]
#[test]
fn a_refused_replace_leaves_the_original_untouched_and_cleans_up() {
    use std::os::windows::fs::OpenOptionsExt;
    let fx = pl_testkit::annotations();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 0);

    // Another program holds the file open without sharing: neither ReplaceFileW nor
    // MoveFileExW can take its place.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let err = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap_err();
    assert!(matches!(err, PdfError::Replace { .. }), "{err:?}");
    drop(lock);

    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);
    assert!(!tmp_path_for(&path).exists());
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        leftovers.len(),
        1,
        "only the original remains: {leftovers:?}"
    );

    // Once the other program lets go, the same save goes through.
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    assert_eq!(file.document().unwrap().annotation_counts(), vec![5]);
}

#[test]
fn save_as_writes_a_new_file_and_leaves_the_source_alone() {
    let fx = pl_testkit::rotated_pages();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let dest = dir.path().join("copy with markup.pdf");
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    for page in 0..doc.page_count() {
        add_square(&mut edits, &doc, page);
    }
    let report = file
        .save_incremental_as(&dest, &edits, &SaveOptions::default())
        .unwrap();
    assert_eq!(report.path, dest);
    if cfg!(windows) {
        assert_eq!(
            report.replace,
            ReplaceMethod::MoveFile,
            "a new destination is a plain move"
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);
    assert_eq!(file.path(), path.as_path());
    assert_eq!(file.document().unwrap().annotation_counts(), vec![0; 4]);

    let copy = std::fs::read(&dest).unwrap();
    assert_eq!(&copy[..fx.bytes.len()], &fx.bytes[..]);
    let after = PdfDocument::from_bytes(copy, None).unwrap();
    assert_eq!(after.annotation_counts(), vec![1; 4]);
    // The annotation rectangle is stored in unrotated page space on every page, whatever /Rotate is.
    for page in 0..4 {
        let square = after.annotations(page).unwrap().pop().unwrap();
        let r = rect_from_object(square.dict.get(b"Rect").unwrap()).unwrap();
        assert_eq!([r.x0, r.y0, r.x1, r.y1], SQUARE_RECT.map(|v| v as f64));
    }
    // Full save-as too.
    let full = dir.path().join("rewritten.pdf");
    file.save_full_as(&full, &edits, &SaveOptions::default())
        .unwrap();
    assert_eq!(
        PdfFile::open(&full)
            .unwrap()
            .document()
            .unwrap()
            .annotation_counts(),
        vec![1; 4]
    );
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);
}

#[test]
fn saving_without_verification_still_replaces_safely() {
    let fx = pl_testkit::plain();
    let dir = tempfile::tempdir().unwrap();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 0);
    let report = file
        .save_incremental(
            &edits,
            &SaveOptions {
                verify: false,
                ..SaveOptions::default()
            },
        )
        .unwrap();
    assert!(report.verification.is_none());
    // Without a verified re-parse to adopt, the handle reloads lazily.
    assert!(!file.is_loaded());
    assert_eq!(file.document().unwrap().annotation_counts(), vec![1]);
    // The public verifier gives the same answer after the fact.
    let expected = Expected {
        page_count: 1,
        annotation_counts: vec![1],
    };
    let v = verify_file(&path, None, &expected, Some(&fx.bytes)).unwrap();
    assert!(v.prefix_checked);
    assert_eq!(v.second_parser, SecondParser::Passed);
}

#[test]
fn verification_rejects_wrong_counts_and_a_broken_prefix() {
    let fx = pl_testkit::annotations();
    let good = Expected {
        page_count: 1,
        annotation_counts: vec![4],
    };
    assert!(verify_bytes(fx.bytes.clone(), None, &good, Some(&fx.bytes)).is_ok());

    let wrong_annots = Expected {
        page_count: 1,
        annotation_counts: vec![5],
    };
    let err = verify_bytes(fx.bytes.clone(), None, &wrong_annots, None).unwrap_err();
    assert!(
        matches!(
            err,
            PdfError::Verify(VerifyError::AnnotationCount {
                page_index: 0,
                expected: 5,
                found: 4,
                ..
            })
        ),
        "{err:?}"
    );

    let wrong_pages = Expected {
        page_count: 2,
        annotation_counts: vec![4, 0],
    };
    let err = verify_bytes(fx.bytes.clone(), None, &wrong_pages, None).unwrap_err();
    assert!(
        matches!(
            err,
            PdfError::Verify(VerifyError::PageCount {
                expected: 2,
                found: 1,
                ..
            })
        ),
        "{err:?}"
    );

    let mut other = fx.bytes.clone();
    other[20] ^= 0x01;
    let err = verify_bytes(fx.bytes.clone(), None, &good, Some(&other)).unwrap_err();
    assert!(
        matches!(
            err,
            PdfError::Verify(VerifyError::PrefixMismatch { offset: 20 })
        ),
        "{err:?}"
    );
}

#[test]
fn a_file_with_damaged_xref_refuses_incremental_save_but_takes_a_full_save() {
    let fx = pl_testkit::xref_classic();
    // Point startxref somewhere useless; lopdf repairs the table by scanning.
    let marker = b"startxref\n";
    let at = fx
        .bytes
        .windows(marker.len())
        .rposition(|w| w == marker)
        .unwrap()
        + marker.len();
    let mut damaged = fx.bytes[..at].to_vec();
    damaged.extend_from_slice(b"7\n%%EOF\n");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("damaged.pdf");
    std::fs::write(&path, &damaged).unwrap();

    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    assert!(doc.xref_was_repaired());
    assert_eq!(doc.page_count(), fx.manifest.page_count());
    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 0);
    let err = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap_err();
    assert!(
        matches!(err, PdfError::IncrementalUnsupported(_)),
        "{err:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), damaged);

    file.save_full(&edits, &SaveOptions::default()).unwrap();
    let after = file.document().unwrap();
    assert!(!after.xref_was_repaired());
    assert_eq!(after.annotation_counts()[0], 1);
}

/// Rewrites the value after the last `startxref` keyword.
fn with_startxref(bytes: &[u8], value: &str) -> Vec<u8> {
    let key = b"startxref";
    let at = bytes.windows(key.len()).rposition(|w| w == key).unwrap() + key.len();
    let digits_from = at + bytes[at..].iter().position(u8::is_ascii_digit).unwrap();
    let digits = bytes[digits_from..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    let mut out = bytes[..digits_from].to_vec();
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(&bytes[digits_from + digits..]);
    out
}

#[test]
fn verification_rejects_a_save_whose_xref_data_only_reads_after_repair() {
    // The writer faults an incremental save is most likely to have are in the appended xref
    // section, trailer or startxref. Both parsers repair those silently by scanning the file,
    // so the counts still match; verification has to notice the repair itself.
    let dir = tempfile::tempdir().unwrap();
    for name in ["xref_classic", "xref_object_streams", "linearized"] {
        let fx = fixtures()
            .into_iter()
            .find(|f| f.manifest.name == name)
            .unwrap();
        let path = write_fixture(dir.path(), &fx);
        let out = dir.path().join(format!("{name}-saved.pdf"));
        let mut file = PdfFile::open(&path).unwrap();
        let doc = file.document().unwrap().clone();
        let mut edits = doc.edit();
        add_square(&mut edits, &doc, 0);
        file.save_incremental_as(&out, &edits, &SaveOptions::default())
            .unwrap();
        let saved = std::fs::read(&out).unwrap();
        let mut counts = doc.annotation_counts();
        counts[0] += 1;
        let expected = Expected {
            page_count: doc.page_count(),
            annotation_counts: counts,
        };
        // The intact save passes the public check, prefix included.
        verify_bytes(saved.clone(), None, &expected, Some(&fx.bytes))
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        // startxref pointing into the header: lopdf rebuilds the table and sees every count.
        let damaged = with_startxref(&saved, "7");
        assert_eq!(damaged[..fx.bytes.len()], fx.bytes[..]);
        let repaired = PdfDocument::from_bytes(damaged.clone(), None)
            .unwrap_or_else(|e| panic!("{name}: lopdf no longer repairs this: {e}"));
        assert!(
            repaired.xref_was_repaired(),
            "{name}: the damage must be one lopdf repairs"
        );
        let err = verify_bytes(damaged, None, &expected, Some(&fx.bytes)).unwrap_err();
        assert!(
            matches!(
                err,
                PdfError::Verify(VerifyError::XrefRepaired { parser: "lopdf" })
            ),
            "{name}: {err:?}"
        );

        // startxref pointing at the previous revision's section: a perfectly readable file,
        // read as written, in which the appended revision is simply not used.
        let stale = with_startxref(&saved, &doc.xref_start().to_string());
        let err = verify_bytes(stale, None, &expected, Some(&fx.bytes)).unwrap_err();
        assert!(
            matches!(
                err,
                PdfError::Verify(
                    VerifyError::XrefNotAppended { .. } | VerifyError::AnnotationCount { .. }
                )
            ),
            "{name}: {err:?}"
        );

        // A trailer whose /Prev does not chain to the previous revision.
        let prev = format!("/Prev {}", doc.xref_start());
        let at = saved
            .windows(prev.len())
            .rposition(|w| w == prev.as_bytes())
            .unwrap();
        assert!(
            at >= fx.bytes.len(),
            "{name}: /Prev is in the appended part"
        );
        let mut wrong_prev = saved.clone();
        // Same width, different section: the first digit is replaced so offsets do not move.
        let digit = at + "/Prev ".len();
        wrong_prev[digit] = if wrong_prev[digit] == b'9' {
            b'8'
        } else {
            b'9'
        };
        match verify_bytes(wrong_prev, None, &expected, Some(&fx.bytes)) {
            Err(PdfError::Verify(
                VerifyError::PrevMismatch { .. }
                | VerifyError::XrefRepaired { .. }
                | VerifyError::Reparse { .. },
            )) => {}
            other => panic!("{name}: a broken /Prev chain was accepted: {other:?}"),
        }
    }
}

#[test]
fn an_in_place_save_refuses_a_file_another_program_changed() {
    let dir = tempfile::tempdir().unwrap();
    let fx = pl_testkit::xref_classic();
    let path = write_fixture(dir.path(), &fx);

    // Handle A loads the file and prepares one annotation.
    let mut a = PdfFile::open(&path).unwrap();
    let doc_a = a.document().unwrap().clone();
    let mut edits_a = doc_a.edit();
    add_square(&mut edits_a, &doc_a, 0);

    // Handle B (another program) saves two annotations to the same path in the meantime.
    let mut b = PdfFile::open(&path).unwrap();
    let doc_b = b.document().unwrap().clone();
    let mut edits_b = doc_b.edit();
    add_square(&mut edits_b, &doc_b, 0);
    add_square(&mut edits_b, &doc_b, 1);
    b.save_incremental(&edits_b, &SaveOptions::default())
        .unwrap();
    let theirs = std::fs::read(&path).unwrap();
    assert!(theirs.len() > fx.bytes.len());

    // A's incremental save must not discard B's work...
    let err = a
        .save_incremental(&edits_a, &SaveOptions::default())
        .unwrap_err();
    assert!(
        matches!(&err, PdfError::ChangedOnDisk { path: p } if p == &path),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        theirs,
        "the file is as B left it"
    );
    assert!(!tmp_path_for(&path).exists());
    // ... and neither must a full save.
    a.unload();
    // (Reloading is what clears the condition: A now sees B's file.)
    let mut stale = PdfFile::open(&path).unwrap();
    let doc = stale.document().unwrap().clone();
    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 2);
    std::fs::write(&path, &fx.bytes).unwrap(); // a third program puts the old file back
    let err = stale
        .save_full(&edits, &SaveOptions::default())
        .unwrap_err();
    assert!(matches!(err, PdfError::ChangedOnDisk { .. }), "{err:?}");
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);

    // Saving under another name is the way out and is not blocked.
    let copy = dir.path().join("copy.pdf");
    let mut again = PdfFile::open(&path).unwrap();
    let doc = again.document().unwrap().clone();
    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 0);
    std::fs::write(&path, &theirs).unwrap();
    again
        .save_incremental_as(&copy, &edits, &SaveOptions::default())
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), theirs);

    // Back-to-back in-place saves through one handle still work: the stamp follows the file.
    let mut file = PdfFile::open(&copy).unwrap();
    for _ in 0..2 {
        let doc = file.document().unwrap().clone();
        let mut edits = doc.edit();
        add_square(&mut edits, &doc, 0);
        file.save_incremental(&edits, &SaveOptions::default())
            .unwrap();
    }
    // Also with verification off, where the handle reloads from disk in between.
    let quick = SaveOptions {
        verify: false,
        ..SaveOptions::default()
    };
    for _ in 0..2 {
        let doc = file.document().unwrap().clone();
        let mut edits = doc.edit();
        add_square(&mut edits, &doc, 0);
        file.save_incremental(&edits, &quick).unwrap();
    }
}

#[test]
fn a_touched_but_identical_file_is_still_saved_in_place() {
    // A backup or sync tool that rewrites the same bytes changes the modification time only.
    let dir = tempfile::tempdir().unwrap();
    let fx = pl_testkit::xref_classic();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 0);
    let handle = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    handle
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(90))
        .unwrap();
    drop(handle);
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    assert!(std::fs::read(&path).unwrap().len() > fx.bytes.len());
}

#[test]
fn an_edit_set_is_stale_after_any_save_even_one_that_adds_no_object_numbers() {
    // Deleting or patching on a classic-table file leaves the highest object number alone, so
    // the number cannot be what tells a stale set from a current one.
    let dir = tempfile::tempdir().unwrap();
    let fx = pl_testkit::annotations();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let annots = doc.annotations(0).unwrap();
    let count = annots.len();
    let (first, second) = (annots[0].id.unwrap(), annots[1].id.unwrap());

    let mut delete = doc.edit();
    delete.delete_annotation(0, first);
    // Started before the save below: it re-attaches an existing annotation.
    let mut early = doc.edit();
    early.attach_annotation(0, second);
    let mut early_patch = doc.edit();
    early_patch.patch(second).set("Contents", text("late"));

    let max_before = doc.max_object_id();
    file.save_incremental(&delete, &SaveOptions::default())
        .unwrap();
    assert_eq!(
        file.document().unwrap().max_object_id(),
        max_before,
        "the test needs a save that adds no object numbers"
    );
    let after_delete = std::fs::read(&path).unwrap();
    for stale in [&early, &early_patch, &delete] {
        let err = file
            .save_incremental(stale, &SaveOptions::default())
            .unwrap_err();
        assert!(matches!(err, PdfError::StaleEditSet), "{err:?}");
        let err = file.save_full(stale, &SaveOptions::default()).unwrap_err();
        assert!(matches!(err, PdfError::StaleEditSet), "{err:?}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), after_delete);
    assert_eq!(file.document().unwrap().annotation_counts()[0], count - 1);

    // A set started from a clone of the current state is accepted; one from a reload of the
    // same bytes is a different state and is not.
    let current = file.document().unwrap().clone();
    let other_load = PdfDocument::from_bytes(after_delete.clone(), None).unwrap();
    let mut foreign = other_load.edit();
    foreign.patch(second).set("Contents", text("foreign"));
    assert!(matches!(
        file.save_incremental(&foreign, &SaveOptions::default()),
        Err(PdfError::StaleEditSet)
    ));
    let mut fresh = current.edit();
    fresh.patch(second).set("Contents", text("fresh"));
    file.save_incremental(&fresh, &SaveOptions::default())
        .unwrap();
}

#[test]
fn a_patch_only_save_is_checked_object_by_object() {
    // Page and annotation counts do not change in a patch-only save, so they prove nothing
    // about it; the verification compares what was written with the re-parse instead.
    let dir = tempfile::tempdir().unwrap();
    for name in ["annotations", "encrypted_rc4", "encrypted_aes256"] {
        let fx = fixtures()
            .into_iter()
            .find(|f| f.manifest.name == name)
            .unwrap();
        let path = write_fixture(dir.path(), &fx);
        let mut file = PdfFile::open(&path).unwrap();
        let doc = file.document().unwrap().clone();
        let page = doc.page_id(0).unwrap();
        let mut edits = doc.edit();
        edits
            .patch(page)
            .set("PlumbTest", text("patched (with) parens"))
            .set("PlumbNumber", Object::Real(841.89));
        let ap = edits.add_stream(Dictionary::new(), b"q 0.5 w 0 0 m 9 9 l S Q".to_vec());
        edits.patch(page).set("PlumbStream", Object::Reference(ap));
        let report = file
            .save_incremental(&edits, &SaveOptions::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(report.verification.unwrap().prefix_checked);
        let after = file.document().unwrap();
        let dict = after.object(page).unwrap().as_dict().unwrap();
        assert_eq!(
            dict.get(b"PlumbTest").unwrap().as_str().unwrap(),
            b"patched (with) parens",
            "{name}"
        );
        assert_eq!(after.annotation_counts(), doc.annotation_counts());
    }
}

#[test]
fn the_linearized_fixture_is_seen_as_linearized_and_saves_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    let fx = pl_testkit::linearized();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    assert!(doc.is_linearized());
    assert!(!doc.xref_was_repaired());
    assert_eq!(doc.revision_count(), 2, "first-page and main sections");
    assert_eq!(doc.page_count(), 2);
    assert!(!PdfDocument::from_bytes(pl_testkit::plain().bytes, None)
        .unwrap()
        .is_linearized());

    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 1);
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(saved[..fx.bytes.len()], fx.bytes[..]);
    let after = file.document().unwrap();
    assert_eq!(after.annotation_counts(), [0, 1]);
    assert_eq!(after.revision_count(), 3);
    // The appended section chains to the section the original startxref named: the
    // first-page table at the front of the file.
    let tail = &saved[fx.bytes.len()..];
    assert!(contains(
        tail,
        format!("/Prev {}", doc.xref_start()).as_bytes()
    ));
    assert!(doc.xref_start() < 1024);
}

#[test]
fn an_external_check_runs_on_the_tmp_file_before_the_replace_and_can_stop_it() {
    let dir = tempfile::tempdir().unwrap();
    let fx = pl_testkit::xref_classic();
    let path = write_fixture(dir.path(), &fx);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    add_square(&mut edits, &doc, 2);
    assert_eq!(edits.annotation_pages(), [2]);
    let tmp = tmp_path_for(&path);

    // A check that refuses: it saw the finished temporary file while the original was still
    // in place, and after its refusal the original is untouched and the temporary file gone.
    let mut seen = None;
    let mut refuse = |candidate: &Path, expected: &Expected| {
        let bytes = std::fs::read(candidate).unwrap();
        seen = Some((
            candidate.to_path_buf(),
            bytes.len(),
            expected.clone(),
            std::fs::read(&path).unwrap() == fx.bytes,
        ));
        Err(VerifyError::RenderChanged {
            parser: "test reader",
            page_index: 0,
        })
    };
    let err = file
        .save_incremental_checked(&edits, &SaveOptions::default(), &mut refuse)
        .unwrap_err();
    assert!(
        matches!(
            err,
            PdfError::Verify(VerifyError::RenderChanged {
                parser: "test reader",
                page_index: 0
            })
        ),
        "{err:?}"
    );
    let (seen_path, seen_len, expected, original_in_place) = seen.unwrap();
    assert_eq!(seen_path, tmp);
    assert!(seen_len > fx.bytes.len());
    assert_eq!(expected.page_count, 3);
    assert_eq!(expected.annotation_counts, [0, 0, 1]);
    assert!(original_in_place);
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes);
    assert!(!tmp.exists());

    // The same edit set with a check that agrees goes through (the failed attempt did not
    // make it stale), also with pl-pdf's own verification switched off.
    let mut calls = 0;
    let mut accept = |_: &Path, _: &Expected| {
        calls += 1;
        Ok(())
    };
    let quick = SaveOptions {
        verify: false,
        ..SaveOptions::default()
    };
    file.save_incremental_checked(&edits, &quick, &mut accept)
        .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(file.document().unwrap().annotation_counts(), [0, 0, 1]);
    // Removing an annotation names its page too.
    let doc = file.document().unwrap().clone();
    let id = doc.annotations(2).unwrap()[0].id.unwrap();
    let mut edits = doc.edit();
    edits.delete_annotation(2, id);
    add_square(&mut edits, &doc, 0);
    assert_eq!(edits.annotation_pages(), [0, 2]);
}
