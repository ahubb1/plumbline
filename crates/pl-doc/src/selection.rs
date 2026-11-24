//! Text selection over a page's extracted characters.
//!
//! Everything works on a [`PageText`] in its own order, which is the reading order PDFium
//! reports. Search hits index the same list, so a hit and a selection are interchangeable.
//! Points are in page space (unrotated user space, y-up).

use pl_geom::Point;
use pl_render::{quads_for_chars, PageText, Quad, RectArray, SearchHit, TextChar};

/// A selected run of characters on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSelection {
    pub page: u32,
    /// Index of the first selected character in [`PageText::chars`].
    pub start: usize,
    /// One past the last selected character.
    pub end: usize,
    /// One quad per line of the selection, in page space.
    pub quads: Vec<Quad>,
    /// The selected characters exactly as extracted (line breaks are `\r\n`).
    pub text: String,
}

impl TextSelection {
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }

    /// The bounding rectangle of the selection, `[x0, y0, x1, y1]` in page space.
    pub fn bounds(&self) -> Option<RectArray> {
        quads_bounds(&self.quads)
    }
}

/// The bounding rectangle of a set of quads.
pub fn quads_bounds(quads: &[Quad]) -> Option<RectArray> {
    let mut it = quads.iter().flat_map(|q| q.iter());
    let first = it.next()?;
    let mut r = [first[0], first[1], first[0], first[1]];
    for p in it {
        r[0] = r[0].min(p[0]);
        r[1] = r[1].min(p[1]);
        r[2] = r[2].max(p[0]);
        r[3] = r[3].max(p[1]);
    }
    Some(r)
}

/// The index of the character whose box contains `p`, if any.
pub fn char_at(text: &PageText, p: Point) -> Option<usize> {
    text.chars.iter().position(|c| {
        c.has_box() && p.x >= c.bbox[0] && p.x <= c.bbox[2] && p.y >= c.bbox[1] && p.y <= c.bbox[3]
    })
}

/// The index of the character nearest to `p`: the one containing it, or else the horizontally
/// closest character on the vertically closest line. Lines come first, so a point far to the
/// side of a short line picks that line's end rather than a character of a longer line above or
/// below. `None` when the page has no boxed characters.
///
/// "Line" is judged along the page's y axis, which suits text that runs along x in page space;
/// for text drawn sideways the result is still a nearby character, but not line-aware.
pub fn nearest_char(text: &PageText, p: Point) -> Option<usize> {
    let dx = |b: &RectArray| (b[0] - p.x).max(p.x - b[2]).max(0.0);
    let dy = |b: &RectArray| (b[1] - p.y).max(p.y - b[3]).max(0.0);
    // The vertically closest character sets the line; ties go to the horizontally closest.
    let mut line: Option<(f64, f64, usize)> = None;
    for (i, c) in text.chars.iter().enumerate().filter(|(_, c)| c.has_box()) {
        let (x, y) = (dx(&c.bbox), dy(&c.bbox));
        if x == 0.0 && y == 0.0 {
            return Some(i);
        }
        if line.is_none_or(|(by, bx, _)| y < by || (y == by && x < bx)) {
            line = Some((y, x, i));
        }
    }
    let (line_dy, _, seed) = line?;
    // Characters of one line differ a little in box height, so everything within half a line
    // height of the closest distance counts as that line.
    let seed_box = text.chars[seed].bbox;
    let slack = 0.5 * (seed_box[3] - seed_box[1]);
    let mut best = (f64::INFINITY, seed);
    for (i, c) in text.chars.iter().enumerate().filter(|(_, c)| c.has_box()) {
        if dy(&c.bbox) <= line_dy + slack {
            let x = dx(&c.bbox);
            if x < best.0 {
                best = (x, i);
            }
        }
    }
    Some(best.1)
}

/// Selects characters `start..end` (clamped to the page). Returns `None` for an empty range.
pub fn select_range(text: &PageText, start: usize, end: usize) -> Option<TextSelection> {
    let end = end.min(text.chars.len());
    if start >= end {
        return None;
    }
    let chars = &text.chars[start..end];
    Some(TextSelection {
        page: text.page,
        start,
        end,
        quads: quads_for_chars(chars),
        text: chars.iter().map(|c| c.ch).collect(),
    })
}

/// A drag from `a` to `b`: selects every character, in reading order, from the one nearest `a`
/// to the one nearest `b`, both included. The direction of the drag does not matter.
pub fn select_between(text: &PageText, a: Point, b: Point) -> Option<TextSelection> {
    let i = nearest_char(text, a)?;
    let j = nearest_char(text, b)?;
    select_range(text, i.min(j), i.max(j) + 1)
}

