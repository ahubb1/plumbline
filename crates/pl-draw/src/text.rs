//! Single-style text: the standard-14 Helvetica family, WinAnsi only.
//!
//! The emitter names a standard-14 font (`Helvetica`, `Helvetica-Bold`, ...) with
//! `WinAnsiEncoding` and embeds nothing. Measuring and rasterising use the bundled Liberation
//! Sans faces (`vendor/fonts`), whose advance widths match Helvetica's, so a run measured here
//! is as wide as the viewer draws it.

use std::sync::OnceLock;

use kurbo::{BezPath, Point};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::DrawError;

/// The four faces of the standard-14 Helvetica family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    pub const ALL: [FontStyle; 4] = [
        FontStyle::Regular,
        FontStyle::Bold,
        FontStyle::Italic,
        FontStyle::BoldItalic,
    ];

    /// The `/BaseFont` name of the standard-14 font.
    pub fn base_font(self) -> &'static str {
        match self {
            FontStyle::Regular => "Helvetica",
            FontStyle::Bold => "Helvetica-Bold",
            FontStyle::Italic => "Helvetica-Oblique",
            FontStyle::BoldItalic => "Helvetica-BoldOblique",
        }
    }

    pub fn is_bold(self) -> bool {
        matches!(self, FontStyle::Bold | FontStyle::BoldItalic)
    }

    pub fn is_italic(self) -> bool {
        matches!(self, FontStyle::Italic | FontStyle::BoldItalic)
    }

    /// The bundled Liberation Sans face used to measure and rasterise this style.
    pub fn font_data(self) -> &'static [u8] {
        match self {
            FontStyle::Regular => {
                include_bytes!("../../../vendor/fonts/LiberationSans-Regular.ttf")
            }
            FontStyle::Bold => include_bytes!("../../../vendor/fonts/LiberationSans-Bold.ttf"),
            FontStyle::Italic => include_bytes!("../../../vendor/fonts/LiberationSans-Italic.ttf"),
            FontStyle::BoldItalic => {
                include_bytes!("../../../vendor/fonts/LiberationSans-BoldItalic.ttf")
            }
        }
    }

    fn face(self) -> &'static Face<'static> {
        static FACES: [OnceLock<Face<'static>>; 4] = [
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
        ];
        FACES[self as usize].get_or_init(|| {
            Face::parse(self.font_data(), 0).expect("the bundled Liberation Sans face parses")
        })
    }
}

/// Windows-1252 code points 0x80..=0x9F. Five codes are unassigned.
const CP1252_HIGH: [Option<char>; 32] = [
    Some('\u{20AC}'),
    None,
    Some('\u{201A}'),
    Some('\u{0192}'),
    Some('\u{201E}'),
    Some('\u{2026}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{02C6}'),
    Some('\u{2030}'),
    Some('\u{0160}'),
    Some('\u{2039}'),
    Some('\u{0152}'),
    None,
    Some('\u{017D}'),
    None,
    None,
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{201C}'),
    Some('\u{201D}'),
    Some('\u{2022}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{02DC}'),
    Some('\u{2122}'),
    Some('\u{0161}'),
    Some('\u{203A}'),
    Some('\u{0153}'),
    None,
    Some('\u{017E}'),
    Some('\u{0178}'),
];

/// The WinAnsi (Windows-1252) byte for a character, or `None` when the character cannot be
/// written with `WinAnsiEncoding`. Control characters (tab, line breaks) have no glyph and are
/// not accepted: a run is one line.
pub fn winansi_byte(ch: char) -> Option<u8> {
    let code = ch as u32;
    match code {
        0x20..=0x7E | 0xA0..=0xFF => Some(code as u8),
        _ => CP1252_HIGH
            .iter()
            .position(|c| *c == Some(ch))
            .map(|i| 0x80 + i as u8),
    }
}

/// The character a WinAnsi byte stands for, the inverse of [`winansi_byte`].
pub fn winansi_char(byte: u8) -> Option<char> {
    match byte {
        0x20..=0x7E | 0xA0..=0xFF => Some(byte as char),
        0x80..=0x9F => CP1252_HIGH[(byte - 0x80) as usize],
        _ => None,
    }
}

/// Encodes a string for a `Tj` operand under `WinAnsiEncoding`. The first character that the
/// encoding lacks is reported; nothing is dropped or replaced.
pub fn winansi_encode(text: &str) -> Result<Vec<u8>, DrawError> {
    text.chars()
        .enumerate()
        .map(|(index, ch)| winansi_byte(ch).ok_or(DrawError::NotWinAnsi { ch, index }))
        .collect()
}

/// The glyph a viewer draws for a WinAnsi character. `WinAnsiEncoding` puts the hyphen at the
/// soft-hyphen code and a space at the no-break-space code.
fn glyph_char(ch: char) -> char {
    match ch {
        '\u{00AD}' => '-',
        '\u{00A0}' => ' ',
        c => c,
    }
}

