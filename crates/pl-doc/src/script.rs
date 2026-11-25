//! The JSON command file: a list of steps run against a [`Session`] with no user interface.
//!
//! This is the headless test driver (`plumb run commands.json`), not a user-facing language.
//! A file looks like this:
//!
//! ```json
//! {
//!   "author": "Tester",
//!   "date": "D:20261124101500Z",
//!   "steps": [
//!     { "op": "open", "path": "drawing.pdf" },
//!     { "op": "add", "id": "box", "kind": "Square", "page": 1,
//!       "rect": [100, 100, 220, 180],
//!       "style": { "stroke": [1, 0, 0], "fill": null, "width": 2, "dash": [6, 3],
//!                  "opacity": 0.8, "cloud": 1.0 },
//!       "contents": "Check this" },
//!     { "op": "add", "kind": "Line", "page": 1, "points": [[50, 50], [200, 90]],
//!       "style": { "line_end": "ClosedArrow" } },
//!     { "op": "add", "kind": "Ink", "page": 2, "strokes": [[[10, 10], [20, 30], [40, 35]]] },
//!     { "op": "add", "kind": "Highlight", "page": 1, "quads": [[72, 596, 240, 612]] },
//!     { "op": "modify", "id": "box", "move": [10, -5], "style": { "width": 4 } },
//!     { "op": "delete", "id": "box" },
//!     { "op": "undo" },
//!     { "op": "redo" },
//!     { "op": "undo" },
//!     { "op": "expect", "count": 4 },
//!     { "op": "render", "page": 1, "scale": 1.0, "output": "page1.png" },
//!     { "op": "save_as", "path": "marked.pdf" }
//!   ]
//! }
//! ```
//!
//! * Pages are numbered from 1, as on the command line. Coordinates are page space: PDF
//!   points in the unrotated page, y up.
//! * `kind` is the PDF subtype: `Square`, `Circle`, `Line`, `PolyLine`, `Polygon`, `Ink`,
//!   `Highlight`, `Underline`, `StrikeOut`, `Squiggly`, `Text` (a note) or `Link`.
//!   Geometry is `rect` (`[x0, y0, x1, y1]`), `points`, `strokes` or `quads` (one rectangle
//!   per run of text) as the kind needs.
//! * In `style`, a key that is left out keeps the kind's default (or, in `modify`, the present
//!   value); `null` for `stroke`, `fill`, `dash` or `cloud` switches that off.
//! * Other steps: `reorder` (`page`, `order` of ids bottom to top), `copy` / `cut` (`ids`),
//!   `paste` (`page`, optional `offset`), `save`, `save_optimised`, `discard`, and `expect`
//!   (`count`, optional `page`, optional `modified`), which fails the run when the document
//!   does not have that many annotations.
//! * Structural commands: `{ "op": "structure", "command": { "DeletePages": { "pages": [0] } } }`
//!   takes a [`crate::StructCommand`] as the crash journal writes it (pages from 0 there), with
//!   an optional `"invalidate_signatures": true`; `expect_pages` (`count`) checks the page
//!   count.
//! * Relative paths are relative to the command file's directory.
//! * A run stops at the first step that fails; the error names the step.
//!
//! # Text boxes, stamps, pictures, text markups and the markups list
//!
//! ```json
//! { "op": "add", "id": "note", "kind": "FreeText", "page": 1, "rect": [72, 600, 260, 660],
//!   "text": "Check the falls\nto the gully.",
//!   "text_style": { "font": "Helvetica-Bold", "size": 10, "color": [0, 0, 0.6],
//!                   "align": "centre", "v_align": "middle", "line_spacing": 1.2, "padding": 4 },
//!   "style": { "stroke": [0, 0, 0.6], "fill": [1, 1, 0.85], "width": 1 } },
//! { "op": "add", "kind": "FreeText", "intent": "callout", "page": 1,
//!   "rect": [300, 500, 420, 540], "tip": [250, 610], "text": "Kerb here" },
//! { "op": "add", "kind": "FreeText", "intent": "typewriter", "page": 1,
//!   "rect": [72, 80, 300, 96], "text": "Typed straight onto the sheet" },
//! { "op": "add", "id": "ok", "kind": "Stamp", "page": 1, "rect": [400, 700, 560, 760],
//!   "stamp": "approved", "detail": "Ada, 24 Nov 2026" },
//! { "op": "add", "kind": "Stamp", "page": 1, "rect": [72, 300, 0, 0], "image": "photo.jpg" },
//! { "op": "markup_text", "id": "hl", "kind": "Highlight", "page": 1, "find": "fire rated" },
//! { "op": "markup_text", "kind": "Underline", "page": 1, "from": [72, 596], "to": [240, 580] },
//! { "op": "set_text", "id": "note", "text": "Falls checked." },
//! { "op": "status", "id": "note", "state": "Accepted", "author": "Bo" },
//! { "op": "reply", "id": "note", "reply_id": "r1", "text": "Agreed.", "author": "Cy" },
//! { "op": "delete_reply", "id": "r1" },
//! { "op": "check", "id": "note", "checked": true },
//! { "op": "lock", "id": "note", "locked": true },
//! { "op": "define_column", "title": "Trade", "kind": "Text" },
//! { "op": "column", "id": "note", "column": "trade", "value": "Drainage" },
//! { "op": "popup", "id": "note", "open": true },
//! { "op": "expect_annot", "id": "note", "kind": "FreeText", "contents": "Falls checked.",
//!   "status": "Accepted", "checked": true, "locked": true, "replies": 0,
//!   "columns": { "trade": "Drainage" } }
//! ```
//!
//! * A `FreeText` needs `rect` and `text`; `intent` is `box` (the default), `callout` (with
//!   `tip`, the point the leader ends at) or `typewriter`. A `Stamp` needs `rect` and either
//!   `stamp` (`approved`, `revised`, `for_review`, `void` or `draft`, or an object with
//!   `name`, `label`, optional `detail` and `scheme`) or `image` (a PNG or JPEG file). For a
//!   picture, a `rect` with no width or height gives its lower left corner and the picture
//!   keeps its natural size (96 pixels per inch), shrunk to fit the page.
//! * `rotation` (0, 90, 180 or 270) turns a text box or stamp anticlockwise in page space.
//!   Left out, it is the page's own `/Rotate`, so the text reads upright as the page is
//!   displayed.
//! * `markup_text` marks text of the page: the `occurrence`-th (default first) match of
//!   `find`, or what a drag from `from` to `to` covers. `kind` is `Highlight`, `Underline`,
//!   `StrikeOut` or `Squiggly`. The marked text becomes the markup's comment.
//! * `define_column` adds a custom column (`kind`: `Text`, `Number`, `Choice` with `options`,
//!   `Checkmark` or `Date`) to the run's column list and logs its id; `column` sets a
//!   markup's value in it, written as a user would type it (an empty `value` clears it).
//! * `expect_annot` fails the run unless the annotation is as described; every key but `id`
//!   is optional.
//!
//! # Scales and measurements
//!
//! `set_scale`, `default_scale`, `calibrate`, `add_viewport`, `edit_viewport`,
//! `remove_viewport`, `measure`, `move_measure`, `cutout`, `remove_cutout`, `depth`, `count`,
//! `expect_measure` and `expect_scale` are described in [`crate::script_measure`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pl_annot::{
    AnnotId, AnnotKind, Annotation, BlendMode, FreeText, Geometry, HAlign, LineEnding, LinkTarget,
    Origin, PdfDate, ReviewState, StampScheme, StampSpec, Style, VAlign,
};
use pl_geom::{Point, Rect, Vec2};
use pl_render::Bitmap;
use serde::{Deserialize, Deserializer, Serialize};

