//! Page labels (ISO 32000-1 12.4.2): the catalog's `/PageLabels` number tree.
//!
//! Page operations keep labels attached to their pages: every page keeps the label it showed
//! (style, prefix and number), and the ranges are worked out again from the new page order.
//! A page that brings no label (a blank page, or one from a file without labels, inserted into a
//! file with labels) is labelled with its new physical page number in decimal.

use lopdf::{decode_text_string, text_string, Dictionary, Document, Object};

use crate::dest::{deref, walk_number_tree};
use crate::document::{number, PdfDocument};
use crate::error::{PdfError, Result};

/// The numbering style of a label range (`/S`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// No `/S`: the label is the prefix alone.
    None,
    /// `/D`: 1, 2, 3.
    Decimal,
    /// `/R`: I, II, III.
    RomanUpper,
    /// `/r`: i, ii, iii.
    RomanLower,
    /// `/A`: A..Z, AA..ZZ.
    LettersUpper,
    /// `/a`: a..z, aa..zz.
    LettersLower,
}

impl LabelStyle {
    fn from_name(n: &[u8]) -> LabelStyle {
        match n {
            b"D" => LabelStyle::Decimal,
            b"R" => LabelStyle::RomanUpper,
            b"r" => LabelStyle::RomanLower,
            b"A" => LabelStyle::LettersUpper,
            b"a" => LabelStyle::LettersLower,
            _ => LabelStyle::None,
        }
    }

    fn name(self) -> Option<&'static [u8]> {
        Some(match self {
            LabelStyle::None => return None,
            LabelStyle::Decimal => b"D",
            LabelStyle::RomanUpper => b"R",
            LabelStyle::RomanLower => b"r",
            LabelStyle::LettersUpper => b"A",
            LabelStyle::LettersLower => b"a",
        })
    }
}

/// The label of one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLabel {
    pub style: LabelStyle,
    pub prefix: String,
    /// The page's number in its range (the range's `/St` plus the offset into it).
    pub number: i64,
}

impl PageLabel {
    /// A plain decimal label.
    pub fn decimal(number: i64) -> PageLabel {
        PageLabel {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            number,
        }
    }

    /// The label as a viewer shows it.
    pub fn text(&self) -> String {
        let n = self.number.max(1);
        let numeral = match self.style {
            LabelStyle::None => String::new(),
            LabelStyle::Decimal => n.to_string(),
            LabelStyle::RomanUpper => roman(n),
            LabelStyle::RomanLower => roman(n).to_lowercase(),
            LabelStyle::LettersUpper => letters(n),
            LabelStyle::LettersLower => letters(n).to_lowercase(),
        };
        format!("{}{}", self.prefix, numeral)
    }

    /// Whether `next` continues this label's range (same style and prefix, the next number).
    fn continued_by(&self, next: &PageLabel) -> bool {
        self.style == next.style
            && self.prefix == next.prefix
            && (self.style == LabelStyle::None || next.number == self.number + 1)
    }
}

fn roman(mut n: i64) -> String {
    const TABLE: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut s = String::new();
    for (value, numeral) in TABLE {
        while n >= value {
            s.push_str(numeral);
            n -= value;
        }
    }
    s
}

/// A, B, ... Z, AA, BB, ... ZZ, AAA (the letter repeated, as the standard defines).
fn letters(n: i64) -> String {
    let index = (n - 1) as usize;
    let letter = char::from(b'A' + (index % 26) as u8);
    std::iter::repeat_n(letter, index / 26 + 1).collect()
}

/// One range of the `/PageLabels` tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRange {
    /// First page (zero-based) of the range.
    pub start: usize,
    pub style: LabelStyle,
    pub prefix: String,
    /// `/St`, the number of the first page (1 when absent).
    pub first: i64,
}

/// The ranges of the document's `/PageLabels`, ordered by start, or `None` without labels.
pub(crate) fn read_ranges(doc: &Document) -> Option<Vec<LabelRange>> {
    let tree = doc.catalog().ok()?.get(b"PageLabels").ok()?;
    let mut ranges = Vec::new();
    walk_number_tree(doc, tree, &mut |key, value| {
        let Some(Object::Dictionary(d)) = deref(doc, value) else {
            return;
        };
        let Ok(start) = usize::try_from(key) else {
            return;
        };
        let style = d
            .get(b"S")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_name().ok())
            .map_or(LabelStyle::None, LabelStyle::from_name);
        let prefix = d
            .get(b"P")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| decode_text_string(o).ok())
            .unwrap_or_default();
        let first = d
            .get(b"St")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(number)
            .map_or(1, |v| (v as i64).max(1));
        ranges.push(LabelRange {
            start,
            style,
            prefix,
            first,
        });
    });
    ranges.sort_by_key(|r| r.start);
    ranges.dedup_by_key(|r| r.start);
    Some(ranges)
}

