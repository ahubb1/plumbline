//! The typed annotation model. Everything is in unrotated page user space (pl-geom's
//! convention); nothing here knows about `/Rotate`, `/UserUnit` or the CropBox.

use std::sync::atomic::{AtomicU64, Ordering};

use std::sync::Arc;

use pl_draw::shapes::{LineEnding, Quad};
use pl_draw::{BlendMode, DrawError, Rgb, RgbaImage, StampSpec};
use pl_geom::{Point, Rect};
use pl_pdf::{Dictionary, Object, ObjectId};

use crate::content::{callout_route, FreeText, FreeTextIntent, Stamp, StampContent};
use crate::measure::Measurement;
use crate::objects::PdfDate;

/// The one key under which Plumbline keeps private data in an annotation dictionary. It always
/// sits beside a standard subtype with a full appearance, so other viewers lose nothing.
pub const PRIVATE_KEY: &str = "PlumblineData";

/// The key inside the [`PRIVATE_KEY`] dictionary that this crate keeps for itself: what a
/// FreeText or a Stamp of ours holds that no standard key can say (vertical alignment, line
/// spacing, padding; a stamp's label, second line and colour scheme). Its presence is also
/// what marks a FreeText or Stamp as written by this crate.
pub const MODEL_KEY: &str = "PlModel";

/// Bluebeam Revu's key for an annotation's custom column values ([`Annotation::column_data`]).
pub const COLUMN_DATA_KEY: &str = "BSIColumnData";

/// The stable identity of an annotation: its `/NM` name.
///
/// An annotation read from a file without `/NM` gets a derived id that is the same on every
/// load of that file; it is written as `/NM` the first time the annotation is modified.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnnotId(pub String);

impl AnnotId {
    /// A fresh id in UUID form, unique for all practical purposes.
    pub fn generate() -> AnnotId {
        use std::hash::{BuildHasher, Hasher};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        // RandomState is seeded from the operating system's random source.
        let word = |salt: u64| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(n);
            h.write_u128(nanos);
            h.write_u64(salt);
            h.finish()
        };
        let (a, b) = (word(1), word(2));
        AnnotId(format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            (a >> 32) as u32,
            (a >> 16) as u16,
            a as u16 & 0x0fff,
            ((b >> 48) as u16 & 0x3fff) | 0x8000,
            b & 0xffff_ffff_ffff
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AnnotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AnnotId {
    fn from(s: &str) -> AnnotId {
        AnnotId(s.to_string())
    }
}

/// The annotation subtype.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnnotKind {
    Square,
    Circle,
    Line,
    PolyLine,
    Polygon,
    Ink,
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
    /// A sticky note (`/Text`).
    Text,
    Link,
    Popup,
    /// A text box, callout or typewriter text. Modelled when the annotation holds a
    /// [`FreeText`] (one of ours, or a foreign one made editable); otherwise it is displayed
    /// from its own appearance and can be moved and deleted only.
    FreeText,
    /// A stamp. Modelled when the annotation holds a [`Stamp`] (one of ours); a foreign stamp
    /// keeps its own appearance and can be moved and deleted only.
    Stamp,
    /// Any other subtype (`Widget`, `FileAttachment`, vendor types), by name. Moved and deleted
    /// only.
    Unknown(String),
}

impl AnnotKind {
    /// The `/Subtype` name.
    pub fn pdf_name(&self) -> &str {
        match self {
            AnnotKind::Square => "Square",
            AnnotKind::Circle => "Circle",
            AnnotKind::Line => "Line",
            AnnotKind::PolyLine => "PolyLine",
            AnnotKind::Polygon => "Polygon",
            AnnotKind::Ink => "Ink",
            AnnotKind::Highlight => "Highlight",
            AnnotKind::Underline => "Underline",
            AnnotKind::StrikeOut => "StrikeOut",
            AnnotKind::Squiggly => "Squiggly",
            AnnotKind::Text => "Text",
            AnnotKind::Link => "Link",
            AnnotKind::Popup => "Popup",
            AnnotKind::FreeText => "FreeText",
            AnnotKind::Stamp => "Stamp",
            AnnotKind::Unknown(name) => name,
        }
    }

    pub fn from_pdf_name(name: &str) -> AnnotKind {
        match name {
            "Square" => AnnotKind::Square,
            "Circle" => AnnotKind::Circle,
            "Line" => AnnotKind::Line,
            "PolyLine" => AnnotKind::PolyLine,
            "Polygon" => AnnotKind::Polygon,
            "Ink" => AnnotKind::Ink,
            "Highlight" => AnnotKind::Highlight,
            "Underline" => AnnotKind::Underline,
            "StrikeOut" => AnnotKind::StrikeOut,
            "Squiggly" => AnnotKind::Squiggly,
            "Text" => AnnotKind::Text,
            "Link" => AnnotKind::Link,
            "Popup" => AnnotKind::Popup,
            "FreeText" => AnnotKind::FreeText,
            "Stamp" => AnnotKind::Stamp,
            other => AnnotKind::Unknown(other.to_string()),
        }
    }

