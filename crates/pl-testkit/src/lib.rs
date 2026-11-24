//! pl-testkit: deterministic synthetic PDF fixtures with ground-truth manifests
//! (see docs/plan.md sections 6 and 8, milestone M0).
//!
//! Every fixture function returns a [`Fixture`]: the PDF bytes plus a
//! [`Manifest`] describing what is in the file. All coordinates in a manifest
//! follow the workspace convention owned by `pl-geom`: **unrotated page user
//! space, PDF points, y-up**. `/Rotate`, `/UserUnit` and the CropBox offset are
//! recorded as page attributes and never baked into coordinates.
//!
//! Fixtures are byte-for-byte reproducible (no timestamps, fixed file IDs)
//! unless [`Manifest::deterministic`] is `false` (AES encryption uses random
//! initialisation vectors).
//!
//! ```no_run
//! let fx = pl_testkit::plain();
//! assert_eq!(fx.manifest.pages.len(), 1);
//! std::fs::write("plain.pdf", &fx.bytes).unwrap();
//! ```

mod build;
mod fixtures;
mod metrics;
mod raw;

pub use fixtures::*;
pub use raw::{linearized, write_scan_set, write_scan_set_with_unit, ScanSet};
pub use metrics::{helvetica_width, HELVETICA_ASCENT, HELVETICA_DESCENT};

use std::io;
use std::path::{Path, PathBuf};

/// `[x, y]` in unrotated page user space, points, y-up.
pub type Point = [f64; 2];
/// `[x0, y0, x1, y1]` (lower-left, upper-right) in unrotated page user space.
pub type Rect = [f64; 4];

/// US Letter portrait.
pub const LETTER: Rect = [0.0, 0.0, 612.0, 792.0];
/// 36 x 24 inch sheet (ARCH D landscape).
pub const ARCH_D: Rect = [0.0, 0.0, 2592.0, 1728.0];

pub const BLACK: [f64; 3] = [0.0, 0.0, 0.0];
pub const RED: [f64; 3] = [1.0, 0.0, 0.0];
pub const GREEN: [f64; 3] = [0.0, 0.5, 0.0];
pub const BLUE: [f64; 3] = [0.0, 0.0, 1.0];
pub const YELLOW: [f64; 3] = [1.0, 1.0, 0.0];

/// One stroked straight line segment (its own `m .. l S` path, butt caps).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub a: Point,
    pub b: Point,
    /// Stroke width in user-space units.
    pub width: f64,
    /// DeviceRGB stroke colour, components 0..=1.
    pub rgb: [f64; 3],
}

impl Segment {
    pub fn new(a: Point, b: Point, width: f64, rgb: [f64; 3]) -> Self {
        Self { a, b, width, rgb }
    }

    pub fn length(&self) -> f64 {
        (self.b[0] - self.a[0]).hypot(self.b[1] - self.a[1])
    }

    pub fn midpoint(&self) -> Point {
        [(self.a[0] + self.b[0]) / 2.0, (self.a[1] + self.b[1]) / 2.0]
    }
}

/// One text string shown with a single `Tj` in standard-14 Helvetica
/// (WinAnsiEncoding, not embedded), black, horizontal in unrotated space.
#[derive(Debug, Clone, PartialEq)]
pub struct TextTruth {
    pub text: String,
    /// Baseline start of the first glyph.
    pub origin: Point,
    pub font_size: f64,
    /// Advance width from the Adobe Helvetica AFM metrics.
    pub width: f64,
}

impl TextTruth {
    pub fn new(text: &str, origin: Point, font_size: f64) -> Self {
        Self { text: text.to_string(), origin, font_size, width: helvetica_width(text, font_size) }
    }

    /// Box from descender to ascender over the advance width (AFM metrics; a
    /// renderer substituting another font will differ slightly).
    pub fn bbox(&self) -> Rect {
        [
            self.origin[0],
            self.origin[1] + HELVETICA_DESCENT * self.font_size,
            self.origin[0] + self.width,
            self.origin[1] + HELVETICA_ASCENT * self.font_size,
        ]
    }
}

