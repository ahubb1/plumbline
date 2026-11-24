//! Measurements: lengths, areas, volumes, angles, radii and counts taken off a drawing.
//!
//! A measurement is an ordinary annotation of a standard subtype that also holds a
//! [`Measurement`]: which kind it is, the scale it was taken at and how its caption is shown.
//! What it measures is its geometry; the value is always worked out from the geometry and the
//! scale ([`Annotation::quantities`], [`Annotation::measure_text`]) and never stored as a
//! number of its own.
//!
//! # What is written
//!
//! Each kind is written in the form Bluebeam Revu writes it (see `docs/interop-notes.md`), so
//! that Revu and other readers recognise it, while the model keeps one carrier per kind
//! ([`MeasureKind::carrier`]; the file's subtype is [`MeasureKind::file_subtype`]):
//!
//! | Kind | Annotation in the file | `/IT` | `/MeasurementTypes` | Geometry | Private data |
//! |---|---|---|---|---|---|
//! | Length | `Line` | `LineDimension` | 130 | `/L`, `/LL`, `/LLE`, `/LLO`, `/Cap`, `/CP`, `/CO` | |
//! | Polylength | `PolyLine` | `PolyLineDimension` | 130 | `/Vertices` | |
//! | Perimeter | `PolyLine`, closed | `PolyLineDimension` | 130 | `/Vertices`, the first repeated at the end | kind |
//! | Area | `Polygon` | `PolygonDimension` | 129 | `/Vertices` | cutout rings |
//! | Volume | `Polygon` | `PolygonVolume` | 132 | `/Vertices` | kind, cutout rings, depth |
//! | Radius | `Polygon` of three vertices | `PolygonRadius` | 130 | edge, centre, edge | kind |
//! | Diameter | `Circle` | `CircleDimension` | 384 | `/Rect` less `/RD` is the circle | kind, the line drawn |
//! | Angle | `PolyLine` of three vertices | `PolyLineAngle` | 1152 | `/Vertices`, the vertex in the middle | kind |
//! | Count | one small `Polygon` per marker | `PolygonCount` | 128 | the marker's outline; `/CountStyle`, `/NumCounts` | kind, symbol, the marker, the group |
//!
//! Every measurement with a scale carries it as a rectilinear `/Measure` (a count under Revu's
//! `/BBMeasure`) besides the page's `/VP`: `/T` formats an angle, and a volume has a `/V` made
//! from its volume format. Every one has `/Contents` with the value as text and a full `/AP`
//! that draws the value, so a viewer that knows nothing about measurements still shows it. The
//! private data sits in the single [`PRIVATE_KEY`](crate::PRIVATE_KEY) dictionary.
//!
//! **Perimeter.** A closed PolyLine dimension is what other applications write for a
//! perimeter, so a reader that goes by the standard keys reports a length, as `/Contents`
//! does. In the model a perimeter is a Polygon (its outline, the first vertex not repeated);
//! only the file has the PolyLine. A closed `/PolyLineDimension` of another application (last
//! vertex on the first) is read as a perimeter, an open one as a polylength.
//!
//! **Volume.** Revu's `PolygonVolume`. Its depth is in the private data only: no key of Revu's
//! for a depth is known (the Revu volume seen has a depth of 0 and none), so another
//! application's volume is read with a depth of 0.
//!
//! **Radius, Diameter.** A Line in the model (centre to edge, or across the circle), a
//! three-vertex Polygon or a Circle in the file, as Revu writes them.
//!
//! **Count.** In the model one annotation holds all the markers of a count, on `Ink` with one
//! stroke per marker. In the file it is Revu's: one small closed Polygon per marker that draws
//! the marker's symbol, the first being the count's own annotation and the others grouped
//! under it with the standard annotation group (`/IRT` to the first, `/RT /Group`), each naming
//! the count in its private data; `/NumCounts` and `/Contents` give the whole count on each. A
//! load folds them back into the one count ([`crate::Baseline::members`]); another
//! application's markers are counts of one each, grouped by [`Annotation::count_group`]. A
//! count an earlier version wrote as one Ink annotation is read as it is and written Revu's
//! way on the next save, like every measurement whose file form is an earlier one.
//!
//! # Scale and `/UserUnit`
//!
//! A [`MeasureSpec`] is the typed form of a PDF measure dictionary (`/Subtype /RL`). Its first
//! `/X` factor converts page units straight to real units, so the page's `/UserUnit` is part
//! of that factor: [`MeasureSpec::new`] takes the user unit and a
//! [`Scale`](pl_geom::Scale) and builds it in.
//!
//! # Leader lines
//!
//! `/LL` moves the dimension line off the measured points. This crate, like PDFBox and MuPDF,
//! takes a positive `/LL` to the left of the direction from the first point to the second
//! (counter-clockwise in page space).

use pl_draw::shapes::LineEnding;
use pl_geom::{
    polygon_perimeter, polyline_length, AreaFormat, AreaUnit, LengthFormat, Point, Precision,
    Scale, Unit, Vec2, VolumeFormat, VolumeUnit,
};

use crate::content::normalise_rotation;
use crate::model::{AnnotId, AnnotKind, Annotation, Geometry, Origin};

/// What a measurement measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeasureKind {
    /// The distance between two points.
    Length,
    /// The length along an open run of segments.
    Polylength,
    /// The length around a closed outline.
    Perimeter,
    /// The area inside an outline, less its cutouts.
    Area,
    /// An area times a depth.
    Volume,
    /// The angle at the middle one of three points.
    Angle,
    /// A line from the centre of a circle to its edge.
    Radius,
    /// A line across a circle through its centre.
    Diameter,
    /// A number of markers.
    Count,
}

impl MeasureKind {
    /// The line endings a new dimension of this kind is drawn with: arrows at both ends of a
    /// length or a diameter, at the edge end of a radius, none for the other kinds.
    pub fn default_endings(self) -> (LineEnding, LineEnding) {
        match self {
            MeasureKind::Length | MeasureKind::Diameter => {
                (LineEnding::ClosedArrow, LineEnding::ClosedArrow)
            }
            MeasureKind::Radius => (LineEnding::None, LineEnding::ClosedArrow),
            _ => (LineEnding::None, LineEnding::None),
        }
    }

    pub const ALL: [MeasureKind; 9] = [
        MeasureKind::Length,
        MeasureKind::Polylength,
        MeasureKind::Perimeter,
        MeasureKind::Area,
        MeasureKind::Volume,
        MeasureKind::Angle,
        MeasureKind::Radius,
        MeasureKind::Diameter,
        MeasureKind::Count,
    ];

    /// The name used in the private data and as the default `/Subj`.
    pub fn name(self) -> &'static str {
        match self {
            MeasureKind::Length => "Length",
            MeasureKind::Polylength => "Polylength",
            MeasureKind::Perimeter => "Perimeter",
            MeasureKind::Area => "Area",
            MeasureKind::Volume => "Volume",
            MeasureKind::Angle => "Angle",
            MeasureKind::Radius => "Radius",
            MeasureKind::Diameter => "Diameter",
            MeasureKind::Count => "Count",
        }
    }

    pub fn from_name(name: &str) -> Option<MeasureKind> {
        MeasureKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The annotation subtype that carries this kind.
    pub fn carrier(self) -> AnnotKind {
        match self {
            MeasureKind::Length | MeasureKind::Radius | MeasureKind::Diameter => AnnotKind::Line,
            MeasureKind::Polylength | MeasureKind::Angle => AnnotKind::PolyLine,
            MeasureKind::Perimeter | MeasureKind::Area | MeasureKind::Volume => AnnotKind::Polygon,
            MeasureKind::Count => AnnotKind::Ink,
        }
    }

    /// The subtype written to the file, as Bluebeam Revu writes the kind (see the module
    /// documentation): a closed `PolyLine` for a Perimeter, a three-vertex `Polygon` for a
    /// Radius, a `Circle` for a Diameter and one small `Polygon` per marker for a Count; the
    /// [`carrier`](MeasureKind::carrier) for the rest.
    pub fn file_subtype(self) -> AnnotKind {
        match self {
            MeasureKind::Perimeter => AnnotKind::PolyLine,
            MeasureKind::Radius | MeasureKind::Count => AnnotKind::Polygon,
            MeasureKind::Diameter => AnnotKind::Circle,
            other => other.carrier(),
        }
    }

    /// The `/IT` intent written for the kind: the standard dimension intents where PDF has
    /// one, and Bluebeam Revu's own names (`PolygonVolume`, `PolygonRadius`,
    /// `CircleDimension`, `PolyLineAngle`, `PolygonCount`) where it has none. Always `Some`.
    pub fn intent(self) -> Option<&'static str> {
        Some(match self {
            MeasureKind::Length => "LineDimension",
            MeasureKind::Polylength | MeasureKind::Perimeter => "PolyLineDimension",
            MeasureKind::Area => "PolygonDimension",
            MeasureKind::Volume => "PolygonVolume",
            MeasureKind::Radius => "PolygonRadius",
            MeasureKind::Diameter => "CircleDimension",
            MeasureKind::Angle => "PolyLineAngle",
            MeasureKind::Count => "PolygonCount",
        })
    }

    /// The kind another application's `/IT` names on an annotation of subtype `subtype`,
    /// for the intents that say the kind outright. `PolyLineDimension` (a Polylength or a
    /// Perimeter, by whether it closes) and the Line-based radius and diameter of earlier
    /// versions are decided by the reader.
    pub(crate) fn from_intent(intent: &str, subtype: &AnnotKind) -> Option<MeasureKind> {
        Some(match (intent, subtype) {
            ("LineDimension", AnnotKind::Line) => MeasureKind::Length,
            ("PolygonDimension", AnnotKind::Polygon) => MeasureKind::Area,
            ("PolygonVolume", AnnotKind::Polygon) => MeasureKind::Volume,
            ("PolygonRadius", AnnotKind::Polygon) => MeasureKind::Radius,
            ("CircleDimension", AnnotKind::Circle) => MeasureKind::Diameter,
            ("PolyLineAngle", AnnotKind::PolyLine) => MeasureKind::Angle,
            ("PolygonCount", AnnotKind::Polygon) => MeasureKind::Count,
            _ => return None,
        })
    }

    /// Bluebeam Revu's `/MeasurementTypes` bit field for the kind, as Revu writes it: 130
    /// for the lengths (a radius included), 129 area, 132 volume, 384 diameter, 1152 angle,
    /// 128 count.
    pub fn measurement_types(self) -> i64 {
        match self {
            MeasureKind::Length
            | MeasureKind::Polylength
            | MeasureKind::Perimeter
            | MeasureKind::Radius => 130,
            MeasureKind::Area => 129,
            MeasureKind::Volume => 132,
            MeasureKind::Diameter => 384,
            MeasureKind::Angle => 1152,
            MeasureKind::Count => 128,
        }
    }

    /// True for the kinds drawn as one dimension line: Length, Radius and Diameter.
    pub fn is_line(self) -> bool {
        self.carrier() == AnnotKind::Line
    }

    /// True for the kinds whose value depends on the drawing's scale (all but Angle and
    /// Count). Every kind carries the scale it was taken at in the file when it has one.
    pub fn is_scaled(self) -> bool {
        !matches!(self, MeasureKind::Angle | MeasureKind::Count)
    }
}

