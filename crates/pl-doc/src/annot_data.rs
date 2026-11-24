//! Annotations as plain serialisable data: what the journal, the clipboard and the command
//! file carry.
//!
//! [`AnnotData`] holds every public field of a [`pl_annot::Annotation`] and nothing of its tie
//! to a file (the baseline). Numbers are stored as `f64` and written as JSON with exact
//! round-tripping, so `AnnotData::from(&a).to_annotation()` equals `a.detached()`.

use pl_annot::{
    AnnotId, AnnotKind, Annotation, BlendMode, Flags, FontStyle, FreeText, FreeTextIntent,
    Geometry, HAlign, LineEnding, LinkTarget, Meta, Origin, PdfDate, Rgb, Stamp, StampContent,
    StampScheme, StampSpec, Style, VAlign,
};
use pl_geom::{Point, Rect};
use pl_pdf::{Dictionary, Object, StringFormat};
use serde::{Deserialize, Serialize};

use crate::images::ImageData;
use crate::measure_data::MeasurementData;

/// A PDF object as plain data (the private application dictionary of an annotation).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PdfValue {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str { bytes: Vec<u8>, hex: bool },
    Array(Vec<PdfValue>),
    Dict(Vec<(Vec<u8>, PdfValue)>),
    Ref(u32, u16),
}

impl PdfValue {
    pub fn from_object(object: &Object) -> PdfValue {
        match object {
            Object::Null => PdfValue::Null,
            Object::Boolean(b) => PdfValue::Bool(*b),
            Object::Integer(i) => PdfValue::Int(*i),
            Object::Real(r) => PdfValue::Real(f64::from(*r)),
            Object::Name(n) => PdfValue::Name(n.clone()),
            Object::String(bytes, format) => PdfValue::Str {
                bytes: bytes.clone(),
                hex: matches!(format, StringFormat::Hexadecimal),
            },
            Object::Array(items) => PdfValue::Array(items.iter().map(Self::from_object).collect()),
            Object::Dictionary(d) => PdfValue::from_dict(d),
            Object::Reference(id) => PdfValue::Ref(id.0, id.1),
            // A stream cannot sit inside a dictionary; only its dictionary can be kept.
            Object::Stream(s) => PdfValue::from_dict(&s.dict),
        }
    }

    pub fn from_dict(dict: &Dictionary) -> PdfValue {
        PdfValue::Dict(
            dict.iter()
                .map(|(k, v)| (k.clone(), PdfValue::from_object(v)))
                .collect(),
        )
    }

    pub fn to_object(&self) -> Object {
        match self {
            PdfValue::Null => Object::Null,
            PdfValue::Bool(b) => Object::Boolean(*b),
            PdfValue::Int(i) => Object::Integer(*i),
            PdfValue::Real(r) => Object::Real(*r as f32),
            PdfValue::Name(n) => Object::Name(n.clone()),
            PdfValue::Str { bytes, hex } => Object::String(
                bytes.clone(),
                if *hex {
                    StringFormat::Hexadecimal
                } else {
                    StringFormat::Literal
                },
            ),
            PdfValue::Array(items) => Object::Array(items.iter().map(Self::to_object).collect()),
            PdfValue::Dict(_) => Object::Dictionary(self.to_dict()),
            PdfValue::Ref(n, g) => Object::Reference((*n, *g)),
        }
    }

    /// The value as a dictionary; anything but `Dict` gives an empty one.
    pub fn to_dict(&self) -> Dictionary {
        let mut dict = Dictionary::new();
        if let PdfValue::Dict(entries) = self {
            for (k, v) in entries {
                dict.set(k.clone(), v.to_object());
            }
        }
        dict
    }
}

/// [`Geometry`] as plain numbers. Rectangles are `[x0, y0, x1, y1]`, points `[x, y]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GeometryData {
    Rect([f64; 4]),
    Line([[f64; 2]; 2]),
    Points(Vec<[f64; 2]>),
    Ink(Vec<Vec<[f64; 2]>>),
    Quads(Vec<[[f64; 2]; 4]>),
}

fn pt(p: Point) -> [f64; 2] {
    [p.x, p.y]
}

fn point(p: [f64; 2]) -> Point {
    Point::new(p[0], p[1])
}

