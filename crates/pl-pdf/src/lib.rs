//! pl-pdf: the only crate that imports lopdf (see docs/plan.md sections 4, 6 and 7).
//!
//! What it does:
//!
//! - [`PdfFile`] is a lazy handle on a PDF on disk. Opening checks only the header; lopdf
//!   parses on the first call that needs the object graph.
//! - [`PdfDocument`] is the parsed file: page tree access ([`PageInfo`], consistent with
//!   `pl_geom::PageGeometry`), and a page's annotation dictionaries exactly as stored, unknown
//!   keys included.
//! - [`EditSet`] collects object-level changes: add, patch dirty keys, replace, delete
//!   annotations, add new objects such as appearance streams.
//! - [`PdfFile::save_incremental`] appends the changes to `name.pdf.tmp` so that the original
//!   bytes are an exact prefix, using the previous revision's cross-reference style, verifies
//!   the result with lopdf and hayro-syntax, and only then replaces the original atomically.
//!   [`PdfFile::save_full`] is the full-rewrite path with the same verify-then-replace tail.
//!   An in-place save refuses, with [`PdfError::ChangedOnDisk`], to replace a file that
//!   another program changed after it was loaded.
//! - [`PdfDocument::layers`] and [`PdfDocument::layer_override_tail`] are the layer-override
//!   builder: the in-memory incremental update that switches optional-content groups for the
//!   render mirror.
//! - [`probe_user_units`] reads `/UserUnit` through hayro-syntax without a lopdf parse.
//!
//! Coordinates are never transformed here: boxes and annotation rectangles are in unrotated
//! page user space (y-up), as pl-geom defines. `/Rotate`, `/UserUnit` and the CropBox origin
//! are only reported, through `PageGeometry`.
//!
//! Other crates build PDF objects with the lopdf object types re-exported below, so they do
//! not need lopdf as a dependency.

pub mod budget;
mod document;
mod edit;
mod error;
mod layers;
mod probe;
mod replace;
mod save;
mod verify;

pub use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat};

pub use document::{
    number, rect_from_object, AnnotationEntry, EncryptionInfo, LoadStats, PageInfo, PdfDocument,
    PdfFile, XrefStyle,
};
pub use edit::{DictPatch, EditSet};
pub use error::{PdfError, Result, VerifyError};
pub use layers::LayerInfo;
pub use probe::{mentions_user_unit, probe_user_units, ProbeError};
pub use replace::{replace_file, ReplaceMethod};
pub use save::{tmp_path_for, ExternalCheck, FailPoint, SaveKind, SaveOptions, SaveReport};
pub use verify::{verify_bytes, verify_file, Expected, SecondParser, VerifyReport};