/// Which scale a measurement was taken at.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum ScaleRef {
    /// The page's own scale.
    #[default]
    Page,
    /// The viewport of this `/Name` on the measurement's page.
    Viewport(String),
}

/// `/F` of a number format: how the last unit's value is rounded and shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NumberStyle {
    /// `/D`: decimal places.
    #[default]
    Decimal,
    /// `/F`: a fraction.
    Fraction,
    /// `/R`: rounded to a whole number.
    Round,
    /// `/T`: cut to a whole number.
    Truncate,
}

impl NumberStyle {
    pub fn pdf_name(self) -> &'static str {
        match self {
            NumberStyle::Decimal => "D",
            NumberStyle::Fraction => "F",
            NumberStyle::Round => "R",
            NumberStyle::Truncate => "T",
        }
    }

    pub fn from_pdf_name(name: &str) -> Option<NumberStyle> {
        match name {
            "D" => Some(NumberStyle::Decimal),
            "F" => Some(NumberStyle::Fraction),
            "R" => Some(NumberStyle::Round),
            "T" => Some(NumberStyle::Truncate),
            _ => None,
        }
    }
}

/// One PDF number format dictionary: one unit of a value shown in one or more units.
#[derive(Debug, Clone, PartialEq)]
pub struct NumberFormat {
    /// `/U`: the unit's label.
    pub label: String,
    /// `/C`: what a value is multiplied by to arrive in this unit. For the first format of an
    /// array that is the conversion from the source value; for a later one, from one of the
    /// previous unit (12 for inches after feet).
    pub factor: f64,
    /// `/D`: for [`NumberStyle::Decimal`] a power of ten (100 is two places); for
    /// [`NumberStyle::Fraction`] the denominator.
    pub precision: u32,
    /// `/F`.
    pub style: NumberStyle,
    /// `/FD`: a fraction is not reduced (`8/16` stays).
    pub fixed_denominator: bool,
    /// `/RT`: put between thousands; empty for nothing. (The PDF default is a comma.)
    pub thousands: String,
    /// `/RD`: the decimal mark.
    pub decimal: String,
    /// `/PS`: what is written just before the label.
    pub prefix_space: String,
    /// `/SS`: what is written just after the label.
    pub suffix_space: String,
    /// `/O /P`: the label comes before the number.
    pub label_first: bool,
}

impl NumberFormat {
    /// A decimal format with no thousands mark: `1.50 m` with `label = "m"` and
    /// `prefix_space = " "`.
    pub fn decimal(label: &str, prefix_space: &str, factor: f64, places: u8) -> NumberFormat {
        NumberFormat {
            label: label.to_string(),
            factor,
            precision: 10u32.pow(u32::from(places.min(9))),
            style: NumberStyle::Decimal,
            fixed_denominator: false,
            thousands: String::new(),
            decimal: ".".to_string(),
            prefix_space: prefix_space.to_string(),
            suffix_space: String::new(),
            label_first: false,
        }
    }

    /// Decimal places of a [`NumberStyle::Decimal`] format.
    pub fn places(&self) -> u8 {
        (f64::from(self.precision.max(1)).log10().round() as u8).min(12)
    }

    fn with_precision(mut self, precision: Precision) -> NumberFormat {
        match precision {
            Precision::Decimal(places) => {
                self.style = NumberStyle::Decimal;
                self.precision = 10u32.pow(u32::from(places.min(9)));
            }
            Precision::Fraction(den) => {
                self.style = NumberStyle::Fraction;
                self.precision = den.clamp(1, 64).next_power_of_two();
            }
        }
        self
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Digits with the thousands mark put in.
fn grouped(digits: &str, mark: &str) -> String {
    if mark.is_empty() || digits.len() <= 3 {
        return digits.to_string();
    }
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(mark);
        }
        out.push(c);
    }
    out
}

/// A whole number of units.
fn whole_text(value: f64, f: &NumberFormat) -> String {
    grouped(&format!("{value:.0}"), &f.thousands)
}

/// How a number format array is read. Bluebeam Revu writes `/FD true` in every format and
/// does not mean it: it reduces fractions all the same, and shows decimals without their
/// trailing zeros (`81.2°` at two places, `0 cu ft`). A measure dictionary of Revu's says so
/// with its `/TargetUnitConversion` key ([`MeasureSpec::target_unit_conversion`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Dialect {
    /// ISO 32000.
    #[default]
    Standard,
    /// As Revu shows values.
    Revu,
}

/// A decimal with the format's marks. `value` is not negative.
fn decimal_text(value: f64, f: &NumberFormat, dialect: Dialect) -> String {
    let places = f.places() as usize;
    let mut s = format!("{value:.places$}");
    if dialect == Dialect::Revu && s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    match s.split_once('.') {
        Some((whole, rest)) => format!("{}{}{rest}", grouped(whole, &f.thousands), f.decimal),
        None => grouped(&s, &f.thousands),
    }
}

/// `ticks` of `1/den` as `3 1/2`, `1/2`, `3` or `0`.
fn fraction_text(ticks: u64, den: u64, f: &NumberFormat, dialect: Dialect) -> String {
    let den = den.max(1);
    let whole = ticks / den;
    let mut num = ticks % den;
    let mut den = den;
    if num != 0 && (!f.fixed_denominator || dialect == Dialect::Revu) {
        let g = gcd(num, den);
        num /= g;
        den /= g;
    }
    let whole_text = grouped(&whole.to_string(), &f.thousands);
    match (whole, num) {
        (_, 0) => whole_text,
        (0, n) => format!("{n}/{den}"),
        (_, n) => format!("{whole_text} {n}/{den}"),
    }
}

/// The steps the last unit of a format is counted in: hundredths for two decimal places,
/// sixteenths for a fraction over 16, whole units otherwise.
fn quantum(f: &NumberFormat) -> f64 {
    match f.style {
        NumberStyle::Decimal => 10f64.powi(i32::from(f.places())),
        NumberStyle::Fraction => f64::from(f.precision.max(1)),
        NumberStyle::Round | NumberStyle::Truncate => 1.0,
    }
}

/// `ticks` (a count of [`quantum`] steps) of the format's unit, as text.
fn last_text(ticks: f64, f: &NumberFormat, dialect: Dialect) -> String {
    let q = quantum(f);
    match f.style {
        NumberStyle::Decimal => decimal_text(ticks / q, f, dialect),
        NumberStyle::Fraction => fraction_text(ticks as u64, q as u64, f, dialect),
        NumberStyle::Round | NumberStyle::Truncate => whole_text(ticks, f),
    }
}

fn with_label(number: &str, f: &NumberFormat) -> String {
    if f.label_first {
        format!("{}{}{}{number}", f.prefix_space, f.label, f.suffix_space)
    } else {
        format!("{number}{}{}{}", f.prefix_space, f.label, f.suffix_space)
    }
}

/// Formats `value` with a PDF number format array, as ISO 32000 describes: the value times the
/// first factor is in the first unit; when more formats follow, its whole part is shown in that
/// unit and the rest carried into the next one, and the last unit is shown to its precision.
///
/// Rounding is done once, in the last unit, and carried upwards, so 11 63/64 inches at
/// sixteenths is `1'-0"` and never `0'-12"`. A single decimal unit is rounded as Rust's
/// formatting rounds; this is what makes the result the same as
/// [`pl_geom::format_length`] for the formats [`MeasureSpec::new`] builds. An empty array
/// gives the bare number.
pub fn format_number(formats: &[NumberFormat], value: f64) -> String {
    format_in(formats, value, Dialect::Standard)
}

