//! The lopdf budget on a scan set (plan M0), measured with a counting allocator. One test in
//! this file, so the peak belongs to it alone.
//!
//! The set here is about 50 MiB so the test stays quick; the budget is stated per byte of
//! file, and the figures for the full 300 MB set (same generator, `examples/measure.rs`) are
//! in docs/measurements.md.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use pl_pdf::{budget, Dictionary, Object, PdfFile, SaveOptions};

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

/// Starts a peak measurement; returns the live heap it starts from.
fn mark() -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    base
}

fn peak_since(base: usize) -> u64 {
    PEAK.load(Ordering::Relaxed).saturating_sub(base) as u64
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[test]
fn lopdf_on_a_scan_set_stays_inside_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan_set.pdf");
    // Six Letter sheets at 300 dpi, incompressible: 50 MiB of the kind of file the memory
    // rules were written for.
    let set = pl_testkit::write_scan_set(&path, 6, [2550, 3300]).unwrap();
    let len = set.file_len;
    assert!(len > 48 * 1024 * 1024);

    // Opening the handle costs nothing: the file is neither read nor parsed.
    let base = mark();
    let mut file = PdfFile::open(&path).unwrap();
    assert!(!file.is_loaded());
    assert!(peak_since(base) < 64 * 1024, "{} bytes", peak_since(base));

    // The first use reads and parses everything.
    let started = Instant::now();
    let doc = file.document().unwrap();
    let load_time = started.elapsed();
    let load_peak = peak_since(base);
    assert_eq!(doc.page_count(), 6);
    assert!(!doc.xref_was_repaired());
    println!(
        "BUDGET load: {:.1} MiB file, {:?} (budget {:?}), peak heap {:.1} MiB = {:.2} x file (budget {:.1} MiB)",
        mib(len),
        load_time,
        budget::load_time(len),
        mib(load_peak),
        load_peak as f64 / len as f64,
        mib(budget::load_peak_bytes(len))
    );
    assert!(
        load_peak <= budget::load_peak_bytes(len),
        "loading held {:.1} MiB",
        mib(load_peak)
    );
    // It really is in memory: a lazy parse that did nothing would also pass the line above.
    assert!(load_peak >= len, "{:.1} MiB", mib(load_peak));
    assert!(load_time <= budget::load_time(len), "{load_time:?}");

    // One verified incremental save of a small edit.
    let mut edits = file.edit().unwrap();
    let mut annot = Dictionary::new();
    annot.set("Type", Object::Name(b"Annot".to_vec()));
    annot.set("Subtype", Object::Name(b"Square".to_vec()));
    annot.set(
        "Rect",
        Object::Array(vec![10.into(), 10.into(), 60.into(), 40.into()]),
    );
    edits.add_annotation(0, annot);
    let started = Instant::now();
    let report = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let save_time = started.elapsed();
    let save_peak = peak_since(base);
    println!(
        "BUDGET save: {:?} (budget {:?}), peak heap {:.1} MiB = {:.2} x file (budget {:.1} MiB), {} bytes appended",
        save_time,
        budget::save_time(len),
        mib(save_peak),
        save_peak as f64 / len as f64,
        mib(budget::save_peak_bytes(len)),
        report.new_len - report.original_len
    );
    assert!(report.verification.unwrap().prefix_checked);
    assert!(report.new_len - report.original_len < 4096);
    assert!(
        save_peak <= budget::save_peak_bytes(len),
        "saving held {:.1} MiB",
        mib(save_peak)
    );
    assert!(save_time <= budget::save_time(len), "{save_time:?}");

    // Unloading gives the memory back.
    file.unload();
    let after = LIVE.load(Ordering::Relaxed).saturating_sub(base) as u64;
    assert!(after < 1024 * 1024, "{after} bytes still held after unload");
}
