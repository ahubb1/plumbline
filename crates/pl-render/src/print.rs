//! Printing: a job description, the page-to-paper layout, and banded rendering into a sink.
//!
//! Nothing here talks to a printer. The application's Win32 printer sink implements
//! [`PrintSink`] (StartDoc / StartPage / StretchDIBits per band / EndPage / EndDoc); tests use
//! [`MemorySink`].
//!
//! # Device units
//!
//! A [`Paper`] is described the way a Windows printer device context reports it, in device
//! pixels: the physical sheet (`PHYSICALWIDTH` / `PHYSICALHEIGHT`), the printable area's offset
//! (`PHYSICALOFFSETX` / `PHYSICALOFFSETY`) and size (`HORZRES` / `VERTRES`), and the resolution
//! (`LOGPIXELSX` / `LOGPIXELSY`). Everything the layout produces is in *printable-area pixels*:
//! y-down, origin at the top-left corner of the printable area, which is where a printer DC's
//! own origin is.
//!
//! # Layout
//!
//! [`layout_page`] is a pure function. The page's visible box (crop box, `/Rotate` applied) is
//! scaled so that one PDF point is `dpi / 72` device pixels, `/UserUnit` included (actual size),
//! or so that it fits the printable area (fit), or the smaller of the two (shrink to fit). With
//! [`PrintOrientation::Auto`] the page is also turned a quarter ([`AUTO_ROTATION`]) when that
//! prints it larger (fit, shrink) or shows more of it (actual size). The page image is then
//! centred in the printable area, or put at its top-left corner. Its pixel size is rounded up
//! as [`page_pixel_size`](crate::page_pixel_size) does, and its position to whole pixels, so
//! [`PagePlacement::page_to_printable`] is exactly where PDFium draws.
//!
//! # Bands
//!
//! A large sheet at printer resolution does not fit in memory as one bitmap (34 x 22 in at
//! 600 dpi is 20,400 x 13,200 px, about 1 GB). [`print_job`] renders each page in horizontal
//! bands of at most [`PrintJob::band_rows`] rows, each one request on the PDFium thread
//! ([`WindowRequest`]), so other work (tiles on screen) can run between bands and the memory
//! held at once is bounded by [`band_memory_bound`]. A band covers only the part of the
//! printable area the page image reaches; rows and columns outside it are paper the sink
//! leaves blank.

use std::ops::Range;

use pl_geom::{Affine, PageGeometry, Rotation};
use serde::{Deserialize, Serialize};

use crate::backend::RenderBackend;
use crate::types::{
    window_scratch_bound, Bitmap, DocId, PageInfo, RenderError, WindowOptions, WindowRequest,
};

/// Band height used by [`PrintJob::new`]: 256 rows. A 34 in wide sheet at 600 dpi then needs
/// about 21 MB for a band plus 10 MB of gutter.
pub const DEFAULT_BAND_ROWS: u32 = 256;

/// The quarter turn [`PrintOrientation::Auto`] applies when turning prints better: 270 degrees
/// clockwise (a quarter turn anticlockwise), so the top of the page comes to the left edge of
/// the paper, as PostScript landscape does.
pub const AUTO_ROTATION: Rotation = Rotation::R270;

/// How the page is sized on the paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrintScaling {
    /// As large as fits the printable area, keeping the aspect ratio.
    Fit,
    /// One PDF point (times `/UserUnit`) is 1/72 inch; larger pages are cut off.
    ActualSize,
    /// Actual size, or fit if the page is larger than the printable area.
    ShrinkToFit,
}

/// Whether the page is turned on the paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrintOrientation {
    /// As displayed (`/Rotate` applied), never turned further.
    AsIs,
    /// Turned by [`AUTO_ROTATION`] when that prints it larger or shows more of it.
    Auto,
    /// Always turned this many degrees clockwise (a multiple of 90).
    Rotate { degrees: i32 },
}

/// The pixel layout of the buffers handed to a [`PrintSink`]. Rows are top-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BandFormat {
    /// Blue, green, red, 255: a 32-bit top-down DIB.
    Bgra8,
    /// Blue, green, red, rows padded to a multiple of 4 bytes: a 24-bit top-down DIB.
    Bgr8,
    /// Red, green, blue, 255, the order of [`Bitmap`].
    Rgba8,
}

impl BandFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            BandFormat::Bgra8 | BandFormat::Rgba8 => 4,
            BandFormat::Bgr8 => 3,
        }
    }

    /// Bytes per row for `width` pixels (a multiple of 4, as a DIB needs).
    pub fn stride(self, width: u32) -> usize {
        (width as usize * self.bytes_per_pixel()).div_ceil(4) * 4
    }

    /// The pixel at column `x` of a row, as RGBA.
    pub fn rgba_at(self, row: &[u8], x: usize) -> [u8; 4] {
        let i = x * self.bytes_per_pixel();
        match self {
            BandFormat::Rgba8 => [row[i], row[i + 1], row[i + 2], 255],
            BandFormat::Bgra8 | BandFormat::Bgr8 => [row[i + 2], row[i + 1], row[i], 255],
        }
    }
}

/// The printable part of the paper, in device pixels from the sheet's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrintableArea {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A sheet of paper on a device, in device pixels (see the [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paper {
    /// Device pixels per inch, horizontally and vertically.
    pub dpi: [f64; 2],
    /// The physical sheet.
    pub size: [u32; 2],
    pub printable: PrintableArea,
}

impl Paper {
    /// A sheet of `width_in x height_in` inches at `dpi`, with the same unprintable `margin_in`
    /// on every side. Pixel sizes are rounded to the nearest pixel.
    pub fn from_inches(width_in: f64, height_in: f64, dpi: f64, margin_in: f64) -> Paper {
        let px = |inches: f64| (inches * dpi).round().max(0.0) as u32;
        let (w, h, m) = (px(width_in), px(height_in), px(margin_in));
        Paper {
            dpi: [dpi, dpi],
            size: [w, h],
            printable: PrintableArea {
                x: m,
                y: m,
                width: w.saturating_sub(2 * m),
                height: h.saturating_sub(2 * m),
            },
        }
    }