use crate::annot_data::rgb;
use crate::clipboard::{Clipboard, PastePlacement};
use crate::command::Command;
use crate::error::{DocError, Result};
use crate::markup_ops;
use crate::script_measure as sm;
pub use crate::script_measure::{
    CalibrateSpec, DefaultScaleSpec, ExpectMeasureSpec, ExpectScaleSpec, MeasureStepSpec,
    SetScaleSpec, ViewportSpec,
};
use crate::session::{Document, DocumentId, Session};

/// Present-but-null is `Some(None)`; absent is `None`.
pub(crate) fn nullable<'de, T, D>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Style keys of a step; every one is optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleSpec {
    /// `[r, g, b]`, each 0 to 1; `null` for no stroke.
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stroke: Option<Option<[f64; 3]>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub fill: Option<Option<[f64; 3]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub dash: Option<Option<Vec<f64>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// A line ending's PDF name: `None`, `OpenArrow`, `ClosedArrow`, `Butt`, `Circle`,
    /// `Square`, `Diamond` or `Slash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<String>,
    /// Cloud intensity; `null` for a plain border.
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub cloud: Option<Option<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiply: Option<bool>,
}

impl StyleSpec {
    fn apply(&self, style: &mut Style) -> std::result::Result<(), String> {
        let ending = |name: &str| {
            LineEnding::from_pdf_name(name).ok_or_else(|| format!("unknown line ending {name:?}"))
        };
        if let Some(stroke) = self.stroke {
            style.stroke = stroke.map(rgb);
        }
        if let Some(fill) = self.fill {
            style.fill = fill.map(rgb);
        }
        if let Some(width) = self.width {
            style.width = width;
        }
        if let Some(dash) = &self.dash {
            style.dash = dash.clone();
        }
        if let Some(opacity) = self.opacity {
            style.opacity = opacity;
        }
        if let Some(name) = &self.line_start {
            style.line_start = ending(name)?;
        }
        if let Some(name) = &self.line_end {
            style.line_end = ending(name)?;
        }
        if let Some(cloud) = self.cloud {
            style.cloud = cloud;
        }
        if let Some(multiply) = self.multiply {
            style.blend = if multiply {
                BlendMode::Multiply
            } else {
                BlendMode::Normal
            };
        }
        Ok(())
    }
}

/// The geometry keys of a step; which one is needed depends on the kind.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GeometrySpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strokes: Option<Vec<Vec<[f64; 2]>>>,
    /// One `[x0, y0, x1, y1]` rectangle per run of marked text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quads: Option<Vec<[f64; 4]>>,
}

fn rect_of(r: [f64; 4]) -> Rect {
    Rect::new(r[0], r[1], r[2], r[3]).abs()
}

fn point_of(p: &[f64; 2]) -> Point {
    Point::new(p[0], p[1])
}

impl GeometrySpec {
    fn is_empty(&self) -> bool {
        self.rect.is_none()
            && self.points.is_none()
            && self.strokes.is_none()
            && self.quads.is_none()
    }

    fn build(&self, kind: &AnnotKind) -> std::result::Result<Geometry, String> {
        let name = kind.pdf_name();
        match kind {
            AnnotKind::Square | AnnotKind::Circle | AnnotKind::Text | AnnotKind::Link => self
                .rect
                .map(|r| Geometry::Rect(rect_of(r)))
                .ok_or_else(|| format!("{name} needs \"rect\"")),
            AnnotKind::Line => match self.points.as_deref() {
                Some([a, b]) => Ok(Geometry::Line {
                    a: point_of(a),
                    b: point_of(b),
                }),
                _ => Err("Line needs \"points\" with exactly two points".into()),
            },
            AnnotKind::PolyLine | AnnotKind::Polygon => {
                let least = if *kind == AnnotKind::Polygon { 3 } else { 2 };
                match &self.points {
                    Some(points) if points.len() >= least => {
                        Ok(Geometry::Points(points.iter().map(point_of).collect()))
                    }
                    _ => Err(format!(
                        "{name} needs \"points\" with at least {least} points"
                    )),
                }
            }
            AnnotKind::Ink => match &self.strokes {
                Some(strokes) if strokes.iter().any(|s| !s.is_empty()) => Ok(Geometry::Ink(
                    strokes
                        .iter()
                        .map(|s| s.iter().map(point_of).collect())
                        .collect(),
                )),
                _ => Err("Ink needs \"strokes\" with at least one point".into()),
            },
            AnnotKind::Highlight
            | AnnotKind::Underline
            | AnnotKind::StrikeOut
            | AnnotKind::Squiggly => match &self.quads {
                Some(quads) if !quads.is_empty() => Ok(Geometry::Quads(
                    quads
                        .iter()
                        .map(|q| pl_draw::shapes::rect_quad(rect_of(*q)))
                        .collect(),
                )),
                _ => Err(format!("{name} needs \"quads\"")),
            },
            _ => Err(format!(
                "a {name} annotation cannot be created by a command file"
            )),
        }
    }
}

/// The text keys of a text box; every one is optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStyleSpec {
    /// `Helvetica`, `Helvetica-Bold`, `Helvetica-Oblique` or `Helvetica-BoldOblique`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// Font size in points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// Text colour, `[r, g, b]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f64; 3]>,
    /// `left`, `centre` (or `center`) or `right`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    /// `top`, `middle` or `bottom`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v_align: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_spacing: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<f64>,
}

impl TextStyleSpec {
    fn is_empty(&self) -> bool {
        *self == TextStyleSpec::default()
    }

    fn apply(&self, text: &mut FreeText) -> std::result::Result<(), String> {
        if let Some(name) = &self.font {
            text.font = crate::annot_data::font_from(name)
                .ok_or_else(|| format!("unknown font {name:?}"))?;
        }
        if let Some(size) = self.size {
            text.size = size;
        }
        if let Some(color) = self.color {
            text.color = rgb(color);
        }
        if let Some(align) = &self.align {
            text.align = match align.to_ascii_lowercase().as_str() {
                "left" => HAlign::Left,
                "centre" | "center" => HAlign::Centre,
                "right" => HAlign::Right,
                _ => return Err(format!("unknown alignment {align:?}")),
            };
        }
        if let Some(v_align) = &self.v_align {
            text.v_align = match v_align.to_ascii_lowercase().as_str() {
                "top" => VAlign::Top,
                "middle" => VAlign::Middle,
                "bottom" => VAlign::Bottom,
                _ => return Err(format!("unknown vertical alignment {v_align:?}")),
            };
        }
        if let Some(spacing) = self.line_spacing {
            text.line_spacing = spacing;
        }
        if let Some(padding) = self.padding {
            text.padding = padding;
        }
        Ok(())
    }
}

