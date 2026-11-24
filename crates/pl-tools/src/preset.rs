//! The tools, and the style each one draws with.

use pl_annot::{
    AnnotKind, Annotation, BlendMode, CaptionPosition, CountSymbol, FontStyle, FreeText, HAlign,
    LineEnding, MeasureFormat, MeasureKind, Rgb, StampScheme, StampSpec, Style, VAlign,
};
use pl_geom::{
    AreaFormat, AreaUnit, LengthFormat, Precision, Scale, Unit, VolumeFormat, VolumeUnit,
};
use serde::{Deserialize, Serialize};

use crate::snap::SnapKinds;

/// The tools this crate implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ToolKind {
    Select,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Polyline,
    Polygon,
    Cloud,
    Pen,
    Highlighter,
    Note,
    /// A box of wrapped text with a border.
    TextBox,
    /// Text typed straight onto the page: no border, no background.
    Typewriter,
    /// A text box with a leader line to what it describes.
    Callout,
    /// A cloud drawn as a dragged rectangle ([`ToolKind::Cloud`] is the one clicked point by
    /// point).
    CloudBox,
    /// A dragged cloud with a callout's text box attached to it.
    CloudCallout,
    /// One of the label stamps; which one is the preset's [`ToolPreset::stamp`].
    Stamp,
    /// A picture ([`ToolState::with_image`](crate::ToolState::with_image)).
    Image,
    /// Highlight, underline, strike out or squiggle the text a drag selects.
    TextHighlight,
    TextUnderline,
    TextStrikeOut,
    TextSquiggly,
    /// Rubs out the pen strokes it is dragged across.
    Eraser,
    /// Sets a scale from two picked points and a real distance the user types.
    Calibrate,
    /// Drags a region of the page that has a scale of its own.
    Viewport,
    /// The distance between two points.
    Length,
    /// The length along clicked points.
    Polylength,
    /// The length around a clicked (or dragged rectangular) outline.
    Perimeter,
    /// The area inside a clicked (or dragged rectangular) outline.
    Area,
    /// A hole cut in an existing area or volume.
    Cutout,
    /// An area times a depth.
    Volume,
    /// The angle at the middle of three clicked points.
    Angle,
    /// The radius of a circle: its centre and a point on it, or three points on it.
    Radius,
    /// The diameter of a circle, picked as a radius is.
    Diameter,
    /// Markers counted in one annotation.
    Count,
}

impl ToolKind {
    pub const ALL: [ToolKind; 35] = [
        ToolKind::Select,
        ToolKind::Rectangle,
        ToolKind::Ellipse,
        ToolKind::Line,
        ToolKind::Arrow,
        ToolKind::Polyline,
        ToolKind::Polygon,
        ToolKind::Cloud,
        ToolKind::Pen,
        ToolKind::Highlighter,
        ToolKind::Note,
        ToolKind::TextBox,
        ToolKind::Typewriter,
        ToolKind::Callout,
        ToolKind::CloudBox,
        ToolKind::CloudCallout,
        ToolKind::Stamp,
        ToolKind::Image,
        ToolKind::TextHighlight,
        ToolKind::TextUnderline,
        ToolKind::TextStrikeOut,
        ToolKind::TextSquiggly,
        ToolKind::Eraser,
        ToolKind::Calibrate,
        ToolKind::Viewport,
        ToolKind::Length,
        ToolKind::Polylength,
        ToolKind::Perimeter,
        ToolKind::Area,
        ToolKind::Cutout,
        ToolKind::Volume,
        ToolKind::Angle,
        ToolKind::Radius,
        ToolKind::Diameter,
        ToolKind::Count,
    ];

    /// The measurement tools: those that make or change a measurement, and Calibrate and
    /// Viewport, which set the scale measurements are taken at.
    pub const MEASURE: [ToolKind; 12] = [
        ToolKind::Calibrate,
        ToolKind::Viewport,
        ToolKind::Length,
        ToolKind::Polylength,
        ToolKind::Perimeter,
        ToolKind::Area,
        ToolKind::Cutout,
        ToolKind::Volume,
        ToolKind::Angle,
        ToolKind::Radius,
        ToolKind::Diameter,
        ToolKind::Count,
    ];

    /// True for the measurement tools ([`ToolKind::MEASURE`]).
    pub fn is_measure(self) -> bool {
        ToolKind::MEASURE.contains(&self)
    }

