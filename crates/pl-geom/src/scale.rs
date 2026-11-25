//! Drawing scales: a length on paper that stands for a length in the world.
//!
//! A [`Scale`] keeps the two lengths as they were stated (`1/4 in` and `1 ft`, `1 mm` and
//! `100 mm`), so it can be shown again the way it was typed, and gives the one number that
//! matters for measuring: [`Scale::factor`], real metres per metre of paper.
//!
//! # `/UserUnit`
//!
//! Paper is measured in inches, not in page units. A page with `/UserUnit 2` has units of 2/72
//! inch, so a segment of 36 units on it is one inch of paper. Every function here that takes a
//! length in page units also takes the page's user unit.
//!
//! # Notations
//!
//! [`Scale::parse`] reads, and `Display` writes:
//!
//! | Family | Examples |
//! |---|---|
//! | Ratio | `1:100`, `1 : 50`, `2:1` |
//! | Architectural | `1/4" = 1'-0"`, `1/4 in = 1 ft-0 in`, `1 1/2" = 1'-0"`, `3/32" = 1'` |
//! | Engineering | `1" = 20'`, `1 in = 20 ft` |
//! | Mixed | `1 cm = 1 m`, `1 in = 5 m` |

use std::fmt;

use kurbo::{Point, Vec2};

use crate::units::{
    format_decimal, parse_length, points_to_metres, to_fraction, LengthParseError, Unit,
    MAX_FRACTION_DENOMINATOR,
};

/// Why a scale could not be made.
#[derive(Clone, Debug, PartialEq)]
pub enum ScaleError {
    /// Nothing was entered.
    Empty,
    /// Neither `a:b` nor `a = b`.
    NoSeparator,
    /// One side of the scale is not a length.
    Length(LengthParseError),
    /// A side of `a = b` has no unit, as in `1 = 20 ft`.
    MissingUnit,
    /// A length is zero, negative or not a number.
    NotPositive,
    /// The two calibration points are the same point.
    ZeroDistance,
}

impl fmt::Display for ScaleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScaleError::Empty => write!(f, "no scale was entered"),
            ScaleError::NoSeparator => {
                write!(f, "a scale is written like 1:100 or 1 in = 20 ft")
            }
            ScaleError::Length(e) => write!(f, "{e}"),
            ScaleError::MissingUnit => write!(f, "each side of the scale needs a unit"),
            ScaleError::NotPositive => write!(f, "both lengths of a scale must be above zero"),
            ScaleError::ZeroDistance => {
                write!(f, "the two calibration points are in the same place")
            }
        }
    }
}

impl std::error::Error for ScaleError {}

impl From<LengthParseError> for ScaleError {
    fn from(e: LengthParseError) -> ScaleError {
        ScaleError::Length(e)
    }
}

/// A drawing scale: `paper` of `paper_unit` on the sheet is `real` of `real_unit` in the world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    paper: f64,
    paper_unit: Unit,
    real: f64,
    real_unit: Unit,
}

/// The families the presets are grouped in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScaleFamily {
    /// Inches to feet and inches: `1/4" = 1'-0"`.
    Architectural,
    /// One inch to a number of feet: `1" = 20'`.
    Engineering,
    /// Ratios: `1:100`.
    Metric,
}

/// One entry of the preset list.
#[derive(Clone, Debug, PartialEq)]
pub struct ScalePreset {
    pub family: ScaleFamily,
    /// The scale as [`Scale`]'s `Display` writes it.
    pub label: String,
    pub scale: Scale,
}

impl Scale {
    /// `paper` of `paper_unit` stands for `real` of `real_unit`. Both lengths must be finite and
    /// above zero. A [`Unit::FeetInches`] length is given in feet.
    pub fn new(
        paper: f64,
        paper_unit: Unit,
        real: f64,
        real_unit: Unit,
    ) -> Result<Scale, ScaleError> {
        let ok = |v: f64| v.is_finite() && v > 0.0;
        if !ok(paper) || !ok(real) {
            return Err(ScaleError::NotPositive);
        }
        let scale = Scale {
            paper,
            paper_unit,
            real,
            real_unit,
        };
        if !ok(scale.factor()) {
            return Err(ScaleError::NotPositive);
        }
        Ok(scale)
    }

