//! Plain, serialisable request and reply data for [`RenderBackend`](crate::RenderBackend).
//!
//! Nothing in this module borrows from PDFium or holds a handle, so a backend can later live in
//! a worker process and exchange these values over a pipe.

use std::path::PathBuf;

use pl_geom::{PageGeometry, Rect, Rotation, TileGrid, TileId};
use serde::{Deserialize, Serialize};

/// Identifies one open document inside a backend. Ids are never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DocId(pub u64);

/// A rectangle as `[x0, y0, x1, y1]` in unrotated page user space (y-up).
pub type RectArray = [f64; 4];

/// A quadrilateral in unrotated page user space, in the order bottom-left, bottom-right,
/// top-right, top-left (as seen in page space, before `/Rotate`).
pub type Quad = [[f64; 2]; 4];

/// The largest bitmap a backend will render in one request, in pixels (about 512 MB of RGBA).
pub const MAX_BITMAP_PIXELS: u64 = 128 * 1024 * 1024;

/// Where a document comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DocSource {
    /// A file on disk. It is memory-mapped and read on demand, never copied into RAM.
    ///
    /// `appended` is a tail of bytes that is presented to the parser as if it followed the file's
    /// own bytes. It is how an in-memory incremental update (for example an `/OCProperties /D`
    /// override that switches layers) is applied without touching the file. Leave it empty for a
    /// plain open.
    File { path: PathBuf, appended: Vec<u8> },
    /// A whole document held in memory. The backend takes ownership; no second copy is made.
    Bytes(Vec<u8>),
}

impl DocSource {
    /// A plain file open.
    pub fn file(path: impl Into<PathBuf>) -> DocSource {
        DocSource::File {
            path: path.into(),
            appended: Vec::new(),
        }
    }
}

/// Request to open a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub source: DocSource,
    /// `None` tries the empty user password.
    pub password: Option<String>,
}

/// Reply to a successful open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocInfo {
    pub doc: DocId,
    pub page_count: u32,
}

/// The geometry of one page as plain data. Convert with [`PageInfo::geometry`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageInfo {
    pub media_box: RectArray,
    pub crop_box: Option<RectArray>,
    /// `/Rotate` in degrees clockwise: 0, 90, 180 or 270.
    pub rotate: i32,
    /// `/UserUnit`; 1.0 unless set. See [`Request::SetPageUserUnit`].
    pub user_unit: f64,
}

impl PageInfo {
    /// The pl-geom view of this page.
    pub fn geometry(&self) -> PageGeometry {
        let rect = |r: RectArray| Rect::new(r[0], r[1], r[2], r[3]);
        PageGeometry::new(
            rect(self.media_box),
            self.crop_box.map(rect),
            Rotation::from_degrees(self.rotate).unwrap_or(Rotation::R0),
            self.user_unit,
        )
    }

    /// A stand-in for a page of which only the displayed size is known (see
    /// [`Request::PageSizes`]): a box of that size at the origin, unrotated. It lays out and
    /// tiles exactly like the real page, because both depend only on the displayed size, but
    /// its page space is not the file's: do not use it to place text boxes, annotation
    /// rectangles or hit tests. Ask for the real [`PageInfo`] first.
    pub fn provisional(display_size: [f64; 2]) -> PageInfo {
        PageInfo {
            media_box: [0.0, 0.0, display_size[0], display_size[1]],
            crop_box: None,
            rotate: 0,
            user_unit: 1.0,
        }
    }

    /// The plain-data form of a pl-geom page.
    pub fn from_geometry(geometry: &PageGeometry) -> PageInfo {
        let arr = |r: Rect| [r.x0, r.y0, r.x1, r.y1];
        PageInfo {
            media_box: arr(geometry.media_box()),
            crop_box: Some(arr(geometry.crop_box())),
            rotate: geometry.rotate().degrees(),
            user_unit: geometry.user_unit(),
        }
    }
}

/// What to draw besides the page content. Form fields are always drawn: PDFium's form pass
/// cannot be switched off without also switching to matrix rendering, which the tile path avoids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RenderOptions {
    /// Draw annotations from their own appearance streams (hidden ones are skipped).
    pub annotations: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { annotations: true }
    }
}

/// Request for one 512 px tile (edge tiles are smaller). See [`pl_geom::TileGrid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileRequest {
    pub doc: DocId,
    pub page: u32,
    pub level: i8,
    pub x: u32,
    pub y: u32,
    pub options: RenderOptions,
}