    /// The kind of measurement the tool creates; `None` for every other tool, and for
    /// Calibrate, Viewport and Cutout, which create none.
    pub fn measure_kind(self) -> Option<MeasureKind> {
        Some(match self {
            ToolKind::Length => MeasureKind::Length,
            ToolKind::Polylength => MeasureKind::Polylength,
            ToolKind::Perimeter => MeasureKind::Perimeter,
            ToolKind::Area => MeasureKind::Area,
            ToolKind::Volume => MeasureKind::Volume,
            ToolKind::Angle => MeasureKind::Angle,
            ToolKind::Radius => MeasureKind::Radius,
            ToolKind::Diameter => MeasureKind::Diameter,
            ToolKind::Count => MeasureKind::Count,
            _ => return None,
        })
    }

    /// The kinds of snap point the caller should look for on this tool's behalf
    /// ([`Pointer::snap`](crate::Pointer::snap)). Freehand and text-selection tools take none;
    /// the measurement tools take every kind, nearest-on-segment included; Count and the
    /// shape tools take the defining points; Viewport takes corners.
    pub fn snap_kinds(self) -> SnapKinds {
        match self {
            ToolKind::Pen
            | ToolKind::Highlighter
            | ToolKind::Eraser
            | ToolKind::TextHighlight
            | ToolKind::TextUnderline
            | ToolKind::TextStrikeOut
            | ToolKind::TextSquiggly => SnapKinds::NONE,
            ToolKind::Viewport => SnapKinds::CORNERS,
            ToolKind::Count => SnapKinds::POINTS,
            t if t.is_measure() => SnapKinds::ALL,
            _ => SnapKinds::POINTS,
        }
    }

    /// True for the tools that make a FreeText and so wait for typed text: Text Box,
    /// Typewriter, Callout and Cloud Callout.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            ToolKind::TextBox | ToolKind::Typewriter | ToolKind::Callout | ToolKind::CloudCallout
        )
    }

    /// True for the four tools that mark up selected text.
    pub fn is_text_markup(self) -> bool {
        matches!(
            self,
            ToolKind::TextHighlight
                | ToolKind::TextUnderline
                | ToolKind::TextStrikeOut
                | ToolKind::TextSquiggly
        )
    }

    /// The annotation subtype the tool creates; `None` for Select, Eraser, Calibrate, Viewport
    /// and Cutout. Cloud Callout creates two, a Square (named here) and a FreeText.
    pub fn annot_kind(self) -> Option<AnnotKind> {
        Some(match self {
            ToolKind::Select
            | ToolKind::Eraser
            | ToolKind::Calibrate
            | ToolKind::Viewport
            | ToolKind::Cutout => return None,
            // The carrier of the measurement (`MeasureKind::carrier`).
            ToolKind::Length | ToolKind::Radius | ToolKind::Diameter => AnnotKind::Line,
            ToolKind::Polylength | ToolKind::Angle => AnnotKind::PolyLine,
            ToolKind::Perimeter | ToolKind::Area | ToolKind::Volume => AnnotKind::Polygon,
            ToolKind::Count => AnnotKind::Ink,
            ToolKind::TextBox | ToolKind::Typewriter | ToolKind::Callout => AnnotKind::FreeText,
            ToolKind::CloudBox | ToolKind::CloudCallout => AnnotKind::Square,
            ToolKind::Stamp | ToolKind::Image => AnnotKind::Stamp,
            ToolKind::TextHighlight => AnnotKind::Highlight,
            ToolKind::TextUnderline => AnnotKind::Underline,
            ToolKind::TextStrikeOut => AnnotKind::StrikeOut,
            ToolKind::TextSquiggly => AnnotKind::Squiggly,
            ToolKind::Rectangle => AnnotKind::Square,
            ToolKind::Ellipse => AnnotKind::Circle,
            ToolKind::Line | ToolKind::Arrow => AnnotKind::Line,
            ToolKind::Polyline => AnnotKind::PolyLine,
            ToolKind::Polygon | ToolKind::Cloud => AnnotKind::Polygon,
            ToolKind::Pen | ToolKind::Highlighter => AnnotKind::Ink,
            ToolKind::Note => AnnotKind::Text,
        })
    }

    /// The tool's name, which is also the `/Subj` of what it draws.
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Select => "Select",
            ToolKind::Rectangle => "Rectangle",
            ToolKind::Ellipse => "Ellipse",
            ToolKind::Line => "Line",
            ToolKind::Arrow => "Arrow",
            ToolKind::Polyline => "Polyline",
            ToolKind::Polygon => "Polygon",
            ToolKind::Cloud => "Cloud",
            ToolKind::Pen => "Pen",
            ToolKind::Highlighter => "Highlighter",
            ToolKind::Note => "Note",
            ToolKind::TextBox => "Text Box",
            ToolKind::Typewriter => "Typewriter",
            ToolKind::Callout => "Callout",
            ToolKind::CloudBox => "Cloud Box",
            ToolKind::CloudCallout => "Cloud Callout",
            ToolKind::Stamp => "Stamp",
            ToolKind::Image => "Image",
            ToolKind::TextHighlight => "Highlight",
            ToolKind::TextUnderline => "Underline",
            ToolKind::TextStrikeOut => "Strikeout",
            ToolKind::TextSquiggly => "Squiggly",
            ToolKind::Eraser => "Eraser",
            ToolKind::Calibrate => "Calibrate",
            ToolKind::Viewport => "Viewport",
            ToolKind::Length => "Length",
            ToolKind::Polylength => "Polylength",
            ToolKind::Perimeter => "Perimeter",
            ToolKind::Area => "Area",
            ToolKind::Cutout => "Cutout",
            ToolKind::Volume => "Volume",
            ToolKind::Angle => "Angle",
            ToolKind::Radius => "Radius",
            ToolKind::Diameter => "Diameter",
            ToolKind::Count => "Count",
        }
    }
}

