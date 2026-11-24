//! XFDF (ISO 19444-1): the annotations of a document as XML, out and back in.
//!
//! # What is written
//!
//! One element per annotation under `<annots>`: `square`, `circle`, `line`, `polyline`,
//! `polygon`, `ink`, `highlight`, `underline`, `strikeout`, `squiggly`, `text`, `link`,
//! `freetext` and `stamp`, each with the standard attributes (`page`, `rect`, `fringe`, `name`,
//! `title`, `subject`, `date`, `creationdate`, `flags`, `color`, `interior-color`, `opacity`,
//! `width`, `style`, `dashes`, `intensity`, `head`, `tail`, `start`, `end`, `coords`,
//! `inreplyto`, `replyType`, `state`, `statemodel`, `icon`, `rotation`, `justification`,
//! `callout`, `intent`) and child elements (`contents`, `contents-richtext`,
//! `defaultappearance`, `defaultstyle`, `vertices`, `inklist`/`gesture`, `popup`,
//! `OnActivation`). A popup is nested in the markup it belongs to.
//!
//! Coordinates are page user space, exactly as in the PDF.
//!
//! # What the standard cannot say
//!
//! A few things in the model have no XFDF attribute, or only a lossy one (a colour is
//! `#RRGGBB`). They are added in a namespace of our own (`xmlns:pl`, attributes and one element
//! prefixed `pl:`), which other readers ignore: exact colours, "no colour at all", the exact
//! box of a shape, the blend mode, the open state of a note, the private text-box settings
//! (vertical alignment, line spacing, padding), a label stamp's text and scheme, and the
//! values of the user's list columns, and Bluebeam Revu's custom column values (`/BSIColumnData`,
//! positional, as `pl:bscolumns`: they mean the columns of the document they are read into,
//! which must define the same `/BSIAnnotColumns`). Reading prefers a `pl:` value only where it agrees with
//! the standard attribute beside it, so a file edited by another tool is read as edited.
//!
//! # What is not carried
//!
//! * Layers (`/OC`): an object reference means nothing outside its file. An imported
//!   annotation has no layer.
//! * Picture stamps and annotations this application cannot model (another application's
//!   stamp, a foreign text box that has not been made editable, unknown subtypes): they are
//!   left out and listed in [`XfdfExport::skipped`].
//! * Private application data other than the list columns.

use pl_annot::{
    default_appearance, default_style, default_style_color, parse_default_appearance, rich_text,
    AnnotId, AnnotKind, Annotation, BlendMode, Flags, FontStyle, FreeText, FreeTextIntent,
    Geometry, HAlign, LineEnding, LinkTarget, Meta, Origin, PdfDate, Quad, Rgb, Stamp,
    StampContent, StampScheme, StampSpec, Style, VAlign,
};
use pl_geom::{Point, Rect};
use pl_pdf::{Dictionary, Object};

use crate::column::COLUMNS_KEY;
use crate::xml::{self, Element, XmlError};

/// The XFDF namespace.
pub const XFDF_NS: &str = "http://ns.adobe.com/xfdf/";
/// The namespace of the `pl:` extension attributes.
pub const PL_NS: &str = "urn:plumbline:xfdf:1";

/// Why an annotation was left out of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// A kind this application can only move and delete; its appearance cannot be described.
    NotModelled,
    /// A picture stamp: XFDF would need the picture itself.
    ImageStamp,
    /// A popup whose markup is not in the export or does not refer to it.
    OrphanPopup,
    /// A coordinate is not a number.
    BadGeometry,
    /// A measurement. XFDF has dimension annotations (an intent, leader keys, a `measure`
    /// element), which this export does not write yet; as a plain line or polygon it would
    /// lose its scale and its kind, so it is left out.
    Measurement,
}

/// The result of [`export_xfdf`].
#[derive(Debug, Clone, PartialEq)]
pub struct XfdfExport {
    pub xml: String,
    /// How many annotations were written (popups included).
    pub written: usize,
    pub skipped: Vec<(AnnotId, SkipReason)>,
}

/// Options for [`export_xfdf`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct XfdfOptions {
    /// The PDF the annotations belong to, written as `<f href="..."/>`.
    pub href: Option<String>,
}

/// The result of [`import_xfdf`].
#[derive(Debug, Clone, PartialEq)]
pub struct XfdfImport {
    /// The annotations, in file order, each popup right after its markup. They are new
    /// annotations (no tie to a file); ids are the `name` attributes, generated where missing.
    pub annotations: Vec<Annotation>,
    /// One message per element that was not read, saying which and why.
    pub skipped: Vec<String>,
    /// `<f href>`.
    pub href: Option<String>,
}

/// What makes a file unreadable as XFDF as a whole.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum XfdfError {
    #[error(transparent)]
    Xml(#[from] XmlError),
    #[error("this is not an XFDF file: the root element is <{0}>")]
    NotXfdf(String),
}

// ---------------------------------------------------------------------------------------------
// Small formats

fn num(v: f64) -> String {
    format!("{}", if v == 0.0 { 0.0 } else { v })
}

fn nums(values: &[f64]) -> String {
    values.iter().map(|v| num(*v)).collect::<Vec<_>>().join(",")
}

fn points(points: &[Point]) -> String {
    points
        .iter()
        .map(|p| format!("{},{}", num(p.x), num(p.y)))
        .collect::<Vec<_>>()
        .join(";")
}