    /// `1:n`. `n` must be finite and above zero (below 1 enlarges).
    pub fn ratio(n: f64) -> Result<Scale, ScaleError> {
        Scale::new(1.0, Unit::Millimetre, n, Unit::Millimetre)
    }

    /// Full size, `1:1`.
    pub const FULL_SIZE: Scale = Scale {
        paper: 1.0,
        paper_unit: Unit::Millimetre,
        real: 1.0,
        real_unit: Unit::Millimetre,
    };

    /// An architectural scale: `paper_inches` on paper is one foot (`0.25` for 1/4" = 1'-0").
    pub fn architectural(paper_inches: f64) -> Result<Scale, ScaleError> {
        Scale::new(paper_inches, Unit::Inch, 1.0, Unit::FeetInches)
    }

    /// An engineering scale: one inch on paper is `feet` feet.
    pub fn engineering(feet: f64) -> Result<Scale, ScaleError> {
        Scale::new(1.0, Unit::Inch, feet, Unit::Foot)
    }

    /// The scale with a given [`Scale::factor`], stated in the way usual for `real_unit`: one
    /// inch to so many feet for an imperial unit, a ratio for a metric one.
    pub fn from_factor(factor: f64, real_unit: Unit) -> Result<Scale, ScaleError> {
        if !(factor.is_finite() && factor > 0.0) {
            return Err(ScaleError::NotPositive);
        }
        match real_unit {
            Unit::Inch => Scale::new(1.0, Unit::Inch, factor, Unit::Inch),
            // Feet and inches stay the real unit: values at this scale are shown that way.
            Unit::Foot | Unit::FeetInches => Scale::new(1.0, Unit::Inch, factor / 12.0, real_unit),
            Unit::Point => Scale::new(1.0, Unit::Point, factor, Unit::Point),
            metric => Scale::new(1.0, metric, factor, metric),
        }
    }

    /// The length on paper, in [`Scale::paper_unit`].
    pub fn paper(&self) -> f64 {
        self.paper
    }

    pub fn paper_unit(&self) -> Unit {
        self.paper_unit
    }

    /// The length in the world, in [`Scale::real_unit`] (feet for [`Unit::FeetInches`]).
    pub fn real(&self) -> f64 {
        self.real
    }

    /// The unit the world length was stated in: the natural unit to show measurements in.
    pub fn real_unit(&self) -> Unit {
        self.real_unit
    }

    /// Real metres per metre of paper: 100 for 1:100, 240 for 1" = 20'.
    pub fn factor(&self) -> f64 {
        self.real_unit.to_metres(self.real) / self.paper_unit.to_metres(self.paper)
    }

    /// Real metres per page unit on a page with the given `/UserUnit`.
    pub fn metres_per_page_unit(&self, user_unit: f64) -> f64 {
        points_to_metres(user_unit) * self.factor()
    }

    /// The real length, in metres, of `page_units` measured on a page with `user_unit`.
    pub fn real_length(&self, page_units: f64, user_unit: f64) -> f64 {
        page_units * self.metres_per_page_unit(user_unit)
    }

    /// The real area, in square metres, of an area in square page units.
    pub fn real_area(&self, square_page_units: f64, user_unit: f64) -> f64 {
        let k = self.metres_per_page_unit(user_unit);
        square_page_units * k * k
    }

    /// The length in page units that stands for `metres` in the world.
    pub fn page_length(&self, metres: f64, user_unit: f64) -> f64 {
        metres / self.metres_per_page_unit(user_unit)
    }

    /// The real distance between two page points, in metres.
    pub fn distance(&self, a: Point, b: Point, user_unit: f64) -> f64 {
        self.real_length(a.distance(b), user_unit)
    }

    /// The scale at which the page distance from `a` to `b` is `real_metres`: calibration from
    /// two picked points and a typed length. The result is stated the usual way for
    /// `real_unit` (see [`Scale::from_factor`]).
    pub fn calibrate(
        a: Point,
        b: Point,
        real_metres: f64,
        user_unit: f64,
        real_unit: Unit,
    ) -> Result<Scale, ScaleError> {
        let paper = points_to_metres(a.distance(b) * user_unit);
        if !paper.is_finite() || paper <= 0.0 {
            return Err(ScaleError::ZeroDistance);
        }
        if !(real_metres.is_finite() && real_metres > 0.0) {
            return Err(ScaleError::NotPositive);
        }
        Scale::from_factor(real_metres / paper, real_unit)
    }