/// An annotation already present in the file, with an `/AP /N` stream.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotTruth {
    /// `/Subtype` without the slash: `Square`, `Line`, `FreeText`, `Highlight`, `Widget`.
    pub subtype: &'static str,
    pub rect: Rect,
    /// `/NM` unique name (empty for widgets).
    pub name: String,
    /// `/Contents` (for widgets: the field value).
    pub contents: String,
    /// `/T`: author for markups, field name for widgets.
    pub title: String,
    /// `/C` colour (border colour for widgets).
    pub rgb: [f64; 3],
    /// `/BS /W` border width (0 when absent).
    pub border_width: f64,
    /// `/L` endpoints for Line annotations.
    pub line: Option<[Point; 2]>,
    /// `/QuadPoints` for Highlight: top-left, top-right, bottom-left, bottom-right.
    pub quad_points: Option<[f64; 8]>,
    /// `/BBox` of the normal appearance stream.
    pub ap_bbox: Rect,
}

/// A raster image XObject drawn on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageTruth {
    pub width_px: u32,
    pub height_px: u32,
    pub bits_per_component: u8,
    /// `DeviceGray`.
    pub color_space: &'static str,
    /// Where the image lands on the page.
    pub placement: Rect,
    /// Solid dark rectangles (gray level [`ImageTruth::dark_level`]) in page space.
    pub dark_rects: Vec<Rect>,
    /// The same rectangles in pixel space `[x0, y0, x1, y1]`, origin top-left, exclusive max.
    pub dark_rects_px: Vec<[u32; 4]>,
    pub dark_level: u8,
    /// Background gray levels (two-tone checkerboard of `checker_px` squares).
    pub background_levels: [u8; 2],
    pub checker_px: u32,
}

impl ImageTruth {
    /// Gray value of pixel (`x`, `y`), origin top-left. This is the generator's own formula.
    pub fn pixel(&self, x: u32, y: u32) -> u8 {
        if self.dark_rects_px.iter().any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]) {
            self.dark_level
        } else {
            self.background_levels[(((x / self.checker_px) + (y / self.checker_px)) & 1) as usize]
        }
    }
}

/// Analytic description of the dense segment grid on the large-format sheet.
///
/// The segments are not stored; [`GridTruth::segment`] reproduces any of them
/// exactly. Coordinates are whole hundredths of a point, written to the
/// content stream with two decimals, so parsing gives identical `f64` values.
#[derive(Debug, Clone, PartialEq)]
pub struct GridTruth {
    /// Number of segments (each its own stroked path).
    pub count: usize,
    pub cols: usize,
    pub rows: usize,
    /// Number of content streams the segments are split over.
    pub streams: usize,
    /// Lower-left corner of cell (0, 0), in hundredths of a point.
    pub origin_cpt: [i64; 2],
    /// Cell pitch in hundredths of a point.
    pub pitch_cpt: [i64; 2],
    /// Offset of a segment's start from its cell corner, hundredths of a point.
    pub inset_cpt: [i64; 2],
    /// Segment extent in hundredths of a point.
    pub extent_cpt: [i64; 2],
    pub width: f64,
    pub rgb: [f64; 3],
}

impl GridTruth {
    /// Endpoints of segment `i` in hundredths of a point: `[ax, ay, bx, by]`.
    ///
    /// Segment `i` sits in cell (`i % cols`, `i / cols`) and is horizontal,
    /// vertical or diagonal for `i % 3` = 0, 1, 2. Segments never touch.
    pub fn segment_cpt(&self, i: usize) -> [i64; 4] {
        assert!(i < self.count, "segment index {i} out of range");
        let col = (i % self.cols) as i64;
        let row = (i / self.cols) as i64;
        let ax = self.origin_cpt[0] + col * self.pitch_cpt[0] + self.inset_cpt[0];
        let ay = self.origin_cpt[1] + row * self.pitch_cpt[1] + self.inset_cpt[1];
        let (dx, dy) = match i % 3 {
            0 => (self.extent_cpt[0], 0),
            1 => (0, self.extent_cpt[1]),
            _ => (self.extent_cpt[0], self.extent_cpt[1]),
        };
        [ax, ay, ax + dx, ay + dy]
    }