/// [`format_number`], read as `dialect` says.
pub(crate) fn format_in(formats: &[NumberFormat], value: f64, dialect: Dialect) -> String {
    let Some(first) = formats.first() else {
        return format!("{value}");
    };
    let v = value * first.factor;
    if !v.is_finite() {
        return with_label(&format!("{v}"), first).trim().to_string();
    }
    let last = &formats[formats.len() - 1];
    let abs = v.abs();

    if formats.len() == 1 && last.style == NumberStyle::Decimal {
        let text = decimal_text(abs, last, dialect);
        let zero = text.chars().all(|c| !c.is_ascii_digit() || c == '0');
        let sign = if v < 0.0 && !zero { "-" } else { "" };
        return format!("{sign}{}", with_label(&text, last))
            .trim()
            .to_string();
    }

    // Everything is counted in steps of the last unit. The factors between units are whole
    // numbers in practice (12 inches, 5280 feet); a format with others is still counted the
    // same way, in the last unit, which is where the rounding belongs.
    let q = quantum(last);
    let in_last: f64 = formats[1..].iter().fold(abs, |acc, f| acc * f.factor);
    let mut ticks = if last.style == NumberStyle::Truncate {
        (in_last * q + 1e-9).floor()
    } else {
        (in_last * q).round()
    };
    let sign = if v < 0.0 && ticks != 0.0 { "-" } else { "" };
    let mut out = String::from(sign);
    // Steps in one of each unit, from the first to the last.
    let mut sizes = vec![q; formats.len()];
    for i in (0..formats.len() - 1).rev() {
        sizes[i] = sizes[i + 1] * formats[i + 1].factor;
    }
    for (f, size) in formats.iter().zip(&sizes).take(formats.len() - 1) {
        let whole = if *size > 0.0 && size.is_finite() {
            (ticks / size).floor()
        } else {
            0.0
        };
        ticks -= whole * size;
        out.push_str(&with_label(&whole_text(whole, f), f));
    }
    out.push_str(&with_label(&last_text(ticks.max(0.0), last, dialect), last));
    out.trim().to_string()
}

/// How the values of a measurement are shown: the units and precision behind a
/// [`MeasureSpec`]'s number formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeasureFormat {
    pub length: LengthFormat,
    pub area: AreaFormat,
    /// Decimal places of an angle in degrees.
    pub angle_places: u8,
}

impl MeasureFormat {
    pub fn new(length: LengthFormat, area: AreaFormat) -> MeasureFormat {
        MeasureFormat {
            length,
            area,
            angle_places: 1,
        }
    }

    /// The usual way to show measurements taken at `scale`: in the unit its real side was
    /// stated in. Feet and inches to a sixteenth, decimal feet and metres to two places,
    /// millimetres whole; areas in square feet or square metres to two places.
    pub fn for_scale(scale: &Scale) -> MeasureFormat {
        let unit = scale.real_unit();
        let length = match unit {
            Unit::FeetInches => LengthFormat::architectural(16),
            Unit::Millimetre => LengthFormat::decimal(Unit::Millimetre, 0),
            Unit::Centimetre => LengthFormat::decimal(Unit::Centimetre, 1),
            other => LengthFormat::decimal(other, 2),
        };
        MeasureFormat::new(length, AreaFormat::new(AreaUnit::for_length(unit), 2))
    }
}

/// A value that is a whole number, or one over a whole number, but for rounding noise is made
/// exactly that.
fn snapped(v: f64) -> f64 {
    let near = |x: f64| (x - x.round()).abs() <= 1e-12 * x.abs().max(1.0) && x.round() >= 1.0;
    if near(v) {
        v.round()
    } else if v > 0.0 && near(1.0 / v) {
        1.0 / (1.0 / v).round()
    } else {
        v
    }
}

/// The label a length unit is written with, and what goes between the number and it.
fn unit_label(unit: Unit) -> (&'static str, &'static str) {
    match unit {
        Unit::Inch => ("\"", ""),
        Unit::Foot | Unit::FeetInches => ("'", ""),
        other => (other.abbreviation(), " "),
    }
}

/// Metres in one of the unit a label names; `None` for a label that is not a length unit this
/// crate knows.
pub(crate) fn label_metres(label: &str) -> Option<f64> {
    let l = label.trim().to_ascii_lowercase();
    Some(match l.as_str() {
        "'" | "ft" | "ft." | "foot" | "feet" => 0.3048,
        "\"" | "in" | "in." | "inch" | "inches" => 0.0254,
        "yd" | "yard" | "yards" => 0.9144,
        "mi" | "mile" | "miles" => 1609.344,
        "pt" | "point" | "points" => 0.0254 / 72.0,
        "mm" => 0.001,
        "cm" => 0.01,
        "m" | "metre" | "metres" | "meter" | "meters" => 1.0,
        "km" => 1000.0,
        _ => return None,
    })
}

/// A PDF rectilinear measure dictionary (`/Type /Measure /Subtype /RL`), typed: the scale of a
/// page region and how values measured in it are shown.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasureSpec {
    /// `/R`: the scale as text, `1 in = 20 ft`.
    pub ratio: String,
    /// `/X`: the first factor converts page units along x to real units; the array as a whole
    /// formats a change along x.
    pub x: Vec<NumberFormat>,
    /// `/Y`: the same along y, when the two axes differ.
    pub y: Option<Vec<NumberFormat>>,
    /// `/D`: formats a distance, given in the units of `/X`.
    pub distance: Vec<NumberFormat>,
    /// `/A`: formats an area, given in the square of the units of `/X`.
    pub area: Vec<NumberFormat>,
    /// `/T`: formats an angle, given in degrees. May be empty.
    pub angle: Vec<NumberFormat>,
    /// `/S`: formats a slope. May be empty.
    pub slope: Vec<NumberFormat>,
    /// `/O`: the origin of the measuring coordinate system.
    pub origin: Option<Point>,
    /// `/CYX`: converts the units of `/Y` to the units of `/X`.
    pub cyx: Option<f64>,
    /// `/V` (Bluebeam Revu): formats a volume, given in the cube of the units of `/X`. Empty
    /// in the dictionaries this crate builds, whose volumes are shown as
    /// [`Measurement::volume`] says.
    pub volume: Vec<NumberFormat>,
    /// `/TargetUnitConversion` (Bluebeam Revu), kept as it was read. Its presence marks a
    /// dictionary of Revu's, whose number formats are read the way Revu shows them (see
    /// [`format_number`]: fractions reduced and decimals without trailing zeros, whatever
    /// `/FD` says).
    pub target_unit_conversion: Option<f64>,
}

impl MeasureSpec {
    /// The measure dictionary for `scale` on a page with the given `/UserUnit`, showing values
    /// as `format` says. The number formats are built so that [`MeasureSpec::format_distance`]
    /// gives the string [`pl_geom::format_length`] gives.
    pub fn new(scale: &Scale, user_unit: f64, format: &MeasureFormat) -> MeasureSpec {
        let unit = format.length.unit;
        let unit_metres = unit.metres_per_unit();
        let (label, gap) = unit_label(unit);
        let precision = format.length.precision;
        // The units of a length, the first with a factor of 1.
        let mut units = if unit == Unit::FeetInches {
            let mut feet = NumberFormat::decimal(label, gap, 1.0, 0);
            feet.suffix_space = "-".to_string();
            let inches = NumberFormat::decimal("\"", "", 12.0, 0).with_precision(precision);
            vec![feet, inches]
        } else {
            vec![NumberFormat::decimal(label, gap, 1.0, 0).with_precision(precision)]
        };
        let distance = units.clone();
        units[0].factor = scale.metres_per_page_unit(user_unit) / unit_metres;
        let area_unit = format.area.unit;
        let area = vec![NumberFormat::decimal(
            area_unit.abbreviation(),
            " ",
            snapped(unit_metres * unit_metres / area_unit.sq_metres_per_unit()),
            format.area.places,
        )];
        MeasureSpec {
            ratio: scale.pdf_ratio(),
            x: units,
            y: None,
            distance,
            area,
            angle: vec![NumberFormat::decimal(
                "\u{b0}",
                "",
                1.0,
                format.angle_places,
            )],
            slope: Vec::new(),
            origin: None,
            cyx: None,
            volume: Vec::new(),
            target_unit_conversion: None,
        }
    }

    /// How this dictionary's number formats are read.
    pub(crate) fn dialect(&self) -> Dialect {
        if self.target_unit_conversion.is_some() {
            Dialect::Revu
        } else {
            Dialect::Standard
        }
    }

    /// As [`MeasureSpec::new`], with a scale of its own along y.
    pub fn with_axes(x: &Scale, y: &Scale, user_unit: f64, format: &MeasureFormat) -> MeasureSpec {
        let mut spec = MeasureSpec::new(x, user_unit, format);
        let mut along_y = spec.x.clone();
        along_y[0].factor =
            y.metres_per_page_unit(user_unit) / format.length.unit.metres_per_unit();
        spec.y = Some(along_y);
        spec.cyx = Some(1.0);
        spec.ratio = format!("{} in X {} in Y", x.pdf_ratio(), y.pdf_ratio());
        spec
    }

    /// Real units (the units of `/X`) per page unit along x.
    pub fn x_factor(&self) -> f64 {
        self.x.first().map_or(1.0, |f| f.factor)
    }

    /// Real units (the units of `/X`) per page unit along y.
    pub fn y_factor(&self) -> f64 {
        match self.y.as_ref().and_then(|y| y.first()) {
            Some(f) => f.factor * self.cyx.unwrap_or(1.0),
            None => self.x_factor(),
        }
    }

    /// Metres in one of the units of `/X`, when its label names a unit this crate knows.
    pub fn metres_per_unit(&self) -> Option<f64> {
        self.x.first().and_then(|f| label_metres(&f.label))
    }