    /// Reads a scale in any of the notations in the module documentation.
    pub fn parse(input: &str) -> Result<Scale, ScaleError> {
        let text = input.trim();
        if text.is_empty() {
            return Err(ScaleError::Empty);
        }
        if let Some((left, right)) = text.split_once('=') {
            let (paper, paper_unit) = parse_side(left)?;
            let (real, real_unit) = parse_side(right)?;
            // An inch-to-foot scale whose paper side is a fraction is architectural, and so
            // is anything whose real side was written with inches (1'-0").
            return Scale::new(paper, paper_unit, real, real_unit);
        }
        if let Some((left, right)) = text.split_once(':') {
            let number = |s: &str| -> Result<f64, ScaleError> {
                let s = s.trim();
                if s.is_empty() {
                    return Err(ScaleError::Empty);
                }
                if side_units(s)?.is_some() {
                    // 1in:20ft is accepted as 1 in = 20 ft.
                    return Err(ScaleError::NoSeparator);
                }
                Ok(parse_length(s, Unit::Metre)?)
            };
            return match (number(left), number(right)) {
                (Ok(paper), Ok(real)) => {
                    Scale::new(paper, Unit::Millimetre, real, Unit::Millimetre)
                }
                (Err(ScaleError::NoSeparator), _) | (_, Err(ScaleError::NoSeparator)) => {
                    let (paper, paper_unit) = parse_side(left)?;
                    let (real, real_unit) = parse_side(right)?;
                    Scale::new(paper, paper_unit, real, real_unit)
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
        }
        Err(ScaleError::NoSeparator)
    }

    /// The scale as the PDF `/R` ratio string of a measure dictionary: plain unit
    /// abbreviations, `1 in = 20 ft`, `0.25 in = 1 ft`, `1 mm = 100 mm`.
    pub fn pdf_ratio(&self) -> String {
        let label = |u: Unit| match u {
            Unit::FeetInches => "ft",
            other => other.abbreviation(),
        };
        format!(
            "{} {} = {} {}",
            trimmed(self.paper),
            label(self.paper_unit),
            trimmed(self.real),
            label(self.real_unit)
        )
    }

    /// The preset list: architectural, engineering and metric scales, each family from the
    /// largest drawing to the smallest.
    pub fn presets() -> Vec<ScalePreset> {
        const ARCHITECTURAL: [f64; 12] = [
            3.0, 1.5, 1.0, 0.75, 0.5, 0.375, 0.25, 0.1875, 0.125, 0.09375, 0.0625, 0.03125,
        ];
        const ENGINEERING: [f64; 9] = [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 100.0, 200.0, 500.0];
        const METRIC: [f64; 15] = [
            1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 1250.0,
            2500.0, 5000.0,
        ];
        let entry = |family, scale: Scale| ScalePreset {
            family,
            label: scale.to_string(),
            scale,
        };
        let mut out = Vec::new();
        for inches in ARCHITECTURAL {
            out.push(entry(
                ScaleFamily::Architectural,
                Scale {
                    paper: inches,
                    paper_unit: Unit::Inch,
                    real: 1.0,
                    real_unit: Unit::FeetInches,
                },
            ));
        }
        for feet in ENGINEERING {
            out.push(entry(
                ScaleFamily::Engineering,
                Scale {
                    paper: 1.0,
                    paper_unit: Unit::Inch,
                    real: feet,
                    real_unit: Unit::Foot,
                },
            ));
        }
        for n in METRIC {
            out.push(entry(
                ScaleFamily::Metric,
                Scale {
                    paper: 1.0,
                    paper_unit: Unit::Millimetre,
                    real: n,
                    real_unit: Unit::Millimetre,
                },
            ));
        }
        out
    }
}

/// A decimal with up to six places and no trailing zeros.
fn trimmed(value: f64) -> String {
    let s = format_decimal(value, 6);
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// A number as a fraction when it is one exactly (to 1/64), otherwise as a decimal.
fn fraction_or_decimal(value: f64) -> String {
    let f = to_fraction(value, MAX_FRACTION_DENOMINATOR);
    if (f.value() - value).abs() <= 1e-12 * value.abs().max(1.0) {
        f.to_string()
    } else {
        trimmed(value)
    }
}

/// The units named in one side of `a = b`: `Some(FeetInches)` when both feet and inches are,
/// `None` when the side has no unit at all.
fn side_units(side: &str) -> Result<Option<Unit>, ScaleError> {
    let normal: String = side
        .chars()
        .map(|c| match c {
            '\u{2032}' | '\u{2018}' | '\u{2019}' => '\'',
            '\u{2033}' | '\u{201C}' | '\u{201D}' => '"',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    let mut found: Vec<Unit> = Vec::new();
    let mut token = String::new();
    let mut flush = |token: &mut String| -> Result<(), ScaleError> {
        if token.is_empty() {
            return Ok(());
        }
        let unit = match token.as_str() {
            "'" | "ft" | "foot" | "feet" => Unit::Foot,
            "\"" | "in" | "inch" | "inches" => Unit::Inch,
            "mm" | "millimetre" | "millimetres" | "millimeter" | "millimeters" => Unit::Millimetre,
            "cm" | "centimetre" | "centimetres" | "centimeter" | "centimeters" => Unit::Centimetre,
            "m" | "metre" | "metres" | "meter" | "meters" => Unit::Metre,
            "pt" | "pts" | "point" | "points" => Unit::Point,
            other => {
                return Err(ScaleError::Length(LengthParseError::UnknownUnit(
                    other.to_string(),
                )))
            }
        };
        if !found.contains(&unit) {
            found.push(unit);
        }
        token.clear();
        Ok(())
    };
    let chars: Vec<char> = normal.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() {
            token.push(c);
        } else {
            flush(&mut token)?;
            if c == '\'' && chars.get(i + 1) == Some(&'\'') {
                // Two apostrophes typed for a double quote.
                token.push('"');
                i += 1;
                flush(&mut token)?;
            } else if c == '\'' || c == '"' {
                token.push(c);
                flush(&mut token)?;
            }
        }
        i += 1;
    }
    flush(&mut token)?;
    Ok(match found.as_slice() {
        [] => None,
        [one] => Some(*one),
        many if many.contains(&Unit::Foot) && many.contains(&Unit::Inch) && many.len() == 2 => {
            Some(Unit::FeetInches)
        }
        // 1 m 20 cm: the first unit named.
        many => Some(many[0]),
    })
}

/// One side of `a = b`: its value in its own unit, and the unit.
fn parse_side(side: &str) -> Result<(f64, Unit), ScaleError> {
    let side = side.trim();
    if side.is_empty() {
        return Err(ScaleError::Empty);
    }
    let unit = side_units(side)?.ok_or(ScaleError::MissingUnit)?;
    let metres = parse_length(side, Unit::Metre)?;
    Ok((unit.from_metres(metres), unit))
}

/// Writes the scale in the notation of its units: `1:100` when both sides share a unit,
/// `1/4" = 1'-0"` for inches to feet-and-inches, `1" = 20'` for inches to feet, and
/// `1 cm = 1 m` otherwise.
impl fmt::Display for Scale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.paper_unit.metres_per_unit() == self.real_unit.metres_per_unit() {
            return write!(f, "{}:{}", trimmed(self.paper), trimmed(self.real));
        }
        let side = |value: f64, unit: Unit| match unit {
            Unit::Inch => format!("{}\"", fraction_or_decimal(value)),
            Unit::Foot => format!("{}'", fraction_or_decimal(value)),
            Unit::FeetInches => {
                let inches = value * 12.0;
                let feet = (inches / 12.0 + 1e-9).floor();
                let rest = (inches - feet * 12.0).max(0.0);
                format!("{}'-{}\"", trimmed(feet), fraction_or_decimal(rest))
            }
            other => format!("{} {}", trimmed(value), other.abbreviation()),
        };
        write!(
            f,
            "{} = {}",
            side(self.paper, self.paper_unit),
            side(self.real, self.real_unit)
        )
    }
}

/// A scale for each axis, for a drawing stretched in one direction (a profile with an
/// exaggerated vertical scale).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisScales {
    pub x: Scale,
    pub y: Scale,
}

impl AxisScales {
    /// The same scale on both axes.
    pub fn uniform(scale: Scale) -> AxisScales {
        AxisScales { x: scale, y: scale }
    }

