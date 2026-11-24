//! Single-style text layout: a string wrapped into a box.
//!
//! [`layout_text`] breaks a string into lines that fit a box, honouring explicit line breaks,
//! and places the lines by horizontal and vertical alignment. The result holds the positioned
//! lines (as [`TextRun`]s for a [`DrawList`](crate::DrawList)), one caret position per character
//! boundary for an editor, and what did not fit.
//!
//! Measuring uses the same Liberation Sans metrics as the rest of the crate, which match the
//! standard-14 Helvetica family the emitter names, so a viewer breaks nothing differently: the
//! lines are emitted one `Tj` each, already broken.
//!
//! Only WinAnsi text can be laid out. The first character outside it is reported with its
//! position; nothing is dropped or replaced.

use kurbo::{Point, Rect};

use crate::list::{DrawOp, TextRun};
use crate::style::{BlendMode, Rgb};
use crate::text::{char_advance, measure_text, FontStyle};
use crate::DrawError;

/// Horizontal alignment of each line in the box (PDF `/Q` quadding 0, 1, 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HAlign {
    #[default]
    Left,
    Centre,
    Right,
}

impl HAlign {
    /// The `/Q` value.
    pub fn quadding(self) -> i64 {
        match self {
            HAlign::Left => 0,
            HAlign::Centre => 1,
            HAlign::Right => 2,
        }
    }

    /// From a `/Q` value; anything unknown is left-aligned, as the specification says.
    pub fn from_quadding(q: i64) -> HAlign {
        match q {
            1 => HAlign::Centre,
            2 => HAlign::Right,
            _ => HAlign::Left,
        }
    }

    /// The CSS `text-align` word.
    pub fn css(self) -> &'static str {
        match self {
            HAlign::Left => "left",
            HAlign::Centre => "center",
            HAlign::Right => "right",
        }
    }
}

/// Vertical placement of the block of lines in the box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

/// How a string is set in a box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextBoxStyle {
    pub font: FontStyle,
    /// Font size in page units.
    pub size: f64,
    pub color: Rgb,
    pub align: HAlign,
    pub v_align: VAlign,
    /// Distance between baselines as a multiple of the font size.
    pub line_spacing: f64,
    /// Space kept clear between each side of the box and the text, in page units.
    pub padding: f64,
}

impl TextBoxStyle {
    /// The default distance between baselines, as a multiple of the font size.
    pub const DEFAULT_LINE_SPACING: f64 = 1.25;

    /// Regular black text of `size`, top-left aligned, with the default line spacing and no
    /// padding.
    pub fn new(size: f64) -> TextBoxStyle {
        TextBoxStyle {
            font: FontStyle::Regular,
            size,
            color: Rgb::BLACK,
            align: HAlign::Left,
            v_align: VAlign::Top,
            line_spacing: TextBoxStyle::DEFAULT_LINE_SPACING,
            padding: 0.0,
        }
    }
}

/// One line of a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutLine {
    /// Character index (not byte index) of the line's first character in the laid-out string.
    pub start: usize,
    /// One past the line's last character. The line break that ends a paragraph is not part
    /// of any line.
    pub end: usize,
    /// What is drawn: the line's characters without the spaces that hang at its end.
    pub text: String,
    /// Start of the baseline.
    pub origin: Point,
    /// Advance width of `text`.
    pub width: f64,
    /// True when the line ends its paragraph (an explicit break, or the end of the string);
    /// false when it was wrapped.
    pub hard_break: bool,
}

/// Where the insertion point sits before one character (or after the last).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    /// Index into [`TextLayout::lines`].
    pub line: usize,
    /// The caret's position on the line's baseline.
    pub position: Point,
}

/// What a layout could not fit in its box. The lines are laid out all the same (a caller that
/// emits them relies on the appearance's clip), so nothing is lost silently.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overflow {
    /// The box height the text needs, padding included.
    pub height_needed: f64,
    /// The box width the widest single character needs, padding included: the narrowest box
    /// in which no line sticks out sideways.
    pub width_needed: f64,
    /// Lines that are not wholly inside the padded box.
    pub clipped_lines: usize,
    /// Character index of the start of the first line that is not wholly inside the box.
    pub first_clipped_char: Option<usize>,
}

