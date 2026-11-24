//! Text search over an extracted [`PageText`]. Pure code shared by every backend, so hits are
//! always expressed as indices into the same character list the caller copies text from.

use crate::types::{PageText, Quad, RectArray, SearchHit, SearchOptions, TextChar};

/// Finds every occurrence of `query` in the page text.
///
/// Matching is literal. Runs of white space (including the line breaks PDFium inserts) match
/// one or more white-space characters in the query, so a phrase is found across a line break.
/// Unless `match_case` is set, characters are compared by their lower-case form. With
/// `whole_word`, a hit must not be preceded or followed by a letter or digit. Hits do not
/// overlap. Each hit carries one quad per line of text it covers.
pub fn search_page_text(text: &PageText, query: &str, options: SearchOptions) -> Vec<SearchHit> {
    let fold = |c: char| -> char {
        if options.match_case {
            c
        } else {
            c.to_lowercase().next().unwrap_or(c)
        }
    };

    // The needle, with white-space runs collapsed to one space and the ends trimmed.
    let mut needle: Vec<char> = Vec::new();
    for c in query.trim().chars() {
        if c.is_whitespace() {
            if needle.last() != Some(&' ') {
                needle.push(' ');
            }
        } else {
            needle.push(fold(c));
        }
    }
    if needle.is_empty() {
        return Vec::new();
    }

    // The haystack, collapsed the same way, with the source index of each kept character.
    let mut hay: Vec<(char, usize)> = Vec::with_capacity(text.chars.len());
    for (i, tc) in text.chars.iter().enumerate() {
        if tc.ch.is_whitespace() || tc.ch == '\u{0}' {
            if hay.last().map(|h| h.0) != Some(' ') {
                hay.push((' ', i));
            }
        } else {
            hay.push((fold(tc.ch), i));
        }
    }

    let mut hits = Vec::new();
    let n = needle.len();
    let mut at = 0;
    while at + n <= hay.len() {
        let matched = (0..n).all(|k| hay[at + k].0 == needle[k]);
        let bounded = !options.whole_word || {
            let before = at.checked_sub(1).map(|i| hay[i].0);
            let after = hay.get(at + n).map(|h| h.0);
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        };
        if matched && bounded {
            let first = hay[at].1;
            let last = hay[at + n - 1].1;
            hits.push(SearchHit {
                page: text.page,
                char_start: first as u32,
                char_len: (last - first + 1) as u32,
                quads: quads_for_chars(&text.chars[first..=last]),
            });
            at += n;
        } else {
            at += 1;
        }
    }
    hits
}

/// One axis-aligned quad per line for a run of characters: consecutive characters whose boxes
/// overlap vertically are merged; a character that drops to a new line starts a new quad.
/// Characters without a box are skipped.
pub fn quads_for_chars(chars: &[TextChar]) -> Vec<Quad> {
    let mut lines: Vec<RectArray> = Vec::new();
    for c in chars.iter().filter(|c| c.has_box()) {
        let b = c.bbox;
        match lines.last_mut() {
            Some(cur) if same_line(cur, &b) => {
                cur[0] = cur[0].min(b[0]);
                cur[1] = cur[1].min(b[1]);
                cur[2] = cur[2].max(b[2]);
                cur[3] = cur[3].max(b[3]);
            }
            _ => lines.push(b),
        }
    }
    lines.into_iter().map(rect_to_quad).collect()
}

/// The quad of an axis-aligned rectangle: bottom-left, bottom-right, top-right, top-left.
pub fn rect_to_quad(r: RectArray) -> Quad {
    [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]]
}

/// The bounding rectangle of a quad.
pub fn quad_bounds(q: &Quad) -> RectArray {
    let xs = q.iter().map(|p| p[0]);
    let ys = q.iter().map(|p| p[1]);
    [
        xs.clone().fold(f64::INFINITY, f64::min),
        ys.clone().fold(f64::INFINITY, f64::min),
        xs.fold(f64::NEG_INFINITY, f64::max),
        ys.fold(f64::NEG_INFINITY, f64::max),
    ]
}