/// How the text tools set their text. The border (colour, width, dash) and the background of
/// the box are the preset's [`Style`]: `stroke`, `width`, `dash` and `fill`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextPreset {
    /// The face, one of the Helvetica family.
    pub font: FontStyle,
    /// Font size in points.
    pub size: f64,
    /// Text colour.
    pub color: Rgb,
    pub align: HAlign,
    pub v_align: VAlign,
    /// Distance between baselines as a multiple of the font size.
    pub line_spacing: f64,
    /// Space between the inside of the border and the text.
    pub padding: f64,
}

impl Default for TextPreset {
    /// The text of a new text box: what `pl_annot::FreeText::default()` has.
    fn default() -> TextPreset {
        let t = FreeText::default();
        TextPreset {
            font: t.font,
            size: t.size,
            color: t.color,
            align: t.align,
            v_align: t.v_align,
            line_spacing: t.line_spacing,
            padding: t.padding,
        }
    }
}

impl TextPreset {
    /// What `tool` sets text with until the user changes it. Typewriter text sits one unit
    /// inside its box, everything else three.
    pub fn default_for(tool: ToolKind) -> TextPreset {
        let mut text = TextPreset::default();
        if tool == ToolKind::Typewriter {
            text.padding = 1.0;
        }
        text
    }

    /// Sets the text fields of `text`, leaving its intent, leader and rotation alone.
    pub fn apply_to(&self, text: &mut FreeText) {
        text.font = self.font;
        text.size = self.size;
        text.color = self.color;
        text.align = self.align;
        text.v_align = self.v_align;
        text.line_spacing = self.line_spacing;
        text.padding = self.padding;
    }
}

/// How the measurement tools take and show a measurement. The line itself (colour, width,
/// dash, line endings, fill of an area) is the preset's [`Style`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasurePreset {
    /// The unit and precision lengths are shown in; `None` shows them in the unit the scale's
    /// real side was stated in (`MeasureFormat::for_scale`).
    pub length: Option<LengthFormat>,
    /// The unit and decimal places of areas; `None` for the one that goes with the scale.
    pub area: Option<AreaFormat>,
    /// The unit and decimal places of volumes; `None` for the one that goes with the scale.
    pub volume: Option<VolumeFormat>,
    /// Decimal places of an angle in degrees.
    pub angle_places: u8,
    /// Whether the value is drawn beside the measurement.
    pub caption: bool,
    /// Where the value of a length, radius or diameter sits (`/CP`).
    pub caption_position: CaptionPosition,
    /// The size of the value's text, in points as seen on screen (divided by the page's
    /// `/UserUnit`).
    pub text_size: f64,
    /// The marker of a count.
    pub symbol: CountSymbol,
    /// The marker's size across, in points as seen on screen.
    pub symbol_size: f64,
    /// Volume: the depth in metres. `None` asks the user for it
    /// ([`MeasureRequest::Depth`](crate::MeasureRequest::Depth)).
    pub depth: Option<f64>,
    /// Radius and Diameter: three clicks on the circle instead of its centre and one point
    /// on it.
    pub three_point: bool,
    /// Shift keeps a segment to multiples of this many degrees from the screen's horizontal:
    /// 45 by default, 90 for ortho only.
    pub angle_step: f64,
}