    /// The kinds this crate can generate an appearance for and edit freely whatever file they
    /// come from. FreeText and Stamp are modelled too, but only per annotation (see
    /// [`Annotation::is_modelled`]).
    pub const MODELLED: [AnnotKind; 13] = [
        AnnotKind::Square,
        AnnotKind::Circle,
        AnnotKind::Line,
        AnnotKind::PolyLine,
        AnnotKind::Polygon,
        AnnotKind::Ink,
        AnnotKind::Highlight,
        AnnotKind::Underline,
        AnnotKind::StrikeOut,
        AnnotKind::Squiggly,
        AnnotKind::Text,
        AnnotKind::Link,
        AnnotKind::Popup,
    ];

    /// False for unknown subtypes, and for FreeText and Stamp, which are modelled only when
    /// the annotation carries their content (see [`Annotation::is_modelled`]).
    pub fn is_modelled(&self) -> bool {
        !matches!(
            self,
            AnnotKind::FreeText | AnnotKind::Stamp | AnnotKind::Unknown(_)
        )
    }

    /// The four text-markup kinds, whose geometry is `/QuadPoints`.
    pub fn is_text_markup(&self) -> bool {
        matches!(
            self,
            AnnotKind::Highlight
                | AnnotKind::Underline
                | AnnotKind::StrikeOut
                | AnnotKind::Squiggly
        )
    }

    /// True for the kinds that are markup annotations in the PDF sense (they carry `/T`,
    /// `/CA`, `/Popup` and so on). Link and Popup are not.
    pub fn is_markup(&self) -> bool {
        !matches!(
            self,
            AnnotKind::Link | AnnotKind::Popup | AnnotKind::Unknown(_)
        )
    }

    /// The `/Subj` a new annotation of this kind starts with.
    pub fn default_subject(&self) -> &str {
        match self {
            AnnotKind::Square => "Rectangle",
            AnnotKind::Circle => "Ellipse",
            AnnotKind::PolyLine => "Polyline",
            AnnotKind::Ink => "Pen",
            AnnotKind::StrikeOut => "Strikeout",
            AnnotKind::Text => "Note",
            other => other.pdf_name(),
        }
    }

    /// The colour a viewer paints an annotation of this kind with when its dictionary has no
    /// `/C` at all: black for the drawn shapes and text rules, yellow for a highlight or a
    /// note. (Only an empty `/C` array means "no colour".) `None` for the kinds that paint
    /// nothing without a colour: a Link, a Popup and everything unmodelled.
    pub fn implied_color(&self) -> Option<Rgb> {
        match self {
            AnnotKind::Square
            | AnnotKind::Circle
            | AnnotKind::Line
            | AnnotKind::PolyLine
            | AnnotKind::Polygon
            | AnnotKind::Ink
            | AnnotKind::Underline
            | AnnotKind::StrikeOut
            | AnnotKind::Squiggly => Some(Rgb::BLACK),
            AnnotKind::Highlight | AnnotKind::Text => Some(Rgb::new(1.0, 0.9, 0.2)),
            _ => None,
        }
    }

    /// The blend mode an annotation of this kind has when it states none: Multiply for a
    /// highlight, Normal for everything else.
    pub fn default_blend(&self) -> BlendMode {
        if *self == AnnotKind::Highlight {
            BlendMode::Multiply
        } else {
            BlendMode::Normal
        }
    }
}

/// Where an annotation is, per kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    /// Square and Circle: the outer edge of the shape (the border is drawn inside it; a cloud
    /// border bulges outside it). A modelled FreeText: the text box, without its callout
    /// leader. Text, Link, Popup, Stamp and every unmodelled kind: `/Rect`.
    Rect(Rect),
    /// Line: `/L`.
    Line { a: Point, b: Point },
    /// PolyLine and Polygon: `/Vertices`.
    Points(Vec<Point>),
    /// Ink: `/InkList`, one list of points per stroke.
    Ink(Vec<Vec<Point>>),
    /// Text markup: `/QuadPoints`, one quad per run of text. Each quad is in ring order,
    /// starting at the start of the baseline: bottom-left, bottom-right, top-right, top-left
    /// for upright text.
    Quads(Vec<Quad>),
}

impl Geometry {
    /// The box around the defining points (not the painted extent: see
    /// [`Annotation::bounds`](crate::Annotation::bounds)).
    pub fn bounds(&self) -> Rect {
        let mut points = Vec::new();
        self.for_each_point(|p| points.push(p));
        let Some(first) = points.first() else {
            return Rect::ZERO;
        };
        points
            .iter()
            .fold(Rect::from_points(*first, *first), |r, p| r.union_pt(*p))
    }

