//! The cell value of the markups list, and the ordering every sort and group uses.

use std::cmp::Ordering;

use pl_annot::PdfDate;
use serde::{Deserialize, Serialize};

/// One cell.
///
/// `Empty` is the absence of a value; a text cell holding only white space counts as empty too
/// ([`Value::is_empty`]). A number is always finite ([`Value::number`]).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(try_from = "ValueRepr", into = "ValueRepr")]
pub enum Value {
    #[default]
    Empty,
    Text(String),
    Number(f64),
    Bool(bool),
    Date(PdfDate),
    /// An 8-bit RGB colour.
    Colour([u8; 3]),
}

/// The serialised form of a [`Value`]: a date is its PDF date string.
#[derive(Serialize, Deserialize)]
enum ValueRepr {
    Empty,
    Text(String),
    Number(f64),
    Bool(bool),
    Date(String),
    Colour([u8; 3]),
}

impl From<Value> for ValueRepr {
    fn from(v: Value) -> ValueRepr {
        match v {
            Value::Empty => ValueRepr::Empty,
            Value::Text(s) => ValueRepr::Text(s),
            Value::Number(n) => ValueRepr::Number(n),
            Value::Bool(b) => ValueRepr::Bool(b),
            Value::Date(d) => ValueRepr::Date(d.to_pdf_string()),
            Value::Colour(c) => ValueRepr::Colour(c),
        }
    }
}

impl TryFrom<ValueRepr> for Value {
    type Error = String;

    fn try_from(v: ValueRepr) -> Result<Value, String> {
        Ok(match v {
            ValueRepr::Empty => Value::Empty,
            ValueRepr::Text(s) => Value::Text(s),
            ValueRepr::Number(n) if n.is_finite() => Value::Number(n + 0.0),
            ValueRepr::Number(_) => Value::Empty,
            ValueRepr::Bool(b) => Value::Bool(b),
            ValueRepr::Date(s) => {
                Value::Date(PdfDate::parse(&s).ok_or_else(|| format!("not a PDF date: {s}"))?)
            }
            ValueRepr::Colour(c) => Value::Colour(c),
        })
    }
}

impl Value {
    /// A text cell; `Empty` for an empty string.
    pub fn text(s: impl Into<String>) -> Value {
        let s = s.into();
        if s.is_empty() {
            Value::Empty
        } else {
            Value::Text(s)
        }
    }

    /// A text cell from an optional string.
    pub fn opt_text(s: Option<&str>) -> Value {
        s.map_or(Value::Empty, Value::text)
    }

    /// A number cell; `Empty` for NaN and the infinities.
    pub fn number(n: f64) -> Value {
        if n.is_finite() {
            // Negative zero is zero: one group, one filter value, as it is one display.
            Value::Number(n + 0.0)
        } else {
            Value::Empty
        }
    }

    /// A date without a time of day (midnight, no time zone).
    pub fn day(year: u16, month: u8, day: u8) -> Value {
        Value::Date(PdfDate {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
            utc_offset_minutes: None,
        })
    }

