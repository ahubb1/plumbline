//! pl-annot: the typed annotation model (see docs/plan.md sections 6 and 7).
//!
//! * [`Annotation`] is one annotation: a stable id (`/NM`), its page, a [`AnnotKind`], a
//!   [`Geometry`] in unrotated page user space, a [`Style`], [`Meta`] and, for an annotation
//!   that was read from a file, a [`Baseline`] holding the original dictionary untouched.
//! * [`AnnotSet`] is the annotations of a document in stacking order.
//!   [`AnnotSet::load`] reads them through pl-pdf and never fails on malformed input.
//! * [`Annotation::draw_list`] is the one description of what an annotation looks like. The
//!   screen painter, the `/AP /N` stream and the tiny-skia raster are all made from it.
//! * [`AnnotSet::plan_save`] compares the set with the file and produces a pl-pdf `EditSet`:
//!   complete dictionaries with generated appearance streams for new annotations, patches
//!   naming only the changed keys for edited ones, `/Annots` removals for deleted ones, and
//!   nothing at all for annotations that were not touched.
//! * [`Annotation::hit_test`], [`Annotation::handles`] and [`AnnotSet::hit_test`] are the
//!   hit-testing helpers; they return plain data and know nothing about any UI.
//!
//! # Coordinates
//!
//! Everything is in unrotated page user space, y up, as pl-geom defines it. `/Rotate`,
//! `/UserUnit` and the CropBox origin never appear here; they belong to the view transform.
//!
//! # What is written
//!
//! Every markup this crate writes is a standard annotation with a full `/AP /N` appearance
//! and the standard keys `/NM`, `/M`, `/CreationDate`, `/T`, `/Subj`, `/F` (print), `/CA`,
//! `/C`, `/IC`, `/BS`, `/BE`, `/LE`, `/Rect`, `/RD` and its geometry key, as they apply to
//! its kind. Private application data, if any, goes under the single key [`PRIVATE_KEY`].
//!
//! # Kinds
//!
//! Square, Circle, Line, PolyLine, Polygon, Ink, Highlight, Underline, StrikeOut, Squiggly,
//! Text (note), Link and Popup are modelled whatever file they come from.
//!
//! A **FreeText** (text box, callout, typewriter) is modelled when it holds a [`FreeText`]:
//! the ones made with [`Annotation::free_text`], [`Annotation::callout`] and
//! [`Annotation::typewriter`], which are read back as such, and another application's after
//! [`Annotation::make_editable`]. Its text is laid out by pl-draw
//! ([`Annotation::text_layout`]) and written as a full appearance with the standard-14
//! Helvetica font, beside `/DA`, `/DS`, `/RC`, `/Q`, `/IT`, `/CL`, `/LE`, `/RD`, `/BS`, `/C`
//! (the background) and `/Rotate`. Text outside WinAnsi is refused by name
//! ([`Annotation::validate`]), never dropped.
//!
//! A **Stamp** is modelled when it holds a [`Stamp`]: one of Plumbline's own
//! ([`Annotation::stamp`]) or a picture ([`Annotation::image_stamp`]).
//!
//! A foreign FreeText or Stamp and every other subtype are read and kept, are shown from
//! their own appearance, and can be moved, re-described and deleted but not reshaped or
//! restyled ([`Annotation::is_modelled`]).
//!
//! # Measurements
//!
//! A Line, PolyLine, Polygon or Ink that holds a [`Measurement`] is a measurement: Length,
//! Polylength, Perimeter, Area (with cutouts), Volume, Angle, Radius, Diameter or Count. It is
//! written as Bluebeam Revu writes the kind: the standard dimension intents
//! (`/IT /LineDimension`, `/PolyLineDimension`, `/PolygonDimension`) and Revu's own
//! (`/PolygonVolume`, `/PolygonRadius`, `/CircleDimension`, `/PolyLineAngle`,
//! `/PolygonCount`, a count as one annotation per marker), with Revu's `/MeasurementTypes`, a
//! rectilinear `/Measure` on the annotation as well as the page's `/VP`, the dimension-line
//! keys of a Line (`/LL`, `/LLE`, `/LLO`, `/Cap`, `/CP`, `/CO`), the value in `/Contents` and
//! drawn in the appearance; what no such key can say is in the private dictionary. Another
//! application's measurements with these keys (Revu's and Acrobat's) are read as measurements
//! too, with the annotation's own `/Measure` (or `/BBMeasure`) and number formats. Page scales
//! and viewports (`/VP`) are [`PageViewports`]. The table of what is written is in the module
//! documentation of `measure.rs` ([`Measurement`]).
//!
//! # Popups, replies, review status
//!
//! See [`AnnotSet::ensure_popup`], [`Annotation::reply`], [`Annotation::review_status`] and
//! [`AnnotSet::review_state`]; the structure written is `/Popup` and `/Parent`, `/IRT` and
//! `/RT`, `/State` and `/StateModel`.

mod appearance;
mod content;
mod hit;
mod markup;
mod measure;
mod measure_draw;
mod measure_io;
mod model;
mod objects;
mod read;
mod review;
mod set;
mod viewport;
mod write;

pub use pl_draw::shapes::{LineEnding, Quad};
pub use pl_draw::{
    BlendMode, Caret, DrawError, DrawList, FontStyle, HAlign, LayoutLine, Overflow, Rgb, RgbaImage,
    StampScheme, StampSpec, TextLayout, VAlign,
};

pub use appearance::{cloud_scallop, Placement};
pub use content::{
    callout_route, default_appearance, default_style, default_style_color, normalise_rotation,
    parse_default_appearance, rich_text, rotated_frame, FreeText, FreeTextIntent, ParsedDa, Stamp,
    StampContent,
};
pub use hit::{Handle, HandleKind, RectHandle};
pub use markup::{merge_quads_by_line, quad_for_box, reading_direction};
pub use measure::{
    format_number, CaptionPosition, CountSymbol, MeasureFormat, MeasureKind, MeasureSpec,
    Measurement, NumberFormat, NumberStyle, Quantities, ScaleRef,
};
pub use measure_draw::Caption;
pub use model::{
    AnnotId, AnnotKind, Annotation, Baseline, Flags, Geometry, LinkTarget, Member, Meta, Origin,
    Style, COLUMN_DATA_KEY, MODEL_KEY, PRIVATE_KEY,
};
pub use objects::{decode_text, encode_text, PdfDate};
pub use review::{default_popup_rect, ReviewState, Status, StatusSet, REVIEW_MODEL};
pub use set::{AnnotSet, Removed};
pub use viewport::{PageViewports, Viewport, ViewportError, PAGE_SCALE_NAME};
pub use write::{write_appearance, AnnotError, Change, Deletion, SavePlan};