impl Overflow {
    /// True when everything fits.
    pub fn is_none(&self) -> bool {
        self.clipped_lines == 0
    }
}

/// A string laid out in a box. Coordinates are those of the box: page space, y up.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLayout {
    /// The box the text was laid out in.
    pub rect: Rect,
    pub style: TextBoxStyle,
    pub lines: Vec<LayoutLine>,
    /// One caret per character boundary: `carets[i]` is the insertion point before character
    /// `i`, and the last entry the one after the final character, so there are
    /// `char_count + 1` of them. The caret of a line-break character sits at the end of the
    /// line it ends.
    pub carets: Vec<Caret>,
    /// Advance width of each character; zero for line breaks.
    pub advances: Vec<f64>,
    /// Height of the face above the baseline.
    pub ascent: f64,
    /// Depth of the face below the baseline (negative).
    pub descent: f64,
    /// Distance from one baseline to the next.
    pub line_advance: f64,
    pub overflow: Overflow,
}

const EPS: f64 = 1e-6;

fn is_break(ch: char) -> bool {
    ch == '\n' || ch == '\r'
}

/// Lays `text` out in `rect`.
///
/// Lines break at spaces and after hyphens; a word wider than the box is broken between
/// characters. Spaces at a wrap hang past the end of the line and take no room. A line feed, a
/// carriage return, and the pair of them, each end a paragraph. Any other character outside
/// WinAnsi (a tab included) fails with [`DrawError::NotWinAnsi`], naming the character and its
/// index.
pub fn layout_text(text: &str, rect: Rect, style: &TextBoxStyle) -> Result<TextLayout, DrawError> {
    let numbers = [
        rect.x0,
        rect.y0,
        rect.x1,
        rect.y1,
        style.size,
        style.line_spacing,
        style.padding,
    ];
    if numbers.iter().any(|v| !v.is_finite())
        || style.size <= 0.0
        || style.line_spacing <= 0.0
        || style.padding < 0.0
    {
        return Err(DrawError::InvalidNumber);
    }
    let rect = rect.abs();
    let chars: Vec<char> = text.chars().collect();
    let mut advances = Vec::with_capacity(chars.len());
    for (index, ch) in chars.iter().enumerate() {
        if is_break(*ch) {
            advances.push(0.0);
            continue;
        }
        match char_advance(*ch, style.font) {
            Some(a) => advances.push(a * style.size),
            None => return Err(DrawError::NotWinAnsi { ch: *ch, index }),
        }
    }

    let metrics = measure_text("", style.font, style.size)?;
    let (ascent, descent) = (metrics.ascent, metrics.descent);
    let line_advance = style.size * style.line_spacing;
    let inner = Rect::new(
        rect.x0 + style.padding,
        rect.y0 + style.padding,
        rect.x1 - style.padding,
        rect.y1 - style.padding,
    );
    let available = (inner.x1 - inner.x0).max(0.0);

    // Break into lines: (start, end, next line's start, hard break).
    let mut spans: Vec<(usize, usize, usize, bool)> = Vec::new();
    let mut widest_piece: f64 = 0.0;
    let mut para = 0;
    loop {
        let para_end = chars[para..]
            .iter()
            .position(|c| is_break(*c))
            .map_or(chars.len(), |i| para + i);
        let after_break = if para_end >= chars.len() {
            chars.len()
        } else if chars[para_end] == '\r' && chars.get(para_end + 1) == Some(&'\n') {
            para_end + 2
        } else {
            para_end + 1
        };
        let mut start = para;
        loop {
            let mut i = start;
            let mut width = 0.0;
            // Where the next line would start if this one has to end early.
            let mut break_at: Option<usize> = None;
            while i < para_end {
                let (ch, advance) = (chars[i], advances[i]);
                if ch == ' ' {
                    // Spaces hang: they never push a line over.
                    width += advance;
                    i += 1;
                    break_at = Some(i);
                    continue;
                }
                if width + advance > available + EPS && i > start {
                    break;
                }
                widest_piece = widest_piece.max(advance);
                width += advance;
                i += 1;
                if ch == '-' {
                    break_at = Some(i);
                }
            }
            if i >= para_end {
                spans.push((start, para_end, after_break, true));
                break;
            }
            let end = break_at.filter(|b| *b > start).unwrap_or(i);
            spans.push((start, end, end, false));
            start = end;
        }
        if para_end >= chars.len() {
            break;
        }
        para = after_break;
    }

    // Place the block of lines.
    let block = ascent - descent + line_advance * (spans.len() - 1) as f64;
    let first_baseline = match style.v_align {
        VAlign::Top => inner.y1 - ascent,
        VAlign::Middle => (inner.y0 + inner.y1) / 2.0 + block / 2.0 - ascent,
        VAlign::Bottom => inner.y0 + block - ascent,
    };

    let mut lines = Vec::with_capacity(spans.len());
    let mut carets = Vec::with_capacity(chars.len() + 1);
    let mut overflow = Overflow {
        height_needed: block + 2.0 * style.padding,
        width_needed: widest_piece + 2.0 * style.padding,
        ..Overflow::default()
    };
    for (n, (start, end, next, hard_break)) in spans.iter().copied().enumerate() {
        let mut drawn_end = end;
        while drawn_end > start && chars[drawn_end - 1] == ' ' {
            drawn_end -= 1;
        }
        let width: f64 = advances[start..drawn_end].iter().sum();
        let x = match style.align {
            HAlign::Left => inner.x0,
            HAlign::Centre => (inner.x0 + inner.x1) / 2.0 - width / 2.0,
            HAlign::Right => inner.x1 - width,
        };
        let baseline = first_baseline - line_advance * n as f64;
        let mut cx = x;
        for advance in &advances[start..end] {
            carets.push(Caret {
                line: n,
                position: Point::new(cx, baseline),
            });
            cx += advance;
        }
        // The line-break characters that end the line, and the end of the text.
        let extra = (next - end) + usize::from(n + 1 == spans.len());
        for _ in 0..extra {
            carets.push(Caret {
                line: n,
                position: Point::new(cx, baseline),
            });
        }
        let inside = baseline + ascent <= inner.y1 + 1e-3
            && baseline + descent >= inner.y0 - 1e-3
            && width <= available + 1e-3;
        if !inside {
            overflow.clipped_lines += 1;
            overflow.first_clipped_char.get_or_insert(start);
        }
        lines.push(LayoutLine {
            start,
            end,
            text: chars[start..drawn_end].iter().collect(),
            origin: Point::new(x, baseline),
            width,
            hard_break,
        });
    }
    debug_assert_eq!(carets.len(), chars.len() + 1);

    Ok(TextLayout {
        rect,
        style: *style,
        lines,
        carets,
        advances,
        ascent,
        descent,
        line_advance,
        overflow,
    })
}

