//! pl-scene: the vector geometry of a page's content, indexed for snapping
//! (see docs/plan.md sections 4, 6 and 7).
//!
//! A [`Scene`] holds the line segments of one page's content stream: straight
//! lines, rectangle edges and flattened curves. It is collected by walking
//! the page with hayro-interpret, and indexed in an R-tree. Annotations are
//! never part of a scene; markups are snapped to by their own model.
//!
//! # Coordinates
//!
//! Everything is in pl-geom page space: unrotated page user space, y-up, in
//! user units. `/Rotate`, `/UserUnit` and the CropBox offset are not applied
//! here; they belong to the view transform. A cursor position has to be
//! brought into page space (`ViewTransform::view_to_page`) before a snap
//! query, and the snap radius converted from pixels to user units.
//!
//! # Caps and degradation
//!
//! hayro can be slow or run away on hostile files, so a build has a time cap,
//! a segment cap and a memory cap ([`SceneLimits`]). A build never returns an
//! error and never lets a panic escape: it returns [`SceneBuild::Ready`] or
//! [`SceneBuild::NoContentSnap`] with a typed [`NoSnapReason`]. Use
//! [`spawn_scene_build`] to run it on a worker thread.
//!
//! Curve flattening is inside the caps: pieces are stored as they are produced, and curves
//! with coordinates no page can have are not flattened at all. A worker that has timed out
//! may still be running and still holds its [`SceneSource`]; [`SceneWorker`] tells when it has
//! really gone (see the note on [`SceneSource`]).
//!
//! # Not here yet
//!
//! Word grouping is not implemented; text arrives from hayro as glyph runs
//! and is ignored by the collector.

mod build;
mod scene;

pub use build::{
    build_scene, spawn_scene_build, NoSnapReason, SceneBuild, SceneJob, SceneLimits, SceneSource,
    SceneWorker,
};
pub use scene::{Scene, SceneRect, SceneStats, Segment, SegmentKind, SnapHit, SnapKind, SnapModes};