    /// True for `Empty` and for text that is empty or all white space.
    pub fn is_empty(&self) -> bool {
        match self {
            Value::Empty => true,
            Value::Text(s) => s.trim().is_empty(),
            _ => false,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The cell as shown in the list and written to CSV: nothing for `Empty`, the shortest
    /// exact decimal for a number, `Yes`/`No`, `YYYY-MM-DD HH:MM:SS` (the time is left out of
    /// a date that has none) and `#RRGGBB`.
    pub fn display(&self) -> String {
        let mut out = String::new();
        self.display_into(&mut out);
        out
    }

    /// Appends [`Value::display`] to `out`.
    pub fn display_into(&self, out: &mut String) {
        use std::fmt::Write;
        match self {
            Value::Empty => {}
            Value::Text(s) => out.push_str(s),
            Value::Number(n) => {
                let _ = write!(out, "{}", if *n == 0.0 { 0.0 } else { *n });
            }
            Value::Bool(b) => out.push_str(if *b { "Yes" } else { "No" }),
            Value::Date(d) => {
                let _ = write!(out, "{}", format_date(d));
            }
            Value::Colour([r, g, b]) => {
                let _ = write!(out, "#{r:02X}{g:02X}{b:02X}");
            }
        }
    }

    /// The rank of the type when two cells of different types are compared.
    fn rank(&self) -> u8 {
        match self {
            Value::Number(_) => 0,
            Value::Date(_) => 1,
            Value::Bool(_) => 2,
            Value::Colour(_) => 3,
            Value::Text(_) => 4,
            Value::Empty => 5,
        }
    }

    /// The order of the list: numbers by value, dates by instant, `No` before `Yes`, colours
    /// by their components, text by [`natural_cmp`]. Cells of different types order by type
    /// (number, date, checkmark, colour, text) and an empty cell comes after everything.
    ///
    /// Two cells that compare `Equal` are the same group key ([`Value::same`]).
    pub fn order(&self, other: &Value) -> Ordering {
        match (self.is_empty(), other.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            _ => {}
        }
        match (self, other) {
            // Adding zero turns a negative zero into zero, which `total_cmp` tells apart.
            (Value::Number(a), Value::Number(b)) => (a + 0.0).total_cmp(&(b + 0.0)),
            (Value::Date(a), Value::Date(b)) => a.to_unix().cmp(&b.to_unix()),
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Colour(a), Value::Colour(b)) => a.cmp(b),
            (Value::Text(a), Value::Text(b)) => natural_cmp(a, b),
            _ => self.rank().cmp(&other.rank()),
        }
    }

    /// Equal under [`Value::order`]: text without regard to case, `2` and `2.0` alike, two
    /// dates at the same instant, any two empty cells.
    pub fn same(&self, other: &Value) -> bool {
        self.order(other) == Ordering::Equal
    }
}

/// `YYYY-MM-DD HH:MM:SS`, or `YYYY-MM-DD` for a date at midnight with no time zone (the form
/// a date-only column holds).
pub fn format_date(d: &PdfDate) -> String {
    if is_day(d) {
        format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
    } else {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            d.year, d.month, d.day, d.hour, d.minute, d.second
        )
    }
}

/// True for a date with no time of day and no time zone.
pub fn is_day(d: &PdfDate) -> bool {
    d.hour == 0 && d.minute == 0 && d.second == 0 && d.utc_offset_minutes.is_none()
}