fn same_line(a: &RectArray, b: &RectArray) -> bool {
    let overlap = a[3].min(b[3]) - a[1].max(b[1]);
    let smaller = (a[3] - a[1]).min(b[3] - b[1]);
    smaller > 0.0 && overlap >= 0.5 * smaller
}

#[cfg(test)]
pub(crate) fn synthetic_text(page: u32, lines: &[(&str, [f64; 2], f64)]) -> PageText {
    // Each character is `0.5 * size` wide; lines are separated by a generated "\r\n".
    let mut chars = Vec::new();
    for (li, (s, origin, size)) in lines.iter().enumerate() {
        if li > 0 {
            for ch in ['\r', '\n'] {
                chars.push(TextChar {
                    ch,
                    bbox: [0.0; 4],
                    origin: [0.0; 2],
                    font_size: *size as f32,
                    generated: true,
                });
            }
        }
        for (k, ch) in s.chars().enumerate() {
            let x = origin[0] + k as f64 * 0.5 * size;
            chars.push(TextChar {
                ch,
                bbox: [
                    x,
                    origin[1] - 0.2 * size,
                    x + 0.5 * size,
                    origin[1] + 0.8 * size,
                ],
                origin: [x, origin[1]],
                font_size: *size as f32,
                generated: false,
            });
        }
    }
    PageText { page, chars }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(match_case: bool, whole_word: bool) -> SearchOptions {
        SearchOptions {
            match_case,
            whole_word,
        }
    }

    #[test]
    fn finds_all_occurrences_case_insensitively() {
        let t = synthetic_text(3, &[("Wall type A, wall type B", [10.0, 100.0], 10.0)]);
        let hits = search_page_text(&t, "WALL", opts(false, false));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page, 3);
        assert_eq!((hits[0].char_start, hits[0].char_len), (0, 4));
        assert_eq!((hits[1].char_start, hits[1].char_len), (13, 4));
        // 4 characters of width 5 starting at x = 10.
        assert_eq!(quad_bounds(&hits[0].quads[0]), [10.0, 98.0, 30.0, 108.0]);
        assert_eq!(quad_bounds(&hits[1].quads[0]), [75.0, 98.0, 95.0, 108.0]);
    }

    #[test]
    fn match_case_and_whole_word_filter_hits() {
        let t = synthetic_text(0, &[("Wall wallpaper WALL", [0.0, 0.0], 10.0)]);
        assert_eq!(search_page_text(&t, "wall", opts(true, false)).len(), 1);
        assert_eq!(search_page_text(&t, "wall", opts(false, false)).len(), 3);
        let whole = search_page_text(&t, "wall", opts(false, true));
        assert_eq!(whole.len(), 2);
        assert_eq!(whole[1].char_start, 15);
    }

    #[test]
    fn a_phrase_is_found_across_a_line_break_with_one_quad_per_line() {
        let t = synthetic_text(
            0,
            &[
                ("fire rated", [10.0, 100.0], 10.0),
                ("door set", [10.0, 80.0], 10.0),
            ],
        );
        let hits = search_page_text(&t, "rated  door", opts(false, false));
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!(h.char_start, 5);
        // "rated" + "\r\n" + "door"
        assert_eq!(h.char_len, 11);
        assert_eq!(h.quads.len(), 2);
        assert_eq!(quad_bounds(&h.quads[0]), [35.0, 98.0, 60.0, 108.0]);
        assert_eq!(quad_bounds(&h.quads[1]), [10.0, 78.0, 30.0, 88.0]);
    }

    #[test]
    fn empty_and_missing_queries_give_no_hits() {
        let t = synthetic_text(0, &[("abc", [0.0, 0.0], 10.0)]);
        assert!(search_page_text(&t, "", opts(false, false)).is_empty());
        assert!(search_page_text(&t, "   ", opts(false, false)).is_empty());
        assert!(search_page_text(&t, "abcd", opts(false, false)).is_empty());
        assert!(search_page_text(&PageText::default(), "a", opts(false, false)).is_empty());
    }

    #[test]
    fn hits_do_not_overlap() {
        let t = synthetic_text(0, &[("aaaa", [0.0, 0.0], 10.0)]);
        assert_eq!(search_page_text(&t, "aa", opts(false, false)).len(), 2);
    }
}