impl TextLayout {
    /// Number of characters that were laid out.
    pub fn char_count(&self) -> usize {
        self.advances.len()
    }

    /// One text run per line that draws something, in the layout's style.
    pub fn runs(&self) -> Vec<TextRun> {
        self.lines
            .iter()
            .filter(|l| !l.text.is_empty())
            .map(|l| TextRun {
                text: l.text.clone(),
                origin: l.origin,
                size: self.style.size,
                style: self.style.font,
                color: self.style.color,
                opacity: 1.0,
                blend: BlendMode::Normal,
            })
            .collect()
    }

    /// The runs as draw ops.
    pub fn ops(&self) -> Vec<DrawOp> {
        self.runs().into_iter().map(DrawOp::Text).collect()
    }

    /// The box around the lines: the widest line by the block's height.
    pub fn text_bounds(&self) -> Option<Rect> {
        let first = self.lines.first()?;
        let last = self.lines.last()?;
        let x0 = self
            .lines
            .iter()
            .map(|l| l.origin.x)
            .fold(f64::MAX, f64::min);
        let x1 = self
            .lines
            .iter()
            .map(|l| l.origin.x + l.width)
            .fold(f64::MIN, f64::max);
        Some(Rect::new(
            x0,
            last.origin.y + self.descent,
            x1,
            first.origin.y + self.ascent,
        ))
    }

