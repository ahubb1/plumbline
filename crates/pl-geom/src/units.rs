//! Length units: conversion, formatting and parsing.
//!
//! Real-world lengths are carried as `f64` **metres** (plan section 7, "SI f64 storage"). Paper
//! lengths are PDF points; [`points_to_metres`] and [`metres_to_points`] convert between the two.
//!
//! Formatting conventions (ASCII only):
//!
//! | Unit | Decimal | Fraction |
//! |---|---|---|
//! | Point | `72.00 pt` | `72 1/2 pt` |
//! | Inch | `3.50"` | `3 1/2"` |
//! | Foot | `2.50'` | `2 1/2'` |
//! | FeetInches | `5'-3.50"` | `5'-3 1/2"` |
//! | Millimetre | `12.5 mm` | `12 1/2 mm` |
//! | Centimetre | `12.5 cm` | `12 1/2 cm` |
//! | Metre | `1.250 m` | `1 1/4 m` |

use std::fmt;

/// Exact by definition.
pub const METRES_PER_INCH: f64 = 0.0254;
/// PDF points per inch.
pub const POINTS_PER_INCH: f64 = 72.0;
/// The finest fraction supported: 1/64.
pub const MAX_FRACTION_DENOMINATOR: u32 = 64;

const INCHES_PER_FOOT: f64 = 12.0;

/// Converts PDF points (1/72 inch) to metres.
pub fn points_to_metres(points: f64) -> f64 {
    points / POINTS_PER_INCH * METRES_PER_INCH
}

/// Converts metres to PDF points (1/72 inch).
pub fn metres_to_points(metres: f64) -> f64 {
    metres / METRES_PER_INCH * POINTS_PER_INCH
}

/// A length unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// PDF point, 1/72 inch.
    Point,
    Inch,
    /// Decimal or fractional feet (`2.50'`).
    Foot,
    /// Feet and inches (`5'-3 1/2"`). As a quantity this is feet: one unit is one foot.
    FeetInches,
    Millimetre,
    Centimetre,
    Metre,
}

impl Unit {
    /// Every unit, in a stable order suitable for a drop-down.
    pub const ALL: [Unit; 7] = [
        Unit::Point,
        Unit::Inch,
        Unit::Foot,
        Unit::FeetInches,
        Unit::Millimetre,
        Unit::Centimetre,
        Unit::Metre,
    ];

    /// Metres in one of this unit. [`Unit::FeetInches`] counts as feet.
    pub fn metres_per_unit(self) -> f64 {
        match self {
            Unit::Point => METRES_PER_INCH / POINTS_PER_INCH,
            Unit::Inch => METRES_PER_INCH,
            Unit::Foot | Unit::FeetInches => METRES_PER_INCH * INCHES_PER_FOOT,
            Unit::Millimetre => 0.001,
            Unit::Centimetre => 0.01,
            Unit::Metre => 1.0,
        }
    }

    /// Converts a value in this unit to metres.
    pub fn to_metres(self, value: f64) -> f64 {
        value * self.metres_per_unit()
    }

    /// Converts metres to a value in this unit.
    pub fn from_metres(self, metres: f64) -> f64 {
        metres / self.metres_per_unit()
    }

    /// The short label: `pt`, `in`, `ft`, `ft-in`, `mm`, `cm`, `m`.
    pub fn abbreviation(self) -> &'static str {
        match self {
            Unit::Point => "pt",
            Unit::Inch => "in",
            Unit::Foot => "ft",
            Unit::FeetInches => "ft-in",
            Unit::Millimetre => "mm",
            Unit::Centimetre => "cm",
            Unit::Metre => "m",
        }
    }

    /// True for inch, foot and feet-inches.
    pub fn is_imperial(self) -> bool {
        matches!(self, Unit::Inch | Unit::Foot | Unit::FeetInches)
    }
}

/// Converts a value from one unit to another.
pub fn convert(value: f64, from: Unit, to: Unit) -> f64 {
    if from.metres_per_unit() == to.metres_per_unit() {
        value
    } else {
        to.from_metres(from.to_metres(value))
    }
}

/// A number rounded to a binary fraction: `sign (whole + num/den)`, in lowest terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fraction {
    /// True for a negative value. Never set when the value rounds to zero.
    pub negative: bool,
    pub whole: u64,
    /// Numerator, less than `den`; 0 when there is no fractional part.
    pub num: u32,
    /// Denominator: a power of two up to 64; 1 when there is no fractional part.
    pub den: u32,
}

impl Fraction {
    /// The value as a float.
    pub fn value(&self) -> f64 {
        let v = self.whole as f64 + self.num as f64 / self.den as f64;
        if self.negative {
            -v
        } else {
            v
        }
    }