/// Which stamp an `add` step places: one of the standard set by name, or a described one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StampChoice {
    /// `approved`, `revised`, `for_review`, `void` or `draft`.
    Standard(String),
    Custom {
        /// The `/Name` the stamp is stored under.
        name: String,
        /// The main line.
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// `Green`, `Red`, `Blue`, `Amber` or `Slate`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheme: Option<String>,
    },
}

impl StampChoice {
    fn spec(&self) -> std::result::Result<StampSpec, String> {
        match self {
            StampChoice::Standard(name) => {
                let key = name.to_ascii_lowercase().replace([' ', '-'], "_");
                Ok(match key.as_str() {
                    "approved" => StampSpec::approved(),
                    "revised" => StampSpec::revised(),
                    "for_review" => StampSpec::for_review(),
                    "void" => StampSpec::void(),
                    "draft" => StampSpec::draft(),
                    _ => {
                        return Err(format!(
                            "unknown stamp {name:?}; the standard ones are approved, revised, \
                             for_review, void and draft"
                        ))
                    }
                })
            }
            StampChoice::Custom {
                name,
                label,
                detail,
                scheme,
            } => {
                let scheme = match scheme {
                    Some(s) => StampScheme::from_name(s)
                        .ok_or_else(|| format!("unknown stamp scheme {s:?}"))?,
                    None => StampScheme::default(),
                };
                let mut spec = StampSpec::new(name, label, scheme);
                spec.detail = detail.clone();
                Ok(spec)
            }
        }
    }
}

/// An `add` step: one new markup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddSpec {
    /// The id (`/NM`) to give it; generated when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: String,
    /// Page number, starting at 1.
    pub page: u32,
    #[serde(flatten)]
    pub geometry: GeometrySpec,
    #[serde(default)]
    pub style: StyleSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// For a `Link`: the address it opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// For a `FreeText`: its words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// For a `FreeText`: `box` (the default), `callout` or `typewriter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// For a callout: the point its leader ends at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "TextStyleSpec::is_empty")]
    pub text_style: TextStyleSpec,
    /// For a `FreeText` or `Stamp`: `/Rotate`. Left out, the page's own rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<i32>,
    /// For a `Stamp`: which one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<StampChoice>,
    /// For a standard `Stamp`: its second line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// For a `Stamp`: a PNG or JPEG file to place instead of a label stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<PathBuf>,
}

/// A `modify` step: changes to one existing annotation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModifySpec {
    pub id: String,
    /// Move by `[dx, dy]` page units (applied before a new geometry, if both are given).
    #[serde(default, rename = "move", skip_serializing_if = "Option::is_none")]
    pub move_by: Option<[f64; 2]>,
    #[serde(flatten)]
    pub geometry: GeometrySpec,
    #[serde(default)]
    pub style: StyleSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// For a text box: new words (as `set_text`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "TextStyleSpec::is_empty")]
    pub text_style: TextStyleSpec,
    /// For a text box or stamp: `/Rotate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<i32>,
    /// For a callout: a new point for its leader to end at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<[f64; 2]>,
}

/// A `markup_text` step: a text markup over text of the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkupTextSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `Highlight`, `Underline`, `StrikeOut` or `Squiggly`.
    pub kind: String,
    /// Page number, starting at 1.
    pub page: u32,
    /// Text to look for on the page (case is ignored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<String>,
    /// Which match of `find`, counting from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<usize>,
    /// The start of a drag across the text, in page space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<[f64; 2]>,
    /// The end of the drag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<[f64; 2]>,
    #[serde(default)]
    pub style: StyleSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// An `expect_annot` step: what one annotation must look like. Keys left out are not checked.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectAnnotSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Page number, starting at 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The review status: `None`, `Accepted`, `Rejected`, `Cancelled` or `Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    /// How many replies were made directly to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replies: Option<usize>,
    /// Whether it has a popup window, and whether that is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    /// `/Rotate` of a text box or stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u16>,
    /// How many quads a text markup has (one per line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quads: Option<usize>,
    /// Custom column values by column id, as the list displays them; an empty string means
    /// the annotation holds no value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub columns: BTreeMap<String, String>,
}

fn one() -> f64 {
    1.0
}

fn yes() -> bool {
    true
}

