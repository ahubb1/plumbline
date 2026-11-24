//! Standard-14 Helvetica advance widths (Adobe AFM, 1/1000 em) for printable ASCII.

/// Widths for character codes 32..=126.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // space .. /
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, // 0 .. 9
    278, 278, 584, 584, 584, 556, 1015, // : .. @
    667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667,
    611, 722, 667, 944, 667, 667, 611, // A .. Z
    278, 278, 278, 469, 556, 333, // [ .. `
    556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500,
    278, 556, 500, 722, 500, 500, 500, // a .. z
    334, 260, 334, 584, // { .. ~
];

/// Helvetica ascender (AFM) as a fraction of the font size.
pub const HELVETICA_ASCENT: f64 = 0.718;
/// Helvetica descender (AFM) as a fraction of the font size (negative).
pub const HELVETICA_DESCENT: f64 = -0.207;

/// Advance width in points of `text` set in standard-14 Helvetica at `size`.
///
/// Only printable ASCII is supported (fixtures use nothing else); other
/// characters panic so a fixture can never carry a silently wrong width.
pub fn helvetica_width(text: &str, size: f64) -> f64 {
    let units: u32 = text
        .chars()
        .map(|c| {
            let code = c as u32;
            assert!((32..=126).contains(&code), "non-ASCII character {c:?} in fixture text");
            u32::from(HELVETICA_WIDTHS[(code - 32) as usize])
        })
        .sum();
    f64::from(units) * size / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_widths() {
        assert_eq!(helvetica_width(" ", 1000.0), 278.0);
        assert_eq!(helvetica_width("A", 1000.0), 667.0);
        assert_eq!(helvetica_width("W", 1000.0), 944.0);
        assert_eq!(helvetica_width("i", 1000.0), 222.0);
        assert_eq!(helvetica_width("~", 1000.0), 584.0);
        // "Hello" = 722 + 556 + 222 + 222 + 556 = 2278
        assert_eq!(helvetica_width("Hello", 1000.0), 2278.0);
    }
}