    /// True when the value is exactly zero.
    pub fn is_zero(&self) -> bool {
        self.whole == 0 && self.num == 0
    }
}

/// Formats as `3 1/2`, `1/2`, `3`, `0` or `-3 1/2`.
impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negative {
            write!(f, "-")?;
        }
        match (self.whole, self.num) {
            (w, 0) => write!(f, "{w}"),
            (0, n) => write!(f, "{n}/{}", self.den),
            (w, n) => write!(f, "{w} {n}/{}", self.den),
        }
    }
}

/// Makes a requested denominator usable: a power of two between 1 and 64 (rounded up).
fn sanitise_denominator(den: u32) -> u32 {
    den.clamp(1, MAX_FRACTION_DENOMINATOR).next_power_of_two()
}

/// Rounds `value` to the nearest `1/max_den` (ties away from zero) and reduces the fraction.
/// `max_den` is forced to a power of two between 1 and 64. A NaN gives zero; an infinite or
/// huge value saturates.
pub fn to_fraction(value: f64, max_den: u32) -> Fraction {
    let den = sanitise_denominator(max_den);
    // `as u64` maps NaN to 0 and saturates.
    let ticks = (value.abs() * den as f64).round() as u64;
    fraction_from_ticks(value < 0.0, ticks, den)
}

fn fraction_from_ticks(negative: bool, ticks: u64, den: u32) -> Fraction {
    let whole = ticks / den as u64;
    let mut num = (ticks % den as u64) as u32;
    let mut den = den;
    if num == 0 {
        den = 1;
    } else {
        // den is a power of two, so reducing is stripping common factors of two.
        while num.is_multiple_of(2) {
            num /= 2;
            den /= 2;
        }
    }
    Fraction {
        negative: negative && ticks != 0,
        whole,
        num,
        den,
    }
}

/// How finely a length is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Precision {
    /// Decimal places (capped at 12).
    Decimal(u8),
    /// The largest denominator: 1, 2, 4, 8, 16, 32 or 64. Other values are rounded up to a power
    /// of two and capped at 64.
    Fraction(u32),
}

/// A unit plus a precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LengthFormat {
    pub unit: Unit,
    pub precision: Precision,
}

impl LengthFormat {
    pub fn new(unit: Unit, precision: Precision) -> LengthFormat {
        LengthFormat { unit, precision }
    }

    /// Feet and inches to the nearest 1/`den` inch, e.g. `LengthFormat::architectural(16)`.
    pub fn architectural(den: u32) -> LengthFormat {
        LengthFormat::new(Unit::FeetInches, Precision::Fraction(den))
    }

    /// A unit with a number of decimal places.
    pub fn decimal(unit: Unit, places: u8) -> LengthFormat {
        LengthFormat::new(unit, Precision::Decimal(places))
    }

    /// Formats a length given in metres. See [`format_length`].
    pub fn format(&self, metres: f64) -> String {
        format_length(metres, self)
    }
}

const MAX_DECIMALS: u8 = 12;

/// A fixed-point decimal without a "-0.00".
pub(crate) fn format_decimal(value: f64, places: u8) -> String {
    let places = places.min(MAX_DECIMALS) as usize;
    let s = format!("{value:.places$}");
    match s.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => s,
    }
}

/// Formats a length given in **metres** in the requested unit and precision.
///
/// Rounding carries properly in feet-inches: 11 63/64" at 1/16 precision becomes `1'-0"`, never
/// `0'-12"`. A NaN formats as `NaN` and an infinity as `inf` / `-inf`, followed by the unit.
pub fn format_length(metres: f64, format: &LengthFormat) -> String {
    let unit = format.unit;
    let value = unit.from_metres(metres);

    if unit == Unit::FeetInches && value.is_finite() {
        return format_feet_inches(value * INCHES_PER_FOOT, format.precision);
    }

    let number = if !value.is_finite() {
        format!("{value}")
    } else {
        match format.precision {
            Precision::Decimal(places) => format_decimal(value, places),
            Precision::Fraction(den) => to_fraction(value, den).to_string(),
        }
    };
    match unit {
        Unit::Inch => format!("{number}\""),
        Unit::Foot | Unit::FeetInches => format!("{number}'"),
        other => format!("{number} {}", other.abbreviation()),
    }
}