/// One step of a command file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Step {
    /// Opens a file; the steps that follow act on it.
    Open {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    Add(Box<AddSpec>),
    Modify(Box<ModifySpec>),
    Delete {
        id: String,
    },
    Reorder {
        page: u32,
        order: Vec<String>,
    },
    Undo,
    Redo,
    Copy {
        ids: Vec<String>,
    },
    Cut {
        ids: Vec<String>,
    },
    Paste {
        page: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<[f64; 2]>,
    },
    Save,
    SaveAs {
        path: PathBuf,
    },
    SaveOptimised,
    /// Drops every unsaved change.
    Discard,
    /// Renders a page as the user sees it (unsaved markups included) to a PNG file.
    Render {
        page: u32,
        /// Pixels per point.
        #[serde(default = "one")]
        scale: f64,
        output: PathBuf,
    },
    /// Fails the run unless the document has `count` annotations (on `page`, if given).
    Expect {
        count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modified: Option<bool>,
    },
    /// Marks text of a page with a Highlight, Underline, StrikeOut or Squiggly.
    MarkupText(MarkupTextSpec),
    /// Sets an annotation's text (`/Contents`; for a text box, its words).
    SetText {
        id: String,
        text: String,
    },
    /// Records a review status for a markup.
    Status {
        id: String,
        /// `None`, `Accepted`, `Rejected`, `Cancelled` or `Completed`.
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<String>,
    },
    /// Replies to a markup or to another reply.
    Reply {
        /// What is replied to.
        id: String,
        text: String,
        /// The id to give the reply; generated when left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<String>,
    },
    /// Deletes a reply or a status annotation.
    DeleteReply {
        id: String,
    },
    /// Ticks a markup off, or clears the tick.
    Check {
        id: String,
        #[serde(default = "yes")]
        checked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<String>,
    },
    /// Locks or unlocks a markup.
    Lock {
        id: String,
        #[serde(default = "yes")]
        locked: bool,
    },
    /// Adds a custom column to the run's column list.
    DefineColumn {
        title: String,
        kind: pl_takeoff::ColumnType,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
    },
    /// Sets a markup's value in a custom column (by column id), as a user would type it.
    Column {
        id: String,
        column: String,
        value: String,
    },
    /// Gives a markup a popup window if it has none; `open` opens or closes it, `rect` places
    /// it.
    Popup {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rect: Option<[f64; 4]>,
    },
    /// Fails the run unless one annotation is as described.
    ExpectAnnot(ExpectAnnotSpec),
    /// Sets the scale of a page, a range of pages or every page (see
    /// [`crate::script_measure`] for these steps).
    SetScale(SetScaleSpec),
    /// Sets the scale of every page that has none of its own.
    DefaultScale(DefaultScaleSpec),
    /// Makes a page's (or a viewport's) scale the one at which two points are a given length
    /// apart.
    Calibrate(CalibrateSpec),
    AddViewport(ViewportSpec),
    EditViewport(ViewportSpec),
    RemoveViewport {
        page: u32,
        name: String,
    },
    /// Takes a measurement on the scale that applies where it is.
    Measure(MeasureStepSpec),
    /// New points for a measurement.
    MoveMeasure {
        id: String,
        points: Vec<[f64; 2]>,
    },
    /// Cuts a ring out of an area or volume.
    Cutout {
        id: String,
        points: Vec<[f64; 2]>,
    },
    RemoveCutout {
        id: String,
        index: usize,
    },
    /// Sets the depth of a volume (an area becomes one).
    Depth {
        id: String,
        depth: String,
    },
    /// Adds a marker to a count (`add`) or takes one off (`remove`, its index from 0).
    Count {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        add: Option<[f64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove: Option<usize>,
    },
    /// Fails the run unless a measurement has the value, caption and scale described.
    ExpectMeasure(ExpectMeasureSpec),
    /// Fails the run unless a page has the scale and viewports described.
    ExpectScale(ExpectScaleSpec),
    /// A structural command (page operation, bookmark, link, flatten) in the crash journal's
    /// form, where pages are numbered from 0 (see [`crate::structure`]).
    Structure {
        command: Box<crate::structure::StructCommand>,
        /// The user has agreed that digital signatures may be invalidated.
        #[serde(default)]
        invalidate_signatures: bool,
    },
    /// Fails the run unless the document has this many pages.
    ExpectPages {
        count: u32,
    },
}

impl Step {
    fn name(&self) -> &'static str {
        match self {
            Step::Open { .. } => "open",
            Step::Add(_) => "add",
            Step::Modify(_) => "modify",
            Step::Delete { .. } => "delete",
            Step::Reorder { .. } => "reorder",
            Step::Undo => "undo",
            Step::Redo => "redo",
            Step::Copy { .. } => "copy",
            Step::Cut { .. } => "cut",
            Step::Paste { .. } => "paste",
            Step::Save => "save",
            Step::SaveAs { .. } => "save_as",
            Step::SaveOptimised => "save_optimised",
            Step::Discard => "discard",
            Step::Render { .. } => "render",
            Step::Expect { .. } => "expect",
            Step::MarkupText(_) => "markup_text",
            Step::SetText { .. } => "set_text",
            Step::Status { .. } => "status",
            Step::Reply { .. } => "reply",
            Step::DeleteReply { .. } => "delete_reply",
            Step::Check { .. } => "check",
            Step::Lock { .. } => "lock",
            Step::DefineColumn { .. } => "define_column",
            Step::Column { .. } => "column",
            Step::Popup { .. } => "popup",
            Step::ExpectAnnot(_) => "expect_annot",
            Step::SetScale(_) => "set_scale",
            Step::DefaultScale(_) => "default_scale",
            Step::Calibrate(_) => "calibrate",
            Step::AddViewport(_) => "add_viewport",
            Step::EditViewport(_) => "edit_viewport",
            Step::RemoveViewport { .. } => "remove_viewport",
            Step::Measure(_) => "measure",
            Step::MoveMeasure { .. } => "move_measure",
            Step::Cutout { .. } => "cutout",
            Step::RemoveCutout { .. } => "remove_cutout",
            Step::Depth { .. } => "depth",
            Step::Count { .. } => "count",
            Step::ExpectMeasure(_) => "expect_measure",
            Step::ExpectScale(_) => "expect_scale",
            Step::Structure { .. } => "structure",
            Step::ExpectPages { .. } => "expect_pages",
        }
    }
}

/// A command file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Script {
    /// The author new markups get, unless a step names its own. Default: `Plumbline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The creation date new markups get, as a PDF date (`D:20261124101500Z`). Default: now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub steps: Vec<Step>,
}

impl Script {
    pub fn from_json(json: &str) -> Result<Script> {
        serde_json::from_str(json).map_err(|e| DocError::Script(e.to_string()))
    }

    /// Reads a command file. Relative paths inside it are meant relative to its directory.
    pub fn read(path: &Path) -> Result<Script> {
        let json = std::fs::read_to_string(path).map_err(|e| DocError::io("read", path, e))?;
        Script::from_json(&json)
    }
}

/// What a run needs from its caller: somewhere to put pictures and progress lines.
pub trait ScriptHost {
    /// Writes a rendered page as a PNG file.
    fn write_png(&mut self, path: &Path, bitmap: &Bitmap) -> std::io::Result<()>;

    /// One line of progress per step.
    fn log(&mut self, line: &str);
}

/// A host that keeps the progress lines and refuses to write pictures.
#[derive(Debug, Default)]
pub struct LogOnly {
    pub lines: Vec<String>,
}

impl ScriptHost for LogOnly {
    fn write_png(&mut self, _path: &Path, _bitmap: &Bitmap) -> std::io::Result<()> {
        Err(std::io::Error::other("this run cannot write pictures"))
    }

    fn log(&mut self, line: &str) {
        self.lines.push(line.to_string());
    }
}

/// What a run did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ScriptReport {
    /// How many steps ran.
    pub steps: usize,
    /// The document the last `open` step opened, still open in the session.
    pub document: Option<DocumentId>,
}

struct Runner<'a> {
    session: &'a mut Session,
    base: &'a Path,
    host: &'a mut dyn ScriptHost,
    origin: Origin,
    current: Option<DocumentId>,
    clipboard: Clipboard,
    /// The custom columns `define_column` steps have made.
    schema: pl_takeoff::Schema,
}

type StepResult = std::result::Result<String, String>;

fn text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

