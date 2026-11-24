//! pl-draw: one description of what a markup looks like, and three ways to show it (see
//! docs/plan.md sections 6 and 7).
//!
//! * [`DrawList`] is plain data: paths with stroke and fill, single-style text runs, images and
//!   groups, in page space. [`shapes`] builds the paths markups are made of (rectangle, ellipse,
//!   polyline, polygon, smoothed ink, cloud border, highlight quads, lines with the PDF `/LE`
//!   endings), and [`DrawList::bounds`] gives the box that holds all of it, strokes and endings
//!   included.
//! * [`layout`] wraps a string into a box (lines, carets, overflow) and [`stamp`] builds the
//!   project's own stamp artwork; both produce ops for a list.
//! * [`raster`] draws a list with tiny-skia into an RGBA pixmap under a page-to-pixel transform,
//!   compositing Multiply ops against whatever the pixmap already holds.
//! * [`emit`] writes the same list as a PDF content stream plus a description of the resources
//!   it needs, as plain data for `pl-pdf` to turn into objects. This crate does not use lopdf.
//!
//! A screen painter (in `pl-app`) is the third reader of the same list. Because every output
//! starts from one `DrawList`, the appearance stream saved in the file, the tiny-skia oracle and
//! what is painted while editing cannot drift apart; the integration tests render the emitted
//! stream with PDFium and compare it with the rasteriser.
//!
//! # Coordinates
//!
//! Everything in a list is in pl-geom *page space*: unrotated page user space, y-up. `/Rotate`,
//! `/UserUnit` and the CropBox offset are never applied here; they are in the transform handed
//! to the rasteriser, and a viewer applies them to the emitted appearance itself.
//!
//! # Opacity
//!
//! Colours are opaque RGB. Opacity belongs to an op or a group. A [`PathOp`]'s opacity applies
//! to its fill and stroke separately; a [`Group`]'s applies to its children as one object (a PDF
//! isolated transparency group). [`DrawList::push_shape`] and [`DrawList::push_composite`]
//! choose between the two so a translucent shape looks like one translucent object.

pub mod emit;
pub mod layout;
pub mod list;
pub mod raster;
pub mod shapes;
pub mod stamp;
pub mod style;
pub mod text;

pub use kurbo;
pub use tiny_skia;

pub use emit::{
    emit, emit_with_bbox, Appearance, ExtGState, FontResource, FormXObject, ImageXObject,
    Resources, TransparencyGroup, XObject, XObjectResource,
};
pub use layout::{
    layout_text, Caret, HAlign, LayoutLine, Overflow, TextBoxStyle, TextLayout, VAlign,
};
pub use list::{stroke_bounds, DrawList, DrawOp, Group, ImageOp, PathOp, RgbaImage, TextRun};
pub use raster::{
    page_transform, pixmap_from_rgba, render, render_new, render_over_rgba, MAX_CANVAS_SIDE,
};
pub use shapes::{
    arrow_length, cloud_path, cloud_rect_path, ellipse_path, ending_shape, ending_size,
    highlight_op, highlight_path, ink_path, inset_for_stroke, line_ops, polygon_path, polyline_ops,
    polyline_path, rect_path, rect_quad, CloudStyle, EndingShape, LineEnding, Quad,
};
pub use stamp::{stamp_list, StampScheme, StampSpec};
pub use style::{BlendMode, Dash, Fill, FillRule, LineCap, LineJoin, Rgb, ShapeStyle, Stroke};
pub use text::{
    char_advance, measure_text, text_outline, winansi_byte, winansi_char, winansi_encode,
    FontStyle, TextMetrics,
};

/// What can go wrong while building, drawing or emitting a list.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum DrawError {
    /// Text can only use the characters of `WinAnsiEncoding` until embedded fonts arrive.
    #[error(
        "the character {ch:?} (position {index}) cannot be written: only Western European \
         (WinAnsi) characters are supported"
    )]
    NotWinAnsi { ch: char, index: usize },
    /// A coordinate, size, width or matrix entry is NaN or infinite, or a width is negative.
    #[error("a coordinate, size or width is not a usable number")]
    InvalidNumber,
    #[error("image data of {actual} bytes does not fit {width} x {height} RGBA pixels")]
    ImageSize {
        width: u32,
        height: u32,
        actual: usize,
    },
    #[error("a {width} x {height} px canvas cannot be allocated")]
    CanvasSize { width: u32, height: u32 },
}