/// The word under (or nearest to) `p`: the run of non-white-space characters around it.
pub fn select_word_at(text: &PageText, p: Point) -> Option<TextSelection> {
    let i = nearest_char(text, p)?;
    let is_break = |k: usize| {
        let c = text.chars[k].ch;
        c.is_whitespace() || c == '\u{0}'
    };
    if is_break(i) {
        return select_range(text, i, i + 1);
    }
    let mut start = i;
    while start > 0 && !is_break(start - 1) {
        start -= 1;
    }
    let mut end = i + 1;
    while end < text.chars.len() && !is_break(end) {
        end += 1;
    }
    select_range(text, start, end)
}

/// The line under (or nearest to) `p`: the characters between the surrounding line breaks.
pub fn select_line_at(text: &PageText, p: Point) -> Option<TextSelection> {
    let i = nearest_char(text, p)?;
    let is_break = |k: usize| matches!(text.chars[k].ch, '\r' | '\n');
    let mut start = i;
    while start > 0 && !is_break(start - 1) {
        start -= 1;
    }
    let mut end = i + 1;
    while end < text.chars.len() && !is_break(end) {
        end += 1;
    }
    select_range(text, start, end)
}

/// The whole page.
pub fn select_all(text: &PageText) -> Option<TextSelection> {
    select_range(text, 0, text.chars.len())
}

/// The selection that covers a search hit.
pub fn select_hit(text: &PageText, hit: &SearchHit) -> Option<TextSelection> {
    let start = hit.char_start as usize;
    select_range(text, start, start + hit.char_len as usize)
}

// ---- selections as text markups --------------------------------------------------------------

/// A run of a page's text as the geometry of a Highlight, Underline, StrikeOut or Squiggly:
/// what the text-markup tools make from a drag across the text.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkupSelection {
    pub page: u32,
    /// Index of the first selected character in [`PageText::chars`].
    pub start: usize,
    /// One past the last selected character.
    pub end: usize,
    /// One quad per line of the selection, in page space, each starting at the start of its
    /// baseline and going round anticlockwise as the text reads (pl-annot's `/QuadPoints`
    /// convention), so the markup follows text that runs up, down or backwards on the page.
    pub quads: Vec<pl_annot::Quad>,
    /// The selected text with every line break as one space: what goes into the markup's
    /// `/Contents`.
    pub text: String,
    /// The direction the first line reads in: 0, 90, 180 or 270 degrees anticlockwise from
    /// +x in page space.
    pub direction: u16,
}

impl MarkupSelection {
    /// The markup of `kind` (Highlight, Underline, StrikeOut or Squiggly) over the selection,
    /// with the selected text as its comment. `None` when no selected character has a box.
    pub fn annotation(
        &self,
        kind: pl_annot::AnnotKind,
        origin: &pl_annot::Origin,
    ) -> Option<pl_annot::Annotation> {
        let mut a = pl_annot::Annotation::text_markup_from_quads(
            kind,
            self.page as usize,
            &self.quads,
            origin,
        )?;
        if !self.text.is_empty() {
            a.meta.contents = Some(self.text.clone());
        }
        Some(a)
    }
}

fn is_line_break(c: &TextChar) -> bool {
    matches!(c.ch, '\r' | '\n')
}

/// The characters of the line `index` is on: the span between the surrounding line breaks.
fn line_span(text: &PageText, index: usize) -> (usize, usize) {
    let mut start = index.min(text.chars.len());
    while start > 0 && !is_line_break(&text.chars[start - 1]) {
        start -= 1;
    }
    let mut end = index.min(text.chars.len());
    while end < text.chars.len() && !is_line_break(&text.chars[end]) {
        end += 1;
    }
    (start, end)
}

fn direction_of(chars: &[TextChar]) -> Option<u16> {
    let origins: Vec<Point> = chars
        .iter()
        .filter(|c| c.has_box())
        .map(|c| Point::new(c.origin[0], c.origin[1]))
        .collect();
    let moves = origins.windows(2).any(|w| w[0] != w[1]);
    moves.then(|| pl_annot::reading_direction(&origins))
}

/// The direction the text around character `index` reads in: that of its line, or of the
/// whole page when the line is too short to tell (0 when nothing tells).
pub fn text_direction_at(text: &PageText, index: usize) -> u16 {
    let (start, end) = line_span(text, index);
    direction_of(&text.chars[start..end])
        .or_else(|| direction_of(&text.chars))
        .unwrap_or(0)
}