impl TileRequest {
    pub fn new(doc: DocId, page: u32, tile: TileId) -> TileRequest {
        TileRequest {
            doc,
            page,
            level: tile.level,
            x: tile.x,
            y: tile.y,
            options: RenderOptions::default(),
        }
    }

    pub fn tile(&self) -> TileId {
        TileId {
            level: self.level,
            x: self.x,
            y: self.y,
        }
    }
}

/// What a [`WindowRequest`] draws besides the page content. The print path uses it
/// ([`crate::print`]); the screen path keeps [`RenderOptions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowOptions {
    /// Draw annotations (other than form-field widgets) from their own appearance streams.
    pub annotations: bool,
    /// Draw form-field widgets (with their values). When off, every widget on the page is
    /// left out for the duration of the request.
    pub form_fields: bool,
    /// Apply the print rules to annotation flags (ISO 32000 12.5.3): an annotation without the
    /// `Print` flag is left out, and one with `Print` and `NoView` is drawn even though the
    /// screen does not show it. When off, annotations are drawn as on screen.
    pub print_flags: bool,
}

impl Default for WindowOptions {
    fn default() -> Self {
        WindowOptions {
            annotations: true,
            form_fields: true,
            print_flags: false,
        }
    }
}

/// Request for a window of a page drawn at an arbitrary pixel size and with an extra
/// rotation: the building block of banded printing ([`crate::print`]).
///
/// The page is drawn, `/Rotate` applied and then `rotation` more degrees clockwise, so that its
/// crop box fills exactly `size` pixels (the two axes may scale differently, for a printer
/// whose horizontal and vertical resolutions differ). Of that image only `window`
/// (`[x, y, width, height]`, in its pixels) is rendered and returned. Like a tile, the window
/// is placed with origin offsets, so a window is pixel-for-pixel the same as the matching part
/// of the whole image rendered in one go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowRequest {
    pub doc: DocId,
    pub page: u32,
    /// Extra clockwise rotation in degrees, on top of `/Rotate`: 0, 90, 180 or 270.
    pub rotation: i32,
    /// The pixel size of the whole page image, after both rotations.
    pub size: [u32; 2],
    /// `[x, y, width, height]` of the part to render, inside `size`.
    pub window: [u32; 4],
    pub options: WindowOptions,
}

/// The largest page image a [`WindowRequest`] may describe, per side (PDFium takes the size as
/// a C `int` and adds offsets to it).
pub const MAX_IMAGE_SIDE: u32 = i32::MAX as u32 / 2;

impl WindowRequest {
    /// Checks the request's numbers and returns the extra rotation.
    pub fn validate(&self) -> Result<Rotation, RenderError> {
        let rotation = Rotation::from_degrees(self.rotation).ok_or_else(|| {
            RenderError::InvalidRequest(format!(
                "the rotation must be a multiple of 90 degrees, got {}",
                self.rotation
            ))
        })?;
        let [w, h] = self.size;
        if w == 0 || h == 0 || w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
            return Err(RenderError::TooLarge {
                width: w as u64,
                height: h as u64,
            });
        }
        let [x, y, ww, wh] = self.window;
        check_bitmap_size(ww, wh)?;
        if x as u64 + ww as u64 > w as u64 || y as u64 + wh as u64 > h as u64 {
            return Err(RenderError::InvalidRequest(format!(
                "the window {:?} is outside the {w} x {h} px page image",
                self.window
            )));
        }
        Ok(rotation)
    }
}

/// The reply to a [`WindowRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowReply {
    /// The window's pixels, `window[2] x window[3]`.
    pub bitmap: Bitmap,
    /// The pixel memory the backend held at its peak while rendering this window, in bytes
    /// (its scratch bitmap plus the reply). At most [`window_scratch_bound`].
    pub scratch_bytes: u64,
}

/// The most pixel memory a backend may hold for one `width x height` window: a scratch bitmap
/// with the [`TILE_GUTTER`](crate::TILE_GUTTER) before and the
/// [`WINDOW_TRAILING_GUTTER`](crate::pdfium::WINDOW_TRAILING_GUTTER) after it on each axis,
/// plus the reply itself.
pub fn window_scratch_bound(width: u32, height: u32) -> u64 {
    let g = (crate::pdfium::TILE_GUTTER + crate::pdfium::WINDOW_TRAILING_GUTTER) as u64;
    let (w, h) = (width as u64, height as u64);
    ((w + g) * (h + g) + w * h) * 4
}