    pub(crate) fn for_each_point(&self, mut f: impl FnMut(Point)) {
        match self {
            Geometry::Rect(r) => {
                f(Point::new(r.x0, r.y0));
                f(Point::new(r.x1, r.y1));
            }
            Geometry::Line { a, b } => {
                f(*a);
                f(*b);
            }
            Geometry::Points(points) => points.iter().for_each(|p| f(*p)),
            Geometry::Ink(strokes) => strokes.iter().flatten().for_each(|p| f(*p)),
            Geometry::Quads(quads) => quads.iter().flatten().for_each(|p| f(*p)),
        }
    }

    /// The same geometry with every point mapped. A rectangle stays a rectangle: it becomes
    /// the box around its two mapped corners.
    pub fn map_points(&self, f: impl Fn(Point) -> Point) -> Geometry {
        match self {
            Geometry::Rect(r) => Geometry::Rect(Rect::from_points(
                f(Point::new(r.x0, r.y0)),
                f(Point::new(r.x1, r.y1)),
            )),
            Geometry::Line { a, b } => Geometry::Line { a: f(*a), b: f(*b) },
            Geometry::Points(points) => Geometry::Points(points.iter().map(|p| f(*p)).collect()),
            Geometry::Ink(strokes) => Geometry::Ink(
                strokes
                    .iter()
                    .map(|s| s.iter().map(|p| f(*p)).collect())
                    .collect(),
            ),
            Geometry::Quads(quads) => Geometry::Quads(quads.iter().map(|q| q.map(&f)).collect()),
        }
    }

    /// True when every coordinate is finite.
    pub fn is_finite(&self) -> bool {
        let mut ok = true;
        self.for_each_point(|p| ok &= p.x.is_finite() && p.y.is_finite());
        ok
    }
}

/// How an annotation is painted. Which fields a kind uses is listed on each.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// `/C`: the border or line colour; the colour of a highlight or a note. `None` paints no
    /// border.
    pub stroke: Option<Rgb>,
    /// `/IC`: the interior of a Square, Circle or Polygon and of closed line endings.
    pub fill: Option<Rgb>,
    /// `/BS /W`: the border or line width, in page units. 0 paints no border.
    pub width: f64,
    /// `/BS /D`: the dash array, when the border is dashed.
    pub dash: Option<Vec<f64>>,
    /// `/CA`: 0 (invisible) to 1 (opaque).
    pub opacity: f64,
    /// `/LE`: the endings of a Line or PolyLine.
    pub line_start: LineEnding,
    pub line_end: LineEnding,
    /// `/BE`: `Some(intensity)` for a cloud border on a Square, Circle or Polygon. Intensity 1
    /// is a small scallop, 2 a large one.
    pub cloud: Option<f64>,
    /// `/BM` (PDF 2.0), and the blend mode of the appearance.
    pub blend: BlendMode,
}

impl Style {
    /// The style a new annotation of `kind` starts with.
    pub fn default_for(kind: &AnnotKind) -> Style {
        let base = Style {
            stroke: Some(Rgb::new(0.8, 0.0, 0.0)),
            fill: None,
            width: 1.0,
            dash: None,
            opacity: 1.0,
            line_start: LineEnding::None,
            line_end: LineEnding::None,
            cloud: None,
            blend: kind.default_blend(),
        };
        match kind {
            AnnotKind::Highlight | AnnotKind::Text => Style {
                stroke: Some(Rgb::new(1.0, 0.9, 0.2)),
                ..base
            },
            AnnotKind::Link => Style {
                stroke: None,
                width: 0.0,
                ..base
            },
            // A popup has no style keys; these are the values every popup reads as.
            AnnotKind::Popup => Style {
                stroke: None,
                ..base
            },
            _ => base,
        }
    }
}

/// `/F` annotation flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(pub u32);

impl Flags {
    pub const INVISIBLE: u32 = 1;
    pub const HIDDEN: u32 = 1 << 1;
    pub const PRINT: u32 = 1 << 2;
    pub const NO_ZOOM: u32 = 1 << 3;
    pub const NO_ROTATE: u32 = 1 << 4;
    pub const NO_VIEW: u32 = 1 << 5;
    pub const READ_ONLY: u32 = 1 << 6;
    pub const LOCKED: u32 = 1 << 7;
    pub const LOCKED_CONTENTS: u32 = 1 << 9;

    pub fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    pub fn with(self, bit: u32, on: bool) -> Flags {
        Flags(if on { self.0 | bit } else { self.0 & !bit })
    }

    /// Not shown on screen: hidden or no-view.
    pub fn is_hidden(self) -> bool {
        self.has(Flags::HIDDEN) || self.has(Flags::NO_VIEW)
    }

