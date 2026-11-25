//! Helpers shared by the document-structure tests (page operations, outlines, links, flatten).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pl_pdf::{Object, PdfDocument, Stream};
use pl_render::{Bitmap, DocId, DocSource, PdfiumBackend, RenderBackend};

pub fn load(bytes: &[u8]) -> PdfDocument {
    PdfDocument::from_bytes(bytes.to_vec(), None).expect("fixture parses")
}

/// A fresh directory under the test temp dir (unique per call, as tests run in parallel).
pub fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("pl-pdf-structure")
        .join(format!("{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The content streams of a page, in order.
pub fn content_streams(doc: &PdfDocument, page: usize) -> Vec<Stream> {
    let id = doc.page_id(page).unwrap();
    let Some(Object::Dictionary(d)) = doc.object(id) else {
        panic!("page {page} is not a dictionary");
    };
    let Ok(contents) = d.get(b"Contents") else {
        return Vec::new();
    };
    let items: Vec<Object> = match doc.resolve(contents) {
        Some(Object::Array(a)) => a.clone(),
        Some(Object::Stream(_)) => vec![contents.clone()],
        _ => Vec::new(),
    };
    items
        .iter()
        .filter_map(|o| match doc.resolve(o) {
            Some(Object::Stream(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// The decoded content of a page (all streams joined).
pub fn page_content(doc: &PdfDocument, page: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for s in content_streams(doc, page) {
        let data = if s.dict.has(b"Filter") {
            s.decompressed_content().expect("decodable content")
        } else {
            s.content.clone()
        };
        out.extend(data);
        out.push(b'\n');
    }
    out
}

/// The `PLM-x-nn` marker a structure fixture page draws, if any.
pub fn marker_in(content: &[u8]) -> Option<String> {
    let at = content.windows(4).position(|w| w == b"PLM-")?;
    let end = (at + 8).min(content.len());
    Some(String::from_utf8_lossy(&content[at..end]).into_owned())
}

pub fn markers(doc: &PdfDocument) -> Vec<Option<String>> {
    (0..doc.page_count())
        .map(|i| marker_in(&page_content(doc, i)))
        .collect()
}

pub fn vendor_dll() -> PathBuf {
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

pub fn pdfium() -> PdfiumBackend {
    PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll")
}

pub fn open_bytes(backend: &PdfiumBackend, bytes: Vec<u8>) -> DocId {
    backend
        .open(DocSource::Bytes(bytes), None)
        .expect("PDFium opens the file")
        .doc
}

/// Largest per-channel difference, and how many pixels differ by more than `tolerance`.
pub fn compare(a: &Bitmap, b: &Bitmap, tolerance: u8) -> (u8, usize) {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    let mut max = 0;
    let mut over = 0;
    for (p, q) in a
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0)
    {
        let d = p
            .iter()
            .zip(q)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        max = max.max(d);
        if d > tolerance {
            over += 1;
        }
    }
    (max, over)
}

/// Number of pixels that are not near white.
pub fn inked(b: &Bitmap) -> usize {
    b.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3].iter().any(|c| *c < 200))
        .count()
}

/// A small deterministic generator (SplitMix64), as pl-geom's property tests use, so every
/// failure reproduces from its seed with no extra crate.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n` > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// A random subset of `0..n` with `1..=max` elements, ascending.
    pub fn subset(&mut self, n: usize, max: usize) -> Vec<usize> {
        let k = 1 + self.below(max.min(n));
        let mut all: Vec<usize> = (0..n).collect();
        for i in 0..k {
            let j = i + self.below(n - i);
            all.swap(i, j);
        }
        let mut out = all[..k].to_vec();
        out.sort_unstable();
        out
    }

    pub fn shuffle(&mut self, items: &mut [usize]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}