/// An RGBA8 image: row-major, top row first, no padding (`stride = width * 4`), alpha always 255.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bitmap({}x{})", self.width, self.height)
    }
}

impl Bitmap {
    /// A bitmap filled with one colour.
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Bitmap {
        let n = width as usize * height as usize;
        let mut pixels = Vec::with_capacity(n * 4);
        for _ in 0..n {
            pixels.extend_from_slice(&rgba);
        }
        Bitmap {
            width,
            height,
            pixels,
        }
    }

    /// Size of the pixel data in bytes.
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    /// The pixel at `(x, y)`. Panics when out of range.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(x < self.width && y < self.height, "pixel out of range");
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Rec. 601 grey level of the pixel at `(x, y)`.
    pub fn grey(&self, x: u32, y: u32) -> u8 {
        let [r, g, b, _] = self.pixel(x, y);
        ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114 + 500) / 1000) as u8
    }

    /// Copies `src` into this bitmap with its top-left corner at `(x, y)`, clipped to this
    /// bitmap. Used to stitch tiles.
    pub fn blit(&mut self, src: &Bitmap, x: u32, y: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let w = src.width.min(self.width - x) as usize;
        let h = src.height.min(self.height - y) as usize;
        for row in 0..h {
            let s = row * src.width as usize * 4;
            let d = ((y as usize + row) * self.width as usize + x as usize) * 4;
            self.pixels[d..d + w * 4].copy_from_slice(&src.pixels[s..s + w * 4]);
        }
    }
}

/// One character of a page's text with its box in unrotated page user space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextChar {
    pub ch: char,
    /// The loose glyph box (full line height, advance width). All zeros for characters that
    /// have no box, such as the line breaks PDFium generates.
    pub bbox: RectArray,
    /// The glyph origin on the baseline.
    pub origin: [f64; 2],
    /// Font size in user-space units.
    pub font_size: f32,
    /// `true` for characters PDFium synthesised (spaces between words, line breaks).
    pub generated: bool,
}

impl TextChar {
    /// Whether the character has a usable box.
    pub fn has_box(&self) -> bool {
        self.bbox[2] > self.bbox[0] && self.bbox[3] > self.bbox[1]
    }
}

/// The text of one page in reading order as PDFium reports it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PageText {
    pub page: u32,
    pub chars: Vec<TextChar>,
}

impl PageText {
    /// The characters as a string (line breaks included, as `\r\n`).
    pub fn text(&self) -> String {
        self.chars.iter().map(|c| c.ch).collect()
    }

    /// The non-empty lines of the text, trimmed.
    pub fn lines(&self) -> Vec<String> {
        self.text()
            .split(['\r', '\n'])
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

/// Options for a text search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    pub match_case: bool,
    pub whole_word: bool,
}

/// One search hit: a run of characters and the quads that cover it (one per line).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub page: u32,
    /// Index of the first matched character in [`PageText::chars`].
    pub char_start: u32,
    /// Number of characters from `char_start` to the last matched character, inclusive.
    pub char_len: u32,
    pub quads: Vec<Quad>,
}

/// One bookmark, in document (pre-order) sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlineItem {
    pub title: String,
    /// 0 for top-level bookmarks.
    pub level: u32,
    /// Target page, if the bookmark has a resolvable destination.
    pub page: Option<u32>,
    /// Target position in page user space, where the destination gives one.
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub zoom: Option<f64>,
}

/// A summary of one annotation in the render mirror.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotInfo {
    /// Index in the page's `/Annots` array.
    pub index: u32,
    /// PDFium's name for the subtype, for example `Square` or `Widget`.
    pub subtype: String,
    pub rect: RectArray,
    pub hidden: bool,
    /// `/NM`, when present.
    pub name: Option<String>,
}