fn format_feet_inches(total_inches: f64, precision: Precision) -> String {
    let negative = total_inches < 0.0;
    let abs = total_inches.abs();
    match precision {
        Precision::Fraction(den) => {
            let den = sanitise_denominator(den);
            let ticks = (abs * den as f64).round() as u64;
            let per_foot = 12 * den as u64;
            let feet = ticks / per_foot;
            let inches = fraction_from_ticks(false, ticks % per_foot, den);
            let sign = if negative && ticks != 0 { "-" } else { "" };
            format!("{sign}{feet}'-{inches}\"")
        }
        Precision::Decimal(places) => {
            let places = places.min(MAX_DECIMALS);
            let pow = 10f64.powi(places as i32);
            let ticks = (abs * pow).round();
            let per_foot = 12.0 * pow;
            let feet = (ticks / per_foot).floor();
            let inches = (ticks - feet * per_foot) / pow;
            let sign = if negative && ticks != 0.0 { "-" } else { "" };
            format!("{sign}{feet:.0}'-{}\"", format_decimal(inches, places))
        }
    }
}

/// Why a length could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LengthParseError {
    /// Nothing but white space (or only a sign).
    Empty,
    /// A number was expected at this character offset.
    InvalidNumber { position: usize },
    /// A unit name that is not recognised.
    UnknownUnit(String),
    /// A number without a unit where one is needed, as in `5 3` or `2m 30`.
    MissingUnit,
    /// A fraction with a zero denominator.
    ZeroDenominator,
    /// The value overflowed.
    OutOfRange,
}

impl fmt::Display for LengthParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LengthParseError::Empty => write!(f, "no length was entered"),
            LengthParseError::InvalidNumber { position } => {
                write!(f, "expected a number at character {}", position + 1)
            }
            LengthParseError::UnknownUnit(u) => write!(f, "unknown unit \"{u}\""),
            LengthParseError::MissingUnit => write!(f, "a number is missing its unit"),
            LengthParseError::ZeroDenominator => write!(f, "a fraction has a zero denominator"),
            LengthParseError::OutOfRange => write!(f, "the length is too large"),
        }
    }
}

impl std::error::Error for LengthParseError {}