impl Default for MeasurePreset {
    fn default() -> MeasurePreset {
        MeasurePreset {
            length: None,
            area: None,
            volume: None,
            angle_places: 1,
            caption: true,
            caption_position: CaptionPosition::Top,
            text_size: 9.0,
            symbol: CountSymbol::Dot,
            symbol_size: 10.0,
            depth: None,
            three_point: false,
            angle_step: 45.0,
        }
    }
}

impl MeasurePreset {
    /// What `tool` measures with until the user changes it: values in the scale's own units,
    /// captions on (a count shows its markers only).
    pub fn default_for(tool: ToolKind) -> MeasurePreset {
        MeasurePreset {
            caption: tool != ToolKind::Count,
            ..MeasurePreset::default()
        }
    }

    /// The number formats for a measurement at `scale`: the scale's own units, with whatever
    /// this preset names instead.
    pub fn format_for(&self, scale: &Scale) -> MeasureFormat {
        let mut format = MeasureFormat::for_scale(scale);
        if let Some(length) = self.length {
            format.length = length;
            if self.area.is_none() {
                format.area =
                    AreaFormat::new(AreaUnit::for_length(length.unit), format.area.places);
            }
        }
        if let Some(area) = self.area {
            format.area = area;
        }
        format.angle_places = self.angle_places;
        format
    }
}

/// The style a tool draws with, under a name.
///
/// Serialises (serde) to a flat record: colours as `[r, g, b]` in 0..=1, line endings and the
/// blend mode by their PDF names. Reading rejects unknown names and non-finite numbers and
/// clamps colours and opacity into range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PresetRepr", into = "PresetRepr")]
pub struct ToolPreset {
    /// Empty for a tool's current style; the user's name for a saved preset.
    pub name: String,
    pub tool: ToolKind,
    pub style: Style,
    /// `/Subj` for what the tool draws.
    pub subject: String,
    /// The text settings of the text tools; other tools carry the default and ignore it.
    pub text: TextPreset,
    /// The stamp the Stamp tool places; `None` for every other tool.
    pub stamp: Option<StampSpec>,
    /// How a measurement tool measures; `None` for every other tool.
    pub measure: Option<MeasurePreset>,
}

impl ToolPreset {
    /// What `tool` draws with until the user changes it.
    pub fn default_for(tool: ToolKind) -> ToolPreset {
        let kind = tool.annot_kind().unwrap_or(AnnotKind::Square);
        let mut style = Style::default_for(&kind);
        match tool {
            ToolKind::Arrow | ToolKind::Callout => style.line_end = LineEnding::OpenArrow,
            ToolKind::Cloud | ToolKind::CloudBox | ToolKind::CloudCallout => {
                style.cloud = Some(1.0);
            }
            ToolKind::Typewriter => {
                style.stroke = None;
                style.width = 0.0;
            }
            ToolKind::Stamp | ToolKind::Image => style.stroke = None,
            ToolKind::Pen => style.width = 2.0,
            // A dimension line has closed arrows, filled with the line's colour.
            ToolKind::Length | ToolKind::Diameter => {
                style.line_start = LineEnding::ClosedArrow;
                style.line_end = LineEnding::ClosedArrow;
                style.fill = style.stroke;
            }
            ToolKind::Radius => {
                style.line_start = LineEnding::None;
                style.line_end = LineEnding::ClosedArrow;
                style.fill = style.stroke;
            }
            ToolKind::Highlighter => {
                style.stroke = Some(Rgb::new(1.0, 0.9, 0.2));
                style.width = 12.0;
                style.blend = BlendMode::Multiply;
            }
            _ => {}
        }
        ToolPreset {
            name: String::new(),
            tool,
            style,
            subject: tool.label().to_string(),
            text: TextPreset::default_for(tool),
            stamp: (tool == ToolKind::Stamp).then(StampSpec::approved),
            measure: tool.is_measure().then(|| MeasurePreset::default_for(tool)),
        }
    }

    /// The Stamp tool placing `spec`.
    pub fn for_stamp(spec: StampSpec) -> ToolPreset {
        ToolPreset {
            stamp: Some(spec),
            ..ToolPreset::default_for(ToolKind::Stamp)
        }
    }

    /// A named preset holding `style`.
    pub fn named(name: impl Into<String>, tool: ToolKind, style: Style) -> ToolPreset {
        ToolPreset {
            name: name.into(),
            tool,
            style,
            subject: tool.label().to_string(),
            text: TextPreset::default_for(tool),
            stamp: (tool == ToolKind::Stamp).then(StampSpec::approved),
            measure: tool.is_measure().then(|| MeasurePreset::default_for(tool)),
        }
    }

