//! Drawing sets against real PDFium and pl-testkit fixtures: the `.plset` file with relative
//! paths, sheets from page labels or file names, navigation across files, missing files, and
//! set search returning the right file and page (stepped, streamed, cancelled, and through the
//! displays of documents open in a session). Headless; every file is in a temporary directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_pdf::SignaturePolicy;
use pl_doc::pl_render::{PdfiumBackend, SearchOptions};
use pl_doc::{
    spawn_set_search, DrawingSet, Session, SessionOptions, SetSearch, SetSearchEvent, SheetInfo,
    SheetRef, StructCommand,
};
use pl_testkit::{
    multi_page, multi_page_token, structure_main, structure_marker, structure_source,
};

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

struct Files {
    _dir: tempfile::TempDir,
    root: PathBuf,
    multi: PathBuf,
    main: PathBuf,
    source: PathBuf,
}

fn files() -> Files {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sheets = root.join("sheets");
    std::fs::create_dir_all(&sheets).unwrap();
    std::fs::create_dir_all(root.join("sets")).unwrap();
    let put = |name: &str, bytes: &[u8]| {
        let p = sheets.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    Files {
        multi: put("Civil.pdf", &multi_page().bytes),
        main: put("Main.pdf", &structure_main().0.bytes),
        source: put("Source.pdf", &structure_source().0.bytes),
        root,
        _dir: dir,
    }
}

fn set_of(f: &Files) -> DrawingSet {
    let mut set = DrawingSet::new("Synthetic set");
    set.add_file(&f.multi).unwrap();
    set.add_file(&f.main).unwrap();
    set.add_file(&f.source).unwrap();
    set
}

#[test]
fn a_set_saves_with_relative_paths_and_reads_back_the_same() {
    let f = files();
    let mut set = set_of(&f);
    assert_eq!(set.len(), 3);
    // Adding a file twice gives its index back.
    assert_eq!(set.add_file(&f.main).unwrap(), 1);
    // Sheets: from the page labels where the file has them, else the file name.
    let (_, truth) = structure_main();
    let main_numbers: Vec<&str> = set.files()[1]
        .sheets
        .iter()
        .map(|s| s.number.as_str())
        .collect();
    assert_eq!(
        main_numbers,
        truth.labels.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(set.files()[0].sheets.len(), 24);
    assert_eq!(set.files()[0].sheets[4].number, "Civil-5");
    assert!(set.files()[2].sheets.iter().all(|s| s.title == "Source"));

    // Edit a sheet and move a file, then save beside the PDFs' folder.
    assert!(set.set_sheet(
        SheetRef { file: 2, page: 0 },
        SheetInfo {
            number: "S-001".into(),
            title: "Source cover".into()
        }
    ));
    assert!(set.move_file(2, 0));
    let plset = f.root.join("sets").join("Synthetic.plset");
    set.save(&plset).unwrap();
    let text = std::fs::read_to_string(&plset).unwrap();
    assert!(
        text.contains("\"../sheets/Source.pdf\""),
        "relative paths: {text}"
    );
    assert!(!text.contains(&f.root.to_string_lossy().replace('\\', "/")));

    let back = DrawingSet::open(&plset).unwrap();
    assert_eq!(back, set);
    assert_eq!(
        back.location(),
        Some(std::path::absolute(&plset).unwrap().as_path())
    );
    assert_eq!(back.files()[0].sheets[0].title, "Source cover");
    assert!(back.files().iter().all(|f| !f.missing));
    assert_eq!(
        back.find_sheet("s-001"),
        Some(SheetRef { file: 0, page: 0 })
    );

    // The whole folder moves: the relative paths still find the files.
    let moved = f.root.with_file_name(format!(
        "{}-moved",
        f.root.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(&f.root, &moved).unwrap();
    let elsewhere = DrawingSet::open(moved.join("sets").join("Synthetic.plset")).unwrap();
    assert!(elsewhere.files().iter().all(|f| !f.missing));
    assert!(elsewhere.files()[1].path.starts_with(&moved));
    std::fs::rename(&moved, &f.root).unwrap();
}

#[test]
fn navigation_crosses_files_and_a_missing_file_is_kept_and_relinked() {
    let f = files();
    let mut set = set_of(&f);
    let plset = f.root.join("sets").join("s.plset");
    set.save(&plset).unwrap();
    let total: usize = set.files().iter().map(|f| f.sheets.len()).sum();
    let mut at = set.first_sheet().unwrap();
    let mut seen = 1;
    while let Some(next) = set.next_sheet(at) {
        assert!(next > at);
        at = next;
        seen += 1;
    }
    assert_eq!(seen, total);
    assert_eq!(at, SheetRef { file: 2, page: 3 });
    assert_eq!(
        set.prev_sheet(SheetRef { file: 1, page: 0 }),
        Some(SheetRef { file: 0, page: 23 })
    );

    // The middle file goes missing: the set still opens, keeps its sheets, and passes over it.
    let renamed = f.main.with_file_name("Main (old).pdf");
    std::fs::rename(&f.main, &renamed).unwrap();
    let mut set = DrawingSet::open(&plset).unwrap();
    assert!(set.files()[1].missing);
    assert_eq!(set.files()[1].sheets.len(), 8);
    assert_eq!(
        set.next_sheet(SheetRef { file: 0, page: 23 }),
        Some(SheetRef { file: 2, page: 0 })
    );
    let backend = backend();
    let mut search = SetSearch::new(
        backend.clone(),
        &set,
        &structure_marker("A", 1),
        SearchOptions::default(),
    );
    assert!(search.run().is_empty());
    assert_eq!(search.skipped().len(), 1);
    assert_eq!(search.skipped()[0].0, 1);

    // Relinked to its new name: found again.
    set.relink(1, &renamed).unwrap();
    assert!(!set.files()[1].missing);
    let mut search = SetSearch::new(
        backend,
        &set,
        &structure_marker("A", 1),
        SearchOptions::default(),
    );
    let hits = search.run().to_vec();
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].file, hits[0].page), (1, 1));
    assert_eq!(set.check_files(), 0);
}

#[test]
fn set_search_returns_the_right_file_and_page() {
    let f = files();
    let set = set_of(&f);
    let backend = backend();
    let (_, main_truth) = structure_main();

    // A token that exists once in the whole set.
    for (query, file, page) in [
        (multi_page_token(17), 0usize, 17u32),
        (multi_page_token(0), 0, 0),
        (structure_marker("A", 5), 1, 5),
        (structure_marker("B", 2), 2, 2),
    ] {
        let mut search = SetSearch::new(backend.clone(), &set, &query, SearchOptions::default());
        let hits = search.run().to_vec();
        assert_eq!(hits.len(), 1, "{query}: {hits:?}");
        let hit = &hits[0];
        assert_eq!((hit.file, hit.page), (file, page), "{query}");
        assert_eq!(hit.path, set.files()[file].path);
        assert_eq!(hit.sheet.as_ref(), set.sheet(hit.sheet_ref()));
        assert!(
            !hit.rects.is_empty()
                && hit
                    .rects
                    .iter()
                    .all(|r| r.width() > 0.0 && r.height() > 0.0)
        );
        assert!(search.skipped().is_empty());
        assert_eq!(search.progress(), (3, 3));
    }
    // A string on many pages of two files: every one, in set order.
    let mut search = SetSearch::new(backend.clone(), &set, "PLM-", SearchOptions::default());
    let pages: Vec<(usize, u32)> = search.run().iter().map(|h| (h.file, h.page)).collect();
    let mut want: Vec<(usize, u32)> = (0..main_truth.markers.len() as u32)
        .map(|p| (1, p))
        .collect();
    want.extend((0..4).map(|p| (2, p)));
    assert_eq!(pages, want);

    // Stepped a page at a time, the hits arrive as they are found and nothing repeats.
    let mut search = SetSearch::new(backend.clone(), &set, "PLM-", SearchOptions::default());
    let mut streamed = Vec::new();
    let mut steps = 0;
    while !search.is_finished() {
        streamed.extend(search.step(1).into_iter().map(|h| (h.file, h.page)));
        steps += 1;
    }
    assert_eq!(streamed, want);
    assert!(steps >= 24 + 8 + 4, "{steps} steps");

    // On a worker thread: streamed events, then the end.
    let mut job = spawn_set_search(SetSearch::new(
        backend.clone(),
        &set,
        &multi_page_token(20),
        SearchOptions::default(),
    ))
    .unwrap();
    let mut got = Vec::new();
    let finished = loop {
        let mut end = None;
        for e in job.try_events() {
            match e {
                SetSearchEvent::Hits(h) => got.extend(h),
                SetSearchEvent::Finished { cancelled } => end = Some(cancelled),
                SetSearchEvent::Skipped { .. } => panic!("nothing is missing"),
            }
        }
        if let Some(c) = end {
            break c;
        }
        std::thread::yield_now();
    };
    assert!(!finished);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].file, got[0].page), (0, 20));
}