/// A point or box corner in the frame where text of `direction` reads along +x.
fn unturn(direction: u16, x: f64, y: f64) -> (f64, f64) {
    match direction {
        90 => (y, -x),
        180 => (-x, -y),
        270 => (-y, x),
        _ => (x, y),
    }
}

/// As [`nearest_char`] for text that reads in `direction` (0, 90, 180 or 270 degrees
/// anticlockwise from +x in page space): "line" is judged across the reading direction, so a
/// drag along sideways text picks characters of one line, not of its neighbours.
pub fn nearest_char_directed(text: &PageText, p: Point, direction: u16) -> Option<usize> {
    if direction == 0 {
        return nearest_char(text, p);
    }
    let (px, py) = unturn(direction, p.x, p.y);
    let turned = PageText {
        page: text.page,
        chars: text
            .chars
            .iter()
            .map(|c| {
                let mut t = *c;
                if c.has_box() {
                    let (ax, ay) = unturn(direction, c.bbox[0], c.bbox[1]);
                    let (bx, by) = unturn(direction, c.bbox[2], c.bbox[3]);
                    t.bbox = [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)];
                }
                t
            })
            .collect(),
    };
    nearest_char(&turned, Point::new(px, py))
}

/// The geometry of a text markup over characters `start..end` (clamped to the page): the
/// boxes of the selected characters merged into one quad per line, each oriented as its line
/// reads. `None` for an empty range or one with no boxed character.
pub fn markup_range(text: &PageText, start: usize, end: usize) -> Option<MarkupSelection> {
    let end = end.min(text.chars.len());
    if start >= end {
        return None;
    }
    let mut quads: Vec<pl_annot::Quad> = Vec::new();
    let mut first_direction = None;
    // The direction is worked out once per line, from the whole line: one selected character
    // is enough to mark, but not to tell which way its line reads.
    let mut line: Option<((usize, usize), u16)> = None;
    for i in start..end {
        let c = &text.chars[i];
        if !c.has_box() {
            continue;
        }
        let direction = match line {
            Some(((ls, le), d)) if i >= ls && i < le => d,
            _ => {
                let span = line_span(text, i);
                let d = direction_of(&text.chars[span.0..span.1])
                    .or_else(|| direction_of(&text.chars))
                    .unwrap_or(0);
                line = Some((span, d));
                d
            }
        };
        first_direction.get_or_insert(direction);
        let b = c.bbox;
        quads.push(pl_annot::quad_for_box(
            pl_geom::Rect::new(b[0], b[1], b[2], b[3]),
            direction,
        ));
    }
    let quads = pl_annot::merge_quads_by_line(&quads);
    if quads.is_empty() {
        return None;
    }
    // One space per line break, none at the ends.
    let raw: String = text.chars[start..end].iter().map(|c| c.ch).collect();
    let words: Vec<&str> = raw
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Some(MarkupSelection {
        page: text.page,
        start,
        end,
        quads,
        text: words.join(" "),
        direction: first_direction.unwrap_or(0),
    })
}

/// A drag from `a` to `b` across the text of a page, as the geometry of a text markup: every
/// character in reading order from the one nearest `a` to the one nearest `b`, merged into
/// one quad per line. The direction of the drag does not matter. Text that runs sideways or
/// upside down on the page (a title block read from the right, a rotated sheet) is followed
/// along its own lines.
pub fn markup_between(text: &PageText, a: Point, b: Point) -> Option<MarkupSelection> {
    // Which way does the text under the start of the drag read?
    let seed = nearest_char(text, a)?;
    let direction = text_direction_at(text, seed);
    let i = nearest_char_directed(text, a, direction)?;
    let j = nearest_char_directed(text, b, direction)?;
    markup_range(text, i.min(j), i.max(j) + 1)
}