    /// The measurement settings: the preset's own, or the defaults for its tool.
    pub fn measure_preset(&self) -> MeasurePreset {
        self.measure
            .unwrap_or_else(|| MeasurePreset::default_for(self.tool))
    }

    /// Gives `annotation` this preset's style and subject and, when it is a modelled FreeText,
    /// this preset's text settings.
    pub fn apply_to(&self, annotation: &mut Annotation) {
        annotation.style = self.style.clone();
        if annotation.kind.is_markup() {
            annotation.meta.subject = Some(self.subject.clone());
        }
        if let Some(text) = annotation.free_text.as_mut() {
            self.text.apply_to(text);
        }
    }
}

/// Why a stored preset was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum PresetError {
    UnknownLineEnding(String),
    UnknownBlendMode(String),
    /// A width, dash length, opacity, colour or cloud size that is not a finite number, or a
    /// negative width or dash; a font size or line spacing that is not above zero.
    BadNumber(&'static str),
    UnknownFont(String),
    UnknownAlignment(String),
    UnknownStampScheme(String),
    /// A length, area or volume unit, caption position or count symbol this crate does not
    /// know.
    UnknownMeasureName(String),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::UnknownLineEnding(n) => write!(f, "unknown line ending \"{n}\""),
            PresetError::UnknownBlendMode(n) => write!(f, "unknown blend mode \"{n}\""),
            PresetError::BadNumber(field) => write!(f, "\"{field}\" is not a usable number"),
            PresetError::UnknownFont(n) => write!(f, "unknown font style \"{n}\""),
            PresetError::UnknownAlignment(n) => write!(f, "unknown alignment \"{n}\""),
            PresetError::UnknownStampScheme(n) => write!(f, "unknown stamp colour \"{n}\""),
            PresetError::UnknownMeasureName(n) => write!(f, "unknown measurement setting \"{n}\""),
        }
    }
}

impl std::error::Error for PresetError {}

fn default_opacity() -> f64 {
    1.0
}

fn default_name() -> String {
    "None".to_string()
}

fn default_blend() -> String {
    "Normal".to_string()
}

fn default_align() -> String {
    "Left".to_string()
}

fn default_v_align() -> String {
    "Top".to_string()
}

fn default_font() -> String {
    "Regular".to_string()
}

fn default_line_spacing() -> f64 {
    TextPreset::default().line_spacing
}

fn default_padding() -> f64 {
    TextPreset::default().padding
}

/// The stored form of a [`TextPreset`].
#[derive(Serialize, Deserialize)]
struct TextRepr {
    #[serde(default = "default_font")]
    font: String,
    size: f64,
    color: [f32; 3],
    #[serde(default = "default_align")]
    align: String,
    #[serde(default = "default_v_align")]
    v_align: String,
    #[serde(default = "default_line_spacing")]
    line_spacing: f64,
    #[serde(default = "default_padding")]
    padding: f64,
}

fn font_name(font: FontStyle) -> &'static str {
    match font {
        FontStyle::Regular => "Regular",
        FontStyle::Bold => "Bold",
        FontStyle::Italic => "Italic",
        FontStyle::BoldItalic => "BoldItalic",
    }
}

fn align_name(align: HAlign) -> &'static str {
    match align {
        HAlign::Left => "Left",
        HAlign::Centre => "Centre",
        HAlign::Right => "Right",
    }
}

fn v_align_name(align: VAlign) -> &'static str {
    match align {
        VAlign::Top => "Top",
        VAlign::Middle => "Middle",
        VAlign::Bottom => "Bottom",
    }
}

impl From<TextPreset> for TextRepr {
    fn from(t: TextPreset) -> TextRepr {
        TextRepr {
            font: font_name(t.font).to_string(),
            size: t.size,
            color: [t.color.r, t.color.g, t.color.b],
            align: align_name(t.align).to_string(),
            v_align: v_align_name(t.v_align).to_string(),
            line_spacing: t.line_spacing,
            padding: t.padding,
        }
    }
}

impl TryFrom<TextRepr> for TextPreset {
    type Error = PresetError;