    pub fn segment(&self, i: usize) -> Segment {
        let c = self.segment_cpt(i);
        Segment {
            a: [c[0] as f64 / 100.0, c[1] as f64 / 100.0],
            b: [c[2] as f64 / 100.0, c[3] as f64 / 100.0],
            width: self.width,
            rgb: self.rgb,
        }
    }

    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        (0..self.count).map(|i| self.segment(i))
    }

    /// Index of the content stream that draws segment `i`.
    pub fn stream_of(&self, i: usize) -> usize {
        i * self.streams / self.count.max(1)
    }

    /// Sum of all segment lengths in points.
    pub fn total_length(&self) -> f64 {
        self.segments().map(|s| s.length()).sum()
    }
}

/// Ground truth for one page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageTruth {
    pub media_box: Rect,
    /// `/CropBox` when the page has one.
    pub crop_box: Option<Rect>,
    /// `/Rotate`: 0, 90, 180 or 270 (clockwise).
    pub rotate: i32,
    /// `/UserUnit` (1.0 when absent).
    pub user_unit: f64,
    /// Number of content streams in `/Contents`.
    pub content_streams: usize,
    /// Every stroked segment of the page content, in drawing order, including
    /// those inside optional-content layers. Empty for the dense sheet: see `grid`.
    pub segments: Vec<Segment>,
    /// Total stroked segments in the page content (`segments.len()` or `grid.count`).
    pub segment_count: usize,
    pub grid: Option<GridTruth>,
    /// Text in the page content (not annotation appearances), in drawing order.
    pub texts: Vec<TextTruth>,
    /// Annotations in `/Annots` order.
    pub annotations: Vec<AnnotTruth>,
    pub images: Vec<ImageTruth>,
}

impl PageTruth {
    pub(crate) fn new(media_box: Rect) -> Self {
        Self {
            media_box,
            crop_box: None,
            rotate: 0,
            user_unit: 1.0,
            content_streams: 1,
            segments: Vec::new(),
            segment_count: 0,
            grid: None,
            texts: Vec::new(),
            annotations: Vec::new(),
            images: Vec::new(),
        }
    }

    /// The box a viewer shows: CropBox if present, else MediaBox.
    pub fn visible_box(&self) -> Rect {
        self.crop_box.unwrap_or(self.media_box)
    }

    /// Width and height as displayed, in points (1/72 in): the visible box
    /// scaled by `/UserUnit` and with sides swapped for `/Rotate` 90 or 270.
    pub fn display_size(&self) -> [f64; 2] {
        let b = self.visible_box();
        let w = (b[2] - b[0]) * self.user_unit;
        let h = (b[3] - b[1]) * self.user_unit;
        if self.rotate.rem_euclid(180) == 90 {
            [h, w]
        } else {
            [w, h]
        }
    }
}

/// One outline (bookmark) item, listed in pre-order.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineTruth {
    pub title: String,
    /// 0 for top-level items.
    pub level: usize,
    /// Zero-based target page.
    pub page_index: usize,
    /// `/XYZ` destination left and top, unrotated page user space.
    pub dest: Point,
}

/// One optional-content group and the marks it alone draws.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerTruth {
    pub name: String,
    /// State in the default configuration `/OCProperties /D`.
    pub default_visible: bool,
    pub page_index: usize,
    pub segments: Vec<Segment>,
    pub texts: Vec<TextTruth>,
}

/// One filled AcroForm text field (field and widget merged in one object).
#[derive(Debug, Clone, PartialEq)]
pub struct FormFieldTruth {
    pub name: String,
    pub value: String,
    pub page_index: usize,
    pub rect: Rect,
    pub font_size: f64,
    /// Baseline start of the value text drawn by the appearance stream, page space.
    pub text_origin: Point,
}

/// Standard security handler parameters of an encrypted fixture.
#[derive(Debug, Clone, PartialEq)]
pub struct EncryptionTruth {
    /// `RC4` or `AESV3`.
    pub algorithm: &'static str,
    pub key_bits: u32,
    /// `/V` and `/R` of the encryption dictionary.
    pub version: i64,
    pub revision: i64,
    pub user_password: String,
    pub owner_password: String,
    pub can_print: bool,
    pub can_modify: bool,
    pub can_copy: bool,
    pub can_annotate: bool,
    pub can_assemble: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefKind {
    /// Classic `xref` table with a `trailer` dictionary.
    Table,
    /// Cross-reference stream (`/Type /XRef`).
    Stream,
}

/// Ground truth for one fixture file.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Short stable name; the file is `<name>.pdf`.
    pub name: String,
    /// Header version, e.g. `1.4`.
    pub pdf_version: String,
    pub xref: XrefKind,
    /// True when non-stream objects are packed in `/ObjStm` object streams.
    pub object_streams: bool,
    /// True when the file has the linearised layout: a `/Linearized` parameter dictionary
    /// first, a first-page cross-reference section near the start and the main one at the end.
    pub linearized: bool,
    /// False when two generations differ byte-wise (random AES IVs and salts).
    pub deterministic: bool,
    pub pages: Vec<PageTruth>,
    /// Pre-order list of bookmarks; empty when the file has no outline.
    pub outline: Vec<OutlineTruth>,
    pub layers: Vec<LayerTruth>,
    pub form_fields: Vec<FormFieldTruth>,
    pub encryption: Option<EncryptionTruth>,
}