fn glyph_for(face: &Face<'_>, ch: char) -> GlyphId {
    face.glyph_index(glyph_char(ch)).unwrap_or(GlyphId(0))
}

/// Size of a text run. All values are in the units of `size` (page units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextMetrics {
    /// Advance width of the run: where the next character would start.
    pub width: f64,
    /// Height of the face above the baseline (positive).
    pub ascent: f64,
    /// Depth of the face below the baseline (negative).
    pub descent: f64,
    /// Height of a flat capital letter above the baseline.
    pub cap_height: f64,
}

impl TextMetrics {
    /// `ascent - descent`: the natural single-line height.
    pub fn line_height(&self) -> f64 {
        self.ascent - self.descent
    }
}

/// The advance width of one character at size 1, or an error when it is outside WinAnsi.
pub fn char_advance(ch: char, style: FontStyle) -> Option<f64> {
    winansi_byte(ch)?;
    let face = style.face();
    let advance = face.glyph_hor_advance(glyph_for(face, ch)).unwrap_or(0);
    Some(advance as f64 / face.units_per_em() as f64)
}

/// Measures a run. Fails on the first character outside WinAnsi.
pub fn measure_text(text: &str, style: FontStyle, size: f64) -> Result<TextMetrics, DrawError> {
    let face = style.face();
    let upem = face.units_per_em() as f64;
    let mut units = 0.0;
    for (index, ch) in text.chars().enumerate() {
        if winansi_byte(ch).is_none() {
            return Err(DrawError::NotWinAnsi { ch, index });
        }
        units += face.glyph_hor_advance(glyph_for(face, ch)).unwrap_or(0) as f64;
    }
    let k = size / upem;
    Ok(TextMetrics {
        width: units * k,
        ascent: face.ascender() as f64 * k,
        descent: face.descender() as f64 * k,
        cap_height: face
            .capital_height()
            .map(|h| h as f64)
            .unwrap_or(0.7 * upem)
            * k,
    })
}

struct Outline {
    path: BezPath,
    dx: f64,
    dy: f64,
    k: f64,
}

impl Outline {
    fn pt(&self, x: f32, y: f32) -> Point {
        Point::new(self.dx + x as f64 * self.k, self.dy + y as f64 * self.k)
    }
}

impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.path.move_to(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.path.line_to(p);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (a, b) = (self.pt(x1, y1), self.pt(x, y));
        self.path.quad_to(a, b);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b, c) = (self.pt(x1, y1), self.pt(x2, y2), self.pt(x, y));
        self.path.curve_to(a, b, c);
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

