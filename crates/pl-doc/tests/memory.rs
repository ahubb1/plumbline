//! What an open costs in this process's own memory, on a large file, measured with a counting
//! allocator. One test in this file, so the peak belongs to it alone.
//!
//! The plan's memory rules (section 7): the original file is memory-mapped, not copied, and
//! lopdf parses lazily on the first edit. Finding `/UserUnit` used to break both at open, on
//! exactly the big files the rules were written for. (PDFium allocates through its own
//! allocator, which is not counted here; this test is about copies *we* make.)

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pl_doc::pl_render::{PdfiumBackend, RenderBackend};
use pl_doc::{OpenMode, Session, SessionOptions, UserUnitPolicy};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, new_size) };
        if !q.is_null() {
            if new_size >= layout.size() {
                let grow = new_size - layout.size();
                let live = LIVE.fetch_add(grow, Ordering::Relaxed) + grow;
                PEAK.fetch_max(live, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn mark() -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    base
}

fn peak_since(base: usize) -> u64 {
    PEAK.load(Ordering::Relaxed).saturating_sub(base) as u64
}

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

const MIB: u64 = 1024 * 1024;

#[test]
fn opening_a_large_file_copies_none_of_it_and_still_finds_the_user_unit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan_set.pdf");
    // 50 MiB of scanned sheets whose pages carry /UserUnit 2, written in the clear: the case
    // that used to send the whole file through lopdf during the open.
    let set = pl_testkit::write_scan_set_with_unit(&path, 6, [2550, 3300], 2.0).unwrap();
    assert!(set.file_len > 48 * MIB);
    let backend = Arc::new(PdfiumBackend::new(Some(&vendor_dll())).expect("vendored pdfium"));

    // The interactive open, and the background probe that follows it.
    let base = mark();
    let mut session = Session::new(backend.clone());
    assert_eq!(session.options().open, OpenMode::Interactive);
    assert_eq!(session.options().user_unit, UserUnitPolicy::Scan);
    let id = session.open(&path, None).unwrap();
    let open_peak = peak_since(base);
    let doc = session.document_mut(id).unwrap();
    assert_eq!(doc.page_count(), 6);
    assert!(doc.wait_background(Duration::from_secs(60)));
    let probe_peak = peak_since(base);
    println!(
        "MEMORY interactive open of a {:.1} MiB file: peak heap {:.2} MiB at open, {:.2} MiB with the /UserUnit probe",
        set.file_len as f64 / MIB as f64,
        open_peak as f64 / MIB as f64,
        probe_peak as f64 / MIB as f64
    );
    // The unit was found, on every page...
    assert!(doc.pages().iter().all(|p| p.user_unit() == 2.0));
    let shown = doc.pages()[0].display_size_points();
    assert_eq!((shown.width, shown.height), (1224.0, 1584.0));
    assert!(doc.warnings().is_empty(), "{:?}", doc.warnings());
    // ... without lopdf, without a page load, and without a copy of the file: the whole open
    // held less than a fiftieth of the file on the heap.
    assert!(!doc.pdf_is_loaded());
    assert_eq!(backend.page_loads(doc.render_id()).unwrap(), 0);
    assert!(
        probe_peak < MIB,
        "the open held {probe_peak} bytes of heap for a {} byte file",
        set.file_len
    );

    // The counter does see a real parse: settling the unit through lopdf holds the file and
    // its object graph, and lets them go again.
    let base = mark();
    assert!(doc.refresh_user_units().unwrap());
    let lopdf_peak = peak_since(base);
    assert!(
        lopdf_peak > set.file_len,
        "a lopdf parse should hold at least the file: {lopdf_peak} bytes"
    );
    assert!(!doc.pdf_is_loaded(), "the parse is dropped again");
    session.close(id).unwrap();

    // The complete mode finds the unit the same cheap way, on the caller's thread.
    let base = mark();
    let mut complete = Session::with_options(backend, SessionOptions::complete());
    let id = complete.open(&path, None).unwrap();
    let doc = complete.document(id).unwrap();
    assert_eq!(doc.page_geometry(0).unwrap().user_unit(), 2.0);
    assert!(!doc.pdf_is_loaded());
    assert!(peak_since(base) < MIB, "{} bytes", peak_since(base));
    complete.close(id).unwrap();
}