/// Parses what a user types for a date: `YYYY-MM-DD`, optionally followed by `HH:MM` or
/// `HH:MM:SS` after a space or a `T`; or a PDF date string (`D:2026...`).
pub fn parse_date(text: &str) -> Option<PdfDate> {
    let t = text.trim();
    if t.starts_with("D:") {
        return PdfDate::parse(t);
    }
    let (date, time) = match t.split_once([' ', 'T']) {
        Some((d, rest)) => (d, Some(rest.trim())),
        None => (t, None),
    };
    let mut parts = date.split('-');
    let year: u16 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || year > 9999 {
        return None;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day == 0 || day > days {
        return None;
    }
    let (mut hour, mut minute, mut second) = (0u8, 0u8, 0u8);
    if let Some(time) = time.filter(|t| !t.is_empty()) {
        let mut p = time.split(':');
        hour = p.next()?.parse().ok()?;
        minute = p.next()?.parse().ok()?;
        second = match p.next() {
            Some(s) => s.parse().ok()?,
            None => 0,
        };
        if p.next().is_some() || hour > 23 || minute > 59 || second > 59 {
            return None;
        }
    }
    Some(PdfDate {
        year,
        month,
        day,
        hour,
        minute,
        second,
        utc_offset_minutes: None,
    })
}

/// Lower case of one character, for comparing without regard to case.
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Natural ordering of text: case is ignored and a run of digits is compared as the whole
/// number it spells, so `Door 2` comes before `Door 10` and `A9` before `a10`. Leading zeros do
/// not change a number's value; `7` and `007` compare equal.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut x = a.chars().peekable();
    let mut y = b.chars().peekable();
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                // Skip leading zeros, then the longer run is the larger number; runs of the
                // same length compare digit by digit.
                while x.peek() == Some(&'0') {
                    x.next();
                }
                while y.peek() == Some(&'0') {
                    y.next();
                }
                let mut first_difference = Ordering::Equal;
                loop {
                    let dp = x.peek().copied().filter(char::is_ascii_digit);
                    let dq = y.peek().copied().filter(char::is_ascii_digit);
                    match (dp, dq) {
                        (None, None) => break,
                        (None, Some(_)) => return Ordering::Less,
                        (Some(_), None) => return Ordering::Greater,
                        (Some(p), Some(q)) => {
                            if first_difference == Ordering::Equal {
                                first_difference = p.cmp(&q);
                            }
                            x.next();
                            y.next();
                        }
                    }
                }
                if first_difference != Ordering::Equal {
                    return first_difference;
                }
            }
            (Some(p), Some(q)) => {
                let (p, q) = (fold(p), fold(q));
                if p != q {
                    return p.cmp(&q);
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_reads_digit_runs_as_numbers() {
        let mut names = vec![
            "Door 10", "door 2", "Door 1", "Door 2b", "Door", "Door 02a", "A9", "a10", "Z",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            ["A9", "a10", "Door", "Door 1", "door 2", "Door 02a", "Door 2b", "Door 10", "Z"]
        );
        assert_eq!(natural_cmp("7", "007"), Ordering::Equal);
        assert_eq!(natural_cmp("x7y", "X007Y"), Ordering::Equal);
        assert_eq!(natural_cmp("x7y", "x7z"), Ordering::Less);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        // Longer than any integer type: still compared as numbers.
        assert_eq!(
            natural_cmp(
                "n99999999999999999999999999",
                "n100000000000000000000000000"
            ),
            Ordering::Less
        );
        assert_eq!(natural_cmp("Ärger", "ärger"), Ordering::Equal);
    }

    #[test]
    fn values_order_by_type_then_value_with_empty_last() {
        let mut v = vec![
            Value::Empty,
            Value::text("b"),
            Value::Number(10.0),
            Value::text("   "),
            Value::Number(-2.5),
            Value::Bool(true),
            Value::text("A"),
            Value::Bool(false),
            Value::day(2026, 1, 2),
            Value::Colour([0, 0, 9]),
        ];
        v.sort_by(|a, b| a.order(b));
        assert_eq!(
            v.iter().map(Value::display).collect::<Vec<_>>(),
            [
                "-2.5",
                "10",
                "2026-01-02",
                "No",
                "Yes",
                "#000009",
                "A",
                "b",
                "",
                "   "
            ]
        );
        assert!(Value::text("Steel").same(&Value::text("STEEL")));
        assert!(Value::Number(2.0).same(&Value::Number(2.0)));
        // Negative zero displays as "0" and is the same key as zero, however it was made.
        assert!(Value::Number(-0.0).same(&Value::Number(0.0)));
        assert_eq!(
            Value::Number(-0.0).order(&Value::Number(0.0)),
            Ordering::Equal
        );
        assert!(matches!(Value::number(-0.0), Value::Number(n) if n.is_sign_positive()));
        let read: Value = serde_json::from_str(r#"{"Number":-0.0}"#).unwrap();
        assert!(matches!(read, Value::Number(n) if n.is_sign_positive()));
        assert_eq!(
            Value::Number(-1.0).order(&Value::Number(-0.0)),
            Ordering::Less
        );
        assert!(!Value::Number(2.0).same(&Value::text("2")));
        assert!(Value::Empty.same(&Value::text(" ")));
    }

    #[test]
    fn display_forms() {
        assert_eq!(Value::Number(3.0).display(), "3");
        assert_eq!(Value::Number(-0.0).display(), "0");
        assert_eq!(Value::Number(0.1 + 0.2).display(), "0.30000000000000004");
        assert_eq!(Value::number(f64::NAN), Value::Empty);
        assert_eq!(
            Value::Date(PdfDate::parse("D:20260309141501+02'00'").unwrap()).display(),
            "2026-03-09 14:15:01"
        );
        assert_eq!(Value::Colour([255, 0, 171]).display(), "#FF00AB");
    }

    #[test]
    fn dates_are_parsed_strictly() {
        assert_eq!(
            parse_date("2024-02-29").map(Value::Date),
            Some(Value::day(2024, 2, 29))
        );
        assert_eq!(parse_date("2023-02-29"), None);
        assert_eq!(parse_date("2023-13-01"), None);
        assert_eq!(parse_date("2023-1-1-1"), None);
        assert_eq!(parse_date("yesterday"), None);
        let d = parse_date("2026-11-24T08:05").unwrap();
        assert_eq!((d.hour, d.minute, d.second), (8, 5, 0));
        assert_eq!(parse_date("2026-11-24 25:00"), None);
        assert_eq!(
            parse_date("D:20261124080500Z"),
            PdfDate::parse("D:20261124080500Z")
        );
    }

    #[test]
    fn values_serialise_and_come_back() {
        let all = vec![
            Value::Empty,
            Value::text("a \"b\""),
            Value::Number(0.1),
            Value::Bool(true),
            Value::Date(PdfDate::parse("D:20260309141501-05'30'").unwrap()),
            Value::Colour([1, 2, 3]),
        ];
        let json = serde_json::to_string(&all).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Value>>(&json).unwrap(), all);
        assert!(serde_json::from_str::<Value>(r#"{"Date":"nonsense"}"#).is_err());
    }
}
