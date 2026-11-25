//! The glue a window needs beyond the commands (`outputs.rs`): combining files into a new one,
//! what a print job reads, and every link of a document at once. Real PDFium and pl-testkit
//! fixtures, in a temporary folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{Annotation, Origin, PdfDate};
use pl_doc::pl_geom::Rect;
use pl_doc::pl_pdf::{LinkAction, PdfFile, SignaturePolicy};
use pl_doc::pl_render::{DocSource, PdfiumBackend, RenderBackend};
use pl_doc::{combine_files, CombinePart, Command, PrintSource, Session, SessionOptions};

fn backend() -> Arc<dyn RenderBackend> {
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

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn contents(path: &Path) -> Vec<u64> {
    let mut file = PdfFile::open(path).unwrap();
    pl_doc::structure::fingerprints(file.document().unwrap())
        .unwrap()
        .iter()
        .map(|f| f.content)
        .collect()
}

#[test]
fn combining_takes_the_parts_in_order_with_their_pages() {
    let dir = tempfile::tempdir().unwrap();
    let main = write(
        dir.path(),
        "main.pdf",
        &pl_testkit::structure_main().0.bytes,
    );
    let other = write(
        dir.path(),
        "other.pdf",
        &pl_testkit::structure_source().0.bytes,
    );
    let (main_fp, other_fp) = (contents(&main), contents(&other));
    let dest = dir.path().join("combined.pdf");
    let backend = backend();
    combine_files(
        &backend,
        &[
            CombinePart::file(other.clone(), Some(vec![3, 0])),
            CombinePart::file(main.clone(), None),
        ],
        &dest,
        SignaturePolicy::Refuse,
    )
    .unwrap();
    let mut want = vec![other_fp[3], other_fp[0]];
    want.extend(&main_fp);
    assert_eq!(contents(&dest), want);
    // PDFium reads it too.
    let info = backend.open(DocSource::file(&dest), None).unwrap();
    assert_eq!(info.page_count, 10);
    backend.close(info.doc).unwrap();

    // The result may not replace one of its parts, and a part needs pages.
    assert!(combine_files(
        &backend,
        &[CombinePart::file(main.clone(), None)],
        &main,
        SignaturePolicy::Refuse
    )
    .is_err());
    assert!(combine_files(
        &backend,
        &[CombinePart::file(main.clone(), Some(Vec::new()))],
        &dir.path().join("none.pdf"),
        SignaturePolicy::Refuse
    )
    .is_err());
    assert!(combine_files(&backend, &[], &dest, SignaturePolicy::Refuse).is_err());
    assert_eq!(contents(&main), main_fp, "no part changes");
}

#[test]
fn a_combination_reads_an_open_document_as_it_stands_and_refuses_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let main = write(
        dir.path(),
        "main.pdf",
        &pl_testkit::structure_main().0.bytes,
    );
    let main_fp = contents(&main);
    let backend = backend();
    // An unsaved page change of an open document is in the combination.
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    let id = s.open(&main, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    doc.apply_structure(
        pl_doc::StructCommand::DeletePages { pages: vec![0] },
        SignaturePolicy::Refuse,
    )
    .unwrap();
    let PrintSource::Bytes(bytes) = doc.print_source().unwrap() else {
        panic!("unsaved changes are read from memory");
    };
    let dest = dir.path().join("combined.pdf");
    let part = CombinePart {
        bytes: Some(std::sync::Arc::new(bytes)),
        ..CombinePart::file(main.clone(), None)
    };
    combine_files(&backend, &[part], &dest, SignaturePolicy::Refuse).unwrap();
    assert_eq!(contents(&dest), main_fp[1..].to_vec());
    assert_eq!(contents(&main), main_fp, "the file itself is untouched");

    // A signed part is refused until the user agrees.
    let signed = write(dir.path(), "signed.pdf", &pl_testkit::signed_stub().bytes);
    let dest = dir.path().join("signed-combined.pdf");
    let refused = combine_files(
        &backend,
        &[CombinePart::file(signed.clone(), None)],
        &dest,
        SignaturePolicy::Refuse,
    );
    assert!(
        refused
            .as_ref()
            .err()
            .and_then(|e| e.signatures_to_confirm())
            .is_some(),
        "{refused:?}"
    );
    assert!(!dest.exists());
    combine_files(
        &backend,
        &[CombinePart::file(signed, None)],
        &dest,
        SignaturePolicy::Invalidate,
    )
    .unwrap();
    assert!(dest.exists());
}

#[test]
fn print_reads_the_file_until_something_is_unsaved() {
    let dir = tempfile::tempdir().unwrap();
    let main = write(
        dir.path(),
        "main.pdf",
        &pl_testkit::structure_main().0.bytes,
    );
    let mut s = Session::with_options(backend(), SessionOptions::complete());
    let id = s.open(&main, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert_eq!(doc.print_source().unwrap(), PrintSource::File(main.clone()));
    let origin = Origin::new("Tester", PdfDate::parse("D:20261124120000Z").unwrap());
    let square = Annotation::square(0, Rect::new(100.0, 100.0, 200.0, 160.0), &origin);
    doc.apply(Command::AddAnnotations(vec![square])).unwrap();
    let PrintSource::Bytes(bytes) = doc.print_source().unwrap() else {
        panic!("an unsaved markup prints from memory");
    };
    // The bytes hold the new square; the file does not.
    let printed = pl_doc::pl_pdf::PdfDocument::from_bytes(bytes, None).unwrap();
    let file_count = PdfFile::open(&main)
        .unwrap()
        .document()
        .unwrap()
        .annotations(0)
        .unwrap()
        .len();
    assert_eq!(printed.annotations(0).unwrap().len(), file_count + 1);
}

#[test]
fn every_link_of_a_document_is_read_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (fixture, truth) = pl_testkit::structure_main();
    let main = write(dir.path(), "main.pdf", &fixture.bytes);
    let mut s = Session::with_options(backend(), SessionOptions::complete());
    let id = s.open(&main, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let links = doc.document_links().unwrap();
    assert_eq!(links.len(), 8);
    let total: usize = links.iter().map(Vec::len).sum();
    assert_eq!(total, truth.links.len());
    for (page, page_links) in links.iter().enumerate() {
        assert_eq!(
            *page_links,
            doc.page_links(page as u32).unwrap(),
            "page {page}"
        );
    }
    assert!(links[0]
        .iter()
        .any(|l| matches!(&l.link.action, LinkAction::Uri(u) if u.contains("example.com"))));
    assert!(
        links[0].iter().all(|l| l.id.is_some()),
        "each has its store id"
    );
}