/// The label of every page under `ranges`. Pages before the first range (which a valid file
/// does not have) get their physical page number.
pub(crate) fn per_page(ranges: &[LabelRange], page_count: usize) -> Vec<PageLabel> {
    (0..page_count)
        .map(|i| match ranges.iter().rev().find(|r| r.start <= i) {
            Some(r) => PageLabel {
                style: r.style,
                prefix: r.prefix.clone(),
                number: r.first + (i - r.start) as i64,
            },
            None => PageLabel::decimal(i as i64 + 1),
        })
        .collect()
}

/// The fewest ranges that give every page its label.
pub(crate) fn compact(labels: &[PageLabel]) -> Vec<LabelRange> {
    let mut out: Vec<LabelRange> = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        if i > 0 && labels[i - 1].continued_by(label) {
            continue;
        }
        out.push(LabelRange {
            start: i,
            style: label.style,
            prefix: label.prefix.clone(),
            first: label.number.max(1),
        });
    }
    out
}

/// Write `ranges` as a flat `/PageLabels << /Nums [...] >>` in the catalog (removed when
/// `ranges` is `None`).
pub(crate) fn store(doc: &mut Document, ranges: Option<&[LabelRange]>) -> Result<()> {
    let value = ranges.map(|ranges| {
        let mut nums = Vec::with_capacity(ranges.len() * 2);
        for r in ranges {
            let mut d = Dictionary::new();
            if let Some(s) = r.style.name() {
                d.set("S", Object::Name(s.to_vec()));
            }
            if !r.prefix.is_empty() {
                d.set("P", text_string(&r.prefix));
            }
            if r.first != 1 {
                d.set("St", Object::Integer(r.first));
            }
            nums.push(Object::Integer(r.start as i64));
            nums.push(Object::Dictionary(d));
        }
        let mut tree = Dictionary::new();
        tree.set("Nums", Object::Array(nums));
        Object::Dictionary(tree)
    });
    let catalog = doc.catalog_mut().map_err(PdfError::Parse)?;
    match value {
        Some(v) => catalog.set("PageLabels", v),
        None => {
            catalog.remove(b"PageLabels");
        }
    }
    Ok(())
}

impl PdfDocument {
    /// Each page's label (`None` when the document has no `/PageLabels`).
    pub fn page_labels(&self) -> Option<Vec<PageLabel>> {
        read_ranges(&self.doc).map(|r| per_page(&r, self.pages.len()))
    }

    /// The label ranges as stored (`None` without `/PageLabels`).
    pub fn page_label_ranges(&self) -> Option<Vec<LabelRange>> {
        read_ranges(&self.doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numerals() {
        assert_eq!(roman(1), "I");
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(14), "XIV");
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(letters(1), "A");
        assert_eq!(letters(26), "Z");
        assert_eq!(letters(27), "AA");
        assert_eq!(letters(53), "AAA");
        let l = PageLabel {
            style: LabelStyle::RomanLower,
            prefix: "A-".into(),
            number: 3,
        };
        assert_eq!(l.text(), "A-iii");
    }

    #[test]
    fn compact_round_trips() {
        let ranges = vec![
            LabelRange {
                start: 0,
                style: LabelStyle::RomanLower,
                prefix: String::new(),
                first: 1,
            },
            LabelRange {
                start: 2,
                style: LabelStyle::Decimal,
                prefix: "C-".into(),
                first: 5,
            },
            LabelRange {
                start: 4,
                style: LabelStyle::None,
                prefix: "Cover".into(),
                first: 1,
            },
        ];
        let labels = per_page(&ranges, 6);
        let texts: Vec<String> = labels.iter().map(PageLabel::text).collect();
        assert_eq!(texts, ["i", "ii", "C-5", "C-6", "Cover", "Cover"]);
        assert_eq!(compact(&labels), ranges);
        // Reordered pages split ranges but keep every label.
        let moved = vec![labels[3].clone(), labels[0].clone(), labels[2].clone()];
        let again = per_page(&compact(&moved), 3);
        assert_eq!(again, moved);
    }
}