impl From<&Geometry> for GeometryData {
    fn from(g: &Geometry) -> GeometryData {
        match g {
            Geometry::Rect(r) => GeometryData::Rect([r.x0, r.y0, r.x1, r.y1]),
            Geometry::Line { a, b } => GeometryData::Line([pt(*a), pt(*b)]),
            Geometry::Points(points) => {
                GeometryData::Points(points.iter().map(|p| pt(*p)).collect())
            }
            Geometry::Ink(strokes) => GeometryData::Ink(
                strokes
                    .iter()
                    .map(|s| s.iter().map(|p| pt(*p)).collect())
                    .collect(),
            ),
            Geometry::Quads(quads) => {
                GeometryData::Quads(quads.iter().map(|q| q.map(pt)).collect())
            }
        }
    }
}

impl GeometryData {
    pub fn to_geometry(&self) -> Geometry {
        match self {
            GeometryData::Rect(r) => Geometry::Rect(Rect::new(r[0], r[1], r[2], r[3])),
            GeometryData::Line([a, b]) => Geometry::Line {
                a: point(*a),
                b: point(*b),
            },
            GeometryData::Points(points) => {
                Geometry::Points(points.iter().map(|p| point(*p)).collect())
            }
            GeometryData::Ink(strokes) => Geometry::Ink(
                strokes
                    .iter()
                    .map(|s| s.iter().map(|p| point(*p)).collect())
                    .collect(),
            ),
            GeometryData::Quads(quads) => {
                Geometry::Quads(quads.iter().map(|q| q.map(point)).collect())
            }
        }
    }
}

/// A colour as `[r, g, b]`, each 0 to 1.
pub type RgbData = [f64; 3];

pub(crate) fn rgb_data(c: Rgb) -> RgbData {
    [f64::from(c.r), f64::from(c.g), f64::from(c.b)]
}

pub(crate) fn rgb(c: RgbData) -> Rgb {
    Rgb::new(c[0] as f32, c[1] as f32, c[2] as f32)
}

/// [`Style`] as plain data. Line endings are their PDF names (`OpenArrow`, `Circle`, ...).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StyleData {
    pub stroke: Option<RgbData>,
    pub fill: Option<RgbData>,
    pub width: f64,
    pub dash: Option<Vec<f64>>,
    pub opacity: f64,
    pub line_start: String,
    pub line_end: String,
    pub cloud: Option<f64>,
    pub multiply: bool,
}

impl From<&Style> for StyleData {
    fn from(s: &Style) -> StyleData {
        StyleData {
            stroke: s.stroke.map(rgb_data),
            fill: s.fill.map(rgb_data),
            width: s.width,
            dash: s.dash.clone(),
            opacity: s.opacity,
            line_start: s.line_start.pdf_name().to_string(),
            line_end: s.line_end.pdf_name().to_string(),
            cloud: s.cloud,
            multiply: s.blend == BlendMode::Multiply,
        }
    }
}

impl StyleData {
    pub fn to_style(&self) -> Style {
        Style {
            stroke: self.stroke.map(rgb),
            fill: self.fill.map(rgb),
            width: self.width,
            dash: self.dash.clone(),
            opacity: self.opacity,
            line_start: LineEnding::from_pdf_name(&self.line_start).unwrap_or(LineEnding::None),
            line_end: LineEnding::from_pdf_name(&self.line_end).unwrap_or(LineEnding::None),
            cloud: self.cloud,
            blend: if self.multiply {
                BlendMode::Multiply
            } else {
                BlendMode::Normal
            },
        }
    }
}

/// [`PdfDate`] as plain data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateData {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub utc_offset_minutes: Option<i16>,
}

impl From<PdfDate> for DateData {
    fn from(d: PdfDate) -> DateData {
        DateData {
            year: d.year,
            month: d.month,
            day: d.day,
            hour: d.hour,
            minute: d.minute,
            second: d.second,
            utc_offset_minutes: d.utc_offset_minutes,
        }
    }
}

impl From<DateData> for PdfDate {
    fn from(d: DateData) -> PdfDate {
        PdfDate {
            year: d.year,
            month: d.month,
            day: d.day,
            hour: d.hour,
            minute: d.minute,
            second: d.second,
            utc_offset_minutes: d.utc_offset_minutes,
        }
    }
}