    pub fn is_locked(self) -> bool {
        self.has(Flags::LOCKED) || self.has(Flags::READ_ONLY)
    }
}

/// Who, when and what: the keys every annotation kind shares.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Meta {
    /// `/T`.
    pub author: Option<String>,
    /// `/Subj`.
    pub subject: Option<String>,
    /// `/Contents`.
    pub contents: Option<String>,
    /// `/CreationDate`. `None` also when the file's value is not a valid date (the value is
    /// then kept as it is in the original dictionary).
    pub created: Option<PdfDate>,
    /// `/M`.
    pub modified: Option<PdfDate>,
    /// `/F`.
    pub flags: Flags,
    /// `/OC`: the optional-content group or membership dictionary that decides visibility.
    pub layer: Option<ObjectId>,
    /// `/IRT`: the annotation this one replies to (or carries a review state for).
    pub reply_to: Option<AnnotId>,
    /// `/RT`: `R` (reply) or `Group`.
    pub reply_type: Option<String>,
    /// `/State` and `/StateModel` of a review-state annotation.
    pub state: Option<String>,
    pub state_model: Option<String>,
}

/// Where a Link goes.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    /// A web address (`/A << /S /URI >>`).
    Uri(String),
    /// A page of this document (`/Dest [page /XYZ left top null]`, or `/Fit` without a
    /// position). `page` is a zero-based index.
    Page { page: usize, at: Option<Point> },
    /// Anything else (a named destination, a launch action, JavaScript). It is kept in the
    /// original dictionary and cannot be written by this crate.
    Other,
}

/// What a new annotation is stamped with.
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    pub author: String,
    pub when: PdfDate,
}

impl Origin {
    pub fn new(author: impl Into<String>, when: PdfDate) -> Origin {
        Origin {
            author: author.into(),
            when,
        }
    }

    /// `author`, now.
    pub fn now(author: impl Into<String>) -> Origin {
        Origin::new(author, PdfDate::now())
    }
}

/// The annotation as the file holds it: the object, its place, the untouched dictionary with
/// every unknown or vendor key, and the model that dictionary was read as.
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    /// The annotation's object id; `None` when its dictionary is written directly inside the
    /// `/Annots` array (it can then be read but not changed).
    pub object: Option<ObjectId>,
    /// Page index and `/Annots` position in the file.
    pub page: usize,
    pub slot: usize,
    /// Every key the file holds, untouched.
    pub dict: Dictionary,
    /// True when the dictionary has a normal appearance (`/AP /N`).
    pub has_appearance: bool,
    /// The typed reading of `dict` (its own `baseline` is `None`).
    pub model: Box<Annotation>,
    /// For a FreeText written by another application: the same dictionary read as a text box
    /// this crate can edit (see [`Annotation::make_editable`]).
    pub(crate) editable: Option<Box<Editable>>,
    /// For a count of ours: the annotations that hold its other markers in the file (a count
    /// is written as one annotation per marker; the first is this one). Their markers are in
    /// the model's geometry, after this annotation's own.
    pub members: Vec<Member>,
}

/// One of the further annotations a count is written as (see [`Baseline::members`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// `/NM`.
    pub name: Option<String>,
    pub object: Option<ObjectId>,
    /// Page index and `/Annots` position in the file.
    pub page: usize,
    pub slot: usize,
    /// `/Rect`.
    pub rect: Rect,
}

/// A foreign FreeText read as a text box: what [`Annotation::make_editable`] installs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Editable {
    pub free_text: FreeText,
    pub style: Style,
    pub geometry: Geometry,
}

impl Baseline {
    /// The model a changed annotation is compared with: the file's reading, or, for a foreign
    /// FreeText that has been made editable, its reading as a text box.
    pub(crate) fn reference(&self, current: &Annotation) -> std::borrow::Cow<'_, Annotation> {
        match &self.editable {
            Some(e) if current.free_text.is_some() && self.model.free_text.is_none() => {
                std::borrow::Cow::Owned(Annotation {
                    free_text: Some(e.free_text.clone()),
                    style: e.style.clone(),
                    geometry: e.geometry.clone(),
                    ..(*self.model).clone()
                })
            }
            _ => std::borrow::Cow::Borrowed(&self.model),
        }
    }
}