impl Runner<'_> {
    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base.join(path)
        }
    }

    fn doc(&mut self) -> std::result::Result<&mut Document, String> {
        let id = self
            .current
            .ok_or("no document is open; start with an \"open\" step")?;
        self.session.document_mut(id).map_err(text)
    }

    fn page(&mut self, page: u32) -> std::result::Result<u32, String> {
        let count = self.doc()?.page_count();
        if page == 0 || page > count {
            return Err(format!(
                "page {page} is out of range: the document has {count} page{}",
                if count == 1 { "" } else { "s" }
            ));
        }
        Ok(page - 1)
    }

    fn origin_for(&self, author: Option<&str>) -> Origin {
        match author {
            Some(author) => Origin::new(author, self.origin.when),
            None => self.origin.clone(),
        }
    }

    fn step(&mut self, step: &Step) -> StepResult {
        match step {
            Step::Open { path, password } => {
                let path = self.resolve(path);
                let id = self
                    .session
                    .open(&path, password.as_deref())
                    .map_err(text)?;
                self.current = Some(id);
                let doc = self.session.document_mut(id).map_err(text)?;
                let recovered = {
                    doc.load_annots().map_err(text)?;
                    doc.recovered_changes()
                };
                let count = doc.editor().map_or(0, |e| e.set().len());
                Ok(format!(
                    "{} page{}, {count} annotation{}{}",
                    doc.page_count(),
                    if doc.page_count() == 1 { "" } else { "s" },
                    if count == 1 { "" } else { "s" },
                    if recovered > 0 {
                        format!(", {recovered} unsaved change(s) recovered")
                    } else {
                        String::new()
                    }
                ))
            }
            Step::Add(spec) => {
                let page = self.page(spec.page)?;
                let kind = AnnotKind::from_pdf_name(&spec.kind);
                let origin = self.origin_for(spec.author.as_deref());
                let mut a = match kind {
                    AnnotKind::FreeText => self.new_text_box(spec, page, &origin)?,
                    AnnotKind::Stamp => self.new_stamp(spec, page, &origin)?,
                    _ => {
                        let geometry = spec.geometry.build(&kind)?;
                        Annotation::new(kind, page as usize, geometry, &origin)
                    }
                };
                if let Some(id) = &spec.id {
                    a.id = AnnotId(id.clone());
                }
                spec.style.apply(&mut a.style)?;
                if spec.contents.is_some() {
                    a.meta.contents = spec.contents.clone();
                }
                if spec.subject.is_some() {
                    a.meta.subject = spec.subject.clone();
                }
                if a.kind == AnnotKind::Link {
                    a.link = Some(match &spec.uri {
                        Some(uri) => LinkTarget::Uri(uri.clone()),
                        None => return Err("Link needs \"uri\"".into()),
                    });
                }
                let line = format!("{} {} on page {}", a.kind.pdf_name(), a.id, spec.page);
                self.doc()?.apply(Command::add(a)).map_err(text)?;
                Ok(line)
            }
            Step::Modify(spec) => {
                let id = AnnotId(spec.id.clone());
                let when = self.origin.when;
                let doc = self.doc()?;
                let mut a = doc
                    .annots()
                    .map_err(text)?
                    .set()
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| format!("there is no annotation {id}"))?;
                let wants_text =
                    spec.text.is_some() || !spec.text_style.is_empty() || spec.tip.is_some();
                if wants_text && a.kind == AnnotKind::FreeText {
                    // Another application's text box is taken over before it is changed.
                    a.make_editable();
                }
                if let Some([dx, dy]) = spec.move_by {
                    a.translate(Vec2::new(dx, dy));
                }
                if !spec.geometry.is_empty() {
                    a.geometry = if a.free_text.is_some() || a.stamp.is_some() {
                        spec.geometry.build(&AnnotKind::Square)?
                    } else {
                        spec.geometry.build(&a.kind)?
                    };
                    a.reroute_callout();
                }
                spec.style.apply(&mut a.style)?;
                if spec.contents.is_some() {
                    a.meta.contents = spec.contents.clone();
                }
                if spec.text.is_some() {
                    a.meta.contents = spec.text.clone();
                }
                if spec.subject.is_some() {
                    a.meta.subject = spec.subject.clone();
                }
                if spec.author.is_some() {
                    a.meta.author = spec.author.clone();
                }
                if !spec.text_style.is_empty() || spec.tip.is_some() {
                    let free = a
                        .free_text
                        .as_mut()
                        .ok_or("\"text_style\" and \"tip\" are for a text box")?;
                    spec.text_style.apply(free)?;
                    if let (Some(tip), Geometry::Rect(rect)) = (spec.tip, &a.geometry) {
                        free.callout = pl_annot::callout_route(*rect, point_of(&tip));
                    }
                }
                if let Some(rotation) = spec.rotation {
                    a.set_rotation(rotation);
                }
                a.touch(when);
                let out = doc.apply(Command::modify(a)).map_err(text)?;
                Ok(format!(
                    "{id}{}",
                    if out.changed { "" } else { " (no change)" }
                ))
            }
            Step::Delete { id } => {
                let out = self
                    .doc()?
                    .apply(Command::delete(AnnotId(id.clone())))
                    .map_err(text)?;
                Ok(format!("{id} ({} removed)", out.touched.len()))
            }
            Step::Reorder { page, order } => {
                let index = self.page(*page)?;
                let order = order.iter().cloned().map(AnnotId).collect();
                let out = self
                    .doc()?
                    .apply(Command::Reorder {
                        page: index as usize,
                        order,
                    })
                    .map_err(text)?;
                Ok(format!(
                    "page {page}{}",
                    if out.changed { "" } else { " (no change)" }
                ))
            }
            Step::Undo => match self.doc()?.undo().map_err(text)? {
                true => Ok(String::new()),
                false => Err("there is nothing to undo".into()),
            },
            Step::Redo => match self.doc()?.redo().map_err(text)? {
                true => Ok(String::new()),
                false => Err("there is nothing to redo".into()),
            },
            Step::Copy { ids } | Step::Cut { ids } => {
                let ids: Vec<AnnotId> = ids.iter().cloned().map(AnnotId).collect();
                let doc = self.doc()?;
                let clip = if matches!(step, Step::Cut { .. }) {
                    doc.cut(&ids)
                } else {
                    doc.copy(&ids)
                }
                .map_err(text)?;
                let line = format!("{} annotation(s), {} skipped", clip.len(), clip.skipped);
                self.clipboard = clip;
                Ok(line)
            }
            Step::Paste { page, offset } => {
                let index = self.page(*page)?;
                let placement = match offset {
                    Some([dx, dy]) => PastePlacement::Offset(Vec2::new(*dx, *dy)),
                    None => PastePlacement::InPlace,
                };
                let clip = self.clipboard.clone();
                let origin = self.origin.clone();
                let ids = self
                    .doc()?
                    .paste(&clip, index, placement, &origin)
                    .map_err(text)?;
                Ok(format!("{} annotation(s) on page {page}", ids.len()))
            }
            Step::Save => self.doc()?.save().map(|s| describe_save(&s)).map_err(text),
            Step::SaveOptimised => self
                .doc()?
                .save_optimised()
                .map(|s| describe_save(&s))
                .map_err(text),
            Step::SaveAs { path } => {
                let path = self.resolve(path);
                self.doc()?
                    .save_as(&path)
                    .map(|s| format!("{}: {}", path.display(), describe_save(&s)))
                    .map_err(text)
            }
            Step::Discard => {
                self.doc()?.discard_changes();
                Ok(String::new())
            }
            Step::Render {
                page,
                scale,
                output,
            } => {
                if !scale.is_finite() || *scale <= 0.0 {
                    return Err(format!("\"scale\" must be a positive number, not {scale}"));
                }
                let index = self.page(*page)?;
                let output = self.resolve(output);
                let doc = self.doc()?;
                let unit = doc.load_geometry(index).map_err(text)?.user_unit();
                let bitmap = doc
                    .render_page_composite(index, scale * unit)
                    .map_err(text)?;
                self.host
                    .write_png(&output, &bitmap)
                    .map_err(|e| format!("write {}: {e}", output.display()))?;
                Ok(format!(
                    "page {page} to {}, {} x {} px",
                    output.display(),
                    bitmap.width,
                    bitmap.height
                ))
            }
            Step::Expect {
                count,
                page,
                modified,
            } => {
                let index = match page {
                    Some(page) => Some(self.page(*page)?),
                    None => None,
                };
                let doc = self.doc()?;
                let set = doc.annots().map_err(text)?.set();
                let found = match index {
                    Some(index) => set.on_page(index as usize).count(),
                    None => set.len(),
                };
                let place = page.map_or(String::new(), |p| format!(" on page {p}"));
                if found != *count {
                    return Err(format!(
                        "expected {count} annotation(s){place}, found {found}"
                    ));
                }
                if let Some(want) = modified {
                    if doc.is_modified() != *want {
                        return Err(format!(
                            "expected the document to be {}",
                            if *want { "modified" } else { "unmodified" }
                        ));
                    }
                }
                Ok(format!("{found} annotation(s){place}"))
            }
            Step::MarkupText(spec) => {
                let index = self.page(spec.page)?;
                let kind = AnnotKind::from_pdf_name(&spec.kind);
                if !kind.is_text_markup() {
                    return Err(format!(
                        "\"kind\" must be Highlight, Underline, StrikeOut or Squiggly, not {}",
                        spec.kind
                    ));
                }
                let origin = self.origin_for(spec.author.as_deref());
                let doc = self.doc()?;
                let page_text = doc.page_text(index).map_err(text)?;
                let selection = match (&spec.find, spec.from, spec.to) {
                    (Some(query), None, None) => {
                        let hits =
                            pl_render::search_page_text(&page_text, query, Default::default());
                        let n = spec.occurrence.unwrap_or(1);
                        let hit = n.checked_sub(1).and_then(|i| hits.get(i)).ok_or_else(|| {
                            format!(
                                "page {} has {} match(es) of {query:?}, not {n}",
                                spec.page,
                                hits.len()
                            )
                        })?;
                        crate::selection::markup_hit(&page_text, hit)
                    }
                    (None, Some(from), Some(to)) => {
                        crate::selection::markup_between(&page_text, point_of(&from), point_of(&to))
                    }
                    _ => {
                        return Err(
                            "markup_text needs either \"find\" or both \"from\" and \"to\"".into(),
                        )
                    }
                };
                let selection =
                    selection.ok_or_else(|| format!("page {} has no text there", spec.page))?;
                let mut a = selection
                    .annotation(kind, &origin)
                    .ok_or("the selected text has no area")?;
                if let Some(id) = &spec.id {
                    a.id = AnnotId(id.clone());
                }
                spec.style.apply(&mut a.style)?;
                let line = format!(
                    "{} {} on page {}: {} line(s), {} character(s)",
                    a.kind.pdf_name(),
                    a.id,
                    spec.page,
                    selection.quads.len(),
                    selection.text.chars().count()
                );
                doc.apply(Command::add(a)).map_err(text)?;
                Ok(line)
            }
            Step::SetText { id, text: new } => {
                let when = self.origin.when;
                let out = self
                    .doc()?
                    .set_text(&AnnotId(id.clone()), new, when)
                    .map_err(text)?;
                Ok(format!(
                    "{id}{}",
                    if out.changed { "" } else { " (no change)" }
                ))
            }
            Step::Status { id, state, author } => {
                let review = ReviewState::from_name(state)
                    .ok_or_else(|| format!("unknown status {state:?}"))?;
                let origin = self.origin_for(author.as_deref());
                let new = self
                    .doc()?
                    .set_status(&AnnotId(id.clone()), review, &origin)
                    .map_err(text)?;
                Ok(format!("{id} is {state} ({new})"))
            }
            Step::Reply {
                id,
                text: words,
                reply_id,
                author,
            } => {
                let origin = self.origin_for(author.as_deref());
                let doc = self.doc()?;
                let (mut command, mut new) = markup_ops::add_reply(
                    doc.annots().map_err(text)?.set(),
                    &AnnotId(id.clone()),
                    words,
                    &origin,
                )
                .map_err(text)?;
                if let (Some(wanted), Command::AddAnnotations(items)) = (reply_id, &mut command) {
                    new = AnnotId(wanted.clone());
                    items[0].id = new.clone();
                }
                doc.apply(command).map_err(text)?;
                Ok(format!("{new} to {id}"))
            }
            Step::DeleteReply { id } => {
                let out = self
                    .doc()?
                    .delete_reply(&AnnotId(id.clone()))
                    .map_err(text)?;
                Ok(format!("{id} ({} removed)", out.touched.len()))
            }
            Step::Check {
                id,
                checked,
                author,
            } => {
                let origin = self.origin_for(author.as_deref());
                let out = self
                    .doc()?
                    .set_checked(&AnnotId(id.clone()), *checked, &origin)
                    .map_err(text)?;
                Ok(format!(
                    "{id} {}{}",
                    if *checked { "checked" } else { "unchecked" },
                    if out.changed { "" } else { " (no change)" }
                ))
            }
            Step::Lock { id, locked } => {
                let when = self.origin.when;
                let out = self
                    .doc()?
                    .set_locked(&AnnotId(id.clone()), *locked, when)
                    .map_err(text)?;
                Ok(format!(
                    "{id} {}{}",
                    if *locked { "locked" } else { "unlocked" },
                    if out.changed { "" } else { " (no change)" }
                ))
            }
            Step::DefineColumn {
                title,
                kind,
                options,
            } => {
                let options: Vec<&str> = options.iter().map(String::as_str).collect();
                let id = self.schema.add(title, *kind, &options).map_err(text)?;
                Ok(format!("{title:?} is column {id}"))
            }
            Step::Column { id, column, value } => {
                let definition = self
                    .schema
                    .get(column)
                    .cloned()
                    .ok_or_else(|| format!("there is no column {column:?}; define it first"))?;
                let when = self.origin.when;
                let out = self
                    .doc()?
                    .set_column_text(&AnnotId(id.clone()), &definition, value, when)
                    .map_err(text)?;
                Ok(format!(
                    "{id} {column} = {value:?}{}",
                    if out.changed { "" } else { " (no change)" }
                ))
            }
            Step::Popup { id, open, rect } => {
                let origin = self.origin.clone();
                let target = AnnotId(id.clone());
                let doc = self.doc()?;
                let popup = doc.ensure_popup(&target, &origin).map_err(text)?;
                if let Some(rect) = rect {
                    doc.set_popup_rect(&target, rect_of(*rect), origin.when)
                        .map_err(text)?;
                }
                if let Some(open) = open {
                    doc.set_popup_open(&target, *open, origin.when)
                        .map_err(text)?;
                }
                Ok(format!("{popup} for {id}"))
            }
            Step::ExpectAnnot(spec) => {
                let schema = self.schema.clone();
                let id = AnnotId(spec.id.clone());
                let doc = self.doc()?;
                let set = doc.annots().map_err(text)?.set();
                let a = set
                    .get(&id)
                    .ok_or_else(|| format!("there is no annotation {id}"))?;
                let mut wrong: Vec<String> = Vec::new();
                let mut check = |what: &str, found: String, want: String| {
                    if found != want {
                        wrong.push(format!("{what} is {found}, expected {want}"));
                    }
                };
                if let Some(kind) = &spec.kind {
                    check("kind", a.kind.pdf_name().to_string(), kind.clone());
                }
                if let Some(page) = spec.page {
                    check("page", (a.page + 1).to_string(), page.to_string());
                }
                if let Some(contents) = &spec.contents {
                    check(
                        "contents",
                        format!("{:?}", a.meta.contents.clone().unwrap_or_default()),
                        format!("{contents:?}"),
                    );
                }
                if let Some(author) = &spec.author {
                    check(
                        "author",
                        format!("{:?}", a.meta.author.clone().unwrap_or_default()),
                        format!("{author:?}"),
                    );
                }
                if let Some(status) = &spec.status {
                    check(
                        "status",
                        set.review_state(&id).name().to_string(),
                        status.clone(),
                    );
                }
                if let Some(checked) = spec.checked {
                    check(
                        "checked",
                        markup_ops::is_checked(set, &id).to_string(),
                        checked.to_string(),
                    );
                }
                if let Some(locked) = spec.locked {
                    check(
                        "locked",
                        markup_ops::is_locked(a).to_string(),
                        locked.to_string(),
                    );
                }
                if let Some(replies) = spec.replies {
                    check(
                        "replies",
                        set.replies(&id).len().to_string(),
                        replies.to_string(),
                    );
                }
                let popup = a.popup.as_ref().and_then(|p| set.get(p));
                if let Some(wanted) = spec.popup {
                    check("popup", popup.is_some().to_string(), wanted.to_string());
                }
                if let Some(open) = spec.open {
                    let is_open = popup.map_or(a.open, |p| p.open);
                    check("open", is_open.to_string(), open.to_string());
                }
                if let Some(rotation) = spec.rotation {
                    check("rotation", a.rotation().to_string(), rotation.to_string());
                }
                if let Some(quads) = spec.quads {
                    let found = match &a.geometry {
                        Geometry::Quads(q) => q.len(),
                        _ => 0,
                    };
                    check("quads", found.to_string(), quads.to_string());
                }
                for (column, want) in &spec.columns {
                    let found = match schema.get(column) {
                        Some(definition) => markup_ops::column_value(a, definition).display(),
                        None => {
                            return Err(format!("there is no column {column:?}; define it first"))
                        }
                    };
                    check(
                        &format!("column {column}"),
                        format!("{found:?}"),
                        format!("{want:?}"),
                    );
                }
                if wrong.is_empty() {
                    Ok(format!("{id} is as expected"))
                } else {
                    Err(format!("annotation {id}: {}", wrong.join("; ")))
                }
            }
            Step::SetScale(spec) => sm::set_scale(self.doc()?, spec),
            Step::DefaultScale(spec) => sm::default_scale(self.doc()?, spec),
            Step::Calibrate(spec) => sm::calibrate(self.doc()?, spec),
            Step::AddViewport(spec) => sm::viewport(self.doc()?, spec, true),
            Step::EditViewport(spec) => sm::viewport(self.doc()?, spec, false),
            Step::RemoveViewport { page, name } => sm::remove_viewport(self.doc()?, *page, name),
            Step::Measure(spec) => {
                let origin = self.origin_for(spec.author.as_deref());
                sm::measure(self.doc()?, spec, &origin)
            }
            Step::MoveMeasure { id, points } => {
                let when = self.origin.when;
                sm::reshape(self.doc()?, id, points, when)
            }
            Step::Cutout { id, points } => {
                let when = self.origin.when;
                sm::cutout(self.doc()?, id, points, when)
            }
            Step::RemoveCutout { id, index } => {
                let when = self.origin.when;
                sm::remove_cutout(self.doc()?, id, *index, when)
            }
            Step::Depth { id, depth } => {
                let when = self.origin.when;
                sm::depth(self.doc()?, id, depth, when)
            }
            Step::Count { id, add, remove } => {
                let when = self.origin.when;
                sm::count(self.doc()?, id, *add, *remove, when)
            }
            Step::ExpectMeasure(spec) => sm::expect_measure(self.doc()?, spec),
            Step::ExpectScale(spec) => sm::expect_scale(self.doc()?, spec),
            Step::Structure {
                command,
                invalidate_signatures,
            } => {
                let policy = if *invalidate_signatures {
                    pl_pdf::SignaturePolicy::Invalidate
                } else {
                    pl_pdf::SignaturePolicy::Refuse
                };
                let out = self
                    .doc()?
                    .apply_structure((**command).clone(), policy)
                    .map_err(text)?;
                Ok(format!(
                    "{}; {} page(s)",
                    out.command, out.page_map.new_page_count
                ))
            }
            Step::ExpectPages { count } => {
                let found = self.doc()?.page_count();
                if found != *count {
                    return Err(format!("expected {count} page(s), found {found}"));
                }
                Ok(format!("{found} page(s)"))
            }
        }
    }

    /// The `/Rotate` a new text box or stamp gets: the step's, or the page's own, so that it
    /// reads upright as the page is displayed.
    fn rotation_for(&mut self, asked: Option<i32>, page: u32) -> std::result::Result<i32, String> {
        match asked {
            Some(rotation) => Ok(rotation),
            None => Ok(self
                .doc()?
                .load_geometry(page)
                .map_err(text)?
                .rotate()
                .degrees()),
        }
    }

    fn new_text_box(
        &mut self,
        spec: &AddSpec,
        page: u32,
        origin: &Origin,
    ) -> std::result::Result<Annotation, String> {
        let rect = spec
            .geometry
            .rect
            .map(rect_of)
            .ok_or("FreeText needs \"rect\"")?;
        let words = spec
            .text
            .as_deref()
            .or(spec.contents.as_deref())
            .ok_or("FreeText needs \"text\"")?;
        let intent = spec.intent.as_deref().unwrap_or("box").to_ascii_lowercase();
        let mut a = match intent.as_str() {
            "box" | "text" | "textbox" | "text_box" => {
                Annotation::free_text(page as usize, rect, words, origin)
            }
            "callout" => {
                let tip = spec.tip.ok_or("a callout needs \"tip\"")?;
                Annotation::callout(page as usize, rect, point_of(&tip), words, origin)
            }
            "typewriter" => Annotation::typewriter(page as usize, rect, words, origin),
            other => return Err(format!("unknown FreeText intent {other:?}")),
        };
        if let Some(free) = a.free_text.as_mut() {
            spec.text_style.apply(free)?;
        }
        a.set_rotation(self.rotation_for(spec.rotation, page)?);
        Ok(a)
    }

    fn new_stamp(
        &mut self,
        spec: &AddSpec,
        page: u32,
        origin: &Origin,
    ) -> std::result::Result<Annotation, String> {
        let given = spec.geometry.rect.ok_or("Stamp needs \"rect\"")?;
        let rotation = self.rotation_for(spec.rotation, page)?;
        let mut a = match (&spec.stamp, &spec.image) {
            (Some(choice), None) => {
                let mut stamp = choice.spec()?;
                if spec.detail.is_some() {
                    stamp.detail = spec.detail.clone();
                }
                Annotation::stamp(page as usize, rect_of(given), stamp, origin)
            }
            (None, Some(file)) => {
                let file = self.resolve(file);
                let picture = crate::images::import_image_file(&file).map_err(text)?;
                let rect = if given[2] == 0.0 && given[3] == 0.0 {
                    // A corner only: the picture's natural size, kept on the page.
                    let page_box = self.doc()?.load_geometry(page).map_err(text)?.crop_box();
                    let (w, h) = picture.size_within(
                        (page_box.x1 - given[0]).max(1.0),
                        (page_box.y1 - given[1]).max(1.0),
                    );
                    // On a page displayed sideways the picture is too: its width runs up.
                    let (w, h) = if rotation.rem_euclid(180) == 90 {
                        (h, w)
                    } else {
                        (w, h)
                    };
                    Rect::new(given[0], given[1], given[0] + w, given[1] + h)
                } else {
                    rect_of(given)
                };
                Annotation::image_stamp(page as usize, rect, picture.image, origin)
            }
            (Some(_), Some(_)) => {
                return Err("a Stamp takes \"stamp\" or \"image\", not both".into())
            }
            (None, None) => return Err("Stamp needs \"stamp\" or \"image\"".into()),
        };
        a.set_rotation(rotation);
        Ok(a)
    }
}