/// [`Meta`] as plain data.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct MetaData {
    pub author: Option<String>,
    pub subject: Option<String>,
    pub contents: Option<String>,
    pub created: Option<DateData>,
    pub modified: Option<DateData>,
    pub flags: u32,
    pub layer: Option<(u32, u16)>,
    pub reply_to: Option<String>,
    pub reply_type: Option<String>,
    pub state: Option<String>,
    pub state_model: Option<String>,
}

/// [`LinkTarget`] as plain data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LinkData {
    Uri(String),
    Page { page: usize, at: Option<[f64; 2]> },
    Other,
}

/// The text side of a FreeText ([`FreeText`]) as plain data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FreeTextData {
    /// The `/IT` name: `FreeText`, `FreeTextCallout` or `FreeTextTypeWriter`.
    pub intent: String,
    /// The standard-14 face: `Helvetica`, `Helvetica-Bold`, `Helvetica-Oblique` or
    /// `Helvetica-BoldOblique`.
    pub font: String,
    pub size: f64,
    pub color: RgbData,
    /// `/Q`: 0 left, 1 centre, 2 right.
    pub align: i64,
    /// `Top`, `Middle` or `Bottom`.
    pub v_align: String,
    pub line_spacing: f64,
    pub padding: f64,
    /// The leader of a callout, from the tip to the box.
    #[serde(default)]
    pub callout: Vec<[f64; 2]>,
    /// `/Rotate`: 0, 90, 180 or 270.
    #[serde(default)]
    pub rotation: u16,
}

pub(crate) fn v_align_name(v: VAlign) -> &'static str {
    match v {
        VAlign::Top => "Top",
        VAlign::Middle => "Middle",
        VAlign::Bottom => "Bottom",
    }
}

pub(crate) fn v_align_from(name: &str) -> Option<VAlign> {
    match name {
        "Top" => Some(VAlign::Top),
        "Middle" => Some(VAlign::Middle),
        "Bottom" => Some(VAlign::Bottom),
        _ => None,
    }
}

pub(crate) fn font_from(name: &str) -> Option<FontStyle> {
    FontStyle::ALL.into_iter().find(|f| f.base_font() == name)
}

impl From<&FreeText> for FreeTextData {
    fn from(t: &FreeText) -> FreeTextData {
        FreeTextData {
            intent: t.intent.pdf_name().to_string(),
            font: t.font.base_font().to_string(),
            size: t.size,
            color: rgb_data(t.color),
            align: t.align.quadding(),
            v_align: v_align_name(t.v_align).to_string(),
            line_spacing: t.line_spacing,
            padding: t.padding,
            callout: t.callout.iter().map(|p| pt(*p)).collect(),
            rotation: t.rotation,
        }
    }
}

impl FreeTextData {
    pub fn to_free_text(&self) -> FreeText {
        FreeText {
            intent: FreeTextIntent::from_pdf_name(&self.intent),
            font: font_from(&self.font).unwrap_or_default(),
            size: self.size,
            color: rgb(self.color),
            align: HAlign::from_quadding(self.align),
            v_align: v_align_from(&self.v_align).unwrap_or(VAlign::Top),
            line_spacing: self.line_spacing,
            padding: self.padding,
            callout: self.callout.iter().map(|p| point(*p)).collect(),
            rotation: self.rotation,
        }
    }
}

/// What a stamp shows ([`Stamp`]) as plain data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StampData {
    /// One of the application's own label stamps.
    Label {
        name: String,
        label: String,
        detail: Option<String>,
        /// The ink scheme's name (`Green`, `Red`, ...).
        scheme: String,
        #[serde(default)]
        rotation: u16,
    },
    /// A picture (see [`ImageData`] for how it is stored).
    Image {
        image: ImageData,
        #[serde(default)]
        rotation: u16,
    },
}

impl From<&Stamp> for StampData {
    fn from(s: &Stamp) -> StampData {
        match &s.content {
            StampContent::Label(spec) => StampData::Label {
                name: spec.name.clone(),
                label: spec.label.clone(),
                detail: spec.detail.clone(),
                scheme: spec.scheme.name().to_string(),
                rotation: s.rotation,
            },
            StampContent::Image(image) => StampData::Image {
                image: ImageData(image.clone()),
                rotation: s.rotation,
            },
        }
    }
}