    /// The same sheet turned a quarter (landscape for portrait and back).
    pub fn turned(&self) -> Paper {
        let p = self.printable;
        Paper {
            dpi: [self.dpi[1], self.dpi[0]],
            size: [self.size[1], self.size[0]],
            printable: PrintableArea {
                x: p.y,
                y: self.size[0].saturating_sub(p.x + p.width),
                width: p.height,
                height: p.width,
            },
        }
    }

    /// Checks that the numbers describe a usable sheet.
    pub fn validate(&self) -> Result<(), PrintError> {
        let p = self.printable;
        if !self.dpi.iter().all(|d| d.is_finite() && *d > 0.0) {
            return Err(PrintError::InvalidJob(format!(
                "the resolution must be positive, got {:?}",
                self.dpi
            )));
        }
        if p.width == 0 || p.height == 0 {
            return Err(PrintError::InvalidJob("the printable area is empty".into()));
        }
        if p.x as u64 + p.width as u64 > self.size[0] as u64
            || p.y as u64 + p.height as u64 > self.size[1] as u64
        {
            return Err(PrintError::InvalidJob(format!(
                "the printable area {p:?} is not inside the {} x {} px sheet",
                self.size[0], self.size[1]
            )));
        }
        Ok(())
    }
}

/// Everything about one print run except the device itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrintJob {
    /// Page indices (zero-based) in print order; one sheet each. See [`parse_page_range`].
    pub pages: Vec<u32>,
    pub paper: Paper,
    pub scaling: PrintScaling,
    /// Centre the page in the printable area; otherwise put it at the top-left corner.
    pub centre: bool,
    pub orientation: PrintOrientation,
    /// Print annotations (markups) from their appearance streams.
    pub annotations: bool,
    /// Print form fields with their values.
    pub form_fields: bool,
    /// Follow the annotation print flags (see [`WindowOptions::print_flags`]).
    pub print_flags: bool,
    /// Rows per band.
    pub band_rows: u32,
    pub format: BandFormat,
}

impl PrintJob {
    /// A job with the usual choices: fit, centred, auto orientation, markups and form fields
    /// printed by their print flags, [`DEFAULT_BAND_ROWS`], 32-bit BGRA bands.
    pub fn new(pages: Vec<u32>, paper: Paper) -> PrintJob {
        PrintJob {
            pages,
            paper,
            scaling: PrintScaling::Fit,
            centre: true,
            orientation: PrintOrientation::Auto,
            annotations: true,
            form_fields: true,
            print_flags: true,
            band_rows: DEFAULT_BAND_ROWS,
            format: BandFormat::Bgra8,
        }
    }

    /// The render options of the job's band requests.
    pub fn window_options(&self) -> WindowOptions {
        WindowOptions {
            annotations: self.annotations,
            form_fields: self.form_fields,
            print_flags: self.print_flags,
        }
    }

    /// Checks the job against a document of `page_count` pages.
    pub fn validate(&self, page_count: u32) -> Result<(), PrintError> {
        self.paper.validate()?;
        if self.pages.is_empty() {
            return Err(PrintError::InvalidJob("no pages to print".into()));
        }
        if let Some(p) = self.pages.iter().find(|p| **p >= page_count) {
            return Err(PrintError::InvalidJob(format!(
                "page {} does not exist (the document has {page_count})",
                p + 1
            )));
        }
        if self.band_rows == 0 {
            return Err(PrintError::InvalidJob("bands need at least one row".into()));
        }
        if let PrintOrientation::Rotate { degrees } = self.orientation {
            if Rotation::from_degrees(degrees).is_none() {
                return Err(PrintError::InvalidJob(format!(
                    "the rotation must be a multiple of 90 degrees, got {degrees}"
                )));
            }
        }
        Ok(())
    }
}

/// Parses a page range as a person types it, one-based: `"1-3, 5, 8-"`. An empty string means
/// every page; `"7-"` runs to the last page. Pages come back zero-based, in the order given,
/// repeats kept.
pub fn parse_page_range(text: &str, page_count: u32) -> Result<Vec<u32>, PrintError> {
    let bad = |why: String| Err(PrintError::InvalidJob(why));
    if text.trim().is_empty() {
        return Ok((0..page_count).collect());
    }
    let number = |s: &str| -> Result<u32, PrintError> {
        let n: u32 = s.trim().parse().map_err(|_| {
            PrintError::InvalidJob(format!("\"{}\" is not a page number", s.trim()))
        })?;
        if n == 0 || n > page_count {
            return Err(PrintError::InvalidJob(format!(
                "page {n} does not exist (the document has {page_count})"
            )));
        }
        Ok(n)
    };
    let mut pages = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            None => pages.push(number(part)? - 1),
            Some((a, b)) => {
                let first = if a.trim().is_empty() { 1 } else { number(a)? };
                let last = if b.trim().is_empty() {
                    page_count
                } else {
                    number(b)?
                };
                if first > last {
                    return bad(format!("the range {part} runs backwards"));
                }
                pages.extend(first - 1..last);
            }
        }
    }
    if pages.is_empty() {
        return bad("no pages to print".into());
    }
    Ok(pages)
}

/// Where and how large one page goes on its sheet. All pixel values are device pixels of the
/// printable area (see the [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PagePlacement {
    pub page: u32,
    /// Extra clockwise rotation on the paper, on top of `/Rotate`, in degrees.
    pub rotation: i32,
    /// The scale asked for: device pixels per user-space unit, per axis.
    pub scale: [f64; 2],
    /// The page image: the visible box after both rotations, in whole pixels.
    pub size: [u32; 2],
    /// The image's top-left corner; negative or past the far edge when it is cut off.
    pub offset: [i64; 2],
    /// Part of the page image falls outside the printable area.
    pub clipped: bool,
}

impl PagePlacement {
    pub fn extra_rotation(&self) -> Rotation {
        Rotation::from_degrees(self.rotation).unwrap_or(Rotation::R0)
    }

    /// Page space (unrotated user space, y-up) to printable-area pixels (y-down), exactly as
    /// the bands are drawn: the visible box maps onto the `size` pixels at `offset`.
    pub fn page_to_printable(&self, geometry: &PageGeometry) -> Affine {
        let turned = geometry.rotated_by(self.extra_rotation());
        let d = turned.display_size();
        Affine::translate((self.offset[0] as f64, self.offset[1] as f64))
            * Affine::scale_non_uniform(
                self.size[0] as f64 / d.width,
                self.size[1] as f64 / d.height,
            )
            * turned.page_to_display()
    }