/// One annotation.
///
/// The public fields are the typed model. [`Annotation::baseline`] is what the file holds for
/// an annotation that was read rather than created: a save compares the fields with it and
/// rewrites only the keys whose fields changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    /// `/NM`.
    pub id: AnnotId,
    /// Zero-based page index.
    pub page: usize,
    pub kind: AnnotKind,
    pub geometry: Geometry,
    pub style: Style,
    pub meta: Meta,
    /// `/Name`: the icon of a Text note (`Note`, `Comment`, ...).
    pub icon: Option<String>,
    /// `/Open`: whether a Popup (or a Text note) is shown open.
    pub open: bool,
    /// `/Popup`: the popup window of a markup annotation.
    pub popup: Option<AnnotId>,
    /// `/Parent`: the markup a Popup belongs to.
    pub parent: Option<AnnotId>,
    /// The target of a Link.
    pub link: Option<LinkTarget>,
    /// Private application data, written under [`PRIVATE_KEY`]. The key [`MODEL_KEY`] inside
    /// it is this crate's and never appears here.
    pub app_data: Option<Dictionary>,
    /// The text side of a FreeText this crate draws.
    pub free_text: Option<FreeText>,
    /// The content of a Stamp this crate draws.
    pub stamp: Option<Stamp>,
    /// The measurement a Line, PolyLine, Polygon or Ink is (see [`crate::Measurement`]).
    pub measure: Option<Measurement>,
    /// Bluebeam Revu's custom column values, `/BSIColumnData`: one entry per column defined in
    /// the catalog's `/BSIAnnotColumns`, by position, each normally a text string (empty for no
    /// value). The entries are kept exactly as the file has them (bytes and string form), so a
    /// value that is not changed is written back as it was. `None` when the key is absent.
    /// pl-takeoff reads and sets the values ([`COLUMN_DATA_KEY`]).
    pub column_data: Option<Vec<Object>>,
    pub(crate) baseline: Option<Baseline>,
}

impl Annotation {
    /// A new annotation with the default style of its kind, stamped with `origin`.
    pub fn new(kind: AnnotKind, page: usize, geometry: Geometry, origin: &Origin) -> Annotation {
        let markup = kind.is_markup();
        let flags = if kind == AnnotKind::Popup {
            Flags(0)
        } else {
            Flags(Flags::PRINT)
        };
        Annotation {
            id: AnnotId::generate(),
            page,
            style: Style::default_for(&kind),
            meta: Meta {
                author: markup.then(|| origin.author.clone()),
                subject: markup.then(|| kind.default_subject().to_string()),
                created: markup.then_some(origin.when),
                modified: Some(origin.when),
                flags,
                ..Meta::default()
            },
            icon: (kind == AnnotKind::Text).then(|| "Note".to_string()),
            open: false,
            popup: None,
            parent: None,
            link: None,
            app_data: None,
            free_text: None,
            stamp: None,
            measure: None,
            column_data: None,
            geometry,
            kind,
            baseline: None,
        }
    }

    /// A text box: `text` wrapped inside `rect`, with a border in the default markup colour.
    pub fn free_text(page: usize, rect: Rect, text: &str, origin: &Origin) -> Annotation {
        let mut a = Annotation::new(
            AnnotKind::FreeText,
            page,
            Geometry::Rect(rect.abs()),
            origin,
        );
        a.meta.subject = Some("Text Box".to_string());
        a.meta.contents = Some(text.to_string());
        a.free_text = Some(FreeText::default());
        a
    }

    /// A callout: a text box with a leader line from `tip`, ending there in an open arrow.
    /// The leader is routed to the side of the box that faces the tip
    /// ([`Annotation::reroute_callout`] does it again after the box or the tip moves).
    pub fn callout(page: usize, rect: Rect, tip: Point, text: &str, origin: &Origin) -> Annotation {
        let mut a = Annotation::free_text(page, rect, text, origin);
        a.meta.subject = Some("Callout".to_string());
        a.style.line_end = LineEnding::OpenArrow;
        if let Some(t) = a.free_text.as_mut() {
            t.intent = FreeTextIntent::Callout;
            t.callout = callout_route(rect.abs(), tip);
        }
        a
    }

    /// Typewriter text: no border, no background.
    pub fn typewriter(page: usize, rect: Rect, text: &str, origin: &Origin) -> Annotation {
        let mut a = Annotation::free_text(page, rect, text, origin);
        a.meta.subject = Some("Typewriter".to_string());
        a.style.stroke = None;
        a.style.width = 0.0;
        if let Some(t) = a.free_text.as_mut() {
            t.intent = FreeTextIntent::TypeWriter;
            t.padding = 1.0;
        }
        a
    }

    /// One of Plumbline's stamps filling `rect`.
    pub fn stamp(page: usize, rect: Rect, spec: StampSpec, origin: &Origin) -> Annotation {
        let mut a = Annotation::new(AnnotKind::Stamp, page, Geometry::Rect(rect.abs()), origin);
        a.icon = Some(spec.name.clone());
        a.meta.contents = Some(spec.label.clone());
        a.style.stroke = None;
        a.stamp = Some(Stamp {
            content: StampContent::Label(spec),
            rotation: 0,
        });
        a
    }

