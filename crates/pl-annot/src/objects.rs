//! Small conversions between model values and PDF objects: numbers, names, text strings and
//! dates. The object types are lopdf's, re-exported by pl-pdf.

use pl_pdf::{Dictionary, Object, PdfDocument, StringFormat};

pub(crate) fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

/// A number as it is written: an integer when the value is whole, otherwise a real. PDF reals
/// are single precision in lopdf, so this is where a coordinate loses its f64 tail.
pub(crate) fn num(v: f64) -> Object {
    if !v.is_finite() {
        return Object::Integer(0);
    }
    if v.fract() == 0.0 && v.abs() < 1e9 {
        Object::Integer(v as i64)
    } else {
        Object::Real(v as f32)
    }
}

pub(crate) fn nums(values: &[f64]) -> Object {
    Object::Array(values.iter().map(|v| num(*v)).collect())
}

/// `dict[key]` with one level of indirection followed.
pub(crate) fn lookup<'a>(
    doc: &'a PdfDocument,
    dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Object> {
    let value = dict.get(key).ok()?;
    match value {
        Object::Reference(_) => doc.resolve(value),
        Object::Null => None,
        other => Some(other),
    }
}

/// An array of numbers (each possibly indirect). `None` if any entry is not a number.
pub(crate) fn number_array(doc: &PdfDocument, object: &Object) -> Option<Vec<f64>> {
    let array = object.as_array().ok()?;
    array
        .iter()
        .map(|item| match item {
            Object::Reference(_) => doc.resolve(item).and_then(pl_pdf::number),
            other => pl_pdf::number(other),
        })
        .collect()
}