    /// Page space to sheet pixels (from the physical top-left corner of the paper).
    pub fn page_to_paper(&self, geometry: &PageGeometry, paper: &Paper) -> Affine {
        Affine::translate((paper.printable.x as f64, paper.printable.y as f64))
            * self.page_to_printable(geometry)
    }

    /// The part of the printable area the page image covers, as column and row ranges; `None`
    /// when it covers none.
    pub fn visible(&self, paper: &Paper) -> Option<(Range<u32>, Range<u32>)> {
        let span = |offset: i64, size: u32, limit: u32| -> Range<u32> {
            let start = offset.clamp(0, limit as i64);
            let end = (offset + size as i64).clamp(0, limit as i64);
            start as u32..end as u32
        };
        let cols = span(self.offset[0], self.size[0], paper.printable.width);
        let rows = span(self.offset[1], self.size[1], paper.printable.height);
        (!cols.is_empty() && !rows.is_empty()).then_some((cols, rows))
    }

    /// The bands that print this page with at most `rows` rows each, top to bottom.
    pub fn bands(&self, paper: &Paper, rows: u32) -> Vec<BandRect> {
        let Some((cols, visible_rows)) = self.visible(paper) else {
            return Vec::new();
        };
        let rows = rows.max(1);
        let mut out = Vec::new();
        let mut y = visible_rows.start;
        while y < visible_rows.end {
            let height = rows.min(visible_rows.end - y);
            out.push(BandRect {
                index: out.len() as u32,
                x: cols.start,
                y,
                width: cols.end - cols.start,
                height,
                window: [
                    (cols.start as i64 - self.offset[0]) as u32,
                    (y as i64 - self.offset[1]) as u32,
                    cols.end - cols.start,
                    height,
                ],
            });
            y += height;
        }
        out
    }

    /// The band request for one of [`bands`](Self::bands).
    pub fn window_request(
        &self,
        doc: DocId,
        band: &BandRect,
        options: WindowOptions,
    ) -> WindowRequest {
        WindowRequest {
            doc,
            page: self.page,
            rotation: self.rotation,
            size: self.size,
            window: band.window,
            options,
        }
    }
}

/// One band of a page: where it goes in the printable area and which part of the page image it
/// is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandRect {
    pub index: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// `[x, y, width, height]` in page-image pixels.
    pub window: [u32; 4],
}

/// Pixels for `units` user-space units at `scale`, rounded up as
/// [`page_pixel_size`](crate::page_pixel_size) does.
fn pixels(units: f64, scale: f64) -> u32 {
    let v = (units * scale - 1e-6).ceil();
    if v.is_finite() {
        v.clamp(1.0, crate::types::MAX_IMAGE_SIDE as f64) as u32
    } else {
        1
    }
}

/// Lays one page out on the job's paper with one extra rotation; also returns the scale factor
/// relative to actual size.
fn place(
    page: u32,
    geometry: &PageGeometry,
    rotation: Rotation,
    job: &PrintJob,
) -> (PagePlacement, f64) {
    let paper = &job.paper;
    let area = paper.printable;
    let turned = geometry.rotated_by(rotation);
    let d = turned.display_size();
    let unit = geometry.user_unit();
    let base = [paper.dpi[0] / 72.0 * unit, paper.dpi[1] / 72.0 * unit];
    let fit =
        (area.width as f64 / (d.width * base[0])).min(area.height as f64 / (d.height * base[1]));
    let k = match job.scaling {
        PrintScaling::Fit => fit,
        PrintScaling::ActualSize => 1.0,
        PrintScaling::ShrinkToFit => fit.min(1.0),
    };
    let scale = [base[0] * k, base[1] * k];
    let mut size = [pixels(d.width, scale[0]), pixels(d.height, scale[1])];
    if k < 1.0 || job.scaling == PrintScaling::Fit {
        // A fitted page never spills over through rounding.
        size = [size[0].min(area.width), size[1].min(area.height)];
    }
    let offset = if job.centre {
        [
            (area.width as i64 - size[0] as i64).div_euclid(2),
            (area.height as i64 - size[1] as i64).div_euclid(2),
        ]
    } else {
        [0, 0]
    };
    let clipped = offset[0] < 0
        || offset[1] < 0
        || offset[0] + size[0] as i64 > area.width as i64
        || offset[1] + size[1] as i64 > area.height as i64;
    (
        PagePlacement {
            page,
            rotation: rotation.degrees(),
            scale,
            size,
            offset,
            clipped,
        },
        k,
    )
}

/// The area of the printable region a placement covers, in pixels.
fn visible_area(p: &PagePlacement, paper: &Paper) -> u64 {
    p.visible(paper)
        .map(|(c, r)| (c.end - c.start) as u64 * (r.end - r.start) as u64)
        .unwrap_or(0)
}

/// Lays out one page: the pure page-to-paper computation (see the [module docs](self)).
///
/// `info` must carry the page's real `/UserUnit` (the backend reports 1 unless it was told
/// otherwise with [`RenderBackend::set_page_user_unit`]).
pub fn layout_page(page: u32, info: &PageInfo, job: &PrintJob) -> PagePlacement {
    let geometry = info.geometry();
    match job.orientation {
        PrintOrientation::AsIs => place(page, &geometry, Rotation::R0, job).0,
        PrintOrientation::Rotate { degrees } => {
            let r = Rotation::from_degrees(degrees).unwrap_or(Rotation::R0);
            place(page, &geometry, r, job).0
        }
        PrintOrientation::Auto => {
            let (upright, k0) = place(page, &geometry, Rotation::R0, job);
            let (turned, k1) = place(page, &geometry, AUTO_ROTATION, job);
            let better = match job.scaling {
                PrintScaling::Fit | PrintScaling::ShrinkToFit => k1 > k0 * (1.0 + 1e-9),
                PrintScaling::ActualSize => {
                    upright.clipped
                        && visible_area(&turned, &job.paper) > visible_area(&upright, &job.paper)
                }
            };
            if better {
                turned
            } else {
                upright
            }
        }
    }
}