/// The geometry of a text markup over a search hit.
pub fn markup_hit(text: &PageText, hit: &SearchHit) -> Option<MarkupSelection> {
    let start = hit.char_start as usize;
    markup_range(text, start, start + hit.char_len as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::synthetic_text;

    fn page() -> PageText {
        // Characters are 5 wide; line 1 has its baseline at y = 100, line 2 at y = 80.
        synthetic_text(
            2,
            &[
                ("fire rated", [10.0, 100.0], 10.0),
                ("door set", [10.0, 80.0], 10.0),
            ],
        )
    }

    #[test]
    fn drag_within_a_line_selects_the_characters_between() {
        let t = page();
        // From inside the "r" of "rated" (x 35..40) to inside its "d" (x 55..60).
        let s = select_between(&t, Point::new(36.0, 103.0), Point::new(58.0, 101.0)).unwrap();
        assert_eq!(s.text, "rated");
        assert_eq!((s.page, s.start, s.end), (2, 5, 10));
        assert_eq!(s.quads.len(), 1);
        assert_eq!(s.bounds(), Some([35.0, 98.0, 60.0, 108.0]));
    }

    #[test]
    fn drag_direction_does_not_matter() {
        let t = page();
        let a = Point::new(36.0, 103.0);
        let b = Point::new(22.0, 83.0);
        let fwd = select_between(&t, a, b).unwrap();
        let back = select_between(&t, b, a).unwrap();
        assert_eq!(fwd, back);
        // Reading order: from "rated" on line 1 through the line break to "doo" on line 2.
        assert_eq!(fwd.text, "rated\r\ndoo");
        assert_eq!(fwd.quads.len(), 2);
    }

    #[test]
    fn points_outside_any_box_pick_the_nearest_character_on_the_nearest_line() {
        let t = page();
        // Far right of line 1 and far left of line 2.
        let s = select_between(&t, Point::new(500.0, 104.0), Point::new(-50.0, 82.0)).unwrap();
        assert_eq!(s.text, "d\r\nd");
        // Below everything, between two characters horizontally: still line 2.
        assert_eq!(nearest_char(&t, Point::new(12.0, 0.0)), Some(12));
        assert_eq!(char_at(&t, Point::new(12.0, 0.0)), None);
        assert_eq!(char_at(&t, Point::new(12.0, 84.0)), Some(12));
    }

    #[test]
    fn word_line_and_all() {
        let t = page();
        assert_eq!(
            select_word_at(&t, Point::new(47.0, 103.0)).unwrap().text,
            "rated"
        );
        assert_eq!(
            select_word_at(&t, Point::new(11.0, 83.0)).unwrap().text,
            "door"
        );
        // On the space between the words: just the space.
        assert_eq!(
            select_word_at(&t, Point::new(32.0, 103.0)).unwrap().text,
            " "
        );
        assert_eq!(
            select_line_at(&t, Point::new(47.0, 103.0)).unwrap().text,
            "fire rated"
        );
        assert_eq!(
            select_line_at(&t, Point::new(47.0, 83.0)).unwrap().text,
            "door set"
        );
        let all = select_all(&t).unwrap();
        assert_eq!(all.text, t.text());
        assert_eq!(all.len(), t.chars.len());
    }

    #[test]
    fn empty_pages_and_ranges_select_nothing() {
        let empty = PageText::default();
        assert!(select_between(&empty, Point::ZERO, Point::new(5.0, 5.0)).is_none());
        assert!(select_all(&empty).is_none());
        let t = page();
        assert!(select_range(&t, 4, 4).is_none());
        assert!(select_range(&t, 99, 200).is_none());
        assert_eq!(select_range(&t, 16, 999).unwrap().text, " set");
        assert_eq!(quads_bounds(&[]), None);
    }

    #[test]
    fn a_search_hit_converts_to_the_same_selection() {
        let t = page();
        let hits = pl_render::search_page_text(&t, "rated door", Default::default());
        let s = select_hit(&t, &hits[0]).unwrap();
        assert_eq!(s.text, "rated\r\ndoor");
        assert_eq!(s.quads, hits[0].quads);
    }

    // ---- text markups -----------------------------------------------------------------------

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    /// `page()` turned by `direction` about the origin: the same text reading up (90), right
    /// to left and upside down (180) or down (270), as it is on a page drawn sideways.
    fn turned(direction: u16) -> PageText {
        let turn = |x: f64, y: f64| match direction {
            90 => (-y, x),
            180 => (-x, -y),
            270 => (y, -x),
            _ => (x, y),
        };
        let mut t = page();
        for c in &mut t.chars {
            if c.has_box() {
                let (ax, ay) = turn(c.bbox[0], c.bbox[1]);
                let (bx, by) = turn(c.bbox[2], c.bbox[3]);
                c.bbox = [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)];
                let (ox, oy) = turn(c.origin[0], c.origin[1]);
                c.origin = [ox, oy];
            }
        }
        t
    }

    #[test]
    fn a_drag_across_two_lines_gives_one_quad_per_line_from_the_baseline_start() {
        let t = page();
        let m = markup_between(&t, pt(36.0, 103.0), pt(22.0, 83.0)).unwrap();
        assert_eq!((m.page, m.start, m.end, m.direction), (2, 5, 15, 0));
        assert_eq!(m.text, "rated doo", "the line break is one space");
        assert_eq!(
            m.quads,
            vec![
                [
                    pt(35.0, 98.0),
                    pt(60.0, 98.0),
                    pt(60.0, 108.0),
                    pt(35.0, 108.0)
                ],
                [
                    pt(10.0, 78.0),
                    pt(25.0, 78.0),
                    pt(25.0, 88.0),
                    pt(10.0, 88.0)
                ],
            ]
        );
        // The drag direction does not matter, and the plain selection covers the same boxes.
        assert_eq!(
            markup_between(&t, pt(22.0, 83.0), pt(36.0, 103.0)),
            Some(m.clone())
        );
        let plain = select_between(&t, pt(36.0, 103.0), pt(22.0, 83.0)).unwrap();
        assert_eq!((plain.start, plain.end), (m.start, m.end));

        let origin = pl_annot::Origin::new("T", pl_annot::PdfDate::from_unix(1_700_000_000));
        let a = m
            .annotation(pl_annot::AnnotKind::Underline, &origin)
            .unwrap();
        assert_eq!(a.kind, pl_annot::AnnotKind::Underline);
        assert_eq!(a.page, 2);
        assert_eq!(a.geometry, pl_annot::Geometry::Quads(m.quads.clone()));
        assert_eq!(a.meta.contents.as_deref(), Some("rated doo"));
        assert!(a.is_modelled());
    }

    #[test]
    fn sideways_and_upside_down_text_is_followed_along_its_own_lines() {
        for direction in [90u16, 180, 270] {
            let t = turned(direction);
            let at = |x: f64, y: f64| match direction {
                90 => pt(-y, x),
                180 => pt(-x, -y),
                _ => pt(y, -x),
            };
            assert_eq!(text_direction_at(&t, 3), direction);
            // The same drag as in the upright test, turned with the page.
            let m = markup_between(&t, at(36.0, 103.0), at(22.0, 83.0)).unwrap();
            assert_eq!(
                (m.start, m.end, m.direction),
                (5, 15, direction),
                "{direction}"
            );
            assert_eq!(m.text, "rated doo");
            assert_eq!(m.quads.len(), 2);
            // Each quad is the upright one turned: it starts at the start of the baseline
            // and its second point is the end of the baseline.
            let want = [
                [
                    at(35.0, 98.0),
                    at(60.0, 98.0),
                    at(60.0, 108.0),
                    at(35.0, 108.0),
                ],
                [
                    at(10.0, 78.0),
                    at(25.0, 78.0),
                    at(25.0, 88.0),
                    at(10.0, 88.0),
                ],
            ];
            for (got, want) in m.quads.iter().zip(want) {
                for (g, w) in got.iter().zip(want) {
                    assert!(
                        (g.x - w.x).abs() < 1e-9 && (g.y - w.y).abs() < 1e-9,
                        "{direction}: {got:?}"
                    );
                }
            }
            // A drag along one sideways line stays on that line, though the other line's
            // characters are nearer along the page's y axis.
            let m = markup_between(&t, at(11.0, 103.0), at(58.0, 103.0)).unwrap();
            assert_eq!(m.text, "fire rated", "{direction}");
            assert_eq!(m.quads.len(), 1);
        }
    }

    #[test]
    fn one_character_is_enough_and_takes_its_lines_direction() {
        let t = turned(90);
        let m = markup_range(&t, 6, 7).unwrap();
        assert_eq!((m.text.as_str(), m.direction), ("a", 90));
        assert_eq!(m.quads.len(), 1);
        // 'a' of "rated" is x 40..45, y 98..108 upright; turned by 90 it is x -108..-98,
        // y 40..45, and its baseline runs up the right-hand edge.
        assert_eq!(m.quads[0][0], pt(-98.0, 40.0));
        assert_eq!(m.quads[0][1], pt(-98.0, 45.0));
        // A range of line breaks only, an empty range and an empty page mark nothing.
        let t = page();
        assert!(markup_range(&t, 10, 12).is_none());
        assert!(markup_range(&t, 4, 4).is_none());
        assert!(markup_between(&PageText::default(), pt(0.0, 0.0), pt(9.0, 9.0)).is_none());
        // The whole page: two lines.
        let all = markup_range(&t, 0, 999).unwrap();
        assert_eq!(all.text, "fire rated door set");
        assert_eq!(all.quads.len(), 2);
    }

    #[test]
    fn a_search_hit_is_marked_like_the_same_drag() {
        let t = page();
        let hits = pl_render::search_page_text(&t, "rated door", Default::default());
        let m = markup_hit(&t, &hits[0]).unwrap();
        assert_eq!(m.text, "rated door");
        assert_eq!(m.quads.len(), 2);
        assert_eq!(Some(m), markup_between(&t, pt(36.0, 103.0), pt(27.0, 83.0)));
    }
}