    /// True when the real unit is feet, inches, yards or miles.
    pub fn is_imperial(&self) -> bool {
        self.metres_per_unit()
            .is_some_and(|m| [0.3048, 0.0254, 0.9144, 1609.344].contains(&m))
    }

    /// The drawing scale this dictionary states, on a page with the given `/UserUnit`: `/R`
    /// when it reads as a scale that agrees with the `/X` factor, otherwise the scale that
    /// factor amounts to. `None` when the unit of `/X` is not one this crate knows.
    pub fn scale(&self, user_unit: f64) -> Option<Scale> {
        let unit_metres = self.metres_per_unit()?;
        let factor = self.x_factor() * unit_metres / pl_geom::points_to_metres(user_unit);
        let stated = self.ratio.split(" in X ").next().unwrap_or("");
        if let Ok(scale) = Scale::parse(stated) {
            if (scale.factor() / factor - 1.0).abs() < 1e-5 {
                return Some(scale);
            }
        }
        let unit = if self.is_imperial() {
            Unit::Foot
        } else {
            Unit::Millimetre
        };
        Scale::from_factor(factor, unit).ok()
    }

    /// A page-space displacement in real units.
    fn real(&self, v: Vec2) -> Vec2 {
        Vec2::new(v.x * self.x_factor(), v.y * self.y_factor())
    }

    /// The real distance from `a` to `b`, in the units of `/X`.
    pub fn distance_between(&self, a: Point, b: Point) -> f64 {
        self.real(b - a).hypot()
    }

    /// An area in square page units as a real one, in the square of the units of `/X`.
    pub fn real_area(&self, square_page_units: f64) -> f64 {
        square_page_units * self.x_factor() * self.y_factor()
    }

    /// A distance in the units of `/X` as text.
    pub fn format_distance(&self, units: f64) -> String {
        format_in(&self.distance, units, self.dialect())
    }

    /// An area in the square of the units of `/X` as text.
    pub fn format_area(&self, square_units: f64) -> String {
        format_in(&self.area, square_units, self.dialect())
    }

    /// An angle in degrees as text; with one decimal place when the dictionary has no `/T`.
    pub fn format_angle(&self, degrees: f64) -> String {
        if self.angle.is_empty() {
            default_angle_text(degrees)
        } else {
            format_in(&self.angle, degrees, self.dialect())
        }
    }

    /// A volume in the cube of the units of `/X` as text, with `/V`; `None` when the
    /// dictionary has no `/V`.
    pub fn format_volume(&self, cubic_units: f64) -> Option<String> {
        (!self.volume.is_empty()).then(|| format_in(&self.volume, cubic_units, self.dialect()))
    }
}

fn default_angle_text(degrees: f64) -> String {
    format_number(&[NumberFormat::decimal("\u{b0}", "", 1.0, 1)], degrees)
}

/// Where the caption of a dimension line sits (`/CP`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CaptionPosition {
    /// In the line, which is broken around it.
    Inline,
    /// Above the line.
    #[default]
    Top,
}

impl CaptionPosition {
    pub fn pdf_name(self) -> &'static str {
        match self {
            CaptionPosition::Inline => "Inline",
            CaptionPosition::Top => "Top",
        }
    }

    pub fn from_pdf_name(name: &str) -> Option<CaptionPosition> {
        match name {
            "Inline" => Some(CaptionPosition::Inline),
            "Top" => Some(CaptionPosition::Top),
            _ => None,
        }
    }
}

/// The marker a count is drawn with. These are Plumbline's own shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CountSymbol {
    /// A solid disc.
    #[default]
    Dot,
    Ring,
    Square,
    Diamond,
    Triangle,
    Hexagon,
    Star,
    /// A diagonal cross.
    Cross,
    /// An upright cross.
    Plus,
    /// A tick.
    Check,
}

impl CountSymbol {
    pub const ALL: [CountSymbol; 10] = [
        CountSymbol::Dot,
        CountSymbol::Ring,
        CountSymbol::Square,
        CountSymbol::Diamond,
        CountSymbol::Triangle,
        CountSymbol::Hexagon,
        CountSymbol::Star,
        CountSymbol::Cross,
        CountSymbol::Plus,
        CountSymbol::Check,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CountSymbol::Dot => "Dot",
            CountSymbol::Ring => "Ring",
            CountSymbol::Square => "Square",
            CountSymbol::Diamond => "Diamond",
            CountSymbol::Triangle => "Triangle",
            CountSymbol::Hexagon => "Hexagon",
            CountSymbol::Star => "Star",
            CountSymbol::Cross => "Cross",
            CountSymbol::Plus => "Plus",
            CountSymbol::Check => "Check",
        }
    }

    pub fn from_name(name: &str) -> Option<CountSymbol> {
        CountSymbol::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// The measurement side of an annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    pub kind: MeasureKind,
    /// Which scale of the page `spec` was taken from.
    pub scale: ScaleRef,
    /// `/Measure`: the scale and number formats the value is worked out and shown with. `None`
    /// for an Angle or a Count that needs none, and then a length is in page units.
    pub spec: Option<MeasureSpec>,
    /// `/LL`: how far the dimension line of a Length, Radius or Diameter is moved off the
    /// measured points, to the left of the direction from the first to the second (negative:
    /// to the right). 0 draws it through the points, without leader lines.
    pub leader: f64,
    /// `/LLE`: how far the leader lines run on past the dimension line.
    pub leader_extension: f64,
    /// `/LLO`: the gap between the measured points and the start of the leader lines.
    pub leader_offset: f64,
    /// `/Cap` for a line; for the other kinds, whether the value is drawn at all.
    pub caption: bool,
    /// `/CP`.
    pub caption_position: CaptionPosition,
    /// `/CO`: the caption moved along the line (x) and away from it (y).
    pub caption_offset: Vec2,
    /// The holes of an Area or Volume: rings inside the outline that are not counted and not
    /// painted (even-odd).
    pub cutouts: Vec<Vec<Point>>,
    /// The depth of a Volume, in metres.
    pub depth: Option<f64>,
    /// How a Volume is shown.
    pub volume: VolumeFormat,
    /// The marker of a Count and its size across, in page units.
    pub symbol: CountSymbol,
    pub symbol_size: f64,
    /// The font size of the caption, in page units.
    pub text_size: f64,
    /// The page's `/Rotate` the caption is drawn upright for: 0, 90, 180 or 270.
    pub rotation: u16,
}

impl Measurement {
    /// A measurement of `kind` with the default presentation: a caption above the line (none
    /// for a Count), no leader lines, 9-unit text.
    pub fn new(kind: MeasureKind, spec: Option<MeasureSpec>) -> Measurement {
        let imperial = spec.as_ref().is_some_and(MeasureSpec::is_imperial);
        Measurement {
            kind,
            scale: ScaleRef::Page,
            leader: 0.0,
            leader_extension: 0.0,
            leader_offset: 0.0,
            caption: kind != MeasureKind::Count,
            caption_position: CaptionPosition::Top,
            caption_offset: Vec2::ZERO,
            cutouts: Vec::new(),
            depth: None,
            volume: VolumeFormat::new(
                if imperial {
                    VolumeUnit::CuYard
                } else {
                    VolumeUnit::CuMetre
                },
                2,
            ),
            symbol: CountSymbol::default(),
            symbol_size: 10.0,
            text_size: 9.0,
            rotation: 0,
            spec,
        }
    }

    /// True when the two would be written the same: equal but for differences below what a
    /// PDF number holds.
    pub(crate) fn same_as(&self, other: &Measurement) -> bool {
        let near = |a: f64, b: f64| (a - b).abs() <= 0.002;
        let rings = self.cutouts.len() == other.cutouts.len()
            && self.cutouts.iter().zip(&other.cutouts).all(|(a, b)| {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(p, q)| near(p.x, q.x) && near(p.y, q.y))
            });
        self.kind == other.kind
            && self.scale == other.scale
            && self.spec == other.spec
            && near(self.leader, other.leader)
            && near(self.leader_extension, other.leader_extension)
            && near(self.leader_offset, other.leader_offset)
            && self.caption == other.caption
            && self.caption_position == other.caption_position
            && near(self.caption_offset.x, other.caption_offset.x)
            && near(self.caption_offset.y, other.caption_offset.y)
            && rings
            && self.depth == other.depth
            && self.volume == other.volume
            && self.symbol == other.symbol
            && self.symbol_size == other.symbol_size
            && self.text_size == other.text_size
            && self.rotation == other.rotation
    }
}

/// What a measurement amounts to, in SI units. A field is `None` when the kind has no such
/// quantity, or when the measurement's real unit is one this crate cannot convert to metres.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Quantities {
    /// Metres: the length of a Length or Polylength, the perimeter of a Perimeter, Area or
    /// Volume (the outer ring only), the radius of a Radius, the diameter of a Diameter.
    pub length: Option<f64>,
    /// Square metres, cutouts taken off.
    pub area: Option<f64>,
    /// Cubic metres: the area times the depth.
    pub volume: Option<f64>,
    /// Radians, in `[0, pi]`.
    pub angle: Option<f64>,
    /// How many markers.
    pub count: Option<usize>,
}

/// The marker points of a count: the first point of each stroke.
fn marker_points(geometry: &Geometry) -> Vec<Point> {
    match geometry {
        Geometry::Ink(strokes) => strokes.iter().filter_map(|s| s.first().copied()).collect(),
        Geometry::Points(points) => points.clone(),
        _ => Vec::new(),
    }
}