/// Every request a backend understands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Open(OpenRequest),
    Close {
        doc: DocId,
    },
    PageCount {
        doc: DocId,
    },
    /// Loads the page (PDFium parses its content) to read its boxes and rotation. Inherited
    /// boxes are resolved. For the sizes of all pages at once, without loading any, see
    /// [`Request::PageSizes`].
    PageInfo {
        doc: DocId,
        page: u32,
    },
    /// The displayed size of every page (visible box after `/Rotate`, in user-space units;
    /// `/UserUnit` is not applied), read from the page dictionaries without loading a page.
    /// The cost does not grow with the amount of drawing on the pages, so this is what an
    /// open uses. A page whose size cannot be read reports `None`; the document stays usable.
    PageSizes {
        doc: DocId,
    },
    /// How many times this document's pages have been loaded (and their content parsed) since
    /// it was opened. A diagnostic: it is how tests show that an open loads no page and that a
    /// search does not make the page on screen load again.
    PageLoads {
        doc: DocId,
    },
    /// Records a page's `/UserUnit`. PDFium 8125 has no API that reports it, so the owner of
    /// the lopdf model supplies it; it only affects the reported [`PageInfo`], never pixels.
    SetPageUserUnit {
        doc: DocId,
        page: u32,
        user_unit: f64,
    },
    RenderTile(TileRequest),
    /// The whole page at `scale` pixels per user-space unit.
    RenderPage {
        doc: DocId,
        page: u32,
        scale: f64,
        options: RenderOptions,
    },
    /// The whole page scaled so its longer side is `max_px` pixels.
    Thumbnail {
        doc: DocId,
        page: u32,
        max_px: u32,
    },
    PageText {
        doc: DocId,
        page: u32,
    },
    Search {
        doc: DocId,
        page: u32,
        query: String,
        options: SearchOptions,
    },
    Outline {
        doc: DocId,
    },
    Annotations {
        doc: DocId,
        page: u32,
    },
    /// Sets or clears the hidden flag of one annotation in the in-memory mirror. Nothing is
    /// saved. The caller must bump the content revision of the tiles under the annotation.
    SetAnnotationHidden {
        doc: DocId,
        page: u32,
        index: u32,
        hidden: bool,
    },
    /// Moves (and resizes) one annotation in the in-memory mirror by giving it a new `/Rect`:
    /// its own appearance is then drawn in the new rectangle. Nothing is saved. This is how
    /// an annotation the application cannot draw itself (a foreign stamp, say) is shown at
    /// its new place before the save. The caller invalidates the tiles under the old and the
    /// new rectangle.
    SetAnnotationRect {
        doc: DocId,
        page: u32,
        index: u32,
        /// `[x0, y0, x1, y1]` in page space; any two opposite corners.
        rect: RectArray,
    },
    /// A window of a page drawn at any pixel size and extra rotation (the print path).
    RenderWindow(WindowRequest),
}

/// Every reply a backend sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Opened(DocInfo),
    Done,
    PageCount(u32),
    PageInfo(PageInfo),
    PageSizes(Vec<Option<[f64; 2]>>),
    PageLoads(u64),
    Bitmap(Bitmap),
    Text(PageText),
    Hits(Vec<SearchHit>),
    Outline(Vec<OutlineItem>),
    Annotations(Vec<AnnotInfo>),
    Window(WindowReply),
}

/// Errors a backend reports. Serialisable like everything else on the wire.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum RenderError {
    #[error("could not load the PDFium library: {0}")]
    LibraryLoad(String),
    #[error("could not open the document: {0}")]
    Open(String),
    #[error("the document needs a password")]
    PasswordRequired,
    #[error("unknown document {0:?}")]
    UnknownDocument(DocId),
    #[error("page {page} is out of range (the document has {count} pages)")]
    PageOutOfRange { page: u32, count: u32 },
    #[error("the tile is outside the page's tile grid")]
    TileOutOfRange,
    #[error("a {width} x {height} px bitmap is too large")]
    TooLarge { width: u64, height: u64 },
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("render backend error: {0}")]
    Backend(String),
    #[error("the render backend has shut down")]
    Disconnected,
    #[error("the render backend sent an unexpected reply")]
    UnexpectedReply,
}

/// The pixel size of a whole page at `scale` pixels per user-space unit. For a power-of-two
/// scale this equals [`TileGrid::width_px`] / [`TileGrid::height_px`] of that level, which is
/// what makes stitched tiles match a full-page render exactly.
pub fn page_pixel_size(geometry: &PageGeometry, scale: f64) -> (u32, u32) {
    let d = geometry.display_size();
    let px = |units: f64| ((units * scale - 1e-6).ceil().max(1.0)) as u32;
    (px(d.width), px(d.height))
}