    fn try_from(r: TextRepr) -> Result<TextPreset, PresetError> {
        let font = FontStyle::ALL
            .into_iter()
            .find(|f| font_name(*f) == r.font)
            .ok_or_else(|| PresetError::UnknownFont(r.font.clone()))?;
        let align = [HAlign::Left, HAlign::Centre, HAlign::Right]
            .into_iter()
            .find(|a| align_name(*a) == r.align)
            .ok_or_else(|| PresetError::UnknownAlignment(r.align.clone()))?;
        let v_align = [VAlign::Top, VAlign::Middle, VAlign::Bottom]
            .into_iter()
            .find(|a| v_align_name(*a) == r.v_align)
            .ok_or_else(|| PresetError::UnknownAlignment(r.v_align.clone()))?;
        if !(r.size.is_finite() && r.size > 0.0) {
            return Err(PresetError::BadNumber("size"));
        }
        if !(r.line_spacing.is_finite() && r.line_spacing > 0.0) {
            return Err(PresetError::BadNumber("line_spacing"));
        }
        if !(r.padding.is_finite() && r.padding >= 0.0) {
            return Err(PresetError::BadNumber("padding"));
        }
        if !r.color.iter().all(|v| v.is_finite()) {
            return Err(PresetError::BadNumber("color"));
        }
        Ok(TextPreset {
            font,
            size: r.size,
            color: Rgb::new(r.color[0], r.color[1], r.color[2]).clamped(),
            align,
            v_align,
            line_spacing: r.line_spacing,
            padding: r.padding,
        })
    }
}

/// The stored form of a stamp: its `/Name`, the words on it and its colour scheme by name.
#[derive(Serialize, Deserialize)]
struct StampRepr {
    name: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    scheme: String,
}

/// The stored form of a length format: the unit's abbreviation and either decimal places or
/// the largest fraction denominator.
#[derive(Serialize, Deserialize)]
struct LengthRepr {
    unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    places: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fraction: Option<u32>,
}

/// The stored form of an area or volume format.
#[derive(Serialize, Deserialize)]
struct UnitPlaces {
    unit: String,
    places: u8,
}

fn default_angle_places() -> u8 {
    1
}

fn default_true() -> bool {
    true
}

fn default_caption_position() -> String {
    "Top".to_string()
}

fn default_text_size() -> f64 {
    MeasurePreset::default().text_size
}

fn default_symbol() -> String {
    "Dot".to_string()
}

fn default_symbol_size() -> f64 {
    MeasurePreset::default().symbol_size
}

fn default_angle_step() -> f64 {
    MeasurePreset::default().angle_step
}

/// The stored form of a [`MeasurePreset`].
#[derive(Serialize, Deserialize)]
struct MeasureRepr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    length: Option<LengthRepr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    area: Option<UnitPlaces>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    volume: Option<UnitPlaces>,
    #[serde(default = "default_angle_places")]
    angle_places: u8,
    #[serde(default = "default_true")]
    caption: bool,
    #[serde(default = "default_caption_position")]
    caption_position: String,
    #[serde(default = "default_text_size")]
    text_size: f64,
    #[serde(default = "default_symbol")]
    symbol: String,
    #[serde(default = "default_symbol_size")]
    symbol_size: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth: Option<f64>,
    #[serde(default)]
    three_point: bool,
    #[serde(default = "default_angle_step")]
    angle_step: f64,
}

impl From<MeasurePreset> for MeasureRepr {
    fn from(m: MeasurePreset) -> MeasureRepr {
        MeasureRepr {
            length: m.length.map(|l| {
                let (places, fraction) = match l.precision {
                    Precision::Decimal(p) => (Some(p), None),
                    Precision::Fraction(d) => (None, Some(d)),
                };
                LengthRepr {
                    unit: l.unit.abbreviation().to_string(),
                    places,
                    fraction,
                }
            }),
            area: m.area.map(|a| UnitPlaces {
                unit: a.unit.abbreviation().to_string(),
                places: a.places,
            }),
            volume: m.volume.map(|v| UnitPlaces {
                unit: v.unit.abbreviation().to_string(),
                places: v.places,
            }),
            angle_places: m.angle_places,
            caption: m.caption,
            caption_position: m.caption_position.pdf_name().to_string(),
            text_size: m.text_size,
            symbol: m.symbol.name().to_string(),
            symbol_size: m.symbol_size,
            depth: m.depth,
            three_point: m.three_point,
            angle_step: m.angle_step,
        }
    }
}

impl TryFrom<MeasureRepr> for MeasurePreset {
    type Error = PresetError;