impl StampData {
    pub fn to_stamp(&self) -> Stamp {
        match self {
            StampData::Label {
                name,
                label,
                detail,
                scheme,
                rotation,
            } => Stamp {
                content: StampContent::Label(StampSpec {
                    name: name.clone(),
                    label: label.clone(),
                    detail: detail.clone(),
                    scheme: StampScheme::from_name(scheme).unwrap_or_default(),
                }),
                rotation: *rotation,
            },
            StampData::Image { image, rotation } => Stamp {
                content: StampContent::Image(image.0.clone()),
                rotation: *rotation,
            },
        }
    }
}

/// One annotation as plain data: every field of the model, nothing of the file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotData {
    pub id: String,
    /// Zero-based page index.
    pub page: usize,
    /// The subtype's PDF name (`Square`, `Ink`, ...).
    pub kind: String,
    pub geometry: GeometryData,
    pub style: StyleData,
    pub meta: MetaData,
    pub icon: Option<String>,
    pub open: bool,
    pub popup: Option<String>,
    pub parent: Option<String>,
    pub link: Option<LinkData>,
    pub app_data: Option<PdfValue>,
    /// The text side of a FreeText this application draws; absent for every other annotation
    /// (and in data written before text boxes existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<FreeTextData>,
    /// The content of a Stamp this application draws.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<StampData>,
    /// The measurement a Line, PolyLine, Polygon or Ink is; absent for every other annotation
    /// (and in data written before measurements existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measure: Option<MeasurementData>,
    /// Bluebeam's custom column values (`/BSIColumnData`), entries as the file has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_data: Option<Vec<PdfValue>>,
}

impl From<&Annotation> for AnnotData {
    fn from(a: &Annotation) -> AnnotData {
        AnnotData {
            id: a.id.0.clone(),
            page: a.page,
            kind: a.kind.pdf_name().to_string(),
            geometry: GeometryData::from(&a.geometry),
            style: StyleData::from(&a.style),
            meta: MetaData {
                author: a.meta.author.clone(),
                subject: a.meta.subject.clone(),
                contents: a.meta.contents.clone(),
                created: a.meta.created.map(DateData::from),
                modified: a.meta.modified.map(DateData::from),
                flags: a.meta.flags.0,
                layer: a.meta.layer,
                reply_to: a.meta.reply_to.as_ref().map(|i| i.0.clone()),
                reply_type: a.meta.reply_type.clone(),
                state: a.meta.state.clone(),
                state_model: a.meta.state_model.clone(),
            },
            icon: a.icon.clone(),
            open: a.open,
            popup: a.popup.as_ref().map(|i| i.0.clone()),
            parent: a.parent.as_ref().map(|i| i.0.clone()),
            link: a.link.as_ref().map(|l| match l {
                LinkTarget::Uri(u) => LinkData::Uri(u.clone()),
                LinkTarget::Page { page, at } => LinkData::Page {
                    page: *page,
                    at: at.map(pt),
                },
                LinkTarget::Other => LinkData::Other,
            }),
            app_data: a.app_data.as_ref().map(PdfValue::from_dict),
            free_text: a.free_text.as_ref().map(FreeTextData::from),
            stamp: a.stamp.as_ref().map(StampData::from),
            measure: a.measure.as_ref().map(MeasurementData::from),
            column_data: a
                .column_data
                .as_ref()
                .map(|v| v.iter().map(PdfValue::from_object).collect()),
        }
    }
}