/// Lays out every page of a job, reading the page geometry from the backend (a preview needs
/// nothing more).
pub fn layout_job(
    backend: &dyn RenderBackend,
    doc: DocId,
    job: &PrintJob,
) -> Result<Vec<PagePlacement>, PrintError> {
    job.validate(backend.page_count(doc)?)?;
    job.pages
        .iter()
        .map(|&page| Ok(layout_page(page, &backend.page_info(doc, page)?, job)))
        .collect()
}

/// The most pixel memory one band of `width x rows` pixels holds at once, in the backend and
/// in [`print_job`] together: the band is converted to the sink's format in place.
pub fn band_memory_bound(width: u32, rows: u32) -> u64 {
    window_scratch_bound(width, rows)
}

/// Converts an RGBA band to `format` in place and returns the stride.
fn convert_in_place(pixels: &mut Vec<u8>, width: u32, height: u32, format: BandFormat) -> usize {
    match format {
        BandFormat::Rgba8 => {}
        BandFormat::Bgra8 => {
            for px in pixels.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }
        BandFormat::Bgr8 => {
            // Each destination byte lies at or before its source byte, so a forward pass in
            // the same buffer never overwrites a pixel it has yet to read.
            let stride = format.stride(width);
            let src_row = width as usize * 4;
            for row in 0..height as usize {
                for col in 0..width as usize {
                    let s = row * src_row + col * 4;
                    let d = row * stride + col * 3;
                    let (r, g, b) = (pixels[s], pixels[s + 1], pixels[s + 2]);
                    pixels[d] = b;
                    pixels[d + 1] = g;
                    pixels[d + 2] = r;
                }
                pixels[row * stride + width as usize * 3..(row + 1) * stride].fill(0);
            }
            pixels.truncate(stride * height as usize);
            return stride;
        }
    }
    format.stride(width)
}

/// The start of a job, for [`PrintSink::begin_job`].
#[derive(Clone, Copy, Debug)]
pub struct JobStart<'a> {
    pub paper: &'a Paper,
    pub sheets: usize,
    pub format: BandFormat,
}

/// The start of one sheet, for [`PrintSink::begin_page`].
#[derive(Clone, Copy, Debug)]
pub struct SheetStart {
    /// Zero-based position in the job.
    pub sheet: usize,
    pub placement: PagePlacement,
    /// How many bands will follow.
    pub bands: u32,
}

/// One band of pixels for [`PrintSink::band`]. `data` holds `height` rows of `stride` bytes,
/// top row first, to be drawn with its top-left corner at `(x, y)` of the printable area.
#[derive(Clone, Copy, Debug)]
pub struct Band<'a> {
    pub sheet: usize,
    pub page: u32,
    pub index: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: BandFormat,
    pub data: &'a [u8],
}

/// Why a sink stopped a job.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SinkError {
    /// The user cancelled (or the sink wants no more bands).
    #[error("printing was cancelled")]
    Cancelled,
    /// The device refused something.
    #[error("the printer reported an error: {0}")]
    Device(String),
}

/// Where printed bands go. The application's Win32 printer implements this; so does
/// [`MemorySink`]. Calls come in the order `begin_job`, then per sheet `begin_page`, `band`
/// (top to bottom), `end_page`, and finally `end_job`; after any error, from the sink or from
/// rendering, `abort` is called instead of the remaining calls.
pub trait PrintSink {
    fn begin_job(&mut self, job: &JobStart<'_>) -> Result<(), SinkError>;
    fn begin_page(&mut self, sheet: &SheetStart) -> Result<(), SinkError>;
    fn band(&mut self, band: &Band<'_>) -> Result<(), SinkError>;
    fn end_page(&mut self) -> Result<(), SinkError>;
    fn end_job(&mut self) -> Result<(), SinkError>;
    /// The job stops early; discard what was sent.
    fn abort(&mut self) {}
}

/// Why a print job failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrintError {
    #[error("the print job is not valid: {0}")]
    InvalidJob(String),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error(transparent)]
    Sink(#[from] SinkError),
}

/// What a finished job did.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintReport {
    pub placements: Vec<PagePlacement>,
    /// Bands sent, all sheets together.
    pub bands: u32,
    /// The most pixel memory one band held, as the backend reported it.
    pub peak_band_bytes: u64,
    /// [`band_memory_bound`] for the widest band of the job.
    pub band_bound: u64,
}

/// Prints a job: lays out each page, renders it band by band on the backend's thread and
/// hands the bands to `sink`. Blocking; run it off the UI thread.
pub fn print_job(
    backend: &dyn RenderBackend,
    doc: DocId,
    job: &PrintJob,
    sink: &mut dyn PrintSink,
) -> Result<PrintReport, PrintError> {
    let placements = layout_job(backend, doc, job)?;
    let result = run(backend, doc, job, &placements, sink);
    match result {
        Ok((bands, peak_band_bytes, band_bound)) => Ok(PrintReport {
            placements,
            bands,
            peak_band_bytes,
            band_bound,
        }),
        Err(e) => {
            sink.abort();
            Err(e)
        }
    }
}

fn run(
    backend: &dyn RenderBackend,
    doc: DocId,
    job: &PrintJob,
    placements: &[PagePlacement],
    sink: &mut dyn PrintSink,
) -> Result<(u32, u64, u64), PrintError> {
    let options = job.window_options();
    sink.begin_job(&JobStart {
        paper: &job.paper,
        sheets: placements.len(),
        format: job.format,
    })?;
    let (mut count, mut peak, mut bound) = (0u32, 0u64, 0u64);
    for (sheet, placement) in placements.iter().enumerate() {
        let bands = placement.bands(&job.paper, job.band_rows);
        sink.begin_page(&SheetStart {
            sheet,
            placement: *placement,
            bands: bands.len() as u32,
        })?;
        for band in &bands {
            let reply = backend.render_window(placement.window_request(doc, band, options))?;
            let Bitmap {
                width,
                height,
                mut pixels,
            } = reply.bitmap;
            if (width, height) != (band.width, band.height) {
                return Err(PrintError::Render(RenderError::Backend(format!(
                    "a {}x{} band came back as {width}x{height}",
                    band.width, band.height
                ))));
            }
            peak = peak.max(reply.scratch_bytes);
            bound = bound.max(band_memory_bound(band.width, job.band_rows));
            let stride = convert_in_place(&mut pixels, width, height, job.format);
            sink.band(&Band {
                sheet,
                page: placement.page,
                index: band.index,
                x: band.x,
                y: band.y,
                width,
                height,
                stride,
                format: job.format,
                data: &pixels,
            })?;
            count += 1;
        }
        sink.end_page()?;
    }
    sink.end_job()?;
    Ok((count, peak, bound))
}

