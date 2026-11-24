//! pl-doc: the UI-free session layer of Plumbline (see docs/plan.md sections 6 and 7).
//!
//! * [`Session`] holds the open [`Document`]s and the render backend they share.
//! * A [`Document`] has its render mirror in the backend, its page geometries, and things made
//!   on first use: the lopdf handle, page text, the bookmark tree and per-page content-snap
//!   scenes. An open reads page sizes only ([`OpenMode::Interactive`]); nothing a user
//!   interface needs afterwards has to wait for the backend (see [`Document`]).
//!   [`Document::save_incremental`] is the save path with PDFium as the third verifier.
//! * A [`ViewState`] is one view of a document: zoom, scroll, view rotation and fit mode over a
//!   continuous vertical page layout, with hit-testing and the visible tile set.
//! * [`selection`] turns a drag between two page points into selected characters.
//! * [`DocSearch`] searches every page and steps through the hits.
//! * [`RecentFiles`] is the recent-files list on disk.
//! * Editing: a document's annotations are an [`Editor`] (pl-annot's set plus undo history
//!   and selection), built on first use. [`Command`]s go through [`Document::apply`];
//!   [`Document::overlay`] is what a user interface paints itself, [`Document::save`],
//!   [`Document::save_as`] and [`Document::save_optimised`] write, the [`journal`] protects
//!   unsaved work against a crash, [`Clipboard`] is copy and paste, [`DocView`] is what a
//!   tool sees, and [`script`] runs a JSON command file (the headless test driver).
//! * Saving and the first read of the annotations also run on a worker thread:
//!   [`Document::start_save`] and [`Document::start_load_annots`] return a ticket at once and
//!   a user interface polls it once per frame ([`background`]).
//! * [`markup_ops`] builds the commands of the markups list (text, review status, replies,
//!   checkmark, lock, custom columns, popups); [`markup_between`] turns a drag across a
//!   page's text into the quads of a text markup; [`import_image`] decodes a PNG or JPEG for
//!   an image stamp.
//! * Measurement (M6/M7): every page's scale and viewports and the document settings are a
//!   [`ScaleStore`] in the [`Editor`]; the scale commands of [`Command`] change them as undo
//!   steps that also put every dependent measurement on its new scale. [`measure_ops`] builds
//!   measurement commands (new measurements, geometry, cutouts, depth, count markers),
//!   [`snap`] is the snap service, and [`scales`] holds the legend, space and sequence
//!   definitions with the [`TakeoffRefresh`] hook.
//!
//! Nothing here uses egui or opens a window, so all of it runs under `cargo test` with
//! [`pl_render::MockBackend`] or with PDFium and the pl-testkit fixtures.
//!
//! Coordinates follow pl-geom: page space is unrotated user space, y-up; view space is logical
//! pixels, y-down.

pub mod annot_data;
pub mod background;
pub mod bookmarks;
pub mod clipboard;
pub mod command;
pub mod docview;
pub mod editing;
pub mod error;
pub mod images;
pub mod journal;
pub mod layers;
pub mod markup_ops;
pub mod measure_data;
pub mod measure_ops;
mod measuring;
pub mod recent;
pub mod scales;
pub mod script;
pub mod script_measure;
pub mod search;
pub mod selection;
pub mod session;
pub mod snap;
pub mod view;

#[cfg(test)]
mod testutil;

pub use annot_data::{AnnotData, FreeTextData, GeometryData, PdfValue, StampData, StyleData};
pub use background::{Busy, LoadTicket, SaveRequest, SaveTarget, SaveTicket};
pub use bookmarks::{BookmarkNode, BookmarkRow, BookmarkTree};
pub use clipboard::{Clipboard, PastePlacement};
pub use command::{Command, EditError, Editor, Outcome, Replayed, Selection, GROUP_KEY};
pub use docview::{DocSnapshot, DocView};
pub use editing::{
    MarkupSave, MirrorSlot, MovedSlot, OverlayItem, PendingJournal, TileInvalidation,
};
pub use error::{DocError, Result};
pub use images::{
    import_image, import_image_file, ImageData, ImportError, ImportedImage, MAX_IMAGE_PIXELS,
};
pub use journal::{default_journal_dir, FileStamp, Journal, Record};
pub use layers::Layer;
pub use measure_data::{
    FormatData, MeasureSpecData, MeasurementData, PageScalesData, PrecisionData, ScaleSetting,
    ViewportData,
};
pub use measure_ops::{MeasureShape, MeasurementRow};
pub use recent::{
    default_data_dir, RecentEntry, RecentFiles, APP_DIR_NAME, DEFAULT_MAX_RECENT, RECENT_FILE_NAME,
};
pub use scales::{DocSettings, LegendDef, ScaleStore, SequenceDef, SpaceDef, TakeoffRefresh};
pub use search::DocSearch;
pub use selection::{
    char_at, markup_between, markup_hit, markup_range, nearest_char, nearest_char_directed,
    quads_bounds, select_all, select_between, select_hit, select_line_at, select_range,
    select_word_at, text_direction_at, MarkupSelection, TextSelection,
};
pub use session::{
    Document, DocumentId, OpenMode, PdfiumCheck, SaveOutcome, SceneState, Session, SessionOptions,
    UserUnitPolicy, FILE_RELEASE_WAIT, SCENE_CACHE_PAGES, TEXT_CACHE_PAGES,
};
pub use snap::{SnapKind, SnapOptions, SnapOutcome, SnapTarget};
pub use view::{
    zoom_step, FitMode, PageHit, PageLayout, PageTiles, ViewState, DEFAULT_PAGE_GAP,
    DEFAULT_PAGE_MARGIN, ZOOM_STEPS,
};

// The crates a caller needs alongside this one, so `pl-app` and `pl-cli` can name their types
// without listing them as separate dependencies.
pub use pl_annot;
pub use pl_draw;
pub use pl_geom;
pub use pl_pdf;
pub use pl_render;
pub use pl_scene;
pub use pl_takeoff;