    /// A picture placed as a stamp, stretched over `rect`.
    pub fn image_stamp(
        page: usize,
        rect: Rect,
        image: Arc<RgbaImage>,
        origin: &Origin,
    ) -> Annotation {
        let mut a = Annotation::new(AnnotKind::Stamp, page, Geometry::Rect(rect.abs()), origin);
        a.icon = Some("Image".to_string());
        a.meta.subject = Some("Image".to_string());
        a.style.stroke = None;
        a.stamp = Some(Stamp {
            content: StampContent::Image(image),
            rotation: 0,
        });
        a
    }

    /// Sets `/Rotate` of a modelled FreeText or Stamp, so that it reads upright on a page
    /// whose `/Rotate` is `degrees` (see [`FreeText::rotation`]). Does nothing for other kinds.
    pub fn set_rotation(&mut self, degrees: i32) {
        let rotation = crate::content::normalise_rotation(i64::from(degrees));
        if let Some(t) = self.free_text.as_mut() {
            t.rotation = rotation;
        }
        if let Some(s) = self.stamp.as_mut() {
            s.rotation = rotation;
        }
        self.set_measure_rotation(degrees);
    }

    /// `/Rotate` of a modelled FreeText or Stamp; 0 for anything else.
    pub fn rotation(&self) -> u16 {
        self.free_text
            .as_ref()
            .map(|t| t.rotation)
            .or(self.stamp.as_ref().map(|s| s.rotation))
            .unwrap_or(0)
    }

    /// Routes a callout's leader again from its tip to the box as it is now. Does nothing for
    /// anything but a callout with a leader.
    pub fn reroute_callout(&mut self) {
        let Geometry::Rect(rect) = self.geometry else {
            return;
        };
        if let Some(t) = self.free_text.as_mut() {
            if let Some(tip) = t.callout.first().copied() {
                t.callout = callout_route(rect, tip);
            }
        }
    }

    /// Makes a FreeText written by another application editable: its `/DA`, `/DS`, `/Q`,
    /// `/BS`, `/C`, `/CL`, `/LE`, `/IT`, `/RD` and `/Rotate` are taken up as a [`FreeText`] and
    /// a style. Until a field is then changed the annotation stays clean and keeps its own
    /// appearance; the first save after a change writes an appearance of ours.
    ///
    /// Returns true when the annotation is (now, or already) a modelled FreeText.
    pub fn make_editable(&mut self) -> bool {
        if self.kind != AnnotKind::FreeText {
            return false;
        }
        if self.free_text.is_some() {
            return true;
        }
        let Some(base) = self.baseline.as_ref() else {
            return false;
        };
        let Some(editable) = base.editable.as_deref() else {
            return false;
        };
        // The file's reading, moved by however far the annotation has been moved since.
        let moved = self.geometry.bounds().origin() - base.model.geometry.bounds().origin();
        let mut free_text = editable.free_text.clone();
        let mut geometry = editable.geometry.clone();
        if moved.x.is_finite() && moved.y.is_finite() && moved.hypot() > 0.0 {
            geometry = geometry.map_points(|p| p + moved);
            for p in &mut free_text.callout {
                *p += moved;
            }
        }
        self.style = editable.style.clone();
        self.free_text = Some(free_text);
        self.geometry = geometry;
        true
    }

    /// Checks what a save would refuse: text a FreeText or a stamp cannot show (a character
    /// outside WinAnsi, named in the error with its position) or a size that is not a number.
    pub fn validate(&self) -> Result<(), DrawError> {
        if let Some(layout) = self.text_layout() {
            layout?;
        }
        if let (Some(stamp), Geometry::Rect(rect)) = (&self.stamp, &self.geometry) {
            if let StampContent::Label(spec) = &stamp.content {
                pl_draw::stamp_list(spec, *rect)?;
            }
        }
        Ok(())
    }

    /// A rectangle whose outer edge is `rect`.
    pub fn square(page: usize, rect: Rect, origin: &Origin) -> Annotation {
        Annotation::new(AnnotKind::Square, page, Geometry::Rect(rect.abs()), origin)
    }

    /// An ellipse inscribed in `rect`.
    pub fn circle(page: usize, rect: Rect, origin: &Origin) -> Annotation {
        Annotation::new(AnnotKind::Circle, page, Geometry::Rect(rect.abs()), origin)
    }

    pub fn line(page: usize, a: Point, b: Point, origin: &Origin) -> Annotation {
        Annotation::new(AnnotKind::Line, page, Geometry::Line { a, b }, origin)
    }

    pub fn polyline(page: usize, points: Vec<Point>, origin: &Origin) -> Annotation {
        Annotation::new(AnnotKind::PolyLine, page, Geometry::Points(points), origin)
    }

    pub fn polygon(page: usize, points: Vec<Point>, origin: &Origin) -> Annotation {
        Annotation::new(AnnotKind::Polygon, page, Geometry::Points(points), origin)
    }