#[test]
fn a_set_search_can_be_cancelled() {
    let f = files();
    let set = set_of(&f);
    let backend = backend();
    let mut search = SetSearch::new(backend.clone(), &set, "PLM-", SearchOptions::default());
    // A few pages into the first file (which has no marker), then cancel.
    assert!(search.step(3).is_empty());
    let token = search.cancel_token();
    token.store(true, std::sync::atomic::Ordering::Release);
    assert!(search.step(100).is_empty());
    assert!(search.is_finished());
    assert!(search.was_cancelled());
    assert!(search.hits().is_empty());

    // A cancelled worker stops and says so.
    let job = spawn_set_search(SetSearch::new(
        backend,
        &set,
        "PLM-",
        SearchOptions::default(),
    ))
    .unwrap();
    job.cancel();
    let hits = job.wait();
    assert!(hits.len() <= 12);
}

#[test]
fn set_search_sees_the_unsaved_page_edits_of_an_open_document() {
    let f = files();
    let set = set_of(&f);
    let backend = backend();
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    let id = s.open(&f.main, None).unwrap();
    s.apply_structure(
        id,
        StructCommand::DeletePages { pages: vec![0] },
        SignaturePolicy::Refuse,
    )
    .unwrap();
    // The first marker is gone from the open document; the second is now on its page 0.
    let mut gone = SetSearch::in_session(
        &s,
        &set,
        &structure_marker("A", 0),
        SearchOptions::default(),
    );
    assert!(gone.run().is_empty());
    let mut moved = SetSearch::in_session(
        &s,
        &set,
        &structure_marker("A", 1),
        SearchOptions::default(),
    );
    let hits = moved.run().to_vec();
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].file, hits[0].page), (1, 0));
    // The document's display is still open and usable afterwards.
    let doc = s.document_mut(id).unwrap();
    assert!(doc
        .page_text(0)
        .unwrap()
        .text()
        .contains(&structure_marker("A", 1)));
    // A plain search reads the file, which is unchanged.
    let mut file = SetSearch::new(
        backend,
        &set,
        &structure_marker("A", 0),
        SearchOptions::default(),
    );
    assert_eq!(file.run().len(), 1);
}