/// Numbers separated by commas, semicolons or white space. `None` when any is not a finite
/// number.
fn parse_nums(text: &str) -> Option<Vec<f64>> {
    text.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect()
}

fn parse_points(text: &str) -> Option<Vec<Point>> {
    let v = parse_nums(text)?;
    (v.len() % 2 == 0).then(|| v.chunks(2).map(|c| Point::new(c[0], c[1])).collect())
}

fn hex(c: Rgb) -> String {
    let [r, g, b] = c.to_u8();
    format!("#{r:02X}{g:02X}{b:02X}")
}

fn parse_hex(text: &str) -> Option<Rgb> {
    let v = text.trim().strip_prefix('#')?;
    if !v.is_ascii() {
        return None;
    }
    let d = |s: &str| u8::from_str_radix(s, 16).ok();
    match v.len() {
        6 => Some(Rgb::from_u8(d(&v[0..2])?, d(&v[2..4])?, d(&v[4..6])?)),
        3 => Some(Rgb::from_u8(
            d(&v[0..1])? * 17,
            d(&v[1..2])? * 17,
            d(&v[2..3])? * 17,
        )),
        _ => None,
    }
}

fn exact(c: Rgb) -> String {
    format!("{} {} {}", c.r, c.g, c.b)
}

fn parse_exact(text: &str) -> Option<Rgb> {
    let v: Vec<f32> = text
        .split_whitespace()
        .map(|t| t.parse::<f32>().ok().filter(|v| v.is_finite()))
        .collect::<Option<_>>()?;
    (v.len() == 3).then(|| Rgb::new(v[0], v[1], v[2]))
}

/// True when two colours are the same to eight bits per component, give or take one step (the
/// standard attribute beside an exact value may have been rounded either way).
fn agrees(a: Rgb, b: Rgb) -> bool {
    a.to_u8()
        .iter()
        .zip(b.to_u8())
        .all(|(x, y)| x.abs_diff(y) <= 1)
}

/// Writes a colour as the standard attribute, with the exact value beside it when eight bits
/// per component do not hold it.
fn set_colour(e: &mut Element, attr: &str, exact_attr: &str, c: Rgb) {
    e.set(attr, hex(c));
    let [r, g, b] = c.to_u8();
    if Rgb::from_u8(r, g, b) != c {
        e.set(exact_attr, exact(c));
    }
}

/// The colour `attr` says, refined by `exact_attr` when the two agree to eight bits.
fn get_colour(e: &Element, attr: &str, exact_attr: &str) -> Option<Rgb> {
    let standard = e.attr(attr).and_then(parse_hex)?;
    Some(
        e.attr(exact_attr)
            .and_then(parse_exact)
            .filter(|x| agrees(*x, standard))
            .unwrap_or(standard),
    )
}

const FLAG_NAMES: [(u32, &str); 10] = [
    (Flags::INVISIBLE, "invisible"),
    (Flags::HIDDEN, "hidden"),
    (Flags::PRINT, "print"),
    (Flags::NO_ZOOM, "nozoom"),
    (Flags::NO_ROTATE, "norotate"),
    (Flags::NO_VIEW, "noview"),
    (Flags::READ_ONLY, "readonly"),
    (Flags::LOCKED, "locked"),
    (1 << 8, "togglenoview"),
    (Flags::LOCKED_CONTENTS, "lockedcontents"),
];

fn element_name(kind: &AnnotKind) -> Option<&'static str> {
    Some(match kind {
        AnnotKind::Square => "square",
        AnnotKind::Circle => "circle",
        AnnotKind::Line => "line",
        AnnotKind::PolyLine => "polyline",
        AnnotKind::Polygon => "polygon",
        AnnotKind::Ink => "ink",
        AnnotKind::Highlight => "highlight",
        AnnotKind::Underline => "underline",
        AnnotKind::StrikeOut => "strikeout",
        AnnotKind::Squiggly => "squiggly",
        AnnotKind::Text => "text",
        AnnotKind::Link => "link",
        AnnotKind::Popup => "popup",
        AnnotKind::FreeText => "freetext",
        AnnotKind::Stamp => "stamp",
        AnnotKind::Unknown(_) => return None,
    })
}

fn kind_of(element: &str) -> Option<AnnotKind> {
    [
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
        AnnotKind::FreeText,
        AnnotKind::Stamp,
    ]
    .into_iter()
    .find(|k| element_name(k) == Some(element))
}

fn font_name(f: FontStyle) -> &'static str {
    match f {
        FontStyle::Regular => "Regular",
        FontStyle::Bold => "Bold",
        FontStyle::Italic => "Italic",
        FontStyle::BoldItalic => "BoldItalic",
    }
}

fn v_align_name(v: VAlign) -> &'static str {
    match v {
        VAlign::Top => "Top",
        VAlign::Middle => "Middle",
        VAlign::Bottom => "Bottom",
    }
}

/// A ring as the eight numbers of `/QuadPoints` (top-left, top-right, bottom-left,
/// bottom-right), the order pl-annot writes to the PDF.
fn quad_to_file(quad: &Quad) -> [f64; 8] {
    let [bl, br, tr, tl] = *quad;
    [tl.x, tl.y, tr.x, tr.y, bl.x, bl.y, br.x, br.y]
}