    fn try_from(r: MeasureRepr) -> Result<MeasurePreset, PresetError> {
        let unknown = |n: &str| PresetError::UnknownMeasureName(n.to_string());
        let positive = |v: f64, field: &'static str| {
            if v.is_finite() && v > 0.0 {
                Ok(v)
            } else {
                Err(PresetError::BadNumber(field))
            }
        };
        let length = match r.length {
            None => None,
            Some(l) => {
                let unit = Unit::ALL
                    .into_iter()
                    .find(|u| u.abbreviation() == l.unit)
                    .ok_or_else(|| unknown(&l.unit))?;
                let precision = match (l.places, l.fraction) {
                    (Some(p), None) => Precision::Decimal(p),
                    (None, Some(d)) if d > 0 => Precision::Fraction(d),
                    _ => return Err(PresetError::BadNumber("length")),
                };
                Some(LengthFormat::new(unit, precision))
            }
        };
        let area = match r.area {
            None => None,
            Some(a) => Some(AreaFormat::new(
                AreaUnit::from_abbreviation(&a.unit).ok_or_else(|| unknown(&a.unit))?,
                a.places,
            )),
        };
        let volume = match r.volume {
            None => None,
            Some(v) => Some(VolumeFormat::new(
                VolumeUnit::from_abbreviation(&v.unit).ok_or_else(|| unknown(&v.unit))?,
                v.places,
            )),
        };
        Ok(MeasurePreset {
            length,
            area,
            volume,
            angle_places: r.angle_places.min(9),
            caption: r.caption,
            caption_position: CaptionPosition::from_pdf_name(&r.caption_position)
                .ok_or_else(|| unknown(&r.caption_position))?,
            text_size: positive(r.text_size, "text_size")?,
            symbol: CountSymbol::from_name(&r.symbol).ok_or_else(|| unknown(&r.symbol))?,
            symbol_size: positive(r.symbol_size, "symbol_size")?,
            depth: r.depth.map(|d| positive(d, "depth")).transpose()?,
            three_point: r.three_point,
            angle_step: positive(r.angle_step, "angle_step")?,
        })
    }
}

/// The stored form of a [`ToolPreset`].
#[derive(Serialize, Deserialize)]
struct PresetRepr {
    #[serde(default)]
    name: String,
    tool: ToolKind,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    stroke: Option<[f32; 3]>,
    #[serde(default)]
    fill: Option<[f32; 3]>,
    width: f64,
    #[serde(default)]
    dash: Option<Vec<f64>>,
    #[serde(default = "default_opacity")]
    opacity: f64,
    #[serde(default = "default_name")]
    line_start: String,
    #[serde(default = "default_name")]
    line_end: String,
    #[serde(default)]
    cloud: Option<f64>,
    #[serde(default = "default_blend")]
    blend: String,
    /// Written for the text tools, and for any preset whose text settings are not the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<TextRepr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stamp: Option<StampRepr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    measure: Option<MeasureRepr>,
}

impl From<ToolPreset> for PresetRepr {
    fn from(p: ToolPreset) -> PresetRepr {
        let rgb = |c: Rgb| [c.r, c.g, c.b];
        let text = (p.tool.is_text() || p.text != TextPreset::default()).then(|| p.text.into());
        let stamp = p.stamp.map(|s| StampRepr {
            name: s.name,
            label: s.label,
            detail: s.detail,
            scheme: s.scheme.name().to_string(),
        });
        PresetRepr {
            text,
            stamp,
            measure: p.measure.map(MeasureRepr::from),
            name: p.name,
            tool: p.tool,
            subject: Some(p.subject),
            stroke: p.style.stroke.map(rgb),
            fill: p.style.fill.map(rgb),
            width: p.style.width,
            dash: p.style.dash,
            opacity: p.style.opacity,
            line_start: p.style.line_start.pdf_name().to_string(),
            line_end: p.style.line_end.pdf_name().to_string(),
            cloud: p.style.cloud,
            blend: p.style.blend.pdf_name().to_string(),
        }
    }
}

impl TryFrom<PresetRepr> for ToolPreset {
    type Error = PresetError;

