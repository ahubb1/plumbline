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
//! - The document-structure layer (milestone M8), as pure operations that give a new
//!   [`PdfDocument`] to save with [`PdfFile::save_restructured`] (or
//!   [`PdfDocument::save_new`]): the full rewrite, verified, atomically replaced, unused
//!   objects pruned, and refused for a signed file unless [`SignaturePolicy::Invalidate`] is
//!   passed ([`PdfDocument::signatures`] says what would break).
//!   - Page operations through [`Restructure`] (insert blank or from another PDF, delete,
//!     reorder, move, rotate, replace, extract) and [`PdfDocument::merge`] /
//!     [`PdfDocument::split`]; each returns a [`PageOpReport`]. The `pages` module
//!     documentation lists what happens to outlines, links, named destinations, labels,
//!     fields, layers and Bluebeam columns.
//!   - Outlines: [`PdfDocument::outline`] reads the whole tree into an [`Outline`] that is
//!     edited by path and written back with [`PdfDocument::with_outline`] (full rewrite) or
//!     [`PdfDocument::outline_edits`] (incremental).
//!   - Links: [`PdfDocument::links`], [`LinkSpec`] for creating and patching them through an
//!     [`EditSet`] (incremental), and resolution of destinations to a page and a point
//!     ([`ResolvedDest`]). Nothing is ever opened or run.
//!   - Page labels ([`PdfDocument::page_labels`]) and named destinations
//!     ([`PdfDocument::named_destinations`]).
//!   - Flatten ([`PdfDocument::flatten`]): appearances burnt into the page content by the
//!     ISO 32000-1 12.5.5 placement, with the flag rules in [`FlattenOptions`]' module.
//!
//! Coordinates are never transformed here: boxes and annotation rectangles are in unrotated
//! page user space (y-up), as pl-geom defines. `/Rotate`, `/UserUnit` and the CropBox origin
//! are only reported, through `PageGeometry`.
//!
//! Other crates build PDF objects with the lopdf object types re-exported below, so they do
//! not need lopdf as a dependency.

pub mod budget;
mod copy;
mod dest;
mod document;
mod edit;
mod error;
mod flatten;
mod labels;
mod layers;
mod links;
mod memory;
mod outline;
mod pages;
mod probe;
mod replace;
mod save;
mod signatures;
mod verify;

pub use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat};

pub use dest::{DestView, Destination, NamedDest, ResolvedDest};
pub use flatten::{FlattenOptions, FlattenReport, FlattenSkip, FlattenTarget, Flattened};
pub use labels::{LabelRange, LabelStyle, PageLabel};
pub use links::{Link, LinkAction, LinkDest, LinkSpec, RemoteDest};
pub use outline::{FlatOutlineItem, Outline, OutlineItem, OutlineTarget};
pub use pages::{BlankPage, PageOpReport, RemovedLink, ReplaceOptions, Restructure};
pub use signatures::SignatureInfo;

/// Decode a PDF text string (PDFDocEncoding, UTF-16BE with a byte-order mark, or UTF-8).
pub fn decode_text(object: &Object) -> Option<String> {
    lopdf::decode_text_string(object).ok()
}

/// Encode text as a PDF text string: literal for ASCII, UTF-16BE (hex) otherwise.
pub fn encode_text(text: &str) -> Object {
    lopdf::text_string(text)
}

pub use document::{
    number, rect_from_object, AnnotationEntry, EncryptionInfo, LoadStats, PageInfo, PdfDocument,
    PdfFile, XrefStyle,
};
pub use edit::{DictPatch, EditSet};
pub use error::{PdfError, Result, VerifyError};
pub use layers::LayerInfo;
pub use probe::{mentions_user_unit, probe_user_units, ProbeError};
pub use replace::{replace_file, ReplaceMethod};
pub use save::{
    tmp_path_for, ExternalCheck, FailPoint, SaveKind, SaveOptions, SaveReport, SignaturePolicy,
};
pub use verify::{verify_bytes, verify_file, Expected, SecondParser, VerifyReport};