/// The reverse, with pl-annot's rule for producers that write a ring: if the usual reading
/// gives a self-crossing quad, the points are taken as they are.
fn quad_from_file(q: &[f64; 8]) -> Quad {
    let p = |i: usize| Point::new(q[2 * i], q[2 * i + 1]);
    let side = |a: Point, b: Point, c: Point| (b - a).cross(c - a);
    let crosses = |a: Point, b: Point, c: Point, d: Point| {
        side(a, b, c) * side(a, b, d) < 0.0 && side(c, d, a) * side(c, d, b) < 0.0
    };
    let ring = [p(2), p(3), p(1), p(0)];
    if crosses(ring[0], ring[1], ring[2], ring[3]) || crosses(ring[1], ring[2], ring[3], ring[0]) {
        [p(0), p(1), p(2), p(3)]
    } else {
        ring
    }
}

/// `/Rect` shrunk by the four `fringe` differences (left, bottom, right, top: the order of a
/// rectangle), as pl-annot reads `/RD`.
fn shrink(rect: Rect, d: &[f64]) -> Rect {
    if d.len() == 4
        && d.iter().all(|v| *v >= 0.0)
        && d[0] + d[2] < rect.width()
        && d[1] + d[3] < rect.height()
    {
        Rect::new(
            rect.x0 + d[0],
            rect.y0 + d[1],
            rect.x1 - d[2],
            rect.y1 - d[3],
        )
    } else {
        rect
    }
}

// ---------------------------------------------------------------------------------------------
// Export

fn columns_element(a: &Annotation) -> Option<Element> {
    let columns = a
        .app_data
        .as_ref()?
        .get(COLUMNS_KEY.as_bytes())
        .ok()?
        .as_dict()
        .ok()?;
    let mut out = Element::new("pl:columns");
    for (key, value) in columns.iter() {
        let (t, text) = match value {
            Object::String(bytes, _) => ("s", pl_annot::decode_text(bytes)),
            Object::Name(bytes) => ("n", String::from_utf8_lossy(bytes).into_owned()),
            Object::Integer(i) => ("i", i.to_string()),
            Object::Real(r) => ("r", r.to_string()),
            Object::Boolean(b) => ("b", b.to_string()),
            _ => continue,
        };
        let mut v = Element::new("pl:v")
            .with("id", String::from_utf8_lossy(key))
            .with("t", t);
        if !text.is_empty() {
            v.push_text(&text);
        }
        out.push(v);
    }
    (!out.children.is_empty()).then_some(out)
}

/// Bluebeam Revu's column values of `a` (`/BSIColumnData`), one `pl:v` per position; a
/// position that is not text is written empty.
fn bluebeam_columns_element(a: &Annotation) -> Option<Element> {
    let data = a.column_data.as_ref()?;
    let mut out = Element::new("pl:bscolumns");
    for value in data {
        let mut v = Element::new("pl:v");
        if let Object::String(bytes, _) = value {
            let text = pl_annot::decode_text(bytes);
            if !text.is_empty() {
                v.push_text(&text);
            }
        }
        out.push(v);
    }
    Some(out)
}

fn link_element(target: &LinkTarget) -> Element {
    let mut activation = Element::new("OnActivation");
    match target {
        LinkTarget::Uri(uri) => {
            let mut action = Element::new("Action").with("Trigger", "U");
            action.push(Element::new("URI").with("Name", uri.as_str()));
            activation.push(action);
        }
        LinkTarget::Page { page, at } => {
            let mut dest = Element::new("Dest");
            dest.push(match at {
                Some(p) => Element::new("XYZ")
                    .with("Page", page.to_string())
                    .with("Left", num(p.x))
                    .with("Top", num(p.y)),
                None => Element::new("Fit").with("Page", page.to_string()),
            });
            let mut go = Element::new("GoTo");
            go.push(dest);
            let mut action = Element::new("Action").with("Trigger", "U");
            action.push(go);
            activation.push(action);
        }
        // Kept in the original PDF only; the element marks that there was a target.
        LinkTarget::Other => {}
    }
    activation
}