    /// Freehand strokes, drawn with round caps and joins.
    pub fn ink(page: usize, strokes: Vec<Vec<Point>>, origin: &Origin) -> Annotation {
        Annotation::new(AnnotKind::Ink, page, Geometry::Ink(strokes), origin)
    }

    /// A Highlight, Underline, StrikeOut or Squiggly over `quads` (see [`Geometry::Quads`]).
    /// Any other `kind` is treated as Highlight.
    pub fn text_markup(
        kind: AnnotKind,
        page: usize,
        quads: Vec<Quad>,
        origin: &Origin,
    ) -> Annotation {
        let kind = if kind.is_text_markup() {
            kind
        } else {
            AnnotKind::Highlight
        };
        Annotation::new(kind, page, Geometry::Quads(quads), origin)
    }

    /// A sticky note whose icon fills `rect`.
    pub fn note(page: usize, rect: Rect, contents: &str, origin: &Origin) -> Annotation {
        let mut a = Annotation::new(AnnotKind::Text, page, Geometry::Rect(rect.abs()), origin);
        a.meta.contents = Some(contents.to_string());
        a
    }

    /// A link over `rect`, with no visible border.
    pub fn link(page: usize, rect: Rect, target: LinkTarget, origin: &Origin) -> Annotation {
        let mut a = Annotation::new(AnnotKind::Link, page, Geometry::Rect(rect.abs()), origin);
        a.link = Some(target);
        a
    }

    /// The popup window of `parent`, on the parent's page. Sets `parent.popup` as well, so the
    /// two refer to each other as the file format expects; add both to the set.
    pub fn popup_for(parent: &mut Annotation, rect: Rect, origin: &Origin) -> Annotation {
        let mut a = Annotation::new(
            AnnotKind::Popup,
            parent.page,
            Geometry::Rect(rect.abs()),
            origin,
        );
        a.parent = Some(parent.id.clone());
        parent.popup = Some(a.id.clone());
        a
    }

    /// What the file holds for this annotation; `None` for one that has not been saved yet.
    pub fn baseline(&self) -> Option<&Baseline> {
        self.baseline.as_ref()
    }

    /// The annotation's dictionary as the file holds it, unknown and vendor keys included.
    pub fn original_dict(&self) -> Option<&Dictionary> {
        self.baseline.as_ref().map(|b| &b.dict)
    }

    /// The annotation's object in the file.
    pub fn object_id(&self) -> Option<ObjectId> {
        self.baseline.as_ref().and_then(|b| b.object)
    }

    /// The typed fields only, with no tie to a file: the form to compare two models in, and
    /// (with a new [`AnnotId`]) the form to paste a copy in.
    pub fn detached(&self) -> Annotation {
        Annotation {
            baseline: None,
            ..self.clone()
        }
    }

    /// A copy with a fresh id and no tie to a file, for copy and paste. Links to other
    /// annotations (popup, parent, reply) are dropped.
    pub fn duplicate(&self, origin: &Origin) -> Annotation {
        let mut copy = self.detached();
        copy.id = AnnotId::generate();
        copy.popup = None;
        copy.parent = None;
        copy.meta.reply_to = None;
        copy.meta.modified = Some(origin.when);
        if copy.kind.is_markup() {
            copy.meta.created = Some(origin.when);
            copy.meta.author = Some(origin.author.clone());
        }
        copy
    }

    /// True when this crate can regenerate the annotation's appearance and edit its shape:
    /// the kind is modelled and the geometry is the kind's own. A FreeText is modelled when it
    /// holds a [`FreeText`] and a Stamp when it holds a [`Stamp`]: the ones this crate wrote,
    /// and a foreign FreeText after [`Annotation::make_editable`]. An unmodelled annotation (a
    /// foreign FreeText or Stamp, an unknown subtype, or a modelled subtype whose geometry
    /// keys could not be read) can be moved, re-described and deleted only.
    pub fn is_modelled(&self) -> bool {
        match self.kind {
            AnnotKind::Square
            | AnnotKind::Circle
            | AnnotKind::Text
            | AnnotKind::Link
            | AnnotKind::Popup => matches!(self.geometry, Geometry::Rect(_)),
            AnnotKind::Line => matches!(self.geometry, Geometry::Line { .. }),
            AnnotKind::PolyLine | AnnotKind::Polygon => {
                matches!(&self.geometry, Geometry::Points(p) if p.len() >= 2)
            }
            AnnotKind::Ink => matches!(self.geometry, Geometry::Ink(_)),
            AnnotKind::Highlight
            | AnnotKind::Underline
            | AnnotKind::StrikeOut
            | AnnotKind::Squiggly => matches!(self.geometry, Geometry::Quads(_)),
            AnnotKind::FreeText => {
                self.free_text.is_some() && matches!(self.geometry, Geometry::Rect(_))
            }
            AnnotKind::Stamp => self.stamp.is_some() && matches!(self.geometry, Geometry::Rect(_)),
            AnnotKind::Unknown(_) => false,
        }
    }