/// One band as a [`MemorySink`] saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandRecord {
    pub sheet: usize,
    pub page: u32,
    pub index: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub bytes: usize,
}

/// One sheet as a [`MemorySink`] composed it: the printable area, white where nothing printed.
#[derive(Clone, Debug)]
pub struct MemorySheet {
    pub placement: PagePlacement,
    pub image: Bitmap,
}

/// An in-memory print device for tests: it checks the call order, composes each sheet's bands
/// into a bitmap of the printable area (unless made with [`MemorySink::records_only`], for
/// sheets too large to hold) and records every band.
#[derive(Debug, Default)]
pub struct MemorySink {
    pub sheets: Vec<MemorySheet>,
    pub bands: Vec<BandRecord>,
    /// Every call, in order: `begin_job`, `begin_page`, `band`, `end_page`, `end_job`, `abort`.
    pub calls: Vec<&'static str>,
    compose: bool,
    printable: Option<PrintableArea>,
    cancel_after: Option<usize>,
    in_page: bool,
}

impl MemorySink {
    pub fn new() -> MemorySink {
        MemorySink {
            compose: true,
            ..MemorySink::default()
        }
    }

    /// A sink that records the bands but keeps no pixels.
    pub fn records_only() -> MemorySink {
        MemorySink::default()
    }

    /// Makes the sink cancel the job when it is handed band number `n` (counting from 0 over
    /// the whole job).
    pub fn cancel_at_band(mut self, n: usize) -> MemorySink {
        self.cancel_after = Some(n);
        self
    }

    fn order_error(&self, call: &str) -> SinkError {
        SinkError::Device(format!("{call} out of order after {:?}", self.calls.last()))
    }
}

impl PrintSink for MemorySink {
    fn begin_job(&mut self, job: &JobStart<'_>) -> Result<(), SinkError> {
        if !self.calls.is_empty() {
            return Err(self.order_error("begin_job"));
        }
        self.calls.push("begin_job");
        self.printable = Some(job.paper.printable);
        Ok(())
    }

    fn begin_page(&mut self, sheet: &SheetStart) -> Result<(), SinkError> {
        if self.in_page || self.printable.is_none() || sheet.sheet != self.sheets.len() {
            return Err(self.order_error("begin_page"));
        }
        self.calls.push("begin_page");
        self.in_page = true;
        let area = self.printable.expect("checked");
        let image = if self.compose {
            Bitmap::filled(area.width, area.height, [255, 255, 255, 255])
        } else {
            Bitmap {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            }
        };
        self.sheets.push(MemorySheet {
            placement: sheet.placement,
            image,
        });
        Ok(())
    }

    fn band(&mut self, band: &Band<'_>) -> Result<(), SinkError> {
        if !self.in_page {
            return Err(self.order_error("band"));
        }
        if self.cancel_after == Some(self.bands.len()) {
            return Err(SinkError::Cancelled);
        }
        let area = self.printable.expect("in a page");
        if band.x + band.width > area.width
            || band.y + band.height > area.height
            || band.stride < band.width as usize * band.format.bytes_per_pixel()
            || band.data.len() < band.stride * band.height as usize
        {
            return Err(SinkError::Device(format!("band {band:?} does not fit")));
        }
        self.calls.push("band");
        self.bands.push(BandRecord {
            sheet: band.sheet,
            page: band.page,
            index: band.index,
            x: band.x,
            y: band.y,
            width: band.width,
            height: band.height,
            stride: band.stride,
            bytes: band.data.len(),
        });
        if self.compose {
            let image = &mut self.sheets.last_mut().expect("in a page").image;
            for row in 0..band.height as usize {
                let src = &band.data[row * band.stride..];
                let dy = band.y as usize + row;
                for col in 0..band.width as usize {
                    let dx = band.x as usize + col;
                    let i = (dy * image.width as usize + dx) * 4;
                    image.pixels[i..i + 4].copy_from_slice(&band.format.rgba_at(src, col));
                }
            }
        }
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), SinkError> {
        if !self.in_page {
            return Err(self.order_error("end_page"));
        }
        self.calls.push("end_page");
        self.in_page = false;
        Ok(())
    }

    fn end_job(&mut self) -> Result<(), SinkError> {
        if self.in_page || self.printable.is_none() {
            return Err(self.order_error("end_job"));
        }
        self.calls.push("end_job");
        Ok(())
    }