fn export_one(a: &Annotation, popup: Option<&Annotation>) -> Result<Element, SkipReason> {
    let Some(name) = element_name(&a.kind) else {
        return Err(SkipReason::NotModelled);
    };
    if !a.is_modelled() {
        return Err(SkipReason::NotModelled);
    }
    if a.measure.is_some() {
        return Err(SkipReason::Measurement);
    }
    if matches!(&a.stamp, Some(s) if matches!(s.content, StampContent::Image(_))) {
        return Err(SkipReason::ImageStamp);
    }
    if !a.geometry.is_finite() {
        return Err(SkipReason::BadGeometry);
    }
    let placement = a.placement();
    let rect = placement.rect;
    if ![rect.x0, rect.y0, rect.x1, rect.y1]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(SkipReason::BadGeometry);
    }
    let s = &a.style;
    let defaults = Style::default_for(&a.kind);
    let free_text = a.free_text.as_ref();

    let mut e = Element::new(name);
    e.set("page", a.page.to_string());
    e.set("rect", nums(&[rect.x0, rect.y0, rect.x1, rect.y1]));
    if let Some(d) = placement.rect_differences {
        e.set("fringe", nums(&d));
    }
    if let Geometry::Rect(frame) = &a.geometry {
        let standard = shrink(rect, &placement.rect_differences.unwrap_or_default());
        if standard != *frame {
            e.set("pl:box", nums(&[frame.x0, frame.y0, frame.x1, frame.y1]));
        }
    }
    e.set("name", a.id.as_str());
    let m = &a.meta;
    if let Some(v) = &m.author {
        e.set("title", v.as_str());
    }
    if let Some(v) = &m.subject {
        e.set("subject", v.as_str());
    }
    if let Some(d) = &m.modified {
        e.set("date", d.to_pdf_string());
    }
    if let Some(d) = &m.created {
        e.set("creationdate", d.to_pdf_string());
    }
    let named: Vec<&str> = FLAG_NAMES
        .iter()
        .filter(|(bit, _)| m.flags.has(*bit))
        .map(|(_, n)| *n)
        .collect();
    if !named.is_empty() {
        e.set("flags", named.join(","));
    }
    let known: u32 = FLAG_NAMES.iter().map(|(bit, _)| *bit).sum();
    if m.flags.0 & !known != 0 {
        e.set("pl:flags", m.flags.0.to_string());
    }

    // Colours. For a text box `color` is the background and the border is in the default
    // appearance; for everything else `color` is the line and `interior-color` the fill.
    if free_text.is_some() {
        if let Some(c) = s.fill {
            set_colour(&mut e, "color", "pl:color", c);
        }
        match s.stroke {
            Some(c) => e.set("pl:border", exact(c)),
            None => e.set("pl:border", "none"),
        }
    } else {
        match s.stroke {
            Some(c) => set_colour(&mut e, "color", "pl:color", c),
            None if a.kind.implied_color().is_some() => e.set("pl:color", "none"),
            None => {}
        }
        if let Some(c) = s.fill {
            set_colour(&mut e, "interior-color", "pl:interior-color", c);
        }
    }
    if s.opacity != 1.0 {
        e.set("opacity", num(s.opacity));
    }
    let has_border = matches!(
        a.kind,
        AnnotKind::Square
            | AnnotKind::Circle
            | AnnotKind::Polygon
            | AnnotKind::Line
            | AnnotKind::PolyLine
            | AnnotKind::Ink
            | AnnotKind::Link
            | AnnotKind::FreeText
    );
    if free_text.is_some() && s.stroke.is_none() {
        // No border is a width of zero to every reader; the width the box would have with
        // one is ours to remember.
        e.set("width", "0");
        if s.width != 0.0 {
            e.set("pl:width", num(s.width));
        }
    } else if has_border || s.width != defaults.width {
        e.set("width", num(s.width));
    }
    match (&s.dash, s.cloud) {
        (_, Some(_)) => e.set("style", "cloudy"),
        (Some(_), None) => e.set("style", "dash"),
        (None, None) if has_border => e.set("style", "solid"),
        _ => {}
    }
    if let Some(dash) = &s.dash {
        e.set("dashes", nums(dash));
    }
    if let Some(intensity) = s.cloud {
        e.set("intensity", num(intensity));
    }
    if free_text.is_some() {
        if s.line_end != LineEnding::None {
            e.set("head", s.line_end.pdf_name());
        }
    } else {
        let lined = matches!(a.kind, AnnotKind::Line | AnnotKind::PolyLine);
        if lined || s.line_start != LineEnding::None {
            e.set("head", s.line_start.pdf_name());
        }
        if lined || s.line_end != LineEnding::None {
            e.set("tail", s.line_end.pdf_name());
        }
    }
    if s.blend != a.kind.default_blend() {
        e.set("pl:blend", s.blend.pdf_name());
    }

    if let Some(v) = &m.reply_to {
        e.set("inreplyto", v.as_str());
    }
    if let Some(v) = &m.reply_type {
        e.set(
            "replyType",
            match v.as_str() {
                "R" => "reply",
                "Group" => "group",
                other => other,
            },
        );
    }
    if let Some(v) = &m.state {
        e.set("state", v.as_str());
    }
    if let Some(v) = &m.state_model {
        e.set("statemodel", v.as_str());
    }
    if let Some(v) = &a.icon {
        e.set("icon", v.as_str());
    }
    if a.kind == AnnotKind::Popup {
        e.set("open", if a.open { "yes" } else { "no" });
    } else if a.open {
        e.set("pl:open", "yes");
    }
    if a.rotation() != 0 {
        e.set("rotation", a.rotation().to_string());
    }

    // Geometry.
    match &a.geometry {
        Geometry::Rect(_) => {}
        Geometry::Line { a: start, b: end } => {
            e.set("start", nums(&[start.x, start.y]));
            e.set("end", nums(&[end.x, end.y]));
        }
        Geometry::Quads(quads) => {
            let all: Vec<f64> = quads.iter().flat_map(quad_to_file).collect();
            e.set("coords", nums(&all));
        }
        Geometry::Points(_) | Geometry::Ink(_) => {}
    }

    // Text box.
    if let Some(t) = free_text {
        e.set(
            "justification",
            match t.align {
                HAlign::Left => "left",
                HAlign::Centre => "centered",
                HAlign::Right => "right",
            },
        );
        if t.intent != FreeTextIntent::Box {
            e.set("intent", t.intent.pdf_name());
        }
        if !t.callout.is_empty() {
            let flat: Vec<f64> = t.callout.iter().flat_map(|p| [p.x, p.y]).collect();
            e.set("callout", nums(&flat));
        }
        e.set("pl:font", font_name(t.font));
        e.set("pl:size", num(t.size));
        e.set("pl:text-color", exact(t.color));
        e.set("pl:v-align", v_align_name(t.v_align));
        e.set("pl:line-spacing", num(t.line_spacing));
        e.set("pl:padding", num(t.padding));
    }
    if let Some(Stamp {
        content: StampContent::Label(spec),
        ..
    }) = &a.stamp
    {
        e.set("pl:stamp", spec.name.as_str());
        e.set("pl:label", spec.label.as_str());
        if let Some(detail) = &spec.detail {
            e.set("pl:detail", detail.as_str());
        }
        e.set("pl:scheme", spec.scheme.name());
    }

    // Children.
    if let Some(t) = free_text {
        if let Some(text) = &m.contents {
            let body = rich_text(text);
            let body = match body.find("?>") {
                Some(i) if body.starts_with("<?xml") => &body[i + 2..],
                _ => body.as_str(),
            };
            if let Ok(parsed) = xml::parse(body) {
                let mut rich = Element::new("contents-richtext");
                rich.push(parsed);
                e.push(rich);
            }
        }
        let mut da = Element::new("defaultappearance");
        da.push_text(&default_appearance(s.stroke, t.color, t.font, t.size));
        e.push(da);
        let mut ds = Element::new("defaultstyle");
        ds.push_text(&default_style(t.color, t.font, t.size, t.align));
        e.push(ds);
    }
    if let Some(text) = &m.contents {
        let mut c = Element::new("contents");
        if !text.is_empty() {
            c.push_text(text);
        }
        e.push(c);
    }
    match &a.geometry {
        Geometry::Points(pts) => {
            let mut v = Element::new("vertices");
            v.push_text(&points(pts));
            e.push(v);
        }
        Geometry::Ink(strokes) => {
            let mut list = Element::new("inklist");
            for stroke in strokes {
                let mut g = Element::new("gesture");
                if !stroke.is_empty() {
                    g.push_text(&points(stroke));
                }
                list.push(g);
            }
            e.push(list);
        }
        _ => {}
    }
    if let Some(target) = &a.link {
        e.push(link_element(target));
    }
    if let Some(columns) = columns_element(a) {
        e.push(columns);
    }
    if let Some(columns) = bluebeam_columns_element(a) {
        e.push(columns);
    }
    if let Some(p) = popup {
        if let Ok(child) = export_one(p, None) {
            e.push(child);
        }
    }
    Ok(e)
}