    /// Sets `/M` (call when a user edit changes the annotation).
    pub fn touch(&mut self, when: PdfDate) {
        self.meta.modified = Some(when);
    }

    /// True when a save would write this annotation: it is new, or a field differs from the
    /// file.
    pub fn is_dirty(&self) -> bool {
        match &self.baseline {
            None => true,
            Some(base) => !crate::write::diff(self, base).keys.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_distinct_uuids() {
        let ids: std::collections::HashSet<AnnotId> =
            (0..2000).map(|_| AnnotId::generate()).collect();
        assert_eq!(ids.len(), 2000);
        for id in ids.iter().take(20) {
            let parts: Vec<&str> = id.as_str().split('-').collect();
            assert_eq!(
                parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
                [8, 4, 4, 4, 12]
            );
            assert!(parts[2].starts_with('4'));
            assert!(id
                .as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-'));
        }
    }

    #[test]
    fn kind_names_round_trip() {
        for kind in AnnotKind::MODELLED {
            assert!(kind.is_modelled());
            assert_eq!(AnnotKind::from_pdf_name(kind.pdf_name()), kind);
        }
        assert!(!AnnotKind::FreeText.is_modelled());
        assert!(!AnnotKind::Stamp.is_modelled());
        let w = AnnotKind::from_pdf_name("Widget");
        assert_eq!(w, AnnotKind::Unknown("Widget".into()));
        assert_eq!(w.pdf_name(), "Widget");
        assert!(!w.is_modelled());
    }

    #[test]
    fn a_new_markup_is_fully_stamped() {
        let origin = Origin::new("Ada", PdfDate::from_unix(1_700_000_000));
        let a = Annotation::square(2, Rect::new(30.0, 40.0, 10.0, 20.0), &origin);
        assert_eq!(
            a.geometry,
            Geometry::Rect(Rect::new(10.0, 20.0, 30.0, 40.0))
        );
        assert_eq!(a.meta.author.as_deref(), Some("Ada"));
        assert_eq!(a.meta.subject.as_deref(), Some("Rectangle"));
        assert_eq!(a.meta.created, Some(origin.when));
        assert_eq!(a.meta.modified, Some(origin.when));
        assert!(a.meta.flags.has(Flags::PRINT));
        assert!(a.is_modelled() && a.is_dirty() && a.baseline().is_none());

        let mut note = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "hello", &origin);
        let popup = Annotation::popup_for(&mut note, Rect::new(20.0, 0.0, 200.0, 100.0), &origin);
        assert_eq!(note.popup.as_ref(), Some(&popup.id));
        assert_eq!(popup.parent.as_ref(), Some(&note.id));
        assert_eq!(popup.meta.author, None);
        assert_eq!(popup.meta.flags, Flags(0));
    }

    #[test]
    fn geometry_bounds_and_mapping() {
        let g = Geometry::Ink(vec![
            vec![Point::new(1.0, 5.0), Point::new(4.0, 2.0)],
            vec![Point::new(-3.0, 0.0)],
        ]);
        assert_eq!(g.bounds(), Rect::new(-3.0, 0.0, 4.0, 5.0));
        let moved = g.map_points(|p| p + pl_geom::Vec2::new(10.0, 10.0));
        assert_eq!(moved.bounds(), Rect::new(7.0, 10.0, 14.0, 15.0));
        assert_eq!(Geometry::Points(vec![]).bounds(), Rect::ZERO);
        assert!(!Geometry::Line {
            a: Point::new(f64::NAN, 0.0),
            b: Point::ZERO
        }
        .is_finite());
        // A mirrored rectangle is normalised.
        let r = Geometry::Rect(Rect::new(0.0, 0.0, 2.0, 1.0)).map_points(|p| Point::new(-p.x, p.y));
        assert_eq!(r, Geometry::Rect(Rect::new(-2.0, 0.0, 0.0, 1.0)));
    }

    #[test]
    fn a_duplicate_is_a_new_unlinked_annotation() {
        let origin = Origin::new("Ada", PdfDate::from_unix(1_700_000_000));
        let later = Origin::new("Bo", PdfDate::from_unix(1_800_000_000));
        let mut a = Annotation::line(0, Point::new(0.0, 0.0), Point::new(5.0, 5.0), &origin);
        a.popup = Some(AnnotId::from("p"));
        let b = a.duplicate(&later);
        assert_ne!(a.id, b.id);
        assert_eq!(b.popup, None);
        assert_eq!(b.meta.author.as_deref(), Some("Bo"));
        assert_eq!(b.geometry, a.geometry);
    }
}