/// The page-space geometry of a measurement in real terms: lengths times `kx`/`ky` per axis.
struct Real {
    kx: f64,
    ky: f64,
}

impl Real {
    fn v(&self, a: Point, b: Point) -> Vec2 {
        Vec2::new((b.x - a.x) * self.kx, (b.y - a.y) * self.ky)
    }

    fn run(&self, points: &[Point], closed: bool) -> f64 {
        if self.kx == self.ky {
            // The page-space sums, scaled: the same arithmetic as pl-geom's.
            return self.kx
                * if closed {
                    polygon_perimeter(points)
                } else {
                    polyline_length(points)
                };
        }
        let mut total: f64 = points.windows(2).map(|w| self.v(w[0], w[1]).hypot()).sum();
        if closed && points.len() > 1 {
            total += self.v(points[points.len() - 1], points[0]).hypot();
        }
        total
    }

    fn area(&self, outer: &[Point], holes: &[Vec<Point>]) -> f64 {
        pl_geom::polygon_area_with_holes(outer, holes) * self.kx * self.ky
    }

    fn angle(&self, a: Point, vertex: Point, b: Point) -> f64 {
        let (u, v) = (self.v(vertex, a), self.v(vertex, b));
        if u.hypot2() == 0.0 || v.hypot2() == 0.0 {
            return 0.0;
        }
        u.cross(v).abs().atan2(u.dot(v))
    }
}

/// The plain numbers of a measurement, in the units `Real` works in.
#[derive(Default)]
struct Raw {
    length: Option<f64>,
    area: Option<f64>,
    angle: Option<f64>,
    count: Option<usize>,
}

fn raw(kind: MeasureKind, geometry: &Geometry, cutouts: &[Vec<Point>], real: &Real) -> Raw {
    let mut out = Raw::default();
    match (kind, geometry) {
        (
            MeasureKind::Length | MeasureKind::Radius | MeasureKind::Diameter,
            Geometry::Line { a, b },
        ) => out.length = Some(real.v(*a, *b).hypot()),
        (MeasureKind::Polylength, Geometry::Points(points)) => {
            out.length = Some(real.run(points, false));
        }
        (MeasureKind::Perimeter, Geometry::Points(points)) => {
            out.length = Some(real.run(points, true));
        }
        (MeasureKind::Area | MeasureKind::Volume, Geometry::Points(points)) => {
            out.length = Some(real.run(points, true));
            out.area = Some(real.area(points, cutouts));
        }
        (MeasureKind::Angle, Geometry::Points(points)) if points.len() >= 3 => {
            out.angle = Some(real.angle(points[0], points[1], points[2]));
        }
        (MeasureKind::Count, g) => out.count = Some(marker_points(g).len()),
        _ => {}
    }
    out
}

impl Quantities {
    /// The quantities of a measurement worked out from nothing but its geometry, a scale and
    /// the page's `/UserUnit`. `depth` is in metres.
    pub fn compute(
        kind: MeasureKind,
        geometry: &Geometry,
        cutouts: &[Vec<Point>],
        depth: Option<f64>,
        scale: &Scale,
        user_unit: f64,
    ) -> Quantities {
        let k = scale.metres_per_page_unit(user_unit);
        let r = raw(kind, geometry, cutouts, &Real { kx: k, ky: k });
        Quantities {
            length: r.length,
            area: r.area,
            volume: match kind {
                MeasureKind::Volume => r.area.zip(depth).map(|(a, d)| a * d),
                _ => None,
            },
            angle: r.angle,
            count: r.count,
        }
    }
}

fn dimension_style(a: &mut Annotation, start: LineEnding, end: LineEnding) {
    a.style.line_start = start;
    a.style.line_end = end;
    // Closed endings are filled with the line's colour.
    a.style.fill = a.style.stroke;
}

impl Annotation {
    fn measured(
        kind: MeasureKind,
        page: usize,
        geometry: Geometry,
        spec: Option<MeasureSpec>,
        origin: &Origin,
    ) -> Annotation {
        let mut a = Annotation::new(kind.carrier(), page, geometry, origin);
        a.meta.subject = Some(kind.name().to_string());
        a.measure = Some(Measurement::new(kind, spec));
        a
    }