/// Writes the annotations as an XFDF document (UTF-8 text).
///
/// Pass every annotation of the document, in stacking order: a popup is written inside the
/// markup that refers to it, and a reply names what it answers by id.
pub fn export_xfdf<'a, I>(annotations: I, options: &XfdfOptions) -> XfdfExport
where
    I: IntoIterator<Item = &'a Annotation>,
{
    let all: Vec<&Annotation> = annotations.into_iter().collect();
    let by_id: std::collections::HashMap<&AnnotId, &Annotation> =
        all.iter().map(|a| (&a.id, *a)).collect();
    let mut annots = Element::new("annots");
    let mut skipped = Vec::new();
    let mut written = 0;
    for a in &all {
        if a.kind == AnnotKind::Popup {
            // Written with its markup, when the two refer to each other.
            let owned = a
                .parent
                .as_ref()
                .and_then(|p| by_id.get(p))
                .is_some_and(|p| p.popup.as_ref() == Some(&a.id) && p.kind != AnnotKind::Popup);
            if !owned {
                skipped.push((a.id.clone(), SkipReason::OrphanPopup));
            }
            continue;
        }
        let popup = a
            .popup
            .as_ref()
            .and_then(|id| by_id.get(id))
            .filter(|p| p.kind == AnnotKind::Popup && p.parent.as_ref() == Some(&a.id))
            .copied();
        match export_one(a, popup) {
            Ok(e) => {
                written += 1 + e.elements().filter(|c| c.name == "popup").count();
                annots.push_text("\n");
                annots.push(e);
            }
            Err(reason) => {
                skipped.push((a.id.clone(), reason));
                if let Some(p) = popup {
                    skipped.push((p.id.clone(), SkipReason::OrphanPopup));
                }
            }
        }
    }
    if !annots.children.is_empty() {
        annots.push_text("\n");
    }
    let mut root = Element::new("xfdf")
        .with("xmlns", XFDF_NS)
        .with("xmlns:pl", PL_NS)
        .with("xml:space", "preserve");
    if let Some(href) = &options.href {
        root.push_text("\n");
        root.push(Element::new("f").with("href", href.as_str()));
    }
    root.push_text("\n");
    root.push(annots);
    root.push_text("\n");
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    root.write(&mut xml);
    xml.push('\n');
    XfdfExport {
        xml,
        written,
        skipped,
    }
}

// ---------------------------------------------------------------------------------------------
// Import

fn child<'e>(e: &'e Element, local: &str) -> Option<&'e Element> {
    e.elements().find(|c| c.local_name() == local)
}

/// The plain text of a rich-text body: one line per paragraph.
fn rich_plain(e: &Element) -> String {
    fn all_text(e: &Element, out: &mut String) {
        for c in &e.children {
            match c {
                xml::Node::Text(t) => out.push_str(t),
                xml::Node::Element(k) => all_text(k, out),
            }
        }
    }
    fn paragraphs(e: &Element, out: &mut Vec<String>) {
        for c in e.elements() {
            if c.local_name() == "p" {
                let mut text = String::new();
                all_text(c, &mut text);
                out.push(text);
            } else {
                paragraphs(c, out);
            }
        }
    }
    let mut lines = Vec::new();
    paragraphs(e, &mut lines);
    if lines.is_empty() {
        let mut text = String::new();
        all_text(e, &mut text);
        return text.trim().to_string();
    }
    lines.join("\n")
}