    pub fn is_uniform(&self) -> bool {
        self.x.factor() == self.y.factor()
    }

    /// A page-space displacement as a real one, in metres.
    pub fn real_vector(&self, v: Vec2, user_unit: f64) -> Vec2 {
        Vec2::new(
            v.x * self.x.metres_per_page_unit(user_unit),
            v.y * self.y.metres_per_page_unit(user_unit),
        )
    }

    /// The real distance between two page points, in metres.
    pub fn distance(&self, a: Point, b: Point, user_unit: f64) -> f64 {
        self.real_vector(b - a, user_unit).hypot()
    }

    /// The real area, in square metres, of an area in square page units.
    pub fn real_area(&self, square_page_units: f64, user_unit: f64) -> f64 {
        square_page_units
            * self.x.metres_per_page_unit(user_unit)
            * self.y.metres_per_page_unit(user_unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::METRES_PER_INCH;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn factors_of_the_common_scales() {
        assert_eq!(Scale::ratio(100.0).unwrap().factor(), 100.0);
        assert_eq!(Scale::FULL_SIZE.factor(), 1.0);
        assert!(close(Scale::engineering(20.0).unwrap().factor(), 240.0));
        assert!(close(Scale::architectural(0.25).unwrap().factor(), 48.0));
        assert!(close(Scale::architectural(0.125).unwrap().factor(), 96.0));
        assert!(close(
            Scale::new(1.0, Unit::Centimetre, 1.0, Unit::Metre)
                .unwrap()
                .factor(),
            100.0
        ));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(Scale::ratio(bad), Err(ScaleError::NotPositive));
            assert_eq!(Scale::architectural(bad), Err(ScaleError::NotPositive));
        }
    }

    #[test]
    fn known_lengths_under_several_scales_and_user_units() {
        // 72 units are one inch of paper on an ordinary page and two on a /UserUnit 2 page.
        let eng = Scale::engineering(20.0).unwrap();
        assert!(close(eng.real_length(72.0, 1.0), 20.0 * 0.3048));
        assert!(close(eng.real_length(72.0, 2.0), 40.0 * 0.3048));
        assert!(close(eng.real_length(36.0, 2.0), 20.0 * 0.3048));
        let arch = Scale::architectural(0.25).unwrap();
        assert!(close(arch.real_length(18.0, 1.0), 0.3048));
        assert!(close(arch.real_length(180.0, 1.0), 3.048));
        let metric = Scale::ratio(100.0).unwrap();
        // 1 mm of paper is 72/25.4 units and stands for 100 mm.
        assert!(close(metric.real_length(72.0 / 25.4, 1.0), 0.1));
        assert!(close(
            metric.real_length(720.0, 1.0),
            10.0 * METRES_PER_INCH * 100.0
        ));
        // Area goes with the square.
        assert!(close(
            eng.real_area(72.0 * 72.0, 1.0),
            400.0 * 0.3048 * 0.3048
        ));
        assert!(close(
            eng.real_area(72.0 * 72.0, 2.0),
            1600.0 * 0.3048 * 0.3048
        ));
        // And back.
        assert!(close(eng.page_length(20.0 * 0.3048, 1.0), 72.0));
        assert!(close(eng.page_length(20.0 * 0.3048, 2.0), 36.0));
        assert!(close(
            eng.distance(Point::new(10.0, 10.0), Point::new(40.0, 50.0), 1.0),
            50.0 / 72.0 * 20.0 * 0.3048
        ));
    }

    #[test]
    fn notations_parse_to_the_right_factor_and_units() {
        let cases: [(&str, f64, Unit, Unit); 16] = [
            ("1:100", 100.0, Unit::Millimetre, Unit::Millimetre),
            (" 1 : 50 ", 50.0, Unit::Millimetre, Unit::Millimetre),
            ("2:1", 0.5, Unit::Millimetre, Unit::Millimetre),
            ("1:1250", 1250.0, Unit::Millimetre, Unit::Millimetre),
            ("1\" = 20'", 240.0, Unit::Inch, Unit::Foot),
            ("1 in = 20 ft", 240.0, Unit::Inch, Unit::Foot),
            ("1in=20ft", 240.0, Unit::Inch, Unit::Foot),
            ("1 inch = 100 feet", 1200.0, Unit::Inch, Unit::Foot),
            ("1/4\" = 1'-0\"", 48.0, Unit::Inch, Unit::FeetInches),
            ("1/4 in = 1 ft-0 in", 48.0, Unit::Inch, Unit::FeetInches),
            ("1 1/2\" = 1'-0\"", 8.0, Unit::Inch, Unit::FeetInches),
            ("3/32\" = 1'", 128.0, Unit::Inch, Unit::Foot),
            (
                "1/8\u{2033} = 1\u{2032}-0\u{2033}",
                96.0,
                Unit::Inch,
                Unit::FeetInches,
            ),
            ("1 cm = 1 m", 100.0, Unit::Centimetre, Unit::Metre),
            ("1mm=1m", 1000.0, Unit::Millimetre, Unit::Metre),
            ("1 in : 20 ft", 240.0, Unit::Inch, Unit::Foot),
        ];
        for (text, factor, paper, real) in cases {
            let s = Scale::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert!(close(s.factor(), factor), "{text}: {}", s.factor());
            assert_eq!((s.paper_unit(), s.real_unit()), (paper, real), "{text}");
        }
        assert_eq!(Scale::parse("  "), Err(ScaleError::Empty));
        assert_eq!(Scale::parse("100"), Err(ScaleError::NoSeparator));
        assert_eq!(Scale::parse("1 = 20 ft"), Err(ScaleError::MissingUnit));
        assert_eq!(Scale::parse("1 in = "), Err(ScaleError::Empty));
        assert_eq!(Scale::parse("0:100"), Err(ScaleError::NotPositive));
        assert_eq!(Scale::parse("1 in = 0 ft"), Err(ScaleError::NotPositive));
        assert!(matches!(
            Scale::parse("1 in = 20 furlongs"),
            Err(ScaleError::Length(LengthParseError::UnknownUnit(u))) if u == "furlongs"
        ));
        assert!(matches!(Scale::parse("a:b"), Err(ScaleError::Length(_))));
    }

    #[test]
    fn scales_are_written_in_their_own_notation_and_read_back() {
        let cases = [
            ("1:100", "1:100"),
            ("1 : 2.5", "1:2.5"),
            ("1\" = 20'", "1\" = 20'"),
            ("1 in = 20 ft", "1\" = 20'"),
            ("1/4 in = 1 ft-0 in", "1/4\" = 1'-0\""),
            ("1 1/2\" = 1'-0\"", "1 1/2\" = 1'-0\""),
            ("3/32\" = 1'-0\"", "3/32\" = 1'-0\""),
            ("1\" = 1'-6\"", "1\" = 1'-6\""),
            ("1 cm = 1 m", "1 cm = 1 m"),
            ("0.3\" = 1'", "0.3\" = 1'"),
        ];
        for (text, shown) in cases {
            let s = Scale::parse(text).unwrap();
            assert_eq!(s.to_string(), shown, "{text}");
            let again = Scale::parse(&s.to_string()).unwrap();
            assert!(close(again.factor(), s.factor()), "{text}");
            assert_eq!(again.to_string(), shown);
        }
    }

    #[test]
    fn pdf_ratio_strings_are_golden() {
        assert_eq!(
            Scale::engineering(20.0).unwrap().pdf_ratio(),
            "1 in = 20 ft"
        );
        assert_eq!(
            Scale::architectural(0.25).unwrap().pdf_ratio(),
            "0.25 in = 1 ft"
        );
        assert_eq!(Scale::ratio(100.0).unwrap().pdf_ratio(), "1 mm = 100 mm");
        assert_eq!(
            Scale::new(1.0, Unit::Centimetre, 2.5, Unit::Metre)
                .unwrap()
                .pdf_ratio(),
            "1 cm = 2.5 m"
        );
        // What is written as /R reads back as the same scale.
        for preset in Scale::presets() {
            let back = Scale::parse(&preset.scale.pdf_ratio()).unwrap();
            assert!(
                close(back.factor(), preset.scale.factor()),
                "{}",
                preset.label
            );
        }
    }

    #[test]
    fn presets_are_grouped_ordered_and_parse_back() {
        let presets = Scale::presets();
        assert_eq!(presets.len(), 36);
        for family in [
            ScaleFamily::Architectural,
            ScaleFamily::Engineering,
            ScaleFamily::Metric,
        ] {
            let factors: Vec<f64> = presets
                .iter()
                .filter(|p| p.family == family)
                .map(|p| p.scale.factor())
                .collect();
            assert!(factors.len() >= 9);
            assert!(factors.windows(2).all(|w| w[0] < w[1]), "{family:?}");
        }
        for p in &presets {
            let back = Scale::parse(&p.label).unwrap_or_else(|e| panic!("{}: {e}", p.label));
            assert!(close(back.factor(), p.scale.factor()), "{}", p.label);
            assert_eq!(back.to_string(), p.label);
        }
        let labels: Vec<&str> = presets.iter().map(|p| p.label.as_str()).collect();
        for want in [
            "1/4\" = 1'-0\"",
            "1/8\" = 1'-0\"",
            "1\" = 20'",
            "1:100",
            "3\" = 1'-0\"",
        ] {
            assert!(labels.contains(&want), "{want}");
        }
    }

    #[test]
    fn calibration_from_two_points_and_a_typed_length() {
        // 144 units apart, typed 40 ft: two inches of paper for 40 ft is 1" = 20'.
        let (a, b) = (Point::new(100.0, 50.0), Point::new(100.0, 194.0));
        let s = Scale::calibrate(a, b, 40.0 * 0.3048, 1.0, Unit::Foot).unwrap();
        assert!(close(s.factor(), 240.0));
        assert_eq!(s.to_string(), "1\" = 20'");
        assert!(close(s.distance(a, b, 1.0), 40.0 * 0.3048));
        // The same points on a /UserUnit 2 page are four inches of paper: 1" = 10'.
        let s2 = Scale::calibrate(a, b, 40.0 * 0.3048, 2.0, Unit::FeetInches).unwrap();
        assert!(close(s2.factor(), 120.0));
        // Typed in feet and inches, it is shown in feet and inches.
        assert_eq!(s2.real_unit(), Unit::FeetInches);
        assert_eq!(s2.to_string(), "1\" = 10'-0\"");
        assert_eq!(s2.pdf_ratio(), "1 in = 10 ft");
        assert!(close(s2.distance(a, b, 2.0), 40.0 * 0.3048));
        // Metric: 10 mm of paper for 1 m is 1:100.
        let c = Point::new(0.0, 0.0);
        let d = Point::new(720.0 / 25.4, 0.0);
        let m = Scale::calibrate(c, d, 1.0, 1.0, Unit::Metre).unwrap();
        assert!(close(m.factor(), 100.0));
        assert_eq!(m.to_string(), "1:100");
        // A slanted pair.
        let e = Point::new(30.0, 40.0);
        let s = Scale::calibrate(c, e, 5.0, 1.0, Unit::Metre).unwrap();
        assert!(close(s.distance(c, e, 1.0), 5.0));
        assert_eq!(
            Scale::calibrate(a, a, 1.0, 1.0, Unit::Metre),
            Err(ScaleError::ZeroDistance)
        );
        assert_eq!(
            Scale::calibrate(a, b, 0.0, 1.0, Unit::Metre),
            Err(ScaleError::NotPositive)
        );
        assert_eq!(
            Scale::calibrate(a, b, f64::NAN, 1.0, Unit::Metre),
            Err(ScaleError::NotPositive)
        );
    }

    #[test]
    fn separate_axis_scales() {
        let s = AxisScales {
            x: Scale::engineering(20.0).unwrap(),
            y: Scale::engineering(2.0).unwrap(),
        };
        assert!(!s.is_uniform());
        let ft = 0.3048;
        let o = Point::ZERO;
        assert!(close(s.distance(o, Point::new(72.0, 0.0), 1.0), 20.0 * ft));
        assert!(close(s.distance(o, Point::new(0.0, 72.0), 1.0), 2.0 * ft));
        assert!(close(
            s.distance(o, Point::new(72.0, 72.0), 1.0),
            (20.0f64 * 20.0 + 2.0 * 2.0).sqrt() * ft
        ));
        assert!(close(s.real_area(72.0 * 72.0, 1.0), 40.0 * ft * ft));
        assert!(close(s.real_area(72.0 * 72.0, 2.0), 160.0 * ft * ft));
        let u = AxisScales::uniform(Scale::ratio(50.0).unwrap());
        assert!(u.is_uniform());
        assert!(close(
            u.distance(o, Point::new(3.0, 4.0), 1.0),
            Scale::ratio(50.0).unwrap().real_length(5.0, 1.0)
        ));
    }
}
