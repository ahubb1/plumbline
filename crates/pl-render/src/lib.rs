//! pl-render: the render side of Plumbline (see docs/plan.md sections 6 and 7).
//!
//! This is the only crate that uses `pdfium-render`. Everything it exposes is plain data:
//!
//! * [`RenderBackend`]: one `call(Request) -> Reply` plus typed helpers. Requests and replies
//!   ([`types`]) are serialisable, so the engine can later move to a worker process.
//! * [`PdfiumBackend`]: PDFium on one dedicated thread, loaded by absolute path, with a small
//!   LRU of open page handles kept for rendering only (text, search and geometry reads use a
//!   separate slot, so they never push a dense sheet out of it). Its in-memory document is the
//!   "render mirror": it can be mutated (an annotation hidden) but is never saved.
//! * [`Scheduler`]: priority queue (visible tiles centre-out, then text/geometry/outline
//!   queries, then the prefetch ring, then thumbnails) with generation-based cancellation.
//!   Everything a user interface needs from the backend goes through it, so a frame never
//!   waits on the PDFium thread.
//! * [`TileCache`]: a byte-budgeted LRU keyed by document, page, content revision, level, x, y,
//!   layer hash and render options; 512 MB by default.
//! * [`MockBackend`]: a deterministic backend for tests.
//!
//! # Conventions
//!
//! Coordinates in requests and replies (text boxes, search quads, annotation rectangles,
//! outline targets) are in pl-geom *page space*: unrotated page user space, y-up. Bitmaps are in
//! *display* orientation: `/Rotate` applied, the crop box's top-left corner at pixel (0, 0).
//! A tile level `L` is `2^L` pixels per user-space unit, as in [`pl_geom::TileGrid`].

pub mod backend;
pub mod budget;
pub mod cache;
pub mod mock;
pub mod pdfium;
pub mod scheduler;
pub mod text;
pub mod types;

pub use backend::RenderBackend;
pub use cache::{CacheStats, TileCache, TileKey, DEFAULT_CACHE_BUDGET, ENTRY_OVERHEAD};
pub use mock::MockBackend;
pub use pdfium::{resolve_pdfium_dll, PdfiumBackend, PAGE_HANDLE_LRU, PDFIUM_DLL_ENV, TILE_GUTTER};
pub use scheduler::{
    Generation, Job, JobKind, JobOutput, JobResult, Priority, QueuedView, Scheduler,
    SchedulerStats, StreamId, ViewRequest,
};
pub use text::{quad_bounds, quads_for_chars, rect_to_quad, search_page_text};
pub use types::{
    check_bitmap_size, page_pixel_size, thumbnail_scale, tile_grid_for, AnnotInfo, Bitmap, DocId,
    DocInfo, DocSource, OpenRequest, OutlineItem, PageInfo, PageText, Quad, RectArray, RenderError,
    RenderOptions, Reply, Request, SearchHit, SearchOptions, TextChar, TileRequest,
    MAX_BITMAP_PIXELS,
};