impl AnnotData {
    /// The annotation this describes, with no tie to any file.
    pub fn to_annotation(&self) -> Annotation {
        let kind = AnnotKind::from_pdf_name(&self.kind);
        // The constructor's stamps are all overwritten below; only the baseline (none) and the
        // field set come from it.
        let origin = Origin::new(String::new(), PdfDate::from_unix(0));
        let mut a = Annotation::new(kind, self.page, self.geometry.to_geometry(), &origin);
        a.id = AnnotId(self.id.clone());
        a.style = self.style.to_style();
        a.meta = Meta {
            author: self.meta.author.clone(),
            subject: self.meta.subject.clone(),
            contents: self.meta.contents.clone(),
            created: self.meta.created.map(PdfDate::from),
            modified: self.meta.modified.map(PdfDate::from),
            flags: Flags(self.meta.flags),
            layer: self.meta.layer,
            reply_to: self.meta.reply_to.as_deref().map(AnnotId::from),
            reply_type: self.meta.reply_type.clone(),
            state: self.meta.state.clone(),
            state_model: self.meta.state_model.clone(),
        };
        a.icon = self.icon.clone();
        a.open = self.open;
        a.popup = self.popup.as_deref().map(AnnotId::from);
        a.parent = self.parent.as_deref().map(AnnotId::from);
        a.link = self.link.as_ref().map(|l| match l {
            LinkData::Uri(u) => LinkTarget::Uri(u.clone()),
            LinkData::Page { page, at } => LinkTarget::Page {
                page: *page,
                at: at.map(point),
            },
            LinkData::Other => LinkTarget::Other,
        });
        a.app_data = self.app_data.as_ref().map(PdfValue::to_dict);
        a.free_text = self.free_text.as_ref().map(FreeTextData::to_free_text);
        a.stamp = self.stamp.as_ref().map(StampData::to_stamp);
        a.measure = self
            .measure
            .as_ref()
            .and_then(MeasurementData::to_measurement);
        a.column_data = self
            .column_data
            .as_ref()
            .map(|v| v.iter().map(PdfValue::to_object).collect());
        a
    }
}

