//! Signature detection and the refusal of a full rewrite that would invalidate a signature;
//! plus page operations on the real-world sample sheet set when it is present.

mod structure_util;

use pl_pdf::{PdfError, PdfFile, SaveOptions, SignaturePolicy};
use pl_testkit::{signed_stub, structure_main, SIGNED_FIELD};
use structure_util::{load, temp_dir};

#[test]
fn signed_fields_and_permissions_are_detected() {
    let doc = load(&signed_stub().bytes);
    let info = doc.signatures();
    assert_eq!(info.signed_fields, vec![SIGNED_FIELD.to_string()]);
    assert_eq!(
        info.empty_signature_fields,
        vec!["Second signature".to_string()]
    );
    assert!(info.doc_mdp);
    assert!(!info.usage_rights);
    assert_eq!(info.sig_flags, 3);
    assert!(info.is_signed());
    assert_eq!(info.descriptions().len(), 2);
    let (fx, _) = structure_main();
    assert!(!load(&fx.bytes).signatures().is_signed());
}

#[test]
fn a_rewrite_of_a_signed_file_needs_consent() {
    let fx = signed_stub();
    let dir = temp_dir("signed");
    let path = dir.join("signed.pdf");
    std::fs::write(&path, &fx.bytes).unwrap();
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut r = doc.restructure().unwrap();
    let report = r.rotate(&[0], 90).unwrap();
    // Every operation's report lists what the rewrite will break.
    assert!(report
        .signatures_invalidated
        .contains(&SIGNED_FIELD.to_string()));
    let new = r.finish().unwrap();
    let err = file
        .save_restructured(
            new.clone(),
            &SaveOptions::default(),
            SignaturePolicy::Refuse,
        )
        .unwrap_err();
    match err {
        PdfError::SignaturesWouldBreak { fields } => {
            assert!(fields.contains(&SIGNED_FIELD.to_string()))
        }
        other => panic!("{other}"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), fx.bytes, "nothing written");
    assert!(!pl_pdf::tmp_path_for(&path).exists());
    // A new file from it is refused the same way.
    let other = dir.join("copy.pdf");
    assert!(matches!(
        new.clone()
            .save_new(&other, &SaveOptions::default(), SignaturePolicy::Refuse),
        Err(PdfError::SignaturesWouldBreak { .. })
    ));
    assert!(!other.exists());
    // With consent it goes ahead.
    file.save_restructured(new, &SaveOptions::default(), SignaturePolicy::Invalidate)
        .unwrap();
    assert_eq!(
        file.document().unwrap().pages().unwrap()[0]
            .rotate
            .degrees(),
        90
    );
    // Flatten reports the signatures too.
    let (_, report) = load(&fx.bytes).flatten(&Default::default()).unwrap();
    assert!(!report.signatures_invalidated.is_empty());
}

/// Page operations on the 12-sheet civil set, when it is on this machine (never committed).
#[test]
fn sample_sheet_set_survives_page_operations() {
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/site-development-plans.pdf");
    if !sample.exists() {
        eprintln!("skipped: {} is not present", sample.display());
        return;
    }
    let dir = temp_dir("sample");
    let copy = dir.join("sample.pdf");
    std::fs::copy(&sample, &copy).unwrap();
    let mut file = PdfFile::open(&copy).unwrap();
    let doc = file.document().unwrap().clone();
    let n = doc.page_count();
    assert!(n >= 4);
    let contents: Vec<Vec<u8>> = (0..n)
        .map(|i| structure_util::page_content(&doc, i))
        .collect();
    let geometry: Vec<_> = doc.pages().unwrap().iter().map(|p| p.geometry).collect();
    let mut r = doc.restructure().unwrap();
    let mut order: Vec<usize> = (0..n).rev().collect();
    order.swap(0, 1);
    r.reorder(&order).unwrap();
    r.rotate(&[0], 90).unwrap();
    r.delete(&[n - 1]).unwrap();
    r.insert_blank(1, pl_pdf::BlankPage::LikeNeighbour).unwrap();
    let new = r.finish().unwrap();
    let report = file
        .save_restructured_as(
            &dir.join("out.pdf"),
            new,
            &SaveOptions::default(),
            SignaturePolicy::Refuse,
        )
        .unwrap();
    assert_eq!(report.verification.unwrap().page_count, n);
    let back = load(&std::fs::read(dir.join("out.pdf")).unwrap());
    assert_eq!(back.page_count(), n);
    // Page k of the result is the original order[k] (after the blank at 1 and the deleted last).
    let expected: Vec<Option<usize>> = {
        let mut v: Vec<Option<usize>> = order[..n - 1].iter().map(|i| Some(*i)).collect();
        v.insert(1, None);
        v
    };
    let pages = back.pages().unwrap();
    for (k, e) in expected.iter().enumerate() {
        match e {
            Some(orig) => {
                assert_eq!(
                    structure_util::page_content(&back, k),
                    contents[*orig],
                    "page {k}"
                );
                if k != 0 {
                    assert_eq!(pages[k].geometry, geometry[*orig], "page {k}");
                }
            }
            None => assert!(structure_util::page_content(&back, k)
                .iter()
                .all(u8::is_ascii_whitespace)),
        }
    }
    // The original copy is untouched.
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(&sample).unwrap()
    );
}