/// Checks a bitmap size against [`MAX_BITMAP_PIXELS`].
pub fn check_bitmap_size(width: u32, height: u32) -> Result<(), RenderError> {
    if width == 0 || height == 0 || width as u64 * height as u64 > MAX_BITMAP_PIXELS {
        return Err(RenderError::TooLarge {
            width: width as u64,
            height: height as u64,
        });
    }
    Ok(())
}

/// The thumbnail scale (pixels per user-space unit) that makes the page's longer side `max_px`.
pub fn thumbnail_scale(geometry: &PageGeometry, max_px: u32) -> f64 {
    let d = geometry.display_size();
    let longest = d.width.max(d.height).max(1e-9);
    max_px.max(1) as f64 / longest
}

/// The tile grid for a tile request, or `TileOutOfRange` if the tile is not in it.
pub fn tile_grid_for(
    geometry: &PageGeometry,
    tile: TileId,
) -> Result<(TileGrid, pl_geom::PixelRect), RenderError> {
    let grid = TileGrid::new(geometry, tile.level);
    if grid.level() != tile.level {
        return Err(RenderError::TileOutOfRange);
    }
    let rect = grid
        .tile_pixel_rect(tile)
        .ok_or(RenderError::TileOutOfRange)?;
    Ok((grid, rect))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_pixel_size_matches_tile_grid_at_power_of_two_scales() {
        let pages = [
            PageGeometry::simple(612.0, 792.0),
            PageGeometry::new(
                Rect::new(10.0, 20.0, 622.3, 812.7),
                Some(Rect::new(30.0, 40.0, 500.5, 700.25)),
                Rotation::R90,
                2.0,
            ),
        ];
        for page in &pages {
            for level in -3i8..=4 {
                let grid = TileGrid::new(page, level);
                let (w, h) = page_pixel_size(page, pl_geom::level_scale(level));
                assert_eq!((w, h), (grid.width_px(), grid.height_px()), "level {level}");
            }
        }
    }

    #[test]
    fn page_info_round_trips_through_geometry() {
        let info = PageInfo {
            media_box: [0.0, 0.0, 612.0, 792.0],
            crop_box: Some([36.0, 36.0, 576.0, 756.0]),
            rotate: 270,
            user_unit: 2.0,
        };
        let back = PageInfo::from_geometry(&info.geometry());
        assert_eq!(back, info);
    }

    #[test]
    fn requests_and_replies_serialise() {
        let requests = vec![
            Request::Open(OpenRequest {
                source: DocSource::file("C:/x/y.pdf"),
                password: Some("pw".into()),
            }),
            Request::RenderTile(TileRequest::new(
                DocId(3),
                7,
                TileId {
                    level: -2,
                    x: 1,
                    y: 2,
                },
            )),
            Request::Search {
                doc: DocId(1),
                page: 0,
                query: "wall".into(),
                options: SearchOptions {
                    match_case: true,
                    whole_word: false,
                },
            },
        ];
        for r in requests {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), r);
        }
        let replies = vec![
            Reply::Bitmap(Bitmap::filled(2, 1, [1, 2, 3, 255])),
            Reply::Hits(vec![SearchHit {
                page: 0,
                char_start: 4,
                char_len: 3,
                quads: vec![[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]],
            }]),
        ];
        for r in replies {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(serde_json::from_str::<Reply>(&json).unwrap(), r);
        }
        let e = RenderError::PageOutOfRange { page: 9, count: 2 };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<RenderError>(&json).unwrap(), e);
    }

    #[test]
    fn blit_clips_and_copies() {
        let mut dst = Bitmap::filled(4, 3, [0, 0, 0, 255]);
        let src = Bitmap::filled(3, 3, [9, 8, 7, 255]);
        dst.blit(&src, 2, 1);
        assert_eq!(dst.pixel(1, 1), [0, 0, 0, 255]);
        assert_eq!(dst.pixel(2, 1), [9, 8, 7, 255]);
        assert_eq!(dst.pixel(3, 2), [9, 8, 7, 255]);
        assert_eq!(dst.pixel(3, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn oversized_bitmaps_are_refused() {
        assert!(check_bitmap_size(512, 512).is_ok());
        assert!(check_bitmap_size(0, 5).is_err());
        assert!(check_bitmap_size(100_000, 100_000).is_err());
    }
}