    fn try_from(r: PresetRepr) -> Result<ToolPreset, PresetError> {
        let rgb = |c: Option<[f32; 3]>, field: &'static str| match c {
            None => Ok(None),
            Some(c) if c.iter().all(|v| v.is_finite()) => {
                Ok(Some(Rgb::new(c[0], c[1], c[2]).clamped()))
            }
            Some(_) => Err(PresetError::BadNumber(field)),
        };
        let ending = |name: &str| {
            LineEnding::from_pdf_name(name)
                .ok_or_else(|| PresetError::UnknownLineEnding(name.to_string()))
        };
        let blend = match r.blend.as_str() {
            "Normal" => BlendMode::Normal,
            "Multiply" => BlendMode::Multiply,
            other => return Err(PresetError::UnknownBlendMode(other.to_string())),
        };
        if !(r.width.is_finite() && r.width >= 0.0) {
            return Err(PresetError::BadNumber("width"));
        }
        if !r.opacity.is_finite() {
            return Err(PresetError::BadNumber("opacity"));
        }
        if let Some(dash) = &r.dash {
            if !dash.iter().all(|d| d.is_finite() && *d >= 0.0) {
                return Err(PresetError::BadNumber("dash"));
            }
        }
        if r.cloud.is_some_and(|c| !(c.is_finite() && c > 0.0)) {
            return Err(PresetError::BadNumber("cloud"));
        }
        let text = match r.text {
            Some(t) => TextPreset::try_from(t)?,
            None => TextPreset::default_for(r.tool),
        };
        let stamp = match r.stamp {
            Some(s) => Some(StampSpec {
                scheme: StampScheme::from_name(&s.scheme)
                    .ok_or_else(|| PresetError::UnknownStampScheme(s.scheme.clone()))?,
                name: s.name,
                label: s.label,
                detail: s.detail,
            }),
            None => (r.tool == ToolKind::Stamp).then(StampSpec::approved),
        };
        let measure = match r.measure {
            Some(m) => Some(MeasurePreset::try_from(m)?),
            None => r
                .tool
                .is_measure()
                .then(|| MeasurePreset::default_for(r.tool)),
        };
        Ok(ToolPreset {
            text,
            stamp,
            measure,
            name: r.name,
            tool: r.tool,
            subject: r.subject.unwrap_or_else(|| r.tool.label().to_string()),
            style: Style {
                stroke: rgb(r.stroke, "stroke")?,
                fill: rgb(r.fill, "fill")?,
                width: r.width,
                dash: r.dash.filter(|d| !d.is_empty()),
                opacity: r.opacity.clamp(0.0, 1.0),
                line_start: ending(&r.line_start)?,
                line_end: ending(&r.line_end)?,
                cloud: r.cloud,
                blend,
            },
        })
    }
}

/// Every tool's current style, and the presets the user has saved by name.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresetLibrary {
    /// At most one per tool: what the tool draws with now. A tool not listed draws with
    /// [`ToolPreset::default_for`].
    #[serde(default)]
    current: Vec<ToolPreset>,
    /// Saved presets; a name is unique within its tool.
    #[serde(default)]
    named: Vec<ToolPreset>,
}

impl PresetLibrary {
    pub fn new() -> PresetLibrary {
        PresetLibrary::default()
    }

    /// What `tool` draws with now.
    pub fn current(&self, tool: ToolKind) -> ToolPreset {
        self.current
            .iter()
            .find(|p| p.tool == tool)
            .cloned()
            .unwrap_or_else(|| ToolPreset::default_for(tool))
    }

    /// Makes `preset` what its tool draws with from now on.
    pub fn set_current(&mut self, preset: ToolPreset) {
        self.current.retain(|p| p.tool != preset.tool);
        self.current.push(preset);
    }

    /// Back to the built-in style for `tool`.
    pub fn reset(&mut self, tool: ToolKind) {
        self.current.retain(|p| p.tool != tool);
    }

    /// Saves `preset` under its name, replacing a preset of the same tool and name. A preset
    /// with an empty name is not saved; returns whether it was.
    pub fn save(&mut self, preset: ToolPreset) -> bool {
        if preset.name.trim().is_empty() {
            return false;
        }
        match self
            .named
            .iter_mut()
            .find(|p| p.tool == preset.tool && p.name == preset.name)
        {
            Some(slot) => *slot = preset,
            None => self.named.push(preset),
        }
        true
    }

    /// Removes a saved preset; returns whether there was one.
    pub fn remove(&mut self, tool: ToolKind, name: &str) -> bool {
        let before = self.named.len();
        self.named.retain(|p| !(p.tool == tool && p.name == name));
        self.named.len() != before
    }

    pub fn find(&self, tool: ToolKind, name: &str) -> Option<&ToolPreset> {
        self.named.iter().find(|p| p.tool == tool && p.name == name)
    }

    /// The saved presets of `tool`, in the order they were saved.
    pub fn named(&self, tool: ToolKind) -> impl Iterator<Item = &ToolPreset> + '_ {
        self.named.iter().filter(move |p| p.tool == tool)
    }

    /// Makes a saved preset the tool's current style and returns it.
    pub fn activate(&mut self, tool: ToolKind, name: &str) -> Option<ToolPreset> {
        let preset = self.find(tool, name)?.clone();
        self.set_current(preset.clone());
        Some(preset)
    }
}
