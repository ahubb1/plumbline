//! Shared helpers for the PDFium integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pl_geom::{PageGeometry, Point, TileGrid};
use pl_render::{Bitmap, DocId, DocSource, PdfiumBackend, RenderBackend};
use pl_testkit::{Fixture, Manifest, PageTruth};

/// The vendored PDFium, as an absolute path (works in debug and release test builds).
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

pub fn backend() -> PdfiumBackend {
    PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll")
}

/// Opens a fixture from memory and applies the manifest's `/UserUnit` values.
pub fn open(backend: &PdfiumBackend, fixture: &Fixture) -> DocId {
    let info = backend
        .open(DocSource::Bytes(fixture.bytes.clone()), None)
        .unwrap_or_else(|e| panic!("open {}: {e}", fixture.manifest.name));
    assert_eq!(info.page_count as usize, fixture.manifest.page_count());
    apply_user_units(backend, info.doc, &fixture.manifest);
    info.doc
}

pub fn apply_user_units(backend: &PdfiumBackend, doc: DocId, manifest: &Manifest) {
    for (i, p) in manifest.pages.iter().enumerate() {
        if p.user_unit != 1.0 {
            backend
                .set_page_user_unit(doc, i as u32, p.user_unit)
                .unwrap();
        }
    }
}

/// Writes a fixture under the test temp directory and returns its path.
pub fn write_temp(fixture: &Fixture, tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pl-render");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!(
        "{}-{tag}-{}.pdf",
        fixture.manifest.name,
        std::process::id()
    ));
    std::fs::write(&path, &fixture.bytes).unwrap();
    path
}

/// Renders every tile of a level and stitches them into one bitmap.
pub fn stitch(backend: &dyn RenderBackend, doc: DocId, page: u32, grid: &TileGrid) -> Bitmap {
    let mut out = Bitmap::filled(grid.width_px(), grid.height_px(), [0, 0, 0, 0]);
    for id in grid.all().iter() {
        let rect = grid.tile_pixel_rect(id).unwrap();
        let tile = backend.render_tile(doc, page, id).unwrap();
        assert_eq!(
            (tile.width, tile.height),
            (rect.width, rect.height),
            "tile {id:?} size"
        );
        out.blit(&tile, rect.x, rect.y);
    }
    out
}

/// The largest per-channel difference between two bitmaps of the same size.
pub fn max_diff(a: &Bitmap, b: &Bitmap) -> u8 {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    a.pixels
        .iter()
        .zip(&b.pixels)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// Per-pixel grey-level differences: (largest difference, pixels differing by more than 1).
pub fn grey_diff(a: &Bitmap, b: &Bitmap) -> (u8, usize) {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    let grey =
        |p: &[u8]| ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114 + 500) / 1000) as u8;
    let (mut worst, mut over) = (0u8, 0usize);
    for (x, y) in a
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0.iter())
    {
        if x != y {
            let d = grey(x).abs_diff(grey(y));
            worst = worst.max(d);
            over += usize::from(d > 1);
        }
    }
    (worst, over)
}

/// Number of pixels that differ at all.
pub fn diff_count(a: &Bitmap, b: &Bitmap) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height), "bitmap sizes");
    a.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0.iter())
        .filter(|(x, y)| x != y)
        .count()
}

/// Where a page-space point lands in a bitmap rendered at `scale` px per user unit.
pub fn page_to_pixel(geometry: &PageGeometry, scale: f64, p: [f64; 2]) -> (f64, f64) {
    let d = geometry.page_to_display() * Point::new(p[0], p[1]);
    (d.x * scale, d.y * scale)
}

/// Whether any pixel within `radius` of `(x, y)` is clearly not white.
pub fn has_ink_near(bitmap: &Bitmap, x: f64, y: f64, radius: i64) -> bool {
    let (cx, cy) = (x.floor() as i64, y.floor() as i64);
    for yy in cy - radius..=cy + radius {
        for xx in cx - radius..=cx + radius {
            if xx < 0 || yy < 0 || xx >= bitmap.width as i64 || yy >= bitmap.height as i64 {
                continue;
            }
            let [r, g, b, _] = bitmap.pixel(xx as u32, yy as u32);
            if r.min(g).min(b) < 180 {
                return true;
            }
        }
    }
    false
}

/// Counts clearly non-white pixels inside a page-space rectangle.
pub fn ink_in_rect(bitmap: &Bitmap, geometry: &PageGeometry, scale: f64, rect: [f64; 4]) -> usize {
    let (ax, ay) = page_to_pixel(geometry, scale, [rect[0], rect[1]]);
    let (bx, by) = page_to_pixel(geometry, scale, [rect[2], rect[3]]);
    let x0 = ax.min(bx).ceil().max(0.0) as u32;
    let x1 = (ax.max(bx).floor() as u32).min(bitmap.width);
    let y0 = ay.min(by).ceil().max(0.0) as u32;
    let y1 = (ay.max(by).floor() as u32).min(bitmap.height);
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let [r, g, b, _] = bitmap.pixel(x, y);
            if r.min(g).min(b) < 180 {
                n += 1;
            }
        }
    }
    n
}

/// Whether a page-space point is inside the page's visible box by at least `margin`.
pub fn inside_visible(truth: &PageTruth, p: [f64; 2], margin: f64) -> bool {
    let b = truth.visible_box();
    p[0] >= b[0] + margin && p[0] <= b[2] - margin && p[1] >= b[1] + margin && p[1] <= b[3] - margin
}