/// The glyph outlines of a run as one path in page space (y-up), with the start of the baseline
/// at `origin`. Fill it with the non-zero rule. Fails on the first character outside WinAnsi.
pub fn text_outline(
    text: &str,
    style: FontStyle,
    size: f64,
    origin: Point,
) -> Result<BezPath, DrawError> {
    let face = style.face();
    let mut out = Outline {
        path: BezPath::new(),
        dx: origin.x,
        dy: origin.y,
        k: size / face.units_per_em() as f64,
    };
    for (index, ch) in text.chars().enumerate() {
        if winansi_byte(ch).is_none() {
            return Err(DrawError::NotWinAnsi { ch, index });
        }
        let glyph = glyph_for(face, ch);
        face.outline_glyph(glyph, &mut out);
        out.dx += face.glyph_hor_advance(glyph).unwrap_or(0) as f64 * out.k;
    }
    Ok(out.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;

    #[test]
    fn ascii_and_latin1_encode_to_themselves() {
        assert_eq!(winansi_encode("Az 09~").unwrap(), b"Az 09~");
        assert_eq!(
            winansi_encode("\u{e9}\u{a3}\u{ff}").unwrap(),
            [0xE9, 0xA3, 0xFF]
        );
    }

    #[test]
    fn cp1252_specials_encode_to_the_high_block() {
        assert_eq!(winansi_byte('\u{20AC}'), Some(0x80));
        assert_eq!(winansi_byte('\u{2019}'), Some(0x92));
        assert_eq!(winansi_byte('\u{2014}'), Some(0x97));
        assert_eq!(winansi_byte('\u{2122}'), Some(0x99));
        assert_eq!(winansi_byte('\u{0178}'), Some(0x9F));
    }

    #[test]
    fn every_byte_round_trips_or_is_unassigned() {
        let mut assigned = 0;
        for b in 0u8..=255 {
            if let Some(ch) = winansi_char(b) {
                assert_eq!(winansi_byte(ch), Some(b), "byte {b:#x}");
                assigned += 1;
            }
        }
        // 95 printable ASCII + 96 Latin-1 + 27 assigned codes in 0x80..=0x9F.
        assert_eq!(assigned, 95 + 96 + 27);
    }

    #[test]
    fn characters_outside_winansi_are_rejected_with_their_position() {
        assert_eq!(
            winansi_encode("ab\u{4e2d}c"),
            Err(DrawError::NotWinAnsi {
                ch: '\u{4e2d}',
                index: 2
            })
        );
        // Greek, a control character, and a C1 code that Windows-1252 leaves unassigned.
        assert!(winansi_encode("\u{3a9}").is_err());
        assert!(winansi_encode("a\nb").is_err());
        assert!(winansi_encode("\t").is_err());
        assert!(winansi_encode("\u{81}").is_err());
        assert_eq!(
            measure_text("x\u{2192}", FontStyle::Regular, 10.0),
            Err(DrawError::NotWinAnsi {
                ch: '\u{2192}',
                index: 1
            })
        );
        assert!(text_outline("\u{1F600}", FontStyle::Bold, 10.0, Point::ZERO).is_err());
        assert_eq!(char_advance('\u{4e2d}', FontStyle::Regular), None);
    }

    /// Helvetica's AFM widths (per 1000) for a sample of glyphs; Liberation Sans must agree,
    /// or measuring here would not predict what a viewer draws.
    #[test]
    fn advances_match_the_helvetica_afm() {
        let regular = [
            (' ', 278),
            ('A', 667),
            ('W', 944),
            ('a', 556),
            ('i', 222),
            ('m', 833),
            ('0', 556),
            ('.', 278),
            ('@', 1015),
            ('\u{e9}', 556),
            ('\u{20AC}', 556),
            ('\u{2014}', 1000),
        ];
        for (ch, w) in regular {
            let got = char_advance(ch, FontStyle::Regular).unwrap() * 1000.0;
            assert!((got - w as f64).abs() < 1.0, "regular {ch:?}: {got} vs {w}");
        }
        let bold = [
            (' ', 278),
            ('A', 722),
            ('a', 556),
            ('i', 278),
            ('m', 889),
            ('W', 944),
        ];
        for (ch, w) in bold {
            let got = char_advance(ch, FontStyle::Bold).unwrap() * 1000.0;
            assert!((got - w as f64).abs() < 1.0, "bold {ch:?}: {got} vs {w}");
        }
        // The oblique faces share the upright widths.
        for ch in ['A', 'a', 'm', '0'] {
            assert_eq!(
                char_advance(ch, FontStyle::Italic),
                char_advance(ch, FontStyle::Regular)
            );
            assert_eq!(
                char_advance(ch, FontStyle::BoldItalic),
                char_advance(ch, FontStyle::Bold)
            );
        }
    }

    #[test]
    fn measuring_sums_advances_and_scales_with_size() {
        let m = measure_text("Wall A", FontStyle::Regular, 10.0).unwrap();
        let sum: f64 = "Wall A"
            .chars()
            .map(|c| char_advance(c, FontStyle::Regular).unwrap())
            .sum();
        assert!((m.width - sum * 10.0).abs() < 1e-9);
        let m2 = measure_text("Wall A", FontStyle::Regular, 20.0).unwrap();
        assert!((m2.width - 2.0 * m.width).abs() < 1e-9);
        assert!(m.ascent > 0.0 && m.descent < 0.0);
        assert!(m.cap_height > 0.6 * 10.0 && m.cap_height < 0.8 * 10.0);
        assert!((m.line_height() - (m.ascent - m.descent)).abs() < 1e-12);
        assert_eq!(measure_text("", FontStyle::Bold, 12.0).unwrap().width, 0.0);
    }

    #[test]
    fn soft_hyphen_and_no_break_space_take_the_winansi_glyph_widths() {
        assert_eq!(
            char_advance('\u{ad}', FontStyle::Regular),
            char_advance('-', FontStyle::Regular)
        );
        assert_eq!(
            char_advance('\u{a0}', FontStyle::Regular),
            char_advance(' ', FontStyle::Regular)
        );
    }

    #[test]
    fn outline_sits_on_the_baseline_inside_the_measured_box() {
        let origin = Point::new(100.0, 50.0);
        let path = text_outline("HEX", FontStyle::Regular, 20.0, origin).unwrap();
        let b = path.bounding_box();
        let m = measure_text("HEX", FontStyle::Regular, 20.0).unwrap();
        assert!(b.x0 >= origin.x && b.x1 <= origin.x + m.width, "{b:?}");
        // Flat capitals stand on the baseline and reach the cap height.
        assert!((b.y0 - origin.y).abs() < 0.05, "{b:?}");
        assert!((b.y1 - (origin.y + m.cap_height)).abs() < 0.3, "{b:?}");
        // A space draws nothing.
        assert!(text_outline("  ", FontStyle::Regular, 20.0, origin)
            .unwrap()
            .elements()
            .is_empty());
    }

    #[test]
    fn every_winansi_character_has_a_glyph_in_every_face() {
        for style in FontStyle::ALL {
            let face = style.face();
            for b in 0u8..=255 {
                if let Some(ch) = winansi_char(b) {
                    assert!(
                        face.glyph_index(glyph_char(ch)).is_some(),
                        "{style:?} lacks {ch:?} ({b:#x})"
                    );
                }
            }
        }
    }
}