    fn abort(&mut self) {
        self.calls.push("abort");
        self.in_page = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;
    use pl_geom::{Point, Rect};

    #[allow(non_snake_case)]
    fn LETTER_300() -> Paper {
        Paper::from_inches(8.5, 11.0, 300.0, 0.25)
    }

    fn info(media: [f64; 4], crop: Option<[f64; 4]>, rotate: i32, user_unit: f64) -> PageInfo {
        PageInfo {
            media_box: media,
            crop_box: crop,
            rotate,
            user_unit,
        }
    }

    fn job(paper: Paper, scaling: PrintScaling, orientation: PrintOrientation) -> PrintJob {
        PrintJob {
            scaling,
            orientation,
            ..PrintJob::new(vec![0], paper)
        }
    }

    fn assert_maps(t: Affine, page: [f64; 2], want: [f64; 2]) {
        let p = t * Point::new(page[0], page[1]);
        assert!(
            (p.x - want[0]).abs() < 1e-9 && (p.y - want[1]).abs() < 1e-9,
            "{page:?} -> ({}, {}), want {want:?}",
            p.x,
            p.y
        );
    }

    #[test]
    fn paper_from_inches_matches_a_device_report() {
        let p = LETTER_300();
        assert_eq!(p.size, [2550, 3300]);
        assert_eq!(
            p.printable,
            PrintableArea {
                x: 75,
                y: 75,
                width: 2400,
                height: 3150
            }
        );
        assert!(p.validate().is_ok());
        let t = p.turned();
        assert_eq!(t.size, [3300, 2550]);
        assert_eq!(t.printable.width, 3150);
        assert_eq!(t.printable.height, 2400);
        assert_eq!(t.turned(), p);
        let mut bad = p;
        bad.printable.width = 2500;
        assert!(bad.validate().is_err());
        bad = p;
        bad.dpi[1] = 0.0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn letter_fits_letter_with_margins_exactly() {
        let j = job(LETTER_300(), PrintScaling::Fit, PrintOrientation::Auto);
        let page = info([0.0, 0.0, 612.0, 792.0], None, 0, 1.0);
        let p = layout_page(0, &page, &j);
        // Fit factor min(2400/2550, 3150/3300) = 2400/2550: the width is exact.
        assert_eq!(p.rotation, 0);
        assert_eq!(p.size, [2400, 3106]);
        assert_eq!(p.offset, [0, 22]);
        assert!(!p.clipped);
        assert!((p.scale[0] - 2400.0 / 612.0).abs() < 1e-12);
        let t = p.page_to_printable(&page.geometry());
        assert_maps(t, [0.0, 792.0], [0.0, 22.0]);
        assert_maps(t, [612.0, 0.0], [2400.0, 22.0 + 3106.0]);
        let tp = p.page_to_paper(&page.geometry(), &j.paper);
        assert_maps(tp, [0.0, 792.0], [75.0, 97.0]);
    }

    #[test]
    fn actual_size_uses_points_and_user_unit() {
        let j = job(
            Paper::from_inches(8.5, 11.0, 300.0, 0.0),
            PrintScaling::ActualSize,
            PrintOrientation::AsIs,
        );
        // Half-size media with /UserUnit 2 is a Letter sheet physically.
        let page = info([0.0, 0.0, 306.0, 396.0], None, 0, 2.0);
        let p = layout_page(0, &page, &j);
        assert_eq!(p.size, [2550, 3300]);
        assert_eq!(p.offset, [0, 0]);
        assert_eq!(p.scale, [300.0 / 72.0 * 2.0; 2]);
        assert!(!p.clipped);
        let t = p.page_to_printable(&page.geometry());
        assert_maps(t, [0.0, 396.0], [0.0, 0.0]);
        assert_maps(t, [306.0, 0.0], [2550.0, 3300.0]);
        assert_maps(t, [153.0, 198.0], [1275.0, 1650.0]);
        // One inch on paper: 36 user units at UserUnit 2 is 300 px.
        let a = t * Point::new(10.0, 100.0);
        let b = t * Point::new(46.0, 100.0);
        assert!(((b.x - a.x) - 300.0).abs() < 1e-9);
    }

    #[test]
    fn crop_box_offset_and_rotate_are_in_the_transform() {
        let j = job(
            Paper::from_inches(11.0, 8.5, 72.0, 0.0),
            PrintScaling::ActualSize,
            PrintOrientation::AsIs,
        );
        // Crop 500 x 640 at (150, 260), /Rotate 270: displayed 640 wide, 500 high.
        let page = info(
            [100.0, 200.0, 712.0, 992.0],
            Some([150.0, 260.0, 650.0, 900.0]),
            270,
            1.0,
        );
        let p = layout_page(0, &page, &j);
        assert_eq!(p.size, [640, 500]);
        // Printable 792 x 612: centred at ((792-640)/2, (612-500)/2).
        assert_eq!(p.offset, [76, 56]);
        let t = p.page_to_printable(&page.geometry());
        // /Rotate 270: the crop box's top-right corner (x1, y1) is the display's top-left.
        assert_maps(t, [650.0, 900.0], [76.0, 56.0]);
        assert_maps(t, [150.0, 260.0], [76.0 + 640.0, 56.0 + 500.0]);
        assert_maps(t, [650.0, 260.0], [76.0 + 640.0, 56.0]);
        assert_maps(t, [150.0, 900.0], [76.0, 56.0 + 500.0]);
    }

    #[test]
    fn a_wide_sheet_turns_onto_portrait_paper_when_that_prints_larger() {
        let j = job(LETTER_300(), PrintScaling::Fit, PrintOrientation::Auto);
        // 34 x 22 in landscape sheet.
        let page = info([0.0, 0.0, 2448.0, 1584.0], None, 0, 1.0);
        let p = layout_page(0, &page, &j);
        assert_eq!(p.rotation, 270);
        // Turned: 1584 x 2448 pt = 6600 x 10200 px actual; fit 3150/10200.
        assert_eq!(p.size, [2039, 3150]);
        assert_eq!(p.offset, [180, 0]);
        let t = p.page_to_printable(&page.geometry());
        // A quarter turn anticlockwise: the page's top-right corner goes to the top-left.
        assert_maps(t, [2448.0, 1584.0], [180.0, 0.0]);
        assert_maps(t, [0.0, 1584.0], [180.0, 3150.0]);
        assert_maps(t, [0.0, 0.0], [180.0 + 2039.0, 3150.0]);

        // AsIs keeps it upright and smaller.
        let upright = layout_page(
            0,
            &page,
            &job(LETTER_300(), PrintScaling::Fit, PrintOrientation::AsIs),
        );
        assert_eq!(upright.rotation, 0);
        assert_eq!(upright.size, [2400, 1553]);
        assert_eq!(upright.offset, [0, 798]);
        // A portrait page stays upright under Auto.
        let portrait = layout_page(0, &info([0.0, 0.0, 612.0, 792.0], None, 0, 1.0), &j);
        assert_eq!(portrait.rotation, 0);
        // A page displayed landscape through /Rotate 90 turns too, on top of its /Rotate.
        let rotated = layout_page(0, &info([0.0, 0.0, 1584.0, 2448.0], None, 90, 1.0), &j);
        assert_eq!(rotated.rotation, 270);
        assert_eq!(rotated.size, [2039, 3150]);
        // Fixed rotation.
        let fixed = layout_page(
            0,
            &page,
            &job(
                LETTER_300(),
                PrintScaling::Fit,
                PrintOrientation::Rotate { degrees: 90 },
            ),
        );
        assert_eq!(fixed.rotation, 90);
        assert_maps(
            fixed.page_to_printable(&page.geometry()),
            [0.0, 0.0],
            [180.0, 0.0],
        );
    }

    #[test]
    fn actual_size_larger_than_the_paper_is_centred_and_clipped() {
        let j = job(
            LETTER_300(),
            PrintScaling::ActualSize,
            PrintOrientation::AsIs,
        );
        let page = info([0.0, 0.0, 2448.0, 1584.0], None, 0, 1.0);
        let p = layout_page(0, &page, &j);
        assert_eq!(p.size, [10200, 6600]);
        assert_eq!(p.offset, [-3900, -1725]);
        assert!(p.clipped);
        let (cols, rows) = p.visible(&j.paper).unwrap();
        assert_eq!((cols, rows), (0..2400, 0..3150));
        // Auto turns it, because more of it then shows (2400 x 3150 either way: the page
        // covers the whole area in both, so it stays upright).
        let auto = layout_page(
            0,
            &page,
            &job(
                LETTER_300(),
                PrintScaling::ActualSize,
                PrintOrientation::Auto,
            ),
        );
        assert_eq!(auto.rotation, 0);
        // A sheet that fits only when turned is turned.
        let tall_paper = Paper::from_inches(11.0, 17.0, 100.0, 0.0);
        let wide = info([0.0, 0.0, 1152.0, 720.0], None, 0, 1.0); // 16 x 10 in
        let a = layout_page(
            0,
            &wide,
            &job(tall_paper, PrintScaling::ActualSize, PrintOrientation::Auto),
        );
        assert_eq!(a.rotation, 270);
        assert_eq!(a.size, [1000, 1600]);
        assert_eq!(a.offset, [50, 50]);
        assert!(!a.clipped);
        // Not centred: top-left of the printable area.
        let mut nc = job(
            LETTER_300(),
            PrintScaling::ActualSize,
            PrintOrientation::AsIs,
        );
        nc.centre = false;
        let p = layout_page(0, &page, &nc);
        assert_eq!(p.offset, [0, 0]);
        let t = p.page_to_printable(&page.geometry());
        assert_maps(t, [0.0, 1584.0], [0.0, 0.0]);
    }

    #[test]
    fn shrink_to_fit_never_enlarges() {
        let j = job(
            LETTER_300(),
            PrintScaling::ShrinkToFit,
            PrintOrientation::Auto,
        );
        let small = info([0.0, 0.0, 288.0, 144.0], None, 0, 1.0); // 4 x 2 in
        let p = layout_page(0, &small, &j);
        assert_eq!(p.size, [1200, 600]);
        assert_eq!(p.rotation, 0, "fits both ways at full size: stays upright");
        assert_eq!(p.offset, [600, 1275]);
        let big = info([0.0, 0.0, 2448.0, 1584.0], None, 0, 1.0);
        let fit = layout_page(
            0,
            &big,
            &job(LETTER_300(), PrintScaling::Fit, PrintOrientation::Auto),
        );
        assert_eq!(layout_page(0, &big, &j), fit);
    }

    #[test]
    fn unequal_resolutions_scale_each_axis() {
        let mut paper = Paper::from_inches(8.5, 11.0, 300.0, 0.0);
        paper.dpi = [300.0, 600.0];
        paper.size = [2550, 6600];
        paper.printable = PrintableArea {
            x: 0,
            y: 0,
            width: 2550,
            height: 6600,
        };
        let page = info([0.0, 0.0, 612.0, 792.0], None, 0, 1.0);
        let p = layout_page(
            0,
            &page,
            &job(paper, PrintScaling::ActualSize, PrintOrientation::AsIs),
        );
        assert_eq!(p.size, [2550, 6600]);
        let p = layout_page(
            0,
            &page,
            &job(paper, PrintScaling::Fit, PrintOrientation::AsIs),
        );
        assert_eq!(p.size, [2550, 6600]);
        assert_maps(
            p.page_to_printable(&page.geometry()),
            [306.0, 396.0],
            [1275.0, 3300.0],
        );
    }

    #[test]
    fn bands_cover_the_visible_rows_without_overlap() {
        let j = job(LETTER_300(), PrintScaling::Fit, PrintOrientation::Auto);
        let page = info([0.0, 0.0, 612.0, 792.0], None, 0, 1.0);
        let p = layout_page(0, &page, &j);
        let bands = p.bands(&j.paper, 1000);
        assert_eq!(bands.len(), 4);
        assert_eq!(bands[0].y, 22);
        assert_eq!(bands[0].window, [0, 0, 2400, 1000]);
        assert_eq!(bands[3].window, [0, 3000, 2400, 106]);
        assert_eq!(bands.iter().map(|b| b.height).sum::<u32>(), 3106);
        for w in bands.windows(2) {
            assert_eq!(w[0].y + w[0].height, w[1].y);
        }
        // A clipped page: bands start inside the image.
        let big = layout_page(
            0,
            &info([0.0, 0.0, 2448.0, 1584.0], None, 0, 1.0),
            &job(
                LETTER_300(),
                PrintScaling::ActualSize,
                PrintOrientation::AsIs,
            ),
        );
        let b = big.bands(&j.paper, 3000);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].window, [3900, 1725, 2400, 3000]);
        assert_eq!((b[1].y, b[1].height), (3000, 150));
    }