    /// A length: the distance from `a` to `b`, drawn as a dimension line with an arrow at each
    /// end and the value above it.
    pub fn measure_length(
        page: usize,
        a: Point,
        b: Point,
        spec: MeasureSpec,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Length,
            page,
            Geometry::Line { a, b },
            Some(spec),
            origin,
        );
        dimension_style(&mut m, LineEnding::ClosedArrow, LineEnding::ClosedArrow);
        m.sync_measurement();
        m
    }

    /// The length along an open run of segments.
    pub fn measure_polylength(
        page: usize,
        points: Vec<Point>,
        spec: MeasureSpec,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Polylength,
            page,
            Geometry::Points(points),
            Some(spec),
            origin,
        );
        m.sync_measurement();
        m
    }

    /// The length around a closed outline.
    pub fn measure_perimeter(
        page: usize,
        points: Vec<Point>,
        spec: MeasureSpec,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Perimeter,
            page,
            Geometry::Points(points),
            Some(spec),
            origin,
        );
        m.sync_measurement();
        m
    }

    /// The area inside `points`, less the `cutouts` (rings inside the outline).
    pub fn measure_area(
        page: usize,
        points: Vec<Point>,
        cutouts: Vec<Vec<Point>>,
        spec: MeasureSpec,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Area,
            page,
            Geometry::Points(points),
            Some(spec),
            origin,
        );
        if let Some(measure) = m.measure.as_mut() {
            measure.cutouts = cutouts;
        }
        m.sync_measurement();
        m
    }

    /// A volume: the area inside `points`, less the `cutouts`, times `depth` metres.
    pub fn measure_volume(
        page: usize,
        points: Vec<Point>,
        cutouts: Vec<Vec<Point>>,
        depth: f64,
        spec: MeasureSpec,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Volume,
            page,
            Geometry::Points(points),
            Some(spec),
            origin,
        );
        if let Some(measure) = m.measure.as_mut() {
            measure.cutouts = cutouts;
            measure.depth = Some(depth);
        }
        m.sync_measurement();
        m
    }

    /// The angle at `vertex` between the rays to `a` and to `b`. `spec` matters only on a
    /// drawing whose axes have different scales, and for how the angle is shown.
    pub fn measure_angle(
        page: usize,
        a: Point,
        vertex: Point,
        b: Point,
        spec: Option<MeasureSpec>,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Angle,
            page,
            Geometry::Points(vec![a, vertex, b]),
            spec,
            origin,
        );
        m.sync_measurement();
        m
    }

    /// A radius: the line from the `centre` of a circle to a point on its `edge`. The circle
    /// is drawn with it.
    pub fn measure_radius(
        page: usize,
        centre: Point,
        edge: Point,
        spec: MeasureSpec,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Radius,
            page,
            Geometry::Line { a: centre, b: edge },
            Some(spec),
            origin,
        );
        dimension_style(&mut m, LineEnding::None, LineEnding::ClosedArrow);
        m.sync_measurement();
        m
    }

    /// A diameter: the line across a circle from `a` to `b`, through its centre. The circle is
    /// drawn with it.
    pub fn measure_diameter(
        page: usize,
        a: Point,
        b: Point,
        spec: MeasureSpec,
        origin: &Origin,
    ) -> Annotation {
        let mut m = Annotation::measured(
            MeasureKind::Diameter,
            page,
            Geometry::Line { a, b },
            Some(spec),
            origin,
        );
        dimension_style(&mut m, LineEnding::ClosedArrow, LineEnding::ClosedArrow);
        m.sync_measurement();
        m
    }

    /// A count: one annotation with a marker at each of `points`.
    pub fn count(
        page: usize,
        points: &[Point],
        symbol: CountSymbol,
        origin: &Origin,
    ) -> Annotation {
        let strokes = points.iter().map(|p| vec![*p, *p]).collect();
        let mut m = Annotation::measured(
            MeasureKind::Count,
            page,
            Geometry::Ink(strokes),
            None,
            origin,
        );
        if let Some(measure) = m.measure.as_mut() {
            measure.symbol = symbol;
        }
        m.sync_measurement();
        m
    }

    /// The measurement this annotation is, when it is one this crate draws: it holds a
    /// [`Measurement`] and its geometry is the kind's own.
    pub fn measurement(&self) -> Option<&Measurement> {
        let m = self.measure.as_ref()?;
        let fits = self.kind == m.kind.carrier()
            && match (m.kind, &self.geometry) {
                (k, Geometry::Line { .. }) => k.is_line(),
                (MeasureKind::Polylength, Geometry::Points(p)) => p.len() >= 2,
                (MeasureKind::Angle, Geometry::Points(p)) => p.len() == 3,
                (
                    MeasureKind::Perimeter | MeasureKind::Area | MeasureKind::Volume,
                    Geometry::Points(p),
                ) => p.len() >= 3,
                (MeasureKind::Count, Geometry::Ink(_)) => true,
                _ => false,
            };
        fits.then_some(m)
    }

    /// The marker points of a count; empty for anything else.
    pub fn count_points(&self) -> Vec<Point> {
        match self.measurement() {
            Some(m) if m.kind == MeasureKind::Count => marker_points(&self.geometry),
            _ => Vec::new(),
        }
    }

    /// The group a count belongs to: the counts that share it are one count in a list of
    /// quantities. A count of Plumbline's is one annotation however many markers it has, so
    /// its group is its own id. Bluebeam Revu writes one annotation per marker and groups them
    /// with the standard annotation group (`/IRT` to the first, `/RT /Group`): such a marker's
    /// group is the first one's id. `None` for anything but a count.
    pub fn count_group(&self) -> Option<AnnotId> {
        let m = self.measurement()?;
        if m.kind != MeasureKind::Count {
            return None;
        }
        match (&self.meta.reply_to, self.meta.reply_type.as_deref()) {
            (Some(head), Some("Group")) => Some(head.clone()),
            _ => Some(self.id.clone()),
        }
    }

    /// Adds a marker to a count. Returns false, changing nothing, for anything else.
    pub fn add_count_point(&mut self, p: Point) -> bool {
        let is_count = self
            .measurement()
            .is_some_and(|m| m.kind == MeasureKind::Count);
        match (&mut self.geometry, is_count) {
            (Geometry::Ink(strokes), true) if p.x.is_finite() && p.y.is_finite() => {
                strokes.push(vec![p, p]);
                self.sync_measurement();
                true
            }
            _ => false,
        }
    }

    /// Takes the marker at `index` off a count. Returns false, changing nothing, when there
    /// is no such marker.
    pub fn remove_count_point(&mut self, index: usize) -> bool {
        let is_count = self
            .measurement()
            .is_some_and(|m| m.kind == MeasureKind::Count);
        match (&mut self.geometry, is_count) {
            (Geometry::Ink(strokes), true) if index < strokes.len() => {
                strokes.remove(index);
                self.sync_measurement();
                true
            }
            _ => false,
        }
    }

    /// The numbers of a measurement in the real units of its `/Measure` (page units without
    /// one): a length, an area in those units squared, an angle in radians, a count.
    fn raw_measure(&self) -> Option<(Raw, &Measurement)> {
        let m = self.measurement()?;
        let real = match &m.spec {
            Some(spec) => Real {
                kx: spec.x_factor(),
                ky: spec.y_factor(),
            },
            None => Real { kx: 1.0, ky: 1.0 },
        };
        Some((raw(m.kind, &self.geometry, &m.cutouts, &real), m))
    }

    /// What the measurement amounts to, in SI units, from its geometry and its scale. `None`
    /// for an annotation that is not a measurement.
    pub fn quantities(&self) -> Option<Quantities> {
        let (r, m) = self.raw_measure()?;
        let unit = m.spec.as_ref().and_then(MeasureSpec::metres_per_unit);
        let length = r.length.zip(unit).map(|(l, u)| l * u);
        let area = r.area.zip(unit).map(|(a, u)| a * u * u);
        Some(Quantities {
            length,
            area,
            volume: match m.kind {
                MeasureKind::Volume => area.zip(m.depth).map(|(a, d)| a * d),
                _ => None,
            },
            angle: r.angle,
            count: r.count,
        })
    }

    /// The value of the measurement as text, in the units and precision of its `/Measure`:
    /// what its caption shows (`R` or `Ø` before a radius or a diameter). `/Contents` is
    /// [`Annotation::measure_value_text`], without them, as Revu writes it. `None` for an
    /// annotation that is not a measurement.
    pub fn measure_text(&self) -> Option<String> {
        let value = self.measure_value_text()?;
        Some(match self.measurement()?.kind {
            MeasureKind::Radius => format!("R {value}"),
            MeasureKind::Diameter => format!("\u{d8} {value}"),
            _ => value,
        })
    }

    /// The value of the measurement as text in the number formats of its `/Measure`, without
    /// the `R` or `Ø` that [`Annotation::measure_text`] puts before a radius or a diameter:
    /// what Bluebeam Revu writes as the `/Contents` of a measurement. A volume with a `/V`
    /// (Revu's volume formats) is shown with it, in the cube of the units of `/X`.
    pub fn measure_value_text(&self) -> Option<String> {
        let (r, m) = self.raw_measure()?;
        let spec = m.spec.as_ref();
        let distance = |units: f64| match spec {
            Some(s) => s.format_distance(units),
            None => format_number(&[NumberFormat::decimal("pt", " ", 1.0, 2)], units),
        };
        Some(match m.kind {
            MeasureKind::Length
            | MeasureKind::Polylength
            | MeasureKind::Perimeter
            | MeasureKind::Radius
            | MeasureKind::Diameter => distance(r.length?),
            MeasureKind::Area => spec?.format_area(r.area?),
            MeasureKind::Volume => {
                let spec = spec?;
                let unit = spec.metres_per_unit();
                match (unit, m.depth) {
                    (Some(u), Some(depth)) => match spec.format_volume(r.area? * depth / u) {
                        Some(text) => text,
                        None => m.volume.format(r.area? * u * u * depth),
                    },
                    // Without a depth, or in a unit that cannot be converted, the area is
                    // what can be said.
                    _ => spec.format_area(r.area?),
                }
            }
            MeasureKind::Angle => {
                let degrees = r.angle?.to_degrees();
                match spec {
                    Some(s) => s.format_angle(degrees),
                    None => default_angle_text(degrees),
                }
            }
            MeasureKind::Count => r.count?.to_string(),
        })
    }

    /// Brings `/Contents` in step with the measurement's value. A save writes the value
    /// whether or not this has been called; calling it after an edit keeps the model the same
    /// as what a reload gives. Does nothing for an annotation that is not a measurement.
    pub fn sync_measurement(&mut self) {
        // `/Contents` holds the bare value, as Revu writes it (no `R` or `Ø` before a radius
        // or a diameter; the caption drawn has them).
        if let Some(text) = self.measure_value_text() {
            self.meta.contents = Some(text);
        }
    }

    /// Puts a measurement on another scale: `scale` says where `spec` comes from (the page or
    /// a viewport). Does nothing for an annotation that holds no measurement.
    pub fn apply_scale(&mut self, scale: ScaleRef, spec: MeasureSpec) {
        if let Some(m) = self.measure.as_mut() {
            m.scale = scale;
            m.spec = Some(spec);
            self.sync_measurement();
        }
    }

    /// The page `/Rotate` a measurement's caption reads upright for; see
    /// [`Annotation::set_rotation`].
    pub(crate) fn set_measure_rotation(&mut self, degrees: i32) {
        if let Some(m) = self.measure.as_mut() {
            m.rotation = normalise_rotation(i64::from(degrees));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::PdfDate;
    use pl_geom::format_length;

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    fn spec(scale: &str, user_unit: f64, length: LengthFormat, area: AreaFormat) -> MeasureSpec {
        MeasureSpec::new(
            &Scale::parse(scale).unwrap(),
            user_unit,
            &MeasureFormat::new(length, area),
        )
    }

    fn eng() -> MeasureSpec {
        spec(
            "1 in = 20 ft",
            1.0,
            LengthFormat::decimal(Unit::Foot, 2),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        )
    }

    #[test]
    fn number_formats_follow_the_specification_example() {
        // ISO 32000-1, 12.9.2: 1.4505 mi shown in miles, feet and inches to an eighth.
        let mut mi = NumberFormat::decimal("mi", " ", 1.0, 0);
        mi.suffix_space = " ".into();
        mi.thousands = ",".into();
        let mut ft = NumberFormat::decimal("ft", " ", 5280.0, 0);
        ft.suffix_space = " ".into();
        ft.thousands = ",".into();
        let mut inch = NumberFormat::decimal("in", " ", 12.0, 0);
        inch.style = NumberStyle::Fraction;
        inch.precision = 8;
        let formats = [mi, ft, inch];
        assert_eq!(format_number(&formats, 1.4505), "1 mi 2,378 ft 7 5/8 in");
        assert_eq!(format_number(&formats, 0.0), "0 mi 0 ft 0 in");
        assert_eq!(format_number(&formats, -1.5), "-1 mi 2,640 ft 0 in");
        // Area in acres from square miles.
        let acres = NumberFormat::decimal("acres", " ", 640.0, 2);
        assert_eq!(format_number(&[acres], 0.25), "160.00 acres");
    }

    #[test]
    fn number_format_styles_marks_and_label_order() {
        let base = NumberFormat::decimal("m", " ", 1.0, 2);
        assert_eq!(
            format_number(std::slice::from_ref(&base), 1234.5),
            "1234.50 m"
        );
        let mut marked = base.clone();
        marked.thousands = ",".into();
        assert_eq!(
            format_number(std::slice::from_ref(&marked), 1234567.891),
            "1,234,567.89 m"
        );
        marked.thousands = ".".into();
        marked.decimal = ",".into();
        assert_eq!(
            format_number(std::slice::from_ref(&marked), 1234.5),
            "1.234,50 m"
        );
        let mut round = base.clone();
        round.style = NumberStyle::Round;
        assert_eq!(format_number(std::slice::from_ref(&round), 2.5), "3 m");
        assert_eq!(format_number(std::slice::from_ref(&round), -2.4), "-2 m");
        round.style = NumberStyle::Truncate;
        assert_eq!(format_number(std::slice::from_ref(&round), 2.99), "2 m");
        assert_eq!(format_number(std::slice::from_ref(&round), 3.0), "3 m");
        let mut fixed = NumberFormat::decimal("\"", "", 1.0, 0);
        fixed.style = NumberStyle::Fraction;
        fixed.precision = 16;
        assert_eq!(format_number(std::slice::from_ref(&fixed), 3.5), "3 1/2\"");
        fixed.fixed_denominator = true;
        assert_eq!(format_number(std::slice::from_ref(&fixed), 3.5), "3 8/16\"");
        // A denominator that is not a power of two (another application's).
        fixed.fixed_denominator = false;
        fixed.precision = 10;
        assert_eq!(format_number(std::slice::from_ref(&fixed), 0.4), "2/5\"");
        let mut prefix = NumberFormat::decimal("$", "", 1.0, 2);
        prefix.label_first = true;
        assert_eq!(format_number(std::slice::from_ref(&prefix), 12.5), "$12.50");
        // No "-0.00", and the factor is applied.
        assert_eq!(format_number(std::slice::from_ref(&base), -0.001), "0.00 m");
        let cm = NumberFormat::decimal("cm", " ", 100.0, 1);
        assert_eq!(format_number(&[cm], 0.255), "25.5 cm");
        assert_eq!(format_number(&[], 2.5), "2.5");
        assert_eq!(
            format_number(std::slice::from_ref(&base), f64::NAN),
            "NaN m"
        );
    }

    #[test]
    fn number_format_arrays_give_the_strings_pl_geom_gives() {
        let formats = [
            LengthFormat::architectural(16),
            LengthFormat::architectural(64),
            LengthFormat::architectural(1),
            LengthFormat::new(Unit::FeetInches, Precision::Decimal(2)),
            LengthFormat::decimal(Unit::Foot, 2),
            LengthFormat::new(Unit::Foot, Precision::Fraction(8)),
            LengthFormat::decimal(Unit::Inch, 3),
            LengthFormat::new(Unit::Inch, Precision::Fraction(32)),
            LengthFormat::decimal(Unit::Millimetre, 0),
            LengthFormat::decimal(Unit::Centimetre, 1),
            LengthFormat::decimal(Unit::Metre, 3),
            LengthFormat::decimal(Unit::Point, 2),
        ];
        let area = AreaFormat::new(AreaUnit::SqMetre, 2);
        // Full size on an ordinary page: a page unit is a point.
        let mut checked = 0;
        for format in formats {
            let s = MeasureSpec::new(&Scale::FULL_SIZE, 1.0, &MeasureFormat::new(format, area));
            let unit_metres = format.unit.metres_per_unit();
            let mut metres = vec![
                0.0,
                0.3048,
                1.6002,
                // 11 63/64 inches: the carry into feet.
                11.984375 * 0.0254,
                // Nothing here sits on a rounding tie: the two sides divide by the unit in a
                // different order, and a tie could fall either way in the last bit.
                0.3048 * 5.0 + 0.0254 * 3.3,
                0.0254 * 0.1,
                12.3456,
                0.001,
                100.0,
                1234.5678,
            ];
            // And a sweep of values that are no special case.
            metres.extend((1..200).map(|i| f64::from(i) * 0.0173 + 0.0001));
            for m in metres {
                let units = m / unit_metres;
                assert_eq!(
                    s.format_distance(units),
                    format_length(unit_metres * units, &format),
                    "{format:?} {m} m"
                );
                checked += 1;
            }
        }
        assert!(checked > 2000);
        // The strings themselves, so that the two cannot be wrong together.
        let arch = MeasureSpec::new(
            &Scale::FULL_SIZE,
            1.0,
            &MeasureFormat::new(LengthFormat::architectural(16), area),
        );
        assert_eq!(arch.format_distance(5.0 + 3.5 / 12.0), "5'-3 1/2\"");
        assert_eq!(arch.format_distance(11.984375 / 12.0), "1'-0\"");
        assert_eq!(arch.format_distance(0.25 / 12.0), "0'-1/4\"");
        assert_eq!(eng().format_distance(123.456), "123.46'");
        assert_eq!(eng().format_area(1234.5), "1234.50 sq ft");
        assert_eq!(eng().format_angle(45.0), "45.0\u{b0}");
    }

    #[test]
    fn measure_factors_account_for_the_scale_and_the_user_unit() {
        // 1 in = 20 ft: a page unit (1/72 in) is 20/72 ft.
        let s = eng();
        assert!(close(s.x_factor(), 20.0 / 72.0));
        assert_eq!(s.y_factor(), s.x_factor());
        assert_eq!(s.ratio, "1 in = 20 ft");
        assert_eq!(s.distance[0].factor, 1.0);
        assert_eq!(s.area[0].factor, 1.0, "square feet from feet squared");
        assert_eq!(s.metres_per_unit(), Some(0.3048));
        assert!(s.is_imperial());
        // On a /UserUnit 2 page a unit is twice the paper.
        let s2 = spec(
            "1 in = 20 ft",
            2.0,
            LengthFormat::decimal(Unit::Foot, 2),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        );
        assert!(close(s2.x_factor(), 40.0 / 72.0));
        // 1/4" = 1'-0" in feet and inches: 4/72 ft per unit, then 12 inches to the foot.
        let arch = spec(
            "1/4\" = 1'-0\"",
            1.0,
            LengthFormat::architectural(16),
            AreaFormat::new(AreaUnit::SqYard, 1),
        );
        assert!(close(arch.x_factor(), 4.0 / 72.0));
        assert_eq!(arch.x.len(), 2);
        assert_eq!((arch.x[1].factor, arch.x[1].precision), (12.0, 16));
        assert_eq!(arch.x[1].style, NumberStyle::Fraction);
        assert_eq!(
            arch.area[0].factor,
            1.0 / 9.0,
            "square yards from feet squared"
        );
        // 1:100 in metres: a point is 0.0254/72 m of paper and 100 times that in the world.
        let metric = spec(
            "1:100",
            1.0,
            LengthFormat::decimal(Unit::Metre, 3),
            AreaFormat::new(AreaUnit::Hectare, 4),
        );
        assert!(close(metric.x_factor(), 2.54 / 72.0));
        assert_eq!(metric.area[0].factor, 1.0 / 10_000.0);
        assert!(!metric.is_imperial());
        // The scale comes back out, whatever the user unit it was built for.
        for (text, unit) in [("1 in = 20 ft", 1.0), ("1 in = 20 ft", 2.0), ("1:100", 1.0)] {
            let s = spec(
                text,
                unit,
                LengthFormat::decimal(Unit::Metre, 2),
                AreaFormat::new(AreaUnit::SqMetre, 2),
            );
            let back = s.scale(unit).unwrap();
            assert!(
                close(back.factor(), Scale::parse(text).unwrap().factor()),
                "{text}"
            );
            assert_eq!(back.pdf_ratio(), s.ratio);
        }
        // A ratio text that disagrees with the factor is not believed.
        let mut odd = eng();
        odd.ratio = "1 in = 50 ft".into();
        assert!(close(odd.scale(1.0).unwrap().factor(), 240.0));
    }

    #[test]
    fn known_segment_lengths_under_several_scales() {
        let og = origin();
        let (a, b) = (p(100.0, 100.0), p(316.0, 100.0));
        // 216 units are 3 inches of paper: 60 ft at 1" = 20'.
        let m = Annotation::measure_length(0, a, b, eng(), &og);
        assert!(close(
            m.quantities().unwrap().length.unwrap(),
            60.0 * 0.3048
        ));
        assert_eq!(m.measure_text().as_deref(), Some("60.00'"));
        assert_eq!(m.meta.contents.as_deref(), Some("60.00'"));
        // 12 ft at 1/4" = 1'-0".
        let arch = spec(
            "1/4\" = 1'-0\"",
            1.0,
            LengthFormat::architectural(16),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        );
        let m = Annotation::measure_length(0, a, b, arch.clone(), &og);
        assert!(close(
            m.quantities().unwrap().length.unwrap(),
            12.0 * 0.3048
        ));
        assert_eq!(m.measure_text().as_deref(), Some("12'-0\""));
        // A 3-4-5 slant: 90 units are 1.25 in, 5 ft.
        let m = Annotation::measure_length(0, p(10.0, 10.0), p(64.0, 82.0), arch, &og);
        assert_eq!(m.measure_text().as_deref(), Some("5'-0\""));
        // 7.62 m at 1:100.
        let metric = spec(
            "1:100",
            1.0,
            LengthFormat::decimal(Unit::Metre, 3),
            AreaFormat::new(AreaUnit::SqMetre, 2),
        );
        let m = Annotation::measure_length(0, a, b, metric, &og);
        assert!(close(m.quantities().unwrap().length.unwrap(), 7.62));
        assert_eq!(m.measure_text().as_deref(), Some("7.620 m"));
        // On a /UserUnit 2 page the same points are 6 inches of paper: 120 ft.
        let double = spec(
            "1 in = 20 ft",
            2.0,
            LengthFormat::decimal(Unit::Foot, 2),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        );
        let m = Annotation::measure_length(0, a, b, double, &og);
        assert!(close(
            m.quantities().unwrap().length.unwrap(),
            120.0 * 0.3048
        ));
        assert_eq!(m.measure_text().as_deref(), Some("120.00'"));
        // The same from nothing but geometry, scale and user unit.
        let q = Quantities::compute(
            MeasureKind::Length,
            &m.geometry,
            &[],
            None,
            &Scale::engineering(20.0).unwrap(),
            2.0,
        );
        assert!(close(q.length.unwrap(), 120.0 * 0.3048));
        assert_eq!(
            (q.area, q.volume, q.angle, q.count),
            (None, None, None, None)
        );
    }

    #[test]
    fn polylength_perimeter_area_volume_angle_radius_and_count() {
        let og = origin();
        // A 144 x 72 unit rectangle: 2 in x 1 in of paper, 40 ft x 20 ft at 1" = 20'.
        let rect = vec![
            p(100.0, 100.0),
            p(244.0, 100.0),
            p(244.0, 172.0),
            p(100.0, 172.0),
        ];
        let ft = 0.3048;

        let m = Annotation::measure_polylength(0, rect.clone(), eng(), &og);
        assert!(close(m.quantities().unwrap().length.unwrap(), 100.0 * ft));
        assert_eq!(m.measure_text().as_deref(), Some("100.00'"));

        let m = Annotation::measure_perimeter(0, rect.clone(), eng(), &og);
        assert!(close(m.quantities().unwrap().length.unwrap(), 120.0 * ft));
        assert_eq!(m.measure_text().as_deref(), Some("120.00'"));

        let m = Annotation::measure_area(0, rect.clone(), Vec::new(), eng(), &og);
        let q = m.quantities().unwrap();
        assert!(close(q.area.unwrap(), 800.0 * ft * ft));
        assert!(close(q.length.unwrap(), 120.0 * ft));
        assert_eq!(q.volume, None);
        assert_eq!(m.measure_text().as_deref(), Some("800.00 sq ft"));

        // A 36 x 36 unit cutout is 10 ft x 10 ft.
        let hole = vec![
            p(120.0, 120.0),
            p(156.0, 120.0),
            p(156.0, 156.0),
            p(120.0, 156.0),
        ];
        let m = Annotation::measure_area(0, rect.clone(), vec![hole.clone()], eng(), &og);
        assert!(close(
            m.quantities().unwrap().area.unwrap(),
            700.0 * ft * ft
        ));
        assert_eq!(m.measure_text().as_deref(), Some("700.00 sq ft"));

        // 700 sq ft, 9 inches deep: 525 cu ft, 19.44 cu yd.
        let m = Annotation::measure_volume(0, rect.clone(), vec![hole], 0.75 * ft, eng(), &og);
        let q = m.quantities().unwrap();
        assert!(close(q.volume.unwrap(), 525.0 * ft * ft * ft));
        assert!(close(q.area.unwrap(), 700.0 * ft * ft));
        assert_eq!(m.measure_text().as_deref(), Some("19.44 cu yd"));
        let q2 = Quantities::compute(
            MeasureKind::Volume,
            &m.geometry,
            &m.measure.as_ref().unwrap().cutouts,
            Some(0.75 * ft),
            &Scale::engineering(20.0).unwrap(),
            1.0,
        );
        assert!(close(q2.volume.unwrap(), q.volume.unwrap()));
        assert!(close(q2.area.unwrap(), q.area.unwrap()));
        assert!(close(q2.length.unwrap(), q.length.unwrap()));

        // A right angle, a 45 degree angle and a 3-4-5 angle.
        let v = p(200.0, 200.0);
        let m = Annotation::measure_angle(0, p(260.0, 200.0), v, p(200.0, 290.0), None, &og);
        assert!(close(
            m.quantities().unwrap().angle.unwrap(),
            std::f64::consts::FRAC_PI_2
        ));
        assert_eq!(m.measure_text().as_deref(), Some("90.0\u{b0}"));
        let m = Annotation::measure_angle(0, p(260.0, 200.0), v, p(250.0, 250.0), Some(eng()), &og);
        assert_eq!(m.measure_text().as_deref(), Some("45.0\u{b0}"));
        let m = Annotation::measure_angle(0, p(240.0, 200.0), v, p(240.0, 230.0), None, &og);
        assert!(close(
            m.quantities().unwrap().angle.unwrap(),
            (3.0f64 / 4.0).atan()
        ));
        assert_eq!(m.measure_text().as_deref(), Some("36.9\u{b0}"));

        // A radius of 54 units is 15 ft; the same line as a diameter is 15 ft across.
        let m = Annotation::measure_radius(0, v, p(200.0, 254.0), eng(), &og);
        assert!(close(m.quantities().unwrap().length.unwrap(), 15.0 * ft));
        assert_eq!(m.measure_text().as_deref(), Some("R 15.00'"));
        let m = Annotation::measure_diameter(0, p(146.0, 200.0), p(254.0, 200.0), eng(), &og);
        assert!(close(m.quantities().unwrap().length.unwrap(), 30.0 * ft));
        assert_eq!(m.measure_text().as_deref(), Some("\u{d8} 30.00'"));

        // Seven markers, then six, then eight.
        let points: Vec<Point> = (0..7)
            .map(|i| p(100.0 + 20.0 * f64::from(i), 300.0))
            .collect();
        let mut m = Annotation::count(0, &points, CountSymbol::Diamond, &og);
        assert_eq!(m.quantities().unwrap().count, Some(7));
        assert_eq!(m.measure_text().as_deref(), Some("7"));
        assert_eq!(m.count_points(), points);
        assert!(m.remove_count_point(2));
        assert!(!m.remove_count_point(6));
        assert_eq!(m.meta.contents.as_deref(), Some("6"));
        assert!(m.add_count_point(p(5.0, 5.0)) && m.add_count_point(p(6.0, 6.0)));
        assert_eq!(m.quantities().unwrap().count, Some(8));
        assert_eq!(m.count_points()[7], p(6.0, 6.0));
        assert!(m.is_modelled() && m.kind == AnnotKind::Ink);
        // Not a count: nothing to add to.
        let mut line = Annotation::measure_length(0, v, p(1.0, 1.0), eng(), &og);
        assert!(!line.add_count_point(v) && line.count_points().is_empty());
    }

    #[test]
    fn separate_axis_scales_are_used_for_lengths_areas_and_angles() {
        let og = origin();
        let format = MeasureFormat::new(
            LengthFormat::decimal(Unit::Foot, 2),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        );
        // 1" = 20' along x, 1" = 2' along y: a profile with a tenfold vertical exaggeration.
        let s = MeasureSpec::with_axes(
            &Scale::engineering(20.0).unwrap(),
            &Scale::engineering(2.0).unwrap(),
            1.0,
            &format,
        );
        assert!(close(s.y_factor(), 2.0 / 72.0));
        assert_eq!(s.ratio, "1 in = 20 ft in X 1 in = 2 ft in Y");
        let m = Annotation::measure_length(0, p(0.0, 0.0), p(72.0, 0.0), s.clone(), &og);
        assert_eq!(m.measure_text().as_deref(), Some("20.00'"));
        let m = Annotation::measure_length(0, p(0.0, 0.0), p(0.0, 72.0), s.clone(), &og);
        assert_eq!(m.measure_text().as_deref(), Some("2.00'"));
        let m = Annotation::measure_length(0, p(0.0, 0.0), p(72.0, 72.0), s.clone(), &og);
        assert!(close(
            m.quantities().unwrap().length.unwrap(),
            404f64.sqrt() * 0.3048
        ));
        let square = vec![p(0.0, 0.0), p(72.0, 0.0), p(72.0, 72.0), p(0.0, 72.0)];
        let m = Annotation::measure_area(0, square.clone(), Vec::new(), s.clone(), &og);
        assert_eq!(m.measure_text().as_deref(), Some("40.00 sq ft"));
        let m = Annotation::measure_perimeter(0, square, s.clone(), &og);
        assert_eq!(m.measure_text().as_deref(), Some("44.00'"));
        // The diagonal of the page square is at atan(2/20) in the world.
        let m =
            Annotation::measure_angle(0, p(72.0, 0.0), p(0.0, 0.0), p(72.0, 72.0), Some(s), &og);
        assert!(close(m.quantities().unwrap().angle.unwrap(), 0.1f64.atan()));
    }

    #[test]
    fn a_measurement_needs_its_own_geometry_and_follows_its_scale() {
        let og = origin();
        let mut m = Annotation::measure_length(0, p(0.0, 0.0), p(72.0, 0.0), eng(), &og);
        assert!(m.measurement().is_some());
        assert_eq!(m.meta.subject.as_deref(), Some("Length"));
        assert_eq!(m.style.line_start, LineEnding::ClosedArrow);
        // Another scale: the value follows.
        let arch = spec(
            "1/4\" = 1'-0\"",
            1.0,
            LengthFormat::architectural(16),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        );
        m.apply_scale(ScaleRef::Viewport("Detail".into()), arch);
        assert_eq!(m.meta.contents.as_deref(), Some("4'-0\""));
        assert_eq!(
            m.measure.as_ref().unwrap().scale,
            ScaleRef::Viewport("Detail".into())
        );
        // A geometry that is not the kind's: not a measurement any more.
        m.geometry = Geometry::Points(vec![p(0.0, 0.0), p(1.0, 1.0)]);
        assert!(m.measurement().is_none() && m.measure_text().is_none());
        assert!(m.quantities().is_none());
        // A unit this crate cannot convert still gives its text, but no SI length.
        let mut odd = eng();
        odd.x[0].label = "chains".into();
        odd.distance[0].label = "chains".into();
        odd.distance[0].prefix_space = " ".into();
        let m = Annotation::measure_length(0, p(0.0, 0.0), p(72.0, 0.0), odd, &og);
        assert_eq!(m.measure_text().as_deref(), Some("20.00 chains"));
        assert_eq!(m.quantities().unwrap().length, None);
        for kind in MeasureKind::ALL {
            assert_eq!(MeasureKind::from_name(kind.name()), Some(kind));
            assert_eq!(
                kind.is_scaled(),
                !matches!(kind, MeasureKind::Angle | MeasureKind::Count)
            );
            // Every kind has an intent, and the intent read back on its subtype is the kind
            // (a Polylength and a Perimeter share theirs and are told apart by closing).
            let intent = kind.intent().unwrap();
            match MeasureKind::from_intent(intent, &kind.file_subtype()) {
                Some(read) => assert_eq!(read, kind),
                None => assert_eq!(intent, "PolyLineDimension"),
            }
        }
        for symbol in CountSymbol::ALL {
            assert_eq!(CountSymbol::from_name(symbol.name()), Some(symbol));
        }
    }
}