fn read_columns(e: &Element) -> Option<Dictionary> {
    let columns = e.child("pl:columns")?;
    let mut dict = Dictionary::new();
    for v in columns.elements().filter(|v| v.name == "pl:v") {
        let Some(id) = v.attr("id").filter(|id| !id.is_empty()) else {
            continue;
        };
        let text = v.text();
        let object = match v.attr("t") {
            Some("s") => pl_annot::encode_text(&text),
            Some("n") => Object::Name(text.into_bytes()),
            Some("i") => match text.parse() {
                Ok(i) => Object::Integer(i),
                Err(_) => continue,
            },
            Some("r") => match text.parse::<f32>() {
                Ok(r) if r.is_finite() => Object::Real(r),
                _ => continue,
            },
            Some("b") => Object::Boolean(text == "true"),
            _ => continue,
        };
        dict.set(id.as_bytes().to_vec(), object);
    }
    (!dict.is_empty()).then_some(dict)
}

fn read_bluebeam_columns(e: &Element) -> Option<Vec<Object>> {
    let columns = e.child("pl:bscolumns")?;
    Some(
        columns
            .elements()
            .filter(|v| v.name == "pl:v")
            .map(|v| crate::bluebeam::text_object(&v.text()))
            .collect(),
    )
}

fn read_link(e: &Element) -> Option<LinkTarget> {
    let activation = e
        .elements()
        .find(|c| matches!(c.local_name(), "OnActivation" | "Action" | "Dest"))?;
    // The element itself may be the action or the destination.
    let mut holder = Element::new("holder");
    holder.push(activation.clone());
    if let Some(uri) = holder.descendant("URI").and_then(|u| u.attr("Name")) {
        return Some(LinkTarget::Uri(uri.to_string()));
    }
    for fit in [
        "XYZ", "Fit", "FitH", "FitV", "FitR", "FitB", "FitBH", "FitBV",
    ] {
        let Some(d) = holder.descendant(fit) else {
            continue;
        };
        let Some(page) = d.attr("Page").and_then(|p| p.trim().parse::<usize>().ok()) else {
            continue;
        };
        let coordinate = |name: &str| {
            d.attr(name)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite())
        };
        let at = match (fit, coordinate("Left"), coordinate("Top")) {
            ("XYZ", Some(x), Some(y)) => Some(Point::new(x, y)),
            _ => None,
        };
        return Some(LinkTarget::Page { page, at });
    }
    Some(LinkTarget::Other)
}

/// The furthest from the origin an imported coordinate may be, in page units. The largest
/// page a PDF allows is 14,400 units a side; this leaves room for a page box that does not
/// start at the origin and keeps out the numbers of a broken or hostile file, which would
/// otherwise be drawn (a cloud border of a million units is a great many scallops).
pub const MAX_COORDINATE: f64 = 100_000.0;

fn within_reach(geometry: &Geometry) -> bool {
    let b = geometry.bounds();
    [b.x0, b.y0, b.x1, b.y1]
        .iter()
        .all(|v| v.abs() <= MAX_COORDINATE)
}