impl Manifest {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn file_name(&self) -> String {
        format!("{}.pdf", self.name)
    }

    /// Stroked segments over all pages.
    pub fn total_segments(&self) -> usize {
        self.pages.iter().map(|p| p.segment_count).sum()
    }
}

/// A generated PDF and its ground truth.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub manifest: Manifest,
    pub bytes: Vec<u8>,
}

/// Size knobs for the two heavy fixtures, so callers can ask for small ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Segments on the large-format sheet.
    pub dense_segments: usize,
    /// Content streams the sheet's segments are split over.
    pub dense_streams: usize,
    /// Pixel size of the scan stand-in image.
    pub raster_px: [u32; 2],
}

/// Default segment count of [`dense_sheet`].
pub const DEFAULT_DENSE_SEGMENTS: usize = 200_000;
/// Default content-stream count of [`dense_sheet`].
pub const DEFAULT_DENSE_STREAMS: usize = 8;
/// Default pixel size of [`raster_scan`]: US Letter at 300 dpi.
pub const DEFAULT_RASTER_PX: [u32; 2] = [2550, 3300];

impl Default for Options {
    fn default() -> Self {
        Self {
            dense_segments: DEFAULT_DENSE_SEGMENTS,
            dense_streams: DEFAULT_DENSE_STREAMS,
            raster_px: DEFAULT_RASTER_PX,
        }
    }
}

impl Options {
    /// Light settings for quick tests: 2,000 segments in 3 streams, 255 x 330 px image.
    pub fn small() -> Self {
        Self { dense_segments: 2_000, dense_streams: 3, raster_px: [255, 330] }
    }
}

/// Every fixture with default sizes (the dense sheet has 200,000 segments).
pub fn all_fixtures() -> Vec<Fixture> {
    all_fixtures_with(&Options::default())
}

/// Every fixture, with the heavy ones sized by `options`.
pub fn all_fixtures_with(options: &Options) -> Vec<Fixture> {
    let mut out = vec![
        plain(),
        multi_page(),
        rotated_pages(),
        user_unit(),
        crop_box(),
        dense_sheet_with(options.dense_segments, options.dense_streams),
        xref_classic(),
        xref_object_streams(),
        annotations(),
        layers(),
        acro_form(),
        raster_scan_with(options.raster_px[0], options.raster_px[1]),
        encrypted_rc4(),
    ];
    out.push(encrypted_aes256());
    out.push(inherited_boxes());
    out.push(user_unit_in_object_stream());
    out.push(linearized());
    out
}

/// `<workspace>/target/fixtures`, the conventional place for generated fixtures.
pub fn default_output_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("fixtures")
}

/// Write every fixture as `<name>.pdf` plus `<name>.manifest.txt` (a debug
/// dump of the manifest) into `dir`, creating it if needed. Returns the PDF
/// paths in [`all_fixtures_with`] order.
pub fn write_all(dir: &Path, options: &Options) -> io::Result<Vec<PathBuf>> {
    write_fixtures(dir, &all_fixtures_with(options))
}

/// Write the given fixtures into `dir` (see [`write_all`]).
pub fn write_fixtures(dir: &Path, fixtures: &[Fixture]) -> io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut paths = Vec::with_capacity(fixtures.len());
    for fx in fixtures {
        let path = dir.join(fx.manifest.file_name());
        std::fs::write(&path, &fx.bytes)?;
        std::fs::write(
            dir.join(format!("{}.manifest.txt", fx.manifest.name)),
            format!("{:#?}\n", fx.manifest),
        )?;
        paths.push(path);
    }
    Ok(paths)
}
