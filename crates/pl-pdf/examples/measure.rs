//! Headless measurement of lopdf parse time and peak memory (plan M0).
//!
//!   cargo run --release -p pl-pdf --example measure -- gen <dir>         write the vector files
//!   cargo run --release -p pl-pdf --example measure -- scan <dir> [MB]   write the scan set (300 MB)
//!   cargo run --release -p pl-pdf --example measure -- parse <file>      measure one file
//!
//! `parse` runs in its own process so the peak working set is not inflated by generating the
//! file. It prints one line of key=value pairs. The figures recorded for the plan's budget are
//! in docs/measurements.md; `tests/budget.rs` asserts the budget on a smaller scan set.

use std::path::{Path, PathBuf};
use std::time::Instant;

use pl_pdf::{Dictionary, Object, PdfFile, SaveOptions, Stream};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["gen", dir] => generate(Path::new(dir)),
        ["scan", dir] => scan_set(Path::new(dir), 300),
        ["scan", dir, mb] => scan_set(Path::new(dir), mb.parse().expect("size in MB")),
        ["parse", file] => parse(Path::new(file)),
        _ => {
            eprintln!("usage: measure gen <dir> | measure scan <dir> [MB] | measure parse <file>");
            std::process::exit(2);
        }
    }
}

fn generate(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let dense = pl_testkit::dense_sheet();
    let path = dir.join("dense_200k.pdf");
    std::fs::write(&path, &dense.bytes).unwrap();
    println!(
        "{} {} bytes, {} segments",
        path.display(),
        dense.bytes.len(),
        dense.manifest.total_segments()
    );
    for count in [500, 5000] {
        let path = dir.join(format!("pages_{count}.pdf"));
        let bytes = many_pages(count);
        std::fs::write(&path, &bytes).unwrap();
        println!("{} {} bytes, {count} pages", path.display(), bytes.len());
    }
}

/// The plan's "300 MB scan set": Letter pages at 300 dpi, each one incompressible 8-bit image,
/// as many as it takes to reach `megabytes`. Streamed to disk by pl-testkit.
fn scan_set(dir: &Path, megabytes: usize) {
    std::fs::create_dir_all(dir).unwrap();
    let px = [2550u32, 3300];
    let per_page = px[0] as usize * px[1] as usize;
    let pages = (megabytes * 1024 * 1024).div_ceil(per_page).max(1);
    let path = dir.join(format!("scan_set_{megabytes}mb.pdf"));
    let set = pl_testkit::write_scan_set(&path, pages, px).unwrap();
    println!(
        "{} {} bytes, {} pages of {} x {} px",
        path.display(),
        set.file_len,
        set.pages,
        px[0],
        px[1]
    );
}

/// `count` Letter pages, each with its own uncompressed content stream of 400 stroked segments
/// and a line of text, sharing one font; written by lopdf as a classic-xref file.
fn many_pages(count: i64) -> Vec<u8> {
    use lopdf::{dictionary, Document};
    let mut doc = Document::with_version("1.7");
    doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    let pages_id = doc.new_object_id();
    let font = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let mut kids = Vec::new();
    for page in 0..count {
        let mut content = format!("BT /F1 12 Tf 72 740 Td (Sheet {page}) Tj ET\n0.5 w\n");
        for i in 0..400 {
            let x = 36 + (i % 40) * 13;
            let y = 60 + (i / 40) * 64;
            content.push_str(&format!("{x} {y} m {} {} l S\n", x + 9, y + 50 + page % 7));
        }
        let stream = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
            "Contents" => stream,
        });
        kids.push(Object::Reference(id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Count" => count, "Kids" => kids }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn parse(path: &Path) {
    let base = peak_working_set();
    let mut file = PdfFile::open(path).unwrap();
    let doc = file.document().unwrap();
    let (pages, objects) = (doc.page_count(), doc.object_count());
    let stats = file.load_stats().unwrap();
    let after_load = peak_working_set();
    // Free the first copy so the save below is measured on its own.
    drop(file);

    // One incremental save of a tiny edit to a copy, verification included.
    let copy: PathBuf = path.with_extension("measure-copy.pdf");
    std::fs::copy(path, &copy).unwrap();
    let mut file = PdfFile::open(&copy).unwrap();
    let mut edits = file.edit().unwrap();
    let mut annot = Dictionary::new();
    annot.set("Type", Object::Name(b"Annot".to_vec()));
    annot.set("Subtype", Object::Name(b"Square".to_vec()));
    annot.set(
        "Rect",
        Object::Array(vec![10.into(), 10.into(), 60.into(), 40.into()]),
    );
    edits.add_annotation(0, annot);
    let t = Instant::now();
    let report = file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let save_time = t.elapsed();
    let after_save = peak_working_set();
    let _ = std::fs::remove_file(&copy);

    println!(
        "file={} bytes={} pages={} objects={} read_ms={:.1} parse_ms={:.1} peak_ws_start_mb={:.1} \
         peak_ws_after_load_mb={:.1} save_verify_ms={:.1} appended_bytes={} peak_ws_after_save_mb={:.1}",
        path.file_name().unwrap().to_string_lossy(),
        stats.file_len,
        pages,
        objects,
        stats.read_time.as_secs_f64() * 1e3,
        stats.parse_time.as_secs_f64() * 1e3,
        mb(base),
        mb(after_load),
        save_time.as_secs_f64() * 1e3,
        report.new_len - report.original_len,
        mb(after_save),
    );
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(windows)]
fn peak_working_set() -> usize {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: the struct is plain data, sized correctly, and the pseudo-handle is always valid.
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) != 0 {
            counters.PeakWorkingSetSize
        } else {
            0
        }
    }
}

#[cfg(not(windows))]
fn peak_working_set() -> usize {
    0
}