/// Parses a typed length and returns it in **metres**.
///
/// Accepted forms (white space is free, units are case-insensitive):
///
/// * a bare number, taken in `default_unit` (feet for [`Unit::FeetInches`]): `2.5`, `.5`, `3 1/2`;
/// * a number with a unit: `12.5mm`, `30 cm`, `1.2 m`, `72pt`, `3.5in`, `3 1/2"`, `2.5'`, `2 ft`;
/// * fractions and mixed numbers: `1/2"`, `3 1/2"`, `3-1/2"`;
/// * feet and inches: `5'-3 1/2"`, `5' 3 1/2"`, `5'3"`, `5ft 3in`, `5' 3` (inches implied);
/// * sums of several terms: `1m 20cm`;
/// * a leading `-` or `+` applying to the whole length;
/// * typographic marks: prime, double prime and curly quotes for `'` and `"`.
///
/// A comma is not accepted as a decimal separator.
pub fn parse_length(input: &str, default_unit: Unit) -> Result<f64, LengthParseError> {
    let chars: Vec<char> = input
        .chars()
        .map(|c| match c {
            '\u{2032}' | '\u{2018}' | '\u{2019}' => '\'',
            '\u{2033}' | '\u{201C}' | '\u{201D}' => '"',
            '\u{2212}' | '\u{2013}' => '-',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect();
    let mut i = 0;
    skip_spaces(&chars, &mut i);

    let mut negative = false;
    if let Some(&c) = chars.get(i) {
        if c == '-' || c == '+' {
            negative = c == '-';
            i += 1;
        }
    }

    let mut terms: Vec<(f64, Option<Unit>)> = Vec::new();
    loop {
        skip_spaces(&chars, &mut i);
        if i >= chars.len() {
            break;
        }
        // A hyphen may separate terms, but only after a unit: 5'-3".
        if chars[i] == '-' {
            match terms.last() {
                Some((_, Some(_))) => {
                    i += 1;
                    skip_spaces(&chars, &mut i);
                }
                _ => return Err(LengthParseError::InvalidNumber { position: i }),
            }
        }
        let value = parse_number(&chars, &mut i)?;
        skip_spaces(&chars, &mut i);
        let unit = parse_unit(&chars, &mut i)?;
        terms.push((value, unit));
    }

    if terms.is_empty() {
        return Err(LengthParseError::Empty);
    }
    let last = terms.len() - 1;
    let mut metres = 0.0;
    for (idx, (value, unit)) in terms.iter().enumerate() {
        let unit = match unit {
            Some(u) => *u,
            None if terms.len() == 1 => match default_unit {
                Unit::FeetInches => Unit::Foot,
                u => u,
            },
            // 5' 3 means five feet three inches.
            None if idx == last && terms[idx - 1].1 == Some(Unit::Foot) => Unit::Inch,
            None => return Err(LengthParseError::MissingUnit),
        };
        metres += unit.to_metres(*value);
    }
    if !metres.is_finite() {
        return Err(LengthParseError::OutOfRange);
    }
    Ok(if negative { -metres } else { metres })
}

fn skip_spaces(chars: &[char], i: &mut usize) {
    while chars.get(*i) == Some(&' ') {
        *i += 1;
    }
}

/// Reads a run of ASCII digits and returns it as text.
fn take_digits(chars: &[char], i: &mut usize) -> String {
    let start = *i;
    while chars.get(*i).is_some_and(|c| c.is_ascii_digit()) {
        *i += 1;
    }
    chars[start..*i].iter().collect()
}

/// Reads `/den` at `*i` if present. `Ok(None)` when there is no slash followed by a digit.
fn take_denominator(chars: &[char], i: &mut usize) -> Result<Option<f64>, LengthParseError> {
    if chars.get(*i) != Some(&'/') || !chars.get(*i + 1).is_some_and(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    *i += 1;
    let den: f64 = take_digits(chars, i)
        .parse()
        .map_err(|_| LengthParseError::OutOfRange)?;
    if den == 0.0 {
        return Err(LengthParseError::ZeroDenominator);
    }
    Ok(Some(den))
}

/// Reads a non-negative number: an integer, a decimal, a fraction (`1/2`) or a mixed number
/// (`3 1/2`, `3-1/2`).
fn parse_number(chars: &[char], i: &mut usize) -> Result<f64, LengthParseError> {
    let start = *i;
    let int_part = take_digits(chars, i);
    let mut text = int_part.clone();
    let mut is_integer = true;
    if chars.get(*i) == Some(&'.') {
        let mut j = *i + 1;
        let frac = take_digits(chars, &mut j);
        // "5." is accepted as 5; a lone "." is not a number.
        if !frac.is_empty() || !int_part.is_empty() {
            is_integer = false;
            text.push('.');
            text.push_str(&frac);
            *i = j;
        }
    }
    if text.is_empty() {
        return Err(LengthParseError::InvalidNumber { position: start });
    }
    let value: f64 = text
        .parse()
        .map_err(|_| LengthParseError::InvalidNumber { position: start })?;
    if !is_integer {
        return Ok(value);
    }

    // A plain fraction: 1/2.
    if let Some(den) = take_denominator(chars, i)? {
        return Ok(value / den);
    }

    // A mixed number: whole, then spaces or one hyphen, then num/den.
    let mut j = *i;
    skip_spaces(chars, &mut j);
    if chars.get(j) == Some(&'-') {
        j += 1;
        skip_spaces(chars, &mut j);
    }
    let num_text = take_digits(chars, &mut j);
    if !num_text.is_empty() && chars.get(j) == Some(&'/') {
        if let Some(den) = take_denominator(chars, &mut j)? {
            let num: f64 = num_text.parse().map_err(|_| LengthParseError::OutOfRange)?;
            *i = j;
            return Ok(value + num / den);
        }
    }
    Ok(value)
}

/// Reads an optional unit at `*i`.
fn parse_unit(chars: &[char], i: &mut usize) -> Result<Option<Unit>, LengthParseError> {
    match chars.get(*i) {
        Some('\'') => {
            *i += 1;
            // Two apostrophes typed for a double quote.
            if chars.get(*i) == Some(&'\'') {
                *i += 1;
                return Ok(Some(Unit::Inch));
            }
            Ok(Some(Unit::Foot))
        }
        Some('"') => {
            *i += 1;
            Ok(Some(Unit::Inch))
        }
        Some(c) if c.is_alphabetic() => {
            let start = *i;
            while chars.get(*i).is_some_and(|c| c.is_alphabetic()) {
                *i += 1;
            }
            let word: String = chars[start..*i].iter().collect::<String>().to_lowercase();
            let unit = match word.as_str() {
                "pt" | "pts" | "point" | "points" => Unit::Point,
                "in" | "ins" | "inch" | "inches" => Unit::Inch,
                "ft" | "foot" | "feet" => Unit::Foot,
                "mm" | "millimetre" | "millimetres" | "millimeter" | "millimeters" => {
                    Unit::Millimetre
                }
                "cm" | "centimetre" | "centimetres" | "centimeter" | "centimeters" => {
                    Unit::Centimetre
                }
                "m" | "metre" | "metres" | "meter" | "meters" => Unit::Metre,
                _ => return Err(LengthParseError::UnknownUnit(word)),
            };
            // An abbreviation's full stop: "3 in." (but not the start of ".5").
            if chars.get(*i) == Some(&'.') && !chars.get(*i + 1).is_some_and(|c| c.is_ascii_digit())
            {
                *i += 1;
            }
            Ok(Some(unit))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    const FT: f64 = 0.3048;
    const IN: f64 = 0.0254;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
    }

    fn parse(s: &str, unit: Unit) -> f64 {
        parse_length(s, unit).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn conversion_factors() {
        assert!(near(Unit::Inch.to_metres(1.0), 0.0254));
        assert!(near(Unit::Foot.to_metres(1.0), 0.3048));
        assert!(near(Unit::FeetInches.to_metres(1.0), 0.3048));
        assert!(near(Unit::Point.to_metres(72.0), 0.0254));
        assert!(near(Unit::Millimetre.to_metres(25.4), 0.0254));
        assert!(near(Unit::Centimetre.to_metres(2.54), 0.0254));
        assert!(near(Unit::Metre.to_metres(1.0), 1.0));
        assert!(near(points_to_metres(72.0), 0.0254));
        assert!(near(metres_to_points(0.0254), 72.0));
        assert!(near(convert(1.0, Unit::Foot, Unit::Inch), 12.0));
        assert!(near(convert(1.0, Unit::Inch, Unit::Point), 72.0));
        assert!(near(convert(1.0, Unit::Inch, Unit::Millimetre), 25.4));
        assert!(near(convert(1000.0, Unit::Millimetre, Unit::Metre), 1.0));
        assert_eq!(convert(7.3, Unit::Foot, Unit::FeetInches), 7.3);
        assert_eq!(convert(7.3, Unit::Metre, Unit::Metre), 7.3);
    }

    #[test]
    fn prop_conversion_round_trips() {
        let mut rng = Rng::new(51);
        for _ in 0..500 {
            let v = rng.range(-1e6, 1e6);
            let a = *rng.pick(&Unit::ALL);
            let b = *rng.pick(&Unit::ALL);
            assert!(near(convert(convert(v, a, b), b, a), v));
            assert!(near(a.from_metres(a.to_metres(v)), v));
        }
    }

    #[test]
    fn unit_labels() {
        let labels: Vec<_> = Unit::ALL.iter().map(|u| u.abbreviation()).collect();
        assert_eq!(labels, ["pt", "in", "ft", "ft-in", "mm", "cm", "m"]);
        assert!(Unit::FeetInches.is_imperial() && Unit::Inch.is_imperial());
        assert!(!Unit::Metre.is_imperial() && !Unit::Point.is_imperial());
    }

    #[test]
    fn fraction_rounding_table() {
        let f = |v: f64, d: u32| to_fraction(v, d).to_string();
        assert_eq!(f(0.0, 16), "0");
        assert_eq!(f(0.5, 16), "1/2");
        assert_eq!(f(3.5, 16), "3 1/2");
        assert_eq!(f(3.25, 16), "3 1/4");
        assert_eq!(f(3.0625, 16), "3 1/16");
        assert_eq!(f(3.03125, 16), "3 1/16"); // tie rounds away from zero
        assert_eq!(f(3.03, 16), "3"); // below half a sixteenth
        assert_eq!(f(3.97, 16), "4"); // carries into the whole
        assert_eq!(f(3.97, 64), "3 31/32");
        assert_eq!(f(0.015625, 64), "1/64");
        assert_eq!(f(0.984375, 64), "63/64");
        assert_eq!(f(0.99, 64), "63/64");
        assert_eq!(f(0.995, 64), "1");
        assert_eq!(f(2.4, 1), "2");
        assert_eq!(f(2.5, 1), "3");
        assert_eq!(f(2.3, 2), "2 1/2");
        assert_eq!(f(-3.5, 8), "-3 1/2");
        assert_eq!(f(-0.001, 8), "0"); // no negative zero
    }

    #[test]
    fn fraction_denominator_is_sanitised() {
        assert_eq!(to_fraction(0.3, 0).den, 1);
        assert_eq!(to_fraction(1.0 / 3.0, 12), to_fraction(1.0 / 3.0, 16)); // rounded up to 16
        assert_eq!(to_fraction(1.0 / 128.0 + 1e-9, 1000).to_string(), "1/64"); // capped at 64
        assert!(to_fraction(f64::NAN, 16).is_zero());
        assert_eq!(to_fraction(f64::INFINITY, 16).whole, u64::MAX / 16);
    }

    /// Property: every k/64 is represented exactly and in lowest terms, and rounding never errs
    /// by more than half a tick.
    #[test]
    fn prop_fraction_is_exact_and_reduced() {
        for k in 0..=64 * 5 {
            let v = k as f64 / 64.0;
            let fr = to_fraction(v, 64);
            assert_eq!(fr.value(), v);
            assert!(fr.den.is_power_of_two() && fr.den <= 64);
            assert!(fr.num < fr.den);
            assert!(fr.num == 0 || fr.num % 2 == 1, "{fr:?} is not reduced");
            if fr.num == 0 {
                assert_eq!(fr.den, 1);
            }
        }
        let mut rng = Rng::new(52);
        for _ in 0..2000 {
            let v = rng.range(-1000.0, 1000.0);
            let den = *rng.pick(&[1u32, 2, 4, 8, 16, 32, 64]);
            let fr = to_fraction(v, den);
            assert!((fr.value() - v).abs() <= 0.5 / den as f64 + 1e-12);
            assert!(fr.den <= den);
        }
    }

    #[test]
    fn format_feet_inches_fractional() {
        let f = LengthFormat::architectural(16);
        assert_eq!(f.format(5.0 * FT + 3.5 * IN), "5'-3 1/2\"");
        assert_eq!(f.format(5.0 * FT), "5'-0\"");
        assert_eq!(f.format(3.5 * IN), "0'-3 1/2\"");
        assert_eq!(f.format(0.5 * IN), "0'-1/2\"");
        assert_eq!(f.format(0.0), "0'-0\"");
        assert_eq!(f.format(-(5.0 * FT + 3.5 * IN)), "-5'-3 1/2\"");
        assert_eq!(f.format(-1e-9), "0'-0\"");
        assert_eq!(f.format(100.0 * FT + 11.0 * IN), "100'-11\"");
    }

    #[test]
    fn format_feet_inches_carries() {
        // 11 63/64" rounds up to a whole foot at 1/16, and stays put at 1/64.
        let v = 11.0 * IN + 63.0 / 64.0 * IN;
        assert_eq!(LengthFormat::architectural(16).format(v), "1'-0\"");
        assert_eq!(LengthFormat::architectural(64).format(v), "0'-11 63/64\"");
        // 5' 11 31/32" at 1/16: carries through inches into feet.
        let v = 5.0 * FT + (11.0 + 31.0 / 32.0) * IN;
        assert_eq!(LengthFormat::architectural(16).format(v), "6'-0\"");
        // Fraction carries into whole inches.
        let v = 2.0 * FT + 3.99 * IN;
        assert_eq!(LengthFormat::architectural(8).format(v), "2'-4\"");
        // Whole-inch precision.
        assert_eq!(
            LengthFormat::architectural(1).format(2.0 * FT + 3.4 * IN),
            "2'-3\""
        );
        assert_eq!(
            LengthFormat::architectural(1).format(2.0 * FT + 11.6 * IN),
            "3'-0\""
        );
    }

    #[test]
    fn format_feet_inches_decimal() {
        let f = LengthFormat::decimal(Unit::FeetInches, 2);
        assert_eq!(f.format(5.0 * FT + 3.5 * IN), "5'-3.50\"");
        assert_eq!(f.format(11.999 * IN), "1'-0.00\"");
        assert_eq!(f.format(-(1.0 * FT + 0.25 * IN)), "-1'-0.25\"");
        assert_eq!(
            LengthFormat::decimal(Unit::FeetInches, 0).format(5.0 * FT + 3.4 * IN),
            "5'-3\""
        );
    }

    #[test]
    fn format_fraction_table_to_64ths() {
        // The fraction table from the M6 acceptance row: every 64th of an inch within a foot.
        let f = LengthFormat::architectural(64);
        for feet in [0u64, 1, 7] {
            for ticks in 0..12 * 64u64 {
                let inches = ticks as f64 / 64.0;
                let metres = feet as f64 * FT + inches * IN;
                let want_in = fraction_from_ticks(false, ticks, 64);
                assert_eq!(f.format(metres), format!("{feet}'-{want_in}\""));
            }
        }
    }

    #[test]
    fn format_other_units() {
        assert_eq!(
            LengthFormat::decimal(Unit::Inch, 2).format(3.5 * IN),
            "3.50\""
        );
        assert_eq!(
            LengthFormat::new(Unit::Inch, Precision::Fraction(16)).format(3.5 * IN),
            "3 1/2\""
        );
        assert_eq!(
            LengthFormat::new(Unit::Inch, Precision::Fraction(16)).format(63.5 * IN),
            "63 1/2\""
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Foot, 2).format(2.5 * FT),
            "2.50'"
        );
        assert_eq!(
            LengthFormat::new(Unit::Foot, Precision::Fraction(4)).format(2.25 * FT),
            "2 1/4'"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Millimetre, 1).format(0.0125),
            "12.5 mm"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Millimetre, 0).format(1.2345),
            "1234 mm"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Centimetre, 1).format(0.125),
            "12.5 cm"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Metre, 3).format(1.25),
            "1.250 m"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Point, 2).format(0.0254),
            "72.00 pt"
        );
        assert_eq!(
            LengthFormat::new(Unit::Metre, Precision::Fraction(4)).format(1.25),
            "1 1/4 m"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Metre, 2).format(-1.5),
            "-1.50 m"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Metre, 2).format(-0.001),
            "0.00 m"
        );
        // Decimal places are capped rather than panicking.
        assert_eq!(
            LengthFormat::decimal(Unit::Metre, 200).format(1.0),
            "1.000000000000 m"
        );
    }

    #[test]
    fn format_non_finite() {
        assert_eq!(
            LengthFormat::decimal(Unit::Metre, 2).format(f64::NAN),
            "NaN m"
        );
        assert_eq!(
            LengthFormat::decimal(Unit::Metre, 2).format(f64::INFINITY),
            "inf m"
        );
        assert_eq!(
            LengthFormat::architectural(16).format(f64::NEG_INFINITY),
            "-inf'"
        );
    }

    #[test]
    fn parse_metric_and_points() {
        assert!(near(parse("12.5mm", Unit::Inch), 0.0125));
        assert!(near(parse("12.5 mm", Unit::Inch), 0.0125));
        assert!(near(parse("  30 cm ", Unit::Inch), 0.30));
        assert!(near(parse("1.2m", Unit::Inch), 1.2));
        assert!(near(parse("1.2 M", Unit::Inch), 1.2));
        assert!(near(parse("2 metres", Unit::Inch), 2.0));
        assert!(near(parse("2 Meters", Unit::Inch), 2.0));
        assert!(near(parse("72pt", Unit::Metre), 0.0254));
        assert!(near(parse("1m 20cm", Unit::Inch), 1.2));
        assert!(near(parse("1 m 20 cm 5 mm", Unit::Inch), 1.205));
        assert!(near(parse(".5 m", Unit::Inch), 0.5));
        assert!(near(parse("5. m", Unit::Inch), 5.0));
    }

    #[test]
    fn parse_bare_numbers_use_the_default_unit() {
        assert!(near(parse("2.5", Unit::Metre), 2.5));
        assert!(near(parse("2.5", Unit::Millimetre), 0.0025));
        assert!(near(parse("2.5", Unit::Inch), 2.5 * IN));
        assert!(near(parse("2.5", Unit::Foot), 2.5 * FT));
        assert!(near(parse("2.5", Unit::FeetInches), 2.5 * FT));
        assert!(near(parse("72", Unit::Point), IN));
        assert!(near(parse("3 1/2", Unit::Inch), 3.5 * IN));
        assert!(near(parse("1/4", Unit::Inch), 0.25 * IN));
        assert!(near(parse("0", Unit::Metre), 0.0));
    }

    #[test]
    fn parse_inches() {
        for s in [
            "3.5\"",
            "3.5 in",
            "3.5in",
            "3.5 inches",
            "3 1/2\"",
            "3-1/2\"",
            "3 1/2 in",
            "3.5''",
            "3.5 in.",
            "3 1/2\u{2033}",
            "3 1/2\u{201D}",
            "7/2\"",
        ] {
            assert!(near(parse(s, Unit::Metre), 3.5 * IN), "{s:?}");
        }
        assert!(near(parse("1/64\"", Unit::Metre), IN / 64.0));
        assert!(near(parse("63/64 in", Unit::Metre), 63.0 / 64.0 * IN));
    }

    #[test]
    fn parse_feet_and_inches() {
        let want = 5.0 * FT + 3.5 * IN;
        for s in [
            "5'-3 1/2\"",
            "5' 3 1/2\"",
            "5'3 1/2\"",
            "5' - 3 1/2\"",
            "5'-3-1/2\"",
            "5ft 3.5in",
            "5 ft 3 1/2 in",
            "5 feet 3.5 inches",
            "5' 3.5",
            "5'-3 1/2",
            "5\u{2032}-3 1/2\u{2033}",
            "5\u{2019} 3 1/2\u{201D}",
            "5 FT 3.5 IN",
        ] {
            assert!(near(parse(s, Unit::Metre), want), "{s:?}");
        }
        assert!(near(parse("5'", Unit::Metre), 5.0 * FT));
        assert!(near(parse("5'-0\"", Unit::Metre), 5.0 * FT));
        assert!(near(parse("0'-3\"", Unit::Metre), 3.0 * IN));
        assert!(near(parse("2.5'", Unit::Metre), 2.5 * FT));
        assert!(near(parse("2 1/2 ft", Unit::Metre), 2.5 * FT));
        assert!(near(parse("5' 3", Unit::Metre), 5.0 * FT + 3.0 * IN));
        assert!(near(parse("5'3", Unit::Metre), 5.0 * FT + 3.0 * IN));
    }

    #[test]
    fn parse_signs() {
        assert!(near(parse("-5'-3\"", Unit::Metre), -(5.0 * FT + 3.0 * IN)));
        assert!(near(parse("- 2.5 m", Unit::Inch), -2.5));
        assert!(near(parse("+2.5 m", Unit::Inch), 2.5));
        assert!(near(parse("\u{2212}3 mm", Unit::Inch), -0.003));
    }

    #[test]
    fn parse_errors() {
        use LengthParseError::*;
        let err = |s: &str| parse_length(s, Unit::Metre).unwrap_err();
        assert_eq!(err(""), Empty);
        assert_eq!(err("   "), Empty);
        assert_eq!(err("-"), Empty);
        assert_eq!(err("abc"), InvalidNumber { position: 0 });
        assert_eq!(err("m"), InvalidNumber { position: 0 });
        assert_eq!(err("."), InvalidNumber { position: 0 });
        assert_eq!(err("--5"), InvalidNumber { position: 1 });
        assert_eq!(err("5 - 3"), InvalidNumber { position: 2 });
        assert_eq!(err("5 m, 3 cm"), InvalidNumber { position: 3 });
        assert_eq!(err("1,5 m"), InvalidNumber { position: 1 });
        assert_eq!(err("5 furlongs"), UnknownUnit("furlongs".into()));
        assert_eq!(err("5 MMM"), UnknownUnit("mmm".into()));
        assert_eq!(err("5 3"), MissingUnit);
        assert_eq!(err("2m 30"), MissingUnit);
        assert_eq!(err("5 3 m"), MissingUnit);
        assert_eq!(err("5\" 3"), MissingUnit);
        assert_eq!(err("1/0 in"), ZeroDenominator);
        assert_eq!(err("3 1/0\""), ZeroDenominator);
        assert_eq!(err("1e5 m"), UnknownUnit("e".into()));
        let huge = format!("{} m", "9".repeat(400));
        assert_eq!(err(&huge), OutOfRange);
        for e in [
            Empty,
            InvalidNumber { position: 3 },
            UnknownUnit("x".into()),
            MissingUnit,
            ZeroDenominator,
            OutOfRange,
        ] {
            assert!(!e.to_string().is_empty());
        }
    }

    /// Property: whatever we format, we can parse back to within the format's rounding.
    #[test]
    fn prop_format_then_parse_round_trips() {
        let mut rng = Rng::new(53);
        for _ in 0..3000 {
            let unit = *rng.pick(&Unit::ALL);
            let precision = if rng.unit() < 0.5 {
                Precision::Decimal(rng.int(0, 6) as u8)
            } else {
                Precision::Fraction(*rng.pick(&[1u32, 2, 4, 8, 16, 32, 64]))
            };
            let fmt = LengthFormat::new(unit, precision);
            // A value of a sensible size in the unit itself.
            let in_unit = rng.range(-500.0, 500.0);
            let metres = unit.to_metres(in_unit);
            let text = fmt.format(metres);
            // Parse with a different default unit to prove the text carries its own unit.
            let back = parse_length(&text, Unit::Point)
                .unwrap_or_else(|e| panic!("{text:?} from {fmt:?}: {e}"));

            // Half a step of the format, in metres. Feet-inches rounds in inches.
            let step_unit = if unit == Unit::FeetInches {
                Unit::Inch
            } else {
                unit
            };
            let step = match precision {
                Precision::Decimal(p) => 10f64.powi(-(p as i32)),
                Precision::Fraction(d) => 1.0 / d as f64,
            };
            let tol = step_unit.to_metres(step) * 0.5 * (1.0 + 1e-9) + 1e-12;
            assert!(
                (back - metres).abs() <= tol,
                "{metres} m -> {text:?} -> {back} m (tol {tol})"
            );
        }
    }

    /// Property: parsing a formatted exact 64th gives the value back exactly (to float precision).
    #[test]
    fn prop_exact_64ths_round_trip() {
        let mut rng = Rng::new(54);
        let fmt = LengthFormat::architectural(64);
        for _ in 0..1000 {
            let ticks = rng.int(0, 200 * 12 * 64);
            let metres = ticks as f64 / 64.0 * IN;
            let text = fmt.format(metres);
            let back = parse(&text, Unit::Metre);
            assert!(near(back, metres), "{text:?}: {back} vs {metres}");
            assert_eq!(fmt.format(back), text);
        }
    }
}