fn import_one(
    e: &Element,
    kind: AnnotKind,
    default_page: Option<usize>,
) -> Result<Annotation, String> {
    let page = match e.attr("page") {
        Some(p) => p
            .trim()
            .parse::<usize>()
            .map_err(|_| format!("page \"{p}\" is not a page number"))?,
        None => default_page.ok_or("it has no page")?,
    };
    let rect = e
        .attr("rect")
        .and_then(parse_nums)
        .filter(|v| v.len() == 4)
        .map(|v| Rect::new(v[0], v[1], v[2], v[3]).abs());
    let list = |name: &str| e.attr(name).and_then(parse_nums);

    let geometry = match &kind {
        AnnotKind::Line => {
            let (Some(a), Some(b)) = (list("start"), list("end")) else {
                return Err("it has no start and end".into());
            };
            if a.len() != 2 || b.len() != 2 {
                return Err("its start or end is not a point".into());
            }
            Geometry::Line {
                a: Point::new(a[0], a[1]),
                b: Point::new(b[0], b[1]),
            }
        }
        AnnotKind::PolyLine | AnnotKind::Polygon => {
            let pts = child(e, "vertices")
                .and_then(|v| parse_points(&v.text()))
                .filter(|p| p.len() >= 2)
                .ok_or("it has no usable vertices")?;
            Geometry::Points(pts)
        }
        AnnotKind::Ink => {
            let list = child(e, "inklist").ok_or("it has no inklist")?;
            let strokes = list
                .elements()
                .filter(|g| g.local_name() == "gesture")
                .map(|g| parse_points(&g.text()))
                .collect::<Option<Vec<_>>>()
                .ok_or("a gesture is not a list of points")?;
            Geometry::Ink(strokes)
        }
        k if k.is_text_markup() => {
            let v = list("coords")
                .filter(|v| v.len() >= 8)
                .ok_or("it has no coords")?;
            Geometry::Quads(v.as_chunks::<8>().0.iter().map(quad_from_file).collect())
        }
        _ => {
            let rect = rect.ok_or("it has no rect")?;
            let standard = shrink(rect, &list("fringe").unwrap_or_default());
            let close = |a: Rect, b: Rect| {
                [a.x0 - b.x0, a.y0 - b.y0, a.x1 - b.x1, a.y1 - b.y1]
                    .iter()
                    .all(|d| d.abs() < 0.01)
            };
            let ours = list("pl:box")
                .filter(|v| v.len() == 4)
                .map(|v| Rect::new(v[0], v[1], v[2], v[3]).abs())
                .filter(|b| close(*b, standard));
            Geometry::Rect(ours.unwrap_or(standard))
        }
    };

    if !geometry.is_finite() || !within_reach(&geometry) {
        return Err(format!(
            "its coordinates are not on any page (beyond {MAX_COORDINATE} units)"
        ));
    }

    // The origin only fills fields that are all replaced below.
    let origin = Origin::new("", PdfDate::from_unix(0));
    let mut a = Annotation::new(kind.clone(), page, geometry, &origin);
    a.id = match e.attr("name").filter(|n| !n.is_empty()) {
        Some(n) => AnnotId::from(n),
        None => AnnotId::generate(),
    };
    let text = |name: &str| e.attr(name).map(str::to_string);
    let mut flags = 0u32;
    for word in e.attr("flags").unwrap_or("").split([',', ' ']) {
        let word = word.trim().to_ascii_lowercase();
        if let Some((bit, _)) = FLAG_NAMES.iter().find(|(_, n)| *n == word) {
            flags |= bit;
        }
    }
    let known: u32 = FLAG_NAMES.iter().map(|(bit, _)| *bit).sum();
    if let Some(extra) = e.attr("pl:flags").and_then(|f| f.parse::<u32>().ok()) {
        flags |= extra & !known;
    }
    let contents = match (child(e, "contents"), child(e, "contents-richtext")) {
        (Some(c), _) => Some(c.text()),
        (None, Some(rich)) => Some(rich_plain(rich)),
        (None, None) => None,
    };
    a.meta = Meta {
        author: text("title"),
        subject: text("subject"),
        contents,
        created: e.attr("creationdate").and_then(PdfDate::parse),
        modified: e.attr("date").and_then(PdfDate::parse),
        flags: Flags(flags),
        layer: None,
        reply_to: e
            .attr("inreplyto")
            .filter(|v| !v.is_empty())
            .map(AnnotId::from),
        reply_type: e.attr("replyType").map(|v| match v {
            "reply" => "R".to_string(),
            "group" => "Group".to_string(),
            other => other.to_string(),
        }),
        state: text("state"),
        state_model: text("statemodel"),
    };
    a.icon = text("icon");
    a.open =
        matches!(e.attr("open"), Some("yes" | "true" | "1")) || e.attr("pl:open") == Some("yes");

    // Style.
    let s = &mut a.style;
    let number = |name: &str| {
        e.attr(name)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
    };
    if kind == AnnotKind::FreeText {
        s.fill = get_colour(e, "color", "pl:color");
    } else {
        s.stroke = if e.attr("pl:color") == Some("none") && e.attr("color").is_none() {
            None
        } else {
            get_colour(e, "color", "pl:color").or(kind.implied_color())
        };
        s.fill = get_colour(e, "interior-color", "pl:interior-color");
    }
    if let Some(o) = number("opacity") {
        s.opacity = o.clamp(0.0, 1.0);
    }
    if let Some(w) = number("width") {
        s.width = w.max(0.0);
    }
    let border = e.attr("style").unwrap_or("");
    s.dash = match (border, list("dashes")) {
        ("dash" | "cloudy", Some(d)) => Some(d),
        ("dash", None) => Some(vec![3.0]),
        _ => None,
    };
    s.cloud = (border == "cloudy").then(|| number("intensity").unwrap_or(1.0));
    let ending = |name: &str| {
        e.attr(name)
            .and_then(LineEnding::from_pdf_name)
            .unwrap_or(LineEnding::None)
    };
    if kind == AnnotKind::FreeText {
        s.line_end = ending("head");
    } else {
        s.line_start = ending("head");
        s.line_end = ending("tail");
    }
    s.blend = match e.attr("pl:blend") {
        Some("Multiply") => BlendMode::Multiply,
        Some("Normal") => BlendMode::Normal,
        _ => kind.default_blend(),
    };
    let rotation = e
        .attr("rotation")
        .and_then(|r| r.trim().parse::<i64>().ok())
        .map_or(0, pl_annot::normalise_rotation);

    match kind {
        AnnotKind::Link => a.link = read_link(e),
        AnnotKind::FreeText => {
            let mut t = FreeText::default();
            let da = child(e, "defaultappearance")
                .map(|d| parse_default_appearance(&d.text()))
                .unwrap_or_default();
            let ds_colour = child(e, "defaultstyle").and_then(|d| default_style_color(&d.text()));
            if let Some(font) = da.font {
                t.font = font;
            }
            if let Some(size) = da.size {
                t.size = size;
            }
            // The standard values, read the way Acrobat means them (and pl-annot reads a
            // FreeText of another application): the text colour is the one in the default
            // style; the colour of the default appearance is the border's, the stroking one
            // when there is one and the non-stroking one otherwise; no width, no border.
            if let Some(c) = ds_colour.or(da.fill) {
                t.color = c;
            }
            a.style.stroke =
                (a.style.width > 0.0).then(|| da.stroke.or(da.fill).unwrap_or(Rgb::BLACK));
            // Our own exact values, where they agree with the standard ones beside them.
            match e.attr("pl:border") {
                Some("none") if a.style.stroke.is_none() => {
                    if let Some(w) = number("pl:width").filter(|w| *w >= 0.0) {
                        a.style.width = w;
                    }
                }
                Some(exact) => {
                    if let (Some(x), Some(std)) = (parse_exact(exact), a.style.stroke) {
                        if agrees(x, std) {
                            a.style.stroke = Some(x);
                        }
                    }
                }
                None => {}
            }
            if let Some(x) = e.attr("pl:text-color").and_then(parse_exact) {
                if agrees(x, t.color) {
                    t.color = x;
                }
            }
            if let Some(f) = FontStyle::ALL
                .into_iter()
                .find(|f| Some(font_name(*f)) == e.attr("pl:font"))
            {
                if da.font.is_none_or(|d| d == f) {
                    t.font = f;
                }
            }
            if let Some(size) = number("pl:size").filter(|x| *x > 0.0) {
                if da.size.is_none_or(|d| (d - size).abs() < 0.01) {
                    t.size = size;
                }
            }
            t.align = match e.attr("justification") {
                Some("centered" | "center") => HAlign::Centre,
                Some("right") => HAlign::Right,
                _ => HAlign::Left,
            };
            if let Some(v) = [VAlign::Top, VAlign::Middle, VAlign::Bottom]
                .into_iter()
                .find(|v| Some(v_align_name(*v)) == e.attr("pl:v-align"))
            {
                t.v_align = v;
            }
            if let Some(x) = number("pl:line-spacing").filter(|x| *x > 0.0) {
                t.line_spacing = x;
            }
            if let Some(x) = number("pl:padding").filter(|x| *x >= 0.0) {
                t.padding = x;
            }
            t.callout = e.attr("callout").and_then(parse_points).unwrap_or_default();
            if t.callout
                .iter()
                .any(|p| !(p.x.abs() <= MAX_COORDINATE && p.y.abs() <= MAX_COORDINATE))
            {
                return Err(format!(
                    "its leader is not on any page (beyond {MAX_COORDINATE} units)"
                ));
            }
            t.intent = match e.attr("intent").or(e.attr("IT")) {
                Some(name) => FreeTextIntent::from_pdf_name(name),
                None if !t.callout.is_empty() => FreeTextIntent::Callout,
                None => FreeTextIntent::Box,
            };
            t.rotation = rotation;
            a.free_text = Some(t);
        }
        AnnotKind::Stamp => {
            let label = e
                .attr("pl:label")
                .ok_or("it is a stamp from another application; its picture is not read")?;
            let spec = StampSpec {
                name: e
                    .attr("icon")
                    .or(e.attr("pl:stamp"))
                    .unwrap_or("Draft")
                    .to_string(),
                label: label.to_string(),
                detail: text("pl:detail"),
                scheme: e
                    .attr("pl:scheme")
                    .and_then(StampScheme::from_name)
                    .unwrap_or(StampScheme::Slate),
            };
            a.stamp = Some(Stamp {
                content: StampContent::Label(spec),
                rotation,
            });
        }
        _ => {}
    }
    a.column_data = read_bluebeam_columns(e);
    a.app_data = read_columns(e).map(|columns| {
        let mut data = Dictionary::new();
        data.set(COLUMNS_KEY, Object::Dictionary(columns));
        data
    });
    Ok(a)
}