pub(crate) fn name_of(object: &Object) -> Option<String> {
    match object {
        Object::Name(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

/// PDFDocEncoding for the bytes where it differs from Latin-1.
fn pdf_doc_char(byte: u8) -> char {
    const HIGH: [char; 33] = [
        '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}',
        '\u{2044}', '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}',
        '\u{201D}', '\u{2018}', '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}',
        '\u{0141}', '\u{0152}', '\u{0160}', '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}',
        '\u{0153}', '\u{0161}', '\u{017E}', '\u{FFFD}', '\u{20AC}',
    ];
    const LOW: [char; 8] = [
        '\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}',
        '\u{02DC}',
    ];
    match byte {
        0x18..=0x1F => LOW[(byte - 0x18) as usize],
        0x80..=0xA0 => HIGH[(byte - 0x80) as usize],
        other => other as char,
    }
}

/// Decodes a PDF text string: UTF-16BE or UTF-8 when it starts with the matching byte-order
/// mark, PDFDocEncoding otherwise. Never fails; undecodable units become U+FFFD.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        // Not allowed by the specification, but written by some producers.
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|b| pdf_doc_char(*b)).collect()
}

/// Encodes a PDF text string: plain bytes when the text is printable ASCII (plus tab, line
/// feed and carriage return), UTF-16BE with a byte-order mark otherwise.
pub fn encode_text(text: &str) -> Object {
    let plain = text
        .chars()
        .all(|c| matches!(c, ' '..='~' | '\n' | '\r' | '\t'));
    if plain {
        return Object::String(text.as_bytes().to_vec(), StringFormat::Literal);
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

pub(crate) fn text_of(object: &Object) -> Option<String> {
    match object {
        Object::String(bytes, _) => Some(decode_text(bytes)),
        _ => None,
    }
}

/// A PDF date (`D:YYYYMMDDHHmmSSOHH'mm'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PdfDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Offset of local time from UTC in minutes; `None` when the date states none.
    pub utc_offset_minutes: Option<i16>,
}

impl PdfDate {
    /// A UTC date from seconds since 1970-01-01T00:00:00Z.
    pub fn from_unix(seconds: i64) -> PdfDate {
        let days = seconds.div_euclid(86_400);
        let rem = seconds.rem_euclid(86_400);
        // Civil date from a day count (proleptic Gregorian calendar).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        PdfDate {
            year: year.clamp(0, 9999) as u16,
            month: month as u8,
            day: day as u8,
            hour: (rem / 3600) as u8,
            minute: (rem % 3600 / 60) as u8,
            second: (rem % 60) as u8,
            utc_offset_minutes: Some(0),
        }
    }

    /// The current time, in UTC.
    pub fn now() -> PdfDate {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        PdfDate::from_unix(seconds)
    }

    /// Parses a PDF date. The `D:` prefix and every field after the year are optional, as the
    /// specification allows; the apostrophes of the offset may be missing.
    pub fn parse(text: &str) -> Option<PdfDate> {
        let s = text.trim();
        let s = s.strip_prefix("D:").unwrap_or(s);
        let b = s.as_bytes();
        let digits = |from: usize, len: usize| -> Option<u32> {
            let part = b.get(from..from + len)?;
            if !part.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(part).ok()?.parse().ok()
        };
        let year = digits(0, 4)?;
        let mut at = 4;
        let mut field = |default: u32, lo: u32, hi: u32| -> Option<u32> {
            match digits(at, 2) {
                Some(v) if (lo..=hi).contains(&v) => {
                    at += 2;
                    Some(v)
                }
                Some(_) => None,
                None => Some(default),
            }
        };
        let month = field(1, 1, 12)?;
        let day = field(1, 1, 31)?;
        let hour = field(0, 0, 23)?;
        let minute = field(0, 0, 59)?;
        let second = field(0, 0, 59)?;
        let rest = &s[at.min(s.len())..];
        let utc_offset_minutes = match rest.as_bytes().first() {
            None => None,
            Some(b'Z') | Some(b'z') => Some(0),
            Some(sign @ (b'+' | b'-')) => {
                let nums: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
                let hh: i16 = nums.get(0..2).and_then(|v| v.parse().ok())?;
                let mm: i16 = nums.get(2..4).and_then(|v| v.parse().ok()).unwrap_or(0);
                if hh > 23 || mm > 59 {
                    return None;
                }
                let total = hh * 60 + mm;
                Some(if *sign == b'-' { -total } else { total })
            }
            Some(_) => return None,
        };
        Some(PdfDate {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            hour: hour as u8,
            minute: minute as u8,
            second: second as u8,
            utc_offset_minutes,
        })
    }

    /// Seconds since 1970-01-01T00:00:00Z, taking a date without an offset as UTC. Two dates
    /// compare in time order by this.
    pub fn to_unix(&self) -> i64 {
        let (y, m, d) = (
            i64::from(self.year),
            i64::from(self.month.clamp(1, 12)),
            i64::from(self.day.clamp(1, 31)),
        );
        // Day count from a civil date (proleptic Gregorian calendar).
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        days * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
            - i64::from(self.utc_offset_minutes.unwrap_or(0)) * 60
    }

    /// The date as the string stored in `/M` and `/CreationDate`.
    pub fn to_pdf_string(&self) -> String {
        let mut out = format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        );
        match self.utc_offset_minutes {
            None => {}
            Some(0) => out.push('Z'),
            Some(offset) => {
                let sign = if offset < 0 { '-' } else { '+' };
                let abs = offset.unsigned_abs();
                out.push_str(&format!("{sign}{:02}'{:02}'", abs / 60, abs % 60));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_text_is_written_as_plain_bytes() {
        let o = encode_text("Line 1\nLine (2)");
        assert_eq!(
            o,
            Object::String(b"Line 1\nLine (2)".to_vec(), StringFormat::Literal)
        );
        assert_eq!(text_of(&o).unwrap(), "Line 1\nLine (2)");
    }

    #[test]
    fn other_text_round_trips_as_utf16() {
        for s in [
            "Ø 25 mm ± 0,5",
            "面積 12 m²",
            "emoji \u{1F4D0} ok",
            "caf\u{e9}",
        ] {
            let o = encode_text(s);
            let Object::String(bytes, _) = &o else {
                panic!()
            };
            assert_eq!(&bytes[..2], &[0xFE, 0xFF]);
            assert_eq!(text_of(&o).unwrap(), s);
        }
    }

    #[test]
    fn pdf_doc_encoding_and_boms_are_decoded() {
        assert_eq!(
            decode_text(b"a\x80b\x92c\xA0\xE9"),
            "a\u{2022}b\u{2122}c\u{20AC}\u{e9}"
        );
        assert_eq!(decode_text(&[0xEF, 0xBB, 0xBF, 0xC3, 0xA9]), "\u{e9}");
        assert_eq!(decode_text(&[0xFF, 0xFE, 0x41, 0x00]), "A");
        // An odd trailing byte does not panic.
        assert_eq!(decode_text(&[0xFE, 0xFF, 0x00, 0x41, 0x00]), "A\u{0}");
        assert_eq!(decode_text(b""), "");
    }

    #[test]
    fn dates_parse_and_print() {
        let d = PdfDate::parse("D:20240102030405Z").unwrap();
        assert_eq!((d.year, d.month, d.day), (2024, 1, 2));
        assert_eq!((d.hour, d.minute, d.second), (3, 4, 5));
        assert_eq!(d.utc_offset_minutes, Some(0));
        assert_eq!(d.to_pdf_string(), "D:20240102030405Z");

        let d = PdfDate::parse("D:20261124101500-05'30'").unwrap();
        assert_eq!(d.utc_offset_minutes, Some(-330));
        assert_eq!(d.to_pdf_string(), "D:20261124101500-05'30'");
        assert_eq!(
            PdfDate::parse("20261124101500+0100")
                .unwrap()
                .utc_offset_minutes,
            Some(60)
        );
        assert_eq!(
            PdfDate::parse("D:20261124101500+01")
                .unwrap()
                .utc_offset_minutes,
            Some(60)
        );

        let d = PdfDate::parse("D:2026").unwrap();
        assert_eq!(
            (d.month, d.day, d.hour, d.utc_offset_minutes),
            (1, 1, 0, None)
        );
        assert_eq!(d.to_pdf_string(), "D:20260101000000");

        for bad in [
            "",
            "D:",
            "yesterday",
            "D:20261340",
            "D:20260101000000Q",
            "D:202",
        ] {
            assert_eq!(PdfDate::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn unix_seconds_become_civil_dates() {
        assert_eq!(PdfDate::from_unix(0).to_pdf_string(), "D:19700101000000Z");
        assert_eq!(
            PdfDate::from_unix(951_782_400).to_pdf_string(),
            "D:20000229000000Z"
        );
        assert_eq!(
            PdfDate::from_unix(1_795_515_296).to_pdf_string(),
            "D:20261124101456Z"
        );
        assert!(PdfDate::now().year >= 2024);
        for seconds in [0, 951_782_400, 1_795_515_296, -86_400, 4_102_444_799] {
            assert_eq!(PdfDate::from_unix(seconds).to_unix(), seconds);
        }
        // An offset moves the instant: 10:00 at +02:00 is 08:00 UTC.
        let local = PdfDate::parse("D:20261124100000+02'00'").unwrap();
        let utc = PdfDate::parse("D:20261124080000Z").unwrap();
        assert_eq!(local.to_unix(), utc.to_unix());
    }

    #[test]
    fn numbers_are_whole_or_real() {
        assert_eq!(num(100.0), Object::Integer(100));
        assert_eq!(num(-0.5), Object::Real(-0.5));
        assert_eq!(num(f64::NAN), Object::Integer(0));
        assert_eq!(pl_pdf::number(&num(12.0625)), Some(12.0625));
    }
}