/// serde adapters that write annotations as [`AnnotData`].
pub(crate) mod as_data {
    use super::AnnotData;
    use pl_annot::Annotation;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub mod list {
        use super::*;

        pub fn serialize<S: Serializer>(v: &[Annotation], s: S) -> Result<S::Ok, S::Error> {
            v.iter()
                .map(AnnotData::from)
                .collect::<Vec<_>>()
                .serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Annotation>, D::Error> {
            Ok(Vec::<AnnotData>::deserialize(d)?
                .iter()
                .map(AnnotData::to_annotation)
                .collect())
        }
    }

    pub mod indexed {
        use super::*;

        pub fn serialize<S: Serializer>(
            v: &[(usize, Annotation)],
            s: S,
        ) -> Result<S::Ok, S::Error> {
            v.iter()
                .map(|(i, a)| (*i, AnnotData::from(a)))
                .collect::<Vec<_>>()
                .serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Vec<(usize, Annotation)>, D::Error> {
            Ok(Vec::<(usize, AnnotData)>::deserialize(d)?
                .iter()
                .map(|(i, a)| (*i, a.to_annotation()))
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_annot::Quad;

    fn origin() -> Origin {
        Origin::new("Ada", PdfDate::parse("D:20261124101500+01'00'").unwrap())
    }

    pub(crate) fn one_of_each() -> Vec<Annotation> {
        let o = origin();
        let quad: Quad = [
            Point::new(10.1, 20.2),
            Point::new(90.3, 20.2),
            Point::new(90.3, 32.7),
            Point::new(10.1, 32.7),
        ];
        let mut square = Annotation::square(0, Rect::new(10.0, 20.0, 110.5, 80.25), &o);
        square.style.fill = Some(Rgb::new(0.1, 0.7, 0.33));
        square.style.dash = Some(vec![3.0, 1.5]);
        square.style.opacity = 0.4;
        square.style.cloud = Some(1.0);
        let mut dict = Dictionary::new();
        dict.set("Kind", Object::Name(b"Count".to_vec()));
        dict.set("N", Object::Integer(7));
        dict.set(
            "Scale",
            Object::Array(vec![
                Object::Real(0.25),
                Object::Boolean(true),
                Object::Null,
            ]),
        );
        dict.set(
            "Label",
            Object::String(b"abc".to_vec(), StringFormat::Hexadecimal),
        );
        dict.set("Ref", Object::Reference((12, 0)));
        square.app_data = Some(dict);
        let mut line = Annotation::line(1, Point::new(1.0 / 3.0, 2.0), Point::new(1e-7, 9e6), &o);
        line.style.line_start = LineEnding::Circle;
        line.style.line_end = LineEnding::ClosedArrow;
        let mut note = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "héllo ✓", &o);
        let popup = Annotation::popup_for(&mut note, Rect::new(30.0, 0.0, 130.0, 60.0), &o);
        let mut reply = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "r", &o);
        reply.meta.reply_to = Some(note.id.clone());
        reply.meta.reply_type = Some("R".into());
        reply.meta.state = Some("Accepted".into());
        reply.meta.state_model = Some("Review".into());
        reply.meta.layer = Some((9, 0));
        let mut text_box = Annotation::free_text(
            1,
            Rect::new(40.0, 500.0, 220.0, 560.0),
            "Check the falls\nto the gully.",
            &o,
        );
        text_box.style.fill = Some(Rgb::new(1.0, 1.0, 0.8));
        if let Some(t) = text_box.free_text.as_mut() {
            t.font = FontStyle::BoldItalic;
            t.size = 10.5;
            t.align = HAlign::Centre;
            t.v_align = VAlign::Middle;
            t.line_spacing = 1.1;
            t.padding = 4.25;
        }
        text_box.set_rotation(270);
        let callout = Annotation::callout(
            0,
            Rect::new(300.0, 300.0, 420.0, 340.0),
            Point::new(250.3, 410.7),
            "Kerb here",
            &o,
        );
        let mut stamp = Annotation::stamp(
            2,
            Rect::new(100.0, 100.0, 260.0, 160.0),
            StampSpec::revised().with_detail("Rev C, Ada"),
            &o,
        );
        stamp.set_rotation(90);
        let picture =
            pl_annot::RgbaImage::new(3, 2, (0..24u8).map(|i| i.wrapping_mul(11)).collect())
                .unwrap();
        let image = Annotation::image_stamp(
            0,
            Rect::new(10.0, 10.0, 70.0, 50.0),
            std::sync::Arc::new(picture),
            &o,
        );
        vec![
            square,
            text_box,
            callout,
            Annotation::typewriter(0, Rect::new(5.0, 5.0, 90.0, 20.0), "typed", &o),
            stamp,
            image,
            Annotation::circle(0, Rect::new(0.0, 0.0, 5.0, 5.0), &o),
            line,
            Annotation::polyline(0, vec![Point::new(0.0, 0.0), Point::new(4.0, 4.0)], &o),
            Annotation::polygon(
                2,
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(4.0, 4.0),
                    Point::new(0.0, 4.0),
                ],
                &o,
            ),
            Annotation::ink(
                0,
                vec![vec![Point::new(0.1, 0.2), Point::new(0.3, 0.4)]],
                &o,
            ),
            Annotation::text_markup(AnnotKind::Highlight, 0, vec![quad], &o),
            Annotation::text_markup(AnnotKind::Squiggly, 0, vec![quad, quad], &o),
            note,
            popup,
            reply,
            Annotation::link(
                0,
                Rect::new(0.0, 0.0, 5.0, 5.0),
                LinkTarget::Uri("https://example.invalid/".into()),
                &o,
            ),
            Annotation::link(
                0,
                Rect::new(0.0, 0.0, 5.0, 5.0),
                LinkTarget::Page {
                    page: 2,
                    at: Some(Point::new(1.0, 2.0)),
                },
                &o,
            ),
        ]
    }

    /// One measurement of each kind, on an engineering scale with a user unit of 2.
    fn measurements() -> Vec<Annotation> {
        use pl_annot::{CountSymbol, MeasureFormat, MeasureSpec};
        let o = origin();
        let scale = pl_geom::Scale::parse("1\" = 20'").unwrap();
        let spec = MeasureSpec::new(&scale, 2.0, &MeasureFormat::for_scale(&scale));
        let p = Point::new;
        let ring = vec![p(0.0, 0.0), p(144.0, 0.0), p(144.0, 72.0), p(0.0, 72.0)];
        let hole = vec![p(10.0, 10.0), p(20.0, 10.0), p(20.0, 1.0 / 3.0)];
        let mut length = Annotation::measure_length(0, p(1.0, 2.0), p(3.5, 9.25), spec.clone(), &o);
        if let Some(m) = length.measure.as_mut() {
            m.leader = 12.5;
            m.leader_extension = 3.0;
            m.scale = pl_annot::ScaleRef::Viewport("Detail".into());
        }
        length.set_rotation(90);
        vec![
            length,
            Annotation::measure_polylength(0, vec![p(0.0, 0.0), p(5.0, 5.0)], spec.clone(), &o),
            Annotation::measure_perimeter(1, ring.clone(), spec.clone(), &o),
            Annotation::measure_area(1, ring.clone(), vec![hole.clone()], spec.clone(), &o),
            Annotation::measure_volume(1, ring, vec![hole], 0.15, spec.clone(), &o),
            Annotation::measure_angle(0, p(1.0, 0.0), p(0.0, 0.0), p(0.0, 1.0), None, &o),
            Annotation::measure_radius(0, p(5.0, 5.0), p(5.0, 9.0), spec.clone(), &o),
            Annotation::measure_diameter(0, p(1.0, 5.0), p(9.0, 5.0), spec, &o),
            Annotation::count(2, &[p(1.0, 1.0), p(2.0, 3.0)], CountSymbol::Star, &o),
        ]
    }

    #[test]
    fn every_measurement_survives_json_exactly() {
        for a in measurements() {
            let data = AnnotData::from(&a);
            let json = serde_json::to_string(&data).unwrap();
            let back: AnnotData = serde_json::from_str(&json).unwrap();
            assert_eq!(back, data);
            let back = back.to_annotation();
            assert_eq!(
                back,
                a.detached(),
                "{:?}",
                a.measure.as_ref().map(|m| m.kind)
            );
            assert_eq!(back.quantities(), a.quantities());
        }
        // Data written before measurements existed has no "measure" key and reads as before.
        let square = Annotation::square(0, Rect::new(1.0, 2.0, 3.0, 4.0), &origin());
        let json = serde_json::to_string(&AnnotData::from(&square)).unwrap();
        assert!(!json.contains("measure"), "{json}");
    }

    #[test]
    fn every_kind_survives_json_exactly() {
        for a in one_of_each() {
            let data = AnnotData::from(&a);
            let json = serde_json::to_string(&data).unwrap();
            let back: AnnotData = serde_json::from_str(&json).unwrap();
            assert_eq!(back, data);
            assert_eq!(back.to_annotation(), a.detached(), "{}", a.kind.pdf_name());
        }
    }

    #[test]
    fn data_written_before_text_boxes_existed_still_reads() {
        // An annotation without a text box or stamp is written without those keys, which is
        // the form older journals and clipboards have.
        let o = origin();
        let square = Annotation::square(0, Rect::new(1.0, 2.0, 3.0, 4.0), &o);
        let json = serde_json::to_string(&AnnotData::from(&square)).unwrap();
        assert!(
            !json.contains("free_text") && !json.contains("stamp"),
            "{json}"
        );
        let back: AnnotData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_annotation(), square.detached());
        // A text box and a stamp name what they hold in words, not numbers.
        let note = Annotation::free_text(0, Rect::new(0.0, 0.0, 90.0, 40.0), "n", &o);
        let json = serde_json::to_string(&AnnotData::from(&note)).unwrap();
        assert!(json.contains(r#""intent":"FreeText""#), "{json}");
        assert!(json.contains(r#""font":"Helvetica""#) && json.contains(r#""v_align":"Top""#));
        let stamp = Annotation::stamp(0, Rect::new(0.0, 0.0, 90.0, 40.0), StampSpec::void(), &o);
        let json = serde_json::to_string(&AnnotData::from(&stamp)).unwrap();
        assert!(
            json.contains(r#""Label""#) && json.contains(r#""scheme":""#),
            "{json}"
        );
    }

    #[test]
    fn awkward_numbers_round_trip_bit_for_bit() {
        let o = origin();
        let values = [
            0.1 + 0.2,
            1.0 / 3.0,
            123456.789e-3,
            f64::MIN_POSITIVE,
            5e-324,
            1.7976931348623157e308,
            -0.0,
            2.225_073_858_507_201e-308,
        ];
        let a = Annotation::polyline(0, values.iter().map(|v| Point::new(*v, -*v)).collect(), &o);
        let json = serde_json::to_vec(&AnnotData::from(&a)).unwrap();
        let back = serde_json::from_slice::<AnnotData>(&json)
            .unwrap()
            .to_annotation();
        let Geometry::Points(points) = &back.geometry else {
            panic!()
        };
        for (p, v) in points.iter().zip(values) {
            assert_eq!(p.x.to_bits(), v.to_bits());
            assert_eq!(p.y.to_bits(), (-v).to_bits());
        }
    }

    #[test]
    fn an_unknown_subtype_keeps_its_name() {
        let o = origin();
        let a = Annotation::new(
            AnnotKind::Unknown("Redact".into()),
            3,
            Geometry::Rect(Rect::new(1.0, 2.0, 3.0, 4.0)),
            &o,
        );
        let back = AnnotData::from(&a).to_annotation();
        assert_eq!(back, a);
        assert!(!back.is_modelled());
    }
}