/// Reads an XFDF document.
///
/// Every annotation element of a kind this application models becomes an [`Annotation`];
/// anything else (file attachments, sounds, carets, another application's stamps, elements
/// with missing or unreadable geometry) is listed in [`XfdfImport::skipped`] and does not stop
/// the import. Only a file that is not well-formed XML, or not XFDF at all, is an error.
///
/// The page numbers are those of the file; the caller checks them against its document.
pub fn import_xfdf(text: &str) -> Result<XfdfImport, XfdfError> {
    let root = xml::parse(text)?;
    if root.local_name() != "xfdf" {
        return Err(XfdfError::NotXfdf(root.name.clone()));
    }
    let mut out = XfdfImport {
        annotations: Vec::new(),
        skipped: Vec::new(),
        href: child(&root, "f").and_then(|f| f.attr("href").map(str::to_string)),
    };
    let Some(annots) = child(&root, "annots") else {
        return Ok(out);
    };
    for e in annots.elements() {
        let label = match e.attr("name") {
            Some(n) => format!("<{}> \"{n}\"", e.name),
            None => format!("<{}>", e.name),
        };
        let kind = match kind_of(e.local_name()) {
            Some(AnnotKind::Popup) | None => {
                out.skipped
                    .push(format!("{label}: this kind of annotation is not read"));
                continue;
            }
            Some(k) => k,
        };
        let mut a = match import_one(e, kind, None) {
            Ok(a) => a,
            Err(why) => {
                out.skipped.push(format!("{label}: {why}"));
                continue;
            }
        };
        let popup =
            child(e, "popup").and_then(|p| match import_one(p, AnnotKind::Popup, Some(a.page)) {
                Ok(popup) => Some(popup),
                Err(why) => {
                    out.skipped.push(format!("popup of {label}: {why}"));
                    None
                }
            });
        match popup {
            Some(mut p) => {
                p.parent = Some(a.id.clone());
                a.popup = Some(p.id.clone());
                out.annotations.push(a);
                out.annotations.push(p);
            }
            None => out.annotations.push(a),
        }
    }
    Ok(out)
}