    /// The caret before character `index` as a vertical bar: zero width, from the line's
    /// descent to its ascent. `None` when `index` is past the end.
    pub fn caret_rect(&self, index: usize) -> Option<Rect> {
        let c = self.carets.get(index)?;
        Some(Rect::new(
            c.position.x,
            c.position.y + self.descent,
            c.position.x,
            c.position.y + self.ascent,
        ))
    }

    /// The loose box of character `index`: its advance wide, the full line height tall. `None`
    /// for a line break and for an index past the end.
    pub fn char_box(&self, index: usize) -> Option<Rect> {
        let advance = *self.advances.get(index)?;
        let c = self.carets.get(index)?;
        let line = self.lines.get(c.line)?;
        if index >= line.end {
            return None;
        }
        Some(Rect::new(
            c.position.x,
            c.position.y + self.descent,
            c.position.x + advance,
            c.position.y + self.ascent,
        ))
    }

    /// The character boundary nearest to `point`: the line whose band the point is in (the
    /// first or last line when it is above or below them all), then the nearest caret on it.
    pub fn index_at(&self, point: Point) -> usize {
        let Some(first) = self.lines.first() else {
            return 0;
        };
        let top = first.origin.y + self.ascent;
        let row = ((top - point.y) / self.line_advance).floor();
        let n = (row.max(0.0) as usize).min(self.lines.len() - 1);
        let line = &self.lines[n];
        // The end of a wrapped line is the start of the next one; stay on this line.
        let last = if line.hard_break {
            line.end
        } else {
            line.end.saturating_sub(1).max(line.start)
        };
        let mut best = line.start;
        let mut best_d = f64::MAX;
        let mut x = line.origin.x;
        for i in line.start..=last {
            let d = (x - point.x).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
            x += self.advances.get(i).copied().unwrap_or(0.0);
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(size: f64) -> TextBoxStyle {
        let mut s = TextBoxStyle::new(size);
        s.line_spacing = 1.2;
        s
    }

    fn width(s: &str, size: f64) -> f64 {
        measure_text(s, FontStyle::Regular, size).unwrap().width
    }

    fn texts(l: &TextLayout) -> Vec<&str> {
        l.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn words_wrap_at_spaces_to_the_box_width() {
        let text = "The quick brown fox jumps over the lazy dog";
        // Wide enough for "The quick brown" and not for one more word.
        let w = width("The quick brown", 12.0) + 1.0;
        let l = layout_text(
            text,
            Rect::new(100.0, 100.0, 100.0 + w, 300.0),
            &style(12.0),
        )
        .unwrap();
        assert_eq!(
            texts(&l),
            ["The quick brown", "fox jumps over", "the lazy dog"]
        );
        for line in &l.lines {
            assert!(line.width <= w + 1e-9);
            assert_eq!(line.origin.x, 100.0);
            assert!((line.width - width(&line.text, 12.0)).abs() < 1e-9);
        }
        assert!(!l.lines[0].hard_break && !l.lines[1].hard_break && l.lines[2].hard_break);
        // Lines step down by the line advance from the top of the box.
        assert!((l.lines[0].origin.y - (300.0 - l.ascent)).abs() < 1e-9);
        assert!((l.lines[0].origin.y - l.lines[1].origin.y - 14.4).abs() < 1e-9);
        assert!(l.overflow.is_none());
        // Every character belongs to exactly one line; the hanging spaces are not drawn.
        assert_eq!(l.lines[0].start, 0);
        assert_eq!(l.lines[0].end, l.lines[1].start);
        assert_eq!(l.lines[2].end, text.chars().count());
        // A little narrower and "brown" drops to the next line.
        let narrow = layout_text(text, Rect::new(0.0, 0.0, w - 2.0, 300.0), &style(12.0)).unwrap();
        assert_eq!(narrow.lines[0].text, "The quick");
    }

    #[test]
    fn explicit_breaks_end_paragraphs_and_empty_lines_are_kept() {
        let l = layout_text(
            "one\ntwo\r\n\r\nfour\rfive\n",
            Rect::new(0.0, 0.0, 500.0, 200.0),
            &style(10.0),
        )
        .unwrap();
        assert_eq!(texts(&l), ["one", "two", "", "four", "five", ""]);
        assert!(l.lines.iter().all(|l| l.hard_break));
        assert_eq!(l.runs().len(), 4, "empty lines draw nothing");
        assert_eq!(l.carets.len(), l.char_count() + 1);
        // The caret of a break character sits at the end of the line it ends, and both
        // characters of a CR LF pair share it.
        let end_of_two = l.carets[7];
        assert_eq!(end_of_two.line, 1);
        assert!((end_of_two.position.x - width("two", 10.0)).abs() < 1e-9);
        assert_eq!(l.carets[8], end_of_two);
        // The caret after the final break is on the last, empty line.
        let last = l.carets.last().unwrap();
        assert_eq!((last.line, last.position.x), (5, 0.0));
        let empty = layout_text("", Rect::new(0.0, 0.0, 9.0, 9.0), &style(10.0)).unwrap();
        assert_eq!((empty.lines.len(), empty.carets.len()), (1, 1));
    }

    #[test]
    fn a_long_word_breaks_between_characters_and_hyphens_allow_a_break() {
        let w = width("WWWW", 10.0) + 0.5;
        let l = layout_text("WWWWWWWWWW", Rect::new(0.0, 0.0, w, 99.0), &style(10.0)).unwrap();
        assert_eq!(texts(&l), ["WWWW", "WWWW", "WW"]);
        let w = width("self-", 10.0) + 0.5;
        let l = layout_text("self-closing", Rect::new(0.0, 0.0, w, 99.0), &style(10.0)).unwrap();
        assert_eq!(l.lines[0].text, "self-");
        // A box narrower than one character still makes progress, one character a line, and
        // says how wide it would have to be.
        let l = layout_text("WW", Rect::new(0.0, 0.0, 2.0, 99.0), &style(10.0)).unwrap();
        assert_eq!(texts(&l), ["W", "W"]);
        assert!((l.overflow.width_needed - width("W", 10.0)).abs() < 1e-9);
        assert_eq!(l.overflow.clipped_lines, 2);
    }

    #[test]
    fn alignment_places_each_line_and_the_block() {
        let rect = Rect::new(50.0, 100.0, 250.0, 200.0);
        let mut s = style(10.0);
        s.padding = 4.0;
        let text = "wide line here\nx";
        let (w0, w1) = (width("wide line here", 10.0), width("x", 10.0));

        s.align = HAlign::Right;
        let l = layout_text(text, rect, &s).unwrap();
        assert!((l.lines[0].origin.x - (246.0 - w0)).abs() < 1e-9);
        assert!((l.lines[1].origin.x - (246.0 - w1)).abs() < 1e-9);

        s.align = HAlign::Centre;
        let l = layout_text(text, rect, &s).unwrap();
        assert!((l.lines[1].origin.x + w1 / 2.0 - 150.0).abs() < 1e-9);
        assert!(
            (l.lines[0].origin.y - (196.0 - l.ascent)).abs() < 1e-9,
            "top"
        );

        s.v_align = VAlign::Bottom;
        let l = layout_text(text, rect, &s).unwrap();
        assert!(
            (l.lines[1].origin.y + l.descent - 104.0).abs() < 1e-9,
            "bottom"
        );

        s.v_align = VAlign::Middle;
        let l = layout_text(text, rect, &s).unwrap();
        let b = l.text_bounds().unwrap();
        assert!((b.center().y - 150.0).abs() < 1e-9, "middle");
        assert!((b.height() - (l.ascent - l.descent + 12.0)).abs() < 1e-9);

        s.line_spacing = 2.0;
        let l = layout_text(text, rect, &s).unwrap();
        assert!((l.lines[0].origin.y - l.lines[1].origin.y - 20.0).abs() < 1e-9);
        assert_eq!(
            HAlign::from_quadding(HAlign::Right.quadding()),
            HAlign::Right
        );
        assert_eq!(HAlign::from_quadding(7), HAlign::Left);
    }

    #[test]
    fn carets_follow_the_advances_and_hit_testing_inverts_them() {
        let text = "ab cd ef gh";
        let w = width("ab cd", 10.0) + 0.5;
        let l = layout_text(text, Rect::new(10.0, 0.0, 10.0 + w, 100.0), &style(10.0)).unwrap();
        assert_eq!(texts(&l), ["ab cd", "ef gh"]);
        assert_eq!(l.carets.len(), 12);
        assert_eq!(l.carets[0].position.x, 10.0);
        assert!((l.carets[2].position.x - (10.0 + width("ab", 10.0))).abs() < 1e-9);
        // The hanging space belongs to the first line; the next character starts line two.
        assert_eq!((l.carets[5].line, l.carets[6].line), (0, 1));
        assert_eq!(l.carets[6].position.x, 10.0);
        assert!((l.carets[11].position.x - (10.0 + width("ef gh", 10.0))).abs() < 1e-9);
        for i in 0..11 {
            let b = l.char_box(i).unwrap();
            assert!((b.width() - l.advances[i]).abs() < 1e-12);
            assert!((b.height() - (l.ascent - l.descent)).abs() < 1e-12);
            let c = l.caret_rect(i).unwrap();
            assert_eq!((c.x0, c.y0, c.y1), (b.x0, b.y0, b.y1));
            // A click just inside the left edge of a character lands before it.
            assert_eq!(
                l.index_at(Point::new(b.x0 + 0.01, b.center().y)),
                i,
                "char {i}"
            );
        }
        assert_eq!(l.char_box(11), None);
        assert_eq!(
            l.index_at(Point::new(1000.0, 99.0)),
            5,
            "the last character of the wrapped line"
        );
        assert_eq!(l.index_at(Point::new(1000.0, -500.0)), 11, "past the end");
        assert_eq!(l.index_at(Point::new(-50.0, 500.0)), 0);
    }

    #[test]
    fn overflow_reports_the_first_line_that_does_not_fit() {
        let s = style(10.0);
        let rect = Rect::new(0.0, 0.0, 300.0, 25.0);
        let l = layout_text("one\ntwo\nthree\nfour", rect, &s).unwrap();
        // 25 units hold two lines: about 11.2 for the first, then 12 a line.
        assert_eq!(l.lines.len(), 4, "the lines are still laid out");
        assert_eq!(l.overflow.clipped_lines, 2);
        assert_eq!(l.overflow.first_clipped_char, Some(8));
        assert!((l.overflow.height_needed - (l.ascent - l.descent + 36.0)).abs() < 1e-9);
        let roomy = Rect::new(0.0, 0.0, 300.0, l.overflow.height_needed);
        assert!(layout_text("one\ntwo\nthree\nfour", roomy, &s)
            .unwrap()
            .overflow
            .is_none());
    }

    #[test]
    fn text_outside_winansi_is_refused_with_the_character_and_its_index() {
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            layout_text("ok\n\u{4e2d}", rect, &style(10.0)),
            Err(DrawError::NotWinAnsi {
                ch: '\u{4e2d}',
                index: 3
            })
        );
        assert_eq!(
            layout_text("a\tb", rect, &style(10.0)),
            Err(DrawError::NotWinAnsi { ch: '\t', index: 1 })
        );
        assert!(layout_text("caf\u{e9} \u{20ac}5 \u{2014} ok", rect, &style(10.0)).is_ok());
        assert_eq!(
            layout_text("a", rect, &style(0.0)),
            Err(DrawError::InvalidNumber)
        );
        assert_eq!(
            layout_text("a", Rect::new(0.0, f64::NAN, 1.0, 1.0), &style(10.0)),
            Err(DrawError::InvalidNumber)
        );
    }

    #[test]
    fn runs_carry_the_style_and_validate() {
        let mut s = style(9.0);
        s.font = FontStyle::Bold;
        s.color = Rgb::BLUE;
        let l = layout_text("Bold text", Rect::new(0.0, 0.0, 200.0, 50.0), &s).unwrap();
        let runs = l.runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            (runs[0].style, runs[0].size, runs[0].color),
            (FontStyle::Bold, 9.0, Rgb::BLUE)
        );
        let list = crate::DrawList::from_ops(l.ops());
        assert!(list.validate().is_ok());
        let bold = measure_text("Bold text", FontStyle::Bold, 9.0)
            .unwrap()
            .width;
        assert!((l.lines[0].width - bold).abs() < 1e-9);
    }
}