fn describe_save(save: &crate::editing::MarkupSave) -> String {
    if save.wrote() {
        format!(
            "wrote {} added, {} modified, {} deleted",
            save.added, save.modified, save.deleted
        )
    } else if save.added + save.modified + save.deleted == 0 {
        "nothing to write".to_string()
    } else {
        "copied".to_string()
    }
}

/// Runs a command file against `session`. Relative paths are resolved against `base`. The
/// run stops at the first failing step with [`DocError::Script`] naming it; steps before it
/// keep their effect. The document of the last `open` stays open in the session.
pub fn run(
    session: &mut Session,
    script: &Script,
    base: &Path,
    host: &mut dyn ScriptHost,
) -> Result<ScriptReport> {
    let when = match &script.date {
        Some(date) => PdfDate::parse(date)
            .ok_or_else(|| DocError::Script(format!("\"date\" is not a PDF date: {date:?}")))?,
        None => PdfDate::now(),
    };
    let author = script.author.clone().unwrap_or_else(|| "Plumbline".into());
    let mut runner = Runner {
        session,
        base,
        host,
        origin: Origin::new(author, when),
        current: None,
        clipboard: Clipboard::default(),
        schema: pl_takeoff::Schema::new(),
    };
    let mut report = ScriptReport::default();
    for (i, step) in script.steps.iter().enumerate() {
        let name = step.name();
        match runner.step(step) {
            Ok(detail) => {
                let line = if detail.is_empty() {
                    format!("{}: {name}", i + 1)
                } else {
                    format!("{}: {name} {detail}", i + 1)
                };
                runner.host.log(&line);
                report.steps += 1;
                report.document = runner.current;
            }
            Err(e) => {
                return Err(DocError::Script(format!("step {} ({name}): {e}", i + 1)));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_file_parses_with_defaults_and_nulls() {
        let script = Script::from_json(
            r#"{
              "author": "T",
              "steps": [
                {"op": "open", "path": "a.pdf"},
                {"op": "add", "id": "b", "kind": "Square", "page": 1, "rect": [1, 2, 3, 4],
                 "style": {"stroke": [1, 0, 0], "fill": null, "width": 2, "dash": [6, 3]}},
                {"op": "modify", "id": "b", "move": [10, -5], "style": {"cloud": null}},
                {"op": "delete", "id": "b"},
                {"op": "undo"}, {"op": "redo"},
                {"op": "render", "page": 1, "output": "p.png"},
                {"op": "expect", "count": 0},
                {"op": "save"}, {"op": "save_as", "path": "b.pdf"}, {"op": "save_optimised"}
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(script.steps.len(), 11);
        let Step::Add(add) = &script.steps[1] else {
            panic!()
        };
        assert_eq!(add.style.stroke, Some(Some([1.0, 0.0, 0.0])));
        assert_eq!(add.style.fill, Some(None), "null switches the fill off");
        assert_eq!(add.style.opacity, None, "absent keeps the default");
        assert_eq!(add.geometry.rect, Some([1.0, 2.0, 3.0, 4.0]));
        let Step::Modify(modify) = &script.steps[2] else {
            panic!()
        };
        assert_eq!(modify.move_by, Some([10.0, -5.0]));
        assert_eq!(modify.style.cloud, Some(None));
        assert!(matches!(script.steps[6], Step::Render { scale, .. } if scale == 1.0));

        // It also writes back out and reads the same.
        let json = serde_json::to_string(&script).unwrap();
        assert_eq!(Script::from_json(&json).unwrap(), script);

        assert!(Script::from_json(r#"{"steps": [{"op": "explode"}]}"#).is_err());
        assert!(Script::from_json(
            r#"{"steps": [{"op": "add", "kind": "Square", "page": 1, "style": {"colour": 1}}]}"#
        )
        .is_err());
    }

    #[test]
    fn geometry_is_checked_against_the_kind() {
        let g = |json: &str| serde_json::from_str::<GeometrySpec>(json).unwrap();
        assert!(
            g(r#"{"rect": [4, 3, 2, 1]}"#).build(&AnnotKind::Square)
                == Ok(Geometry::Rect(Rect::new(2.0, 1.0, 4.0, 3.0)))
        );
        assert!(g(r#"{"points": [[0, 0]]}"#)
            .build(&AnnotKind::Line)
            .is_err());
        assert!(g(r#"{"points": [[0, 0], [1, 1]]}"#)
            .build(&AnnotKind::Line)
            .is_ok());
        assert!(g(r#"{"points": [[0, 0], [1, 1]]}"#)
            .build(&AnnotKind::Polygon)
            .is_err());
        assert!(g(r#"{"points": [[0, 0], [1, 1]]}"#)
            .build(&AnnotKind::PolyLine)
            .is_ok());
        assert!(g(r#"{"strokes": [[]]}"#).build(&AnnotKind::Ink).is_err());
        assert!(g(r#"{"strokes": [[[1, 1]]]}"#)
            .build(&AnnotKind::Ink)
            .is_ok());
        assert!(g(r#"{"rect": [0, 0, 1, 1]}"#)
            .build(&AnnotKind::Highlight)
            .is_err());
        let Ok(Geometry::Quads(quads)) =
            g(r#"{"quads": [[0, 0, 10, 4]]}"#).build(&AnnotKind::Underline)
        else {
            panic!()
        };
        assert_eq!(quads.len(), 1);
        assert!(g(r#"{"rect": [0, 0, 1, 1]}"#)
            .build(&AnnotKind::FreeText)
            .is_err());
        let mut style = Style::default_for(&AnnotKind::Line);
        let bad = StyleSpec {
            line_end: Some("Pointy".into()),
            ..StyleSpec::default()
        };
        assert!(bad.apply(&mut style).is_err());
    }
}