    #[test]
    fn page_ranges_parse_like_a_print_dialog() {
        assert_eq!(parse_page_range("", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(
            parse_page_range("1-3, 5, 8-", 9).unwrap(),
            vec![0, 1, 2, 4, 7, 8]
        );
        assert_eq!(parse_page_range(" -2 ,2", 4).unwrap(), vec![0, 1, 1]);
        assert!(parse_page_range("0", 4).is_err());
        assert!(parse_page_range("5", 4).is_err());
        assert!(parse_page_range("3-2", 4).is_err());
        assert!(parse_page_range("x", 4).is_err());
        assert!(parse_page_range(" , ", 4).is_err());
    }

    #[test]
    fn job_validation_catches_bad_input() {
        let ok = PrintJob::new(vec![0, 1], LETTER_300());
        assert!(ok.validate(2).is_ok());
        assert!(ok.validate(1).is_err());
        assert!(PrintJob::new(vec![], LETTER_300()).validate(2).is_err());
        let mut j = ok.clone();
        j.band_rows = 0;
        assert!(j.validate(2).is_err());
        j = ok.clone();
        j.orientation = PrintOrientation::Rotate { degrees: 45 };
        assert!(j.validate(2).is_err());
    }

    #[test]
    fn band_conversion_produces_dib_layouts() {
        let src: Vec<u8> = vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        let mut bgra = src.clone();
        assert_eq!(convert_in_place(&mut bgra, 2, 2, BandFormat::Bgra8), 8);
        assert_eq!(bgra[..8], [3, 2, 1, 255, 6, 5, 4, 255]);
        let mut bgr = src.clone();
        // 2 px * 3 bytes = 6, padded to 8.
        assert_eq!(convert_in_place(&mut bgr, 2, 2, BandFormat::Bgr8), 8);
        assert_eq!(bgr, vec![3, 2, 1, 6, 5, 4, 0, 0, 9, 8, 7, 12, 11, 10, 0, 0]);
        for f in [BandFormat::Bgra8, BandFormat::Bgr8, BandFormat::Rgba8] {
            let mut px = src.clone();
            let stride = convert_in_place(&mut px, 2, 2, f);
            assert_eq!(f.rgba_at(&px[stride..], 1), [10, 11, 12, 255], "{f:?}");
        }
    }

    fn mock_job(format: BandFormat, rows: u32) -> (MockBackend, DocId, PrintJob) {
        let pages = [
            PageGeometry::simple(612.0, 792.0),
            PageGeometry::new(Rect::new(0.0, 0.0, 792.0, 612.0), None, Rotation::R0, 1.0),
        ];
        let mock = MockBackend::with_pages(&pages);
        let doc = mock.add_document();
        let mut j = PrintJob::new(vec![1, 0], Paper::from_inches(8.5, 11.0, 50.0, 0.5));
        j.format = format;
        j.band_rows = rows;
        (mock, doc, j)
    }

    #[test]
    fn mock_bands_stitch_into_the_page_image_in_every_format() {
        for format in [BandFormat::Bgra8, BandFormat::Bgr8, BandFormat::Rgba8] {
            let (mock, doc, j) = mock_job(format, 37);
            let mut sink = MemorySink::new();
            let report = print_job(&mock, doc, &j, &mut sink).unwrap();
            assert_eq!(sink.sheets.len(), 2);
            assert_eq!(sink.calls.first(), Some(&"begin_job"));
            assert_eq!(sink.calls.last(), Some(&"end_job"));
            assert_eq!(report.bands as usize, sink.bands.len());
            assert!(report.peak_band_bytes <= report.band_bound);
            for sheet in &sink.sheets {
                let p = sheet.placement;
                let (cols, rows) = p.visible(&j.paper).unwrap();
                for y in 0..sheet.image.height {
                    for x in 0..sheet.image.width {
                        let want = if cols.contains(&x) && rows.contains(&y) {
                            MockBackend::pixel(
                                p.page,
                                (x as i64 - p.offset[0]) as u32,
                                (y as i64 - p.offset[1]) as u32,
                            )
                        } else {
                            [255; 4]
                        };
                        assert_eq!(sheet.image.pixel(x, y), want, "{format:?} sheet ({x}, {y})");
                    }
                }
            }
            // The landscape page (printed first) was turned onto the portrait paper.
            assert_eq!(sink.sheets[0].placement.rotation, 270);
            for b in &sink.bands {
                assert!(b.height <= 37);
                assert_eq!(b.stride, format.stride(b.width));
            }
        }
    }

    #[test]
    fn a_cancel_from_the_sink_stops_the_job_and_aborts() {
        let (mock, doc, j) = mock_job(BandFormat::Bgra8, 50);
        let mut sink = MemorySink::new().cancel_at_band(3);
        let err = print_job(&mock, doc, &j, &mut sink).unwrap_err();
        assert_eq!(err, PrintError::Sink(SinkError::Cancelled));
        assert_eq!(sink.bands.len(), 3);
        assert_eq!(sink.calls.last(), Some(&"abort"));
        assert!(!sink.calls.contains(&"end_job"));
        let rendered = mock.rendered().len();
        assert_eq!(rendered, 4, "no band is rendered after the cancel");
    }

    #[test]
    fn a_render_failure_aborts_the_sink() {
        let (mock, doc, j) = mock_job(BandFormat::Bgra8, 50);
        mock.set_fail_renders(true);
        let mut sink = MemorySink::new();
        let err = print_job(&mock, doc, &j, &mut sink).unwrap_err();
        assert!(matches!(err, PrintError::Render(_)));
        assert_eq!(sink.calls, vec!["begin_job", "begin_page", "abort"]);
        // An invalid job never reaches the sink.
        let mut bad = j.clone();
        bad.pages = vec![7];
        let mut sink = MemorySink::new();
        assert!(matches!(
            print_job(&mock, doc, &bad, &mut sink),
            Err(PrintError::InvalidJob(_))
        ));
        assert!(sink.calls.is_empty());
    }

    #[test]
    fn window_requests_are_validated() {
        let base = WindowRequest {
            doc: DocId(1),
            page: 0,
            rotation: 90,
            size: [100, 50],
            window: [0, 10, 100, 40],
            options: WindowOptions::default(),
        };
        assert_eq!(base.validate().unwrap(), Rotation::R90);
        for bad in [
            WindowRequest {
                rotation: 45,
                ..base
            },
            WindowRequest {
                size: [0, 50],
                ..base
            },
            WindowRequest {
                window: [0, 11, 100, 40],
                ..base
            },
            WindowRequest {
                window: [0, 0, 0, 40],
                ..base
            },
            WindowRequest {
                size: [u32::MAX, 50],
                ..base
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let json = serde_json::to_string(&crate::Request::RenderWindow(base)).unwrap();
        assert_eq!(
            serde_json::from_str::<crate::Request>(&json).unwrap(),
            crate::Request::RenderWindow(base)
        );
    }
}
