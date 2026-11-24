//! What a FreeText and a Stamp hold beyond the fields every annotation has, and the small text
//! formats a FreeText carries for other viewers: the `/DA` default-appearance string, the
//! `/DS` default style string and the `/RC` rich-text body.

use std::sync::Arc;

use pl_draw::{FontStyle, HAlign, Rgb, RgbaImage, StampSpec, TextBoxStyle, VAlign};
use pl_geom::{Affine, Point, Rect, Vec2};

/// What a FreeText is used as (`/IT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FreeTextIntent {
    /// A plain text box.
    #[default]
    Box,
    /// A text box with a leader line to what it describes (`/FreeTextCallout`, `/CL`, `/LE`).
    Callout,
    /// Text typed straight onto the page: no border, no background (`/FreeTextTypeWriter`).
    TypeWriter,
}

impl FreeTextIntent {
    /// The `/IT` name. A plain box is written as `FreeText`, the default.
    pub fn pdf_name(self) -> &'static str {
        match self {
            FreeTextIntent::Box => "FreeText",
            FreeTextIntent::Callout => "FreeTextCallout",
            FreeTextIntent::TypeWriter => "FreeTextTypeWriter",
        }
    }

    pub fn from_pdf_name(name: &str) -> FreeTextIntent {
        match name {
            "FreeTextCallout" => FreeTextIntent::Callout,
            "FreeTextTypeWriter" | "FreeTextTypewriter" => FreeTextIntent::TypeWriter,
            _ => FreeTextIntent::Box,
        }
    }
}

/// The text side of a FreeText annotation. The string itself is the annotation's `/Contents`
/// ([`Meta::contents`](crate::Meta::contents)); the box is its geometry; the border colour,
/// width and dash, the background (`style.fill`, written as `/C`) and the callout's line
/// ending (`style.line_end`) are in its [`Style`](crate::Style).
#[derive(Debug, Clone, PartialEq)]
pub struct FreeText {
    pub intent: FreeTextIntent,
    /// The face, one of the standard-14 Helvetica family.
    pub font: FontStyle,
    /// Font size in page units.
    pub size: f64,
    /// Text colour.
    pub color: Rgb,
    /// `/Q`.
    pub align: HAlign,
    pub v_align: VAlign,
    /// Distance between baselines as a multiple of the font size.
    pub line_spacing: f64,
    /// Space between the inside of the border and the text.
    pub padding: f64,
    /// `/CL`: the leader of a callout, two or three points from the tip (where the line
    /// ending is drawn) to the box. Empty for anything but a callout.
    pub callout: Vec<Point>,
    /// `/Rotate`: 0, 90, 180 or 270. The text is turned anticlockwise by this much in page
    /// space, which makes it read upright on a page whose own `/Rotate` has the same value.
    pub rotation: u16,
}

impl Default for FreeText {
    fn default() -> FreeText {
        FreeText {
            intent: FreeTextIntent::Box,
            font: FontStyle::Regular,
            size: 12.0,
            color: Rgb::new(0.8, 0.0, 0.0),
            align: HAlign::Left,
            v_align: VAlign::Top,
            line_spacing: TextBoxStyle::DEFAULT_LINE_SPACING,
            padding: 3.0,
            callout: Vec::new(),
            rotation: 0,
        }
    }
}

impl FreeText {
    /// The layout style these fields describe.
    pub fn box_style(&self) -> TextBoxStyle {
        TextBoxStyle {
            font: self.font,
            size: self.size,
            color: self.color,
            align: self.align,
            v_align: self.v_align,
            line_spacing: self.line_spacing,
            padding: self.padding.max(0.0),
        }
    }
}

/// What a stamp shows.
#[derive(Debug, Clone, PartialEq)]
pub enum StampContent {
    /// One of Plumbline's own stamps, drawn by `pl_draw::stamp_list`.
    Label(StampSpec),
    /// A picture stretched over the annotation's rectangle.
    Image(Arc<RgbaImage>),
}

/// A stamp this crate can draw. A stamp read from another application's file has none of
/// this and keeps its own appearance.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamp {
    pub content: StampContent,
    /// `/Rotate`: as [`FreeText::rotation`].
    pub rotation: u16,
}

/// 0, 90, 180 or 270 from any whole number of degrees; other angles snap to 0.
pub fn normalise_rotation(degrees: i64) -> u16 {
    match degrees.rem_euclid(360) {
        90 => 90,
        180 => 180,
        270 => 270,
        _ => 0,
    }
}

/// The frame content is laid out in when it is turned by `rotation` inside `rect`: a
/// rectangle centred on the origin (its sides swapped for a quarter turn) and the transform
/// that carries it onto `rect`. With no rotation the frame is `rect` itself.
pub fn rotated_frame(rect: Rect, rotation: u16) -> (Rect, Affine) {
    let (w, h) = (rect.width(), rect.height());
    let c = rect.center();
    let to = Affine::translate(Vec2::new(c.x, c.y));
    let centred = |w: f64, h: f64| Rect::new(-w / 2.0, -h / 2.0, w / 2.0, h / 2.0);
    match rotation {
        90 => (
            centred(h, w),
            to * Affine::new([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
        ),
        180 => (
            centred(w, h),
            to * Affine::new([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
        ),
        270 => (
            centred(h, w),
            to * Affine::new([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
        ),
        _ => (rect, Affine::IDENTITY),
    }
}

/// A leader for a callout: from `tip` to the middle of the side of `rect` that faces it, with
/// a knee a short way out from the box so the last stretch meets the box square on.
pub fn callout_route(rect: Rect, tip: Point) -> Vec<Point> {
    const KNEE: f64 = 14.0;
    let c = rect.center();
    let dx = if tip.x < rect.x0 {
        tip.x - rect.x0
    } else if tip.x > rect.x1 {
        tip.x - rect.x1
    } else {
        0.0
    };
    let dy = if tip.y < rect.y0 {
        tip.y - rect.y0
    } else if tip.y > rect.y1 {
        tip.y - rect.y1
    } else {
        0.0
    };
    let (end, out) = if dx.abs() >= dy.abs() {
        if tip.x < c.x {
            (Point::new(rect.x0, c.y), Vec2::new(-1.0, 0.0))
        } else {
            (Point::new(rect.x1, c.y), Vec2::new(1.0, 0.0))
        }
    } else if tip.y < c.y {
        (Point::new(c.x, rect.y0), Vec2::new(0.0, -1.0))
    } else {
        (Point::new(c.x, rect.y1), Vec2::new(0.0, 1.0))
    };
    let reach = dx.abs().max(dy.abs());
    if reach <= KNEE * 1.5 {
        return vec![tip, end];
    }
    vec![tip, end + out * KNEE, end]
}

fn fmt(v: f64) -> String {
    // The shortest text that reads back as the same number.
    let v = if v == 0.0 { 0.0 } else { v };
    format!("{v}")
}

fn fmt_color(c: Rgb) -> String {
    let c = c.clamped();
    format!("{} {} {}", c.r, c.g, c.b)
}

/// The font name written in `/DA` for a face.
fn da_font_name(font: FontStyle) -> &'static str {
    match font {
        FontStyle::Regular => "Helv",
        other => other.base_font(),
    }
}

fn font_from_name(name: &str) -> FontStyle {
    let lower = name.to_ascii_lowercase();
    let bold = lower.contains("bold") || lower.ends_with("bo") || lower.contains(",b");
    let italic = lower.contains("oblique") || lower.contains("italic") || lower.ends_with("ob");
    match (bold, italic) {
        (true, true) => FontStyle::BoldItalic,
        (true, false) => FontStyle::Bold,
        (false, true) => FontStyle::Italic,
        (false, false) => FontStyle::Regular,
    }
}

/// The `/DA` string, written the way Acrobat reads it: the non-stroking colour is the
/// **border** colour (Acrobat, and the libraries that follow it, redraw the border in the
/// last non-stroking colour of `/DA` and take the text colour from `/DS` or `/RC`). The
/// border colour is repeated as the stroking colour for readers that look there. A box with
/// no border has only the text colour, as the non-stroking colour.
pub fn default_appearance(border: Option<Rgb>, text: Rgb, font: FontStyle, size: f64) -> String {
    let mut out = String::new();
    let fill = match border {
        Some(b) => {
            out.push_str(&format!("{} RG ", fmt_color(b)));
            b
        }
        None => text,
    };
    out.push_str(&format!(
        "{} rg /{} {} Tf",
        fmt_color(fill),
        da_font_name(font),
        fmt(size)
    ));
    out
}

/// What a `/DA` string says. Every part is optional: producers write what they please.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ParsedDa {
    /// The stroking colour (`G`, `RG`, `K`).
    pub stroke: Option<Rgb>,
    /// The non-stroking colour (`g`, `rg`, `k`).
    pub fill: Option<Rgb>,
    pub font: Option<FontStyle>,
    pub size: Option<f64>,
}

fn color_from(operands: &[f64]) -> Option<Rgb> {
    let c = |x: f64| x.clamp(0.0, 1.0) as f32;
    match operands {
        [g] => Some(Rgb::new(c(*g), c(*g), c(*g))),
        [r, g, b] => Some(Rgb::new(c(*r), c(*g), c(*b))),
        [cy, m, y, k] => Some(Rgb::new(
            c((1.0 - cy) * (1.0 - k)),
            c((1.0 - m) * (1.0 - k)),
            c((1.0 - y) * (1.0 - k)),
        )),
        _ => None,
    }
}

/// Reads a `/DA` string. Never fails: what cannot be read is left out.
pub fn parse_default_appearance(da: &str) -> ParsedDa {
    let mut out = ParsedDa::default();
    let mut numbers: Vec<f64> = Vec::new();
    let mut font_name: Option<String> = None;
    for token in da.split_whitespace() {
        if let Some(name) = token.strip_prefix('/') {
            font_name = Some(name.to_string());
            numbers.clear();
            continue;
        }
        if let Ok(v) = token.parse::<f64>() {
            numbers.push(v);
            continue;
        }
        let take = |n: usize, numbers: &[f64]| -> Option<Vec<f64>> {
            (numbers.len() >= n).then(|| numbers[numbers.len() - n..].to_vec())
        };
        match token {
            "g" => out.fill = take(1, &numbers).and_then(|v| color_from(&v)).or(out.fill),
            "rg" => out.fill = take(3, &numbers).and_then(|v| color_from(&v)).or(out.fill),
            "k" => out.fill = take(4, &numbers).and_then(|v| color_from(&v)).or(out.fill),
            "G" => {
                out.stroke = take(1, &numbers)
                    .and_then(|v| color_from(&v))
                    .or(out.stroke)
            }
            "RG" => {
                out.stroke = take(3, &numbers)
                    .and_then(|v| color_from(&v))
                    .or(out.stroke)
            }
            "K" => {
                out.stroke = take(4, &numbers)
                    .and_then(|v| color_from(&v))
                    .or(out.stroke)
            }
            "Tf" => {
                if let Some(name) = font_name.take() {
                    out.font = Some(font_from_name(&name));
                }
                out.size = numbers
                    .last()
                    .copied()
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .or(out.size);
            }
            _ => {}
        }
        numbers.clear();
    }
    out
}

fn hex(c: Rgb) -> String {
    let [r, g, b] = c.to_u8();
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// The `/DS` default style string: the same font, size, alignment and text colour as CSS.
pub fn default_style(text: Rgb, font: FontStyle, size: f64, align: HAlign) -> String {
    let mut face = String::new();
    if font.is_italic() {
        face.push_str("italic ");
    }
    if font.is_bold() {
        face.push_str("bold ");
    }
    format!(
        "font: {face}Helvetica,sans-serif {}pt; text-align:{}; color:{}",
        fmt(size),
        align.css(),
        hex(text)
    )
}

/// The `color` of a `/DS` string, when it has one in `#RRGGBB` or `#RGB` form.
pub fn default_style_color(ds: &str) -> Option<Rgb> {
    for part in ds.split(';') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("color") {
            continue;
        }
        let v = value.trim().strip_prefix('#')?;
        let digit = |s: &str| u8::from_str_radix(s, 16).ok();
        return match v.len() {
            6 => Some(Rgb::from_u8(
                digit(&v[0..2])?,
                digit(&v[2..4])?,
                digit(&v[4..6])?,
            )),
            3 => {
                let d = |i: usize| digit(&v[i..i + 1]).map(|x| x * 17);
                Some(Rgb::from_u8(d(0)?, d(1)?, d(2)?))
            }
            _ => None,
        };
    }
    None
}

/// The `/RC` rich-text body for a plain string: one paragraph per line, styled by `/DS`.
pub fn rich_text(text: &str) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\"?><body xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\" xfa:APIVersion=\"Acrobat:11.0.0\" \
         xfa:spec=\"2.0.2\">",
    );
    let normal = text.replace("\r\n", "\n").replace('\r', "\n");
    for line in normal.split('\n') {
        out.push_str("<p>");
        for ch in line.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                c => out.push(c),
            }
        }
        out.push_str("</p>");
    }
    out.push_str("</body>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_appearance_round_trips_exactly() {
        let border = Rgb::new(0.1, 0.25, 1.0 / 3.0);
        let text = Rgb::new(0.8, 0.0, 0.123_456_7);
        for font in FontStyle::ALL {
            for size in [12.0, 9.5, 10.1, 0.3] {
                let da = default_appearance(Some(border), text, font, size);
                let p = parse_default_appearance(&da);
                assert_eq!(p.stroke, Some(border), "{da}");
                // Acrobat takes the border colour from the non-stroking colour.
                assert_eq!(p.fill, Some(border), "{da}");
                assert_eq!(p.font, Some(font), "{da}");
                assert_eq!(p.size, Some(size), "{da}");
                let da = default_appearance(None, text, font, size);
                let p = parse_default_appearance(&da);
                assert_eq!((p.stroke, p.fill), (None, Some(text)), "{da}");
            }
        }
        assert_eq!(
            default_appearance(
                Some(Rgb::new(0.0, 0.0, 1.0)),
                Rgb::RED,
                FontStyle::Regular,
                12.0
            ),
            "0 0 1 RG 0 0 1 rg /Helv 12 Tf"
        );
        let da = default_appearance(None, Rgb::BLACK, FontStyle::Regular, 12.0);
        assert_eq!(da, "0 0 0 rg /Helv 12 Tf");
        assert_eq!(parse_default_appearance(&da).stroke, None);
    }

    #[test]
    fn foreign_default_appearances_are_read_as_far_as_they_go() {
        let p = parse_default_appearance("/Helv 12 Tf 0 g");
        assert_eq!(
            (p.fill, p.stroke, p.size),
            (Some(Rgb::BLACK), None, Some(12.0))
        );
        assert_eq!(p.font, Some(FontStyle::Regular));
        let p = parse_default_appearance("0 G 1 0 0 rg 0 Tc 0 Tw 100 Tz 0 TL 0 Ts 0 Tr /HeBo 9 Tf");
        assert_eq!(p.stroke, Some(Rgb::BLACK));
        assert_eq!(p.fill, Some(Rgb::RED));
        assert_eq!((p.font, p.size), (Some(FontStyle::Bold), Some(9.0)));
        let p = parse_default_appearance("0 0 0 1 k /Arial,BoldItalic 0 Tf");
        assert_eq!(p.fill, Some(Rgb::BLACK));
        assert_eq!(p.font, Some(FontStyle::BoldItalic));
        assert_eq!(p.size, None, "size 0 means auto: not a size");
        assert_eq!(parse_default_appearance(""), ParsedDa::default());
        assert_eq!(parse_default_appearance("rg Tf ) ("), ParsedDa::default());
    }

    #[test]
    fn default_style_names_font_alignment_and_colour() {
        let ds = default_style(
            Rgb::from_u8(229, 34, 55),
            FontStyle::BoldItalic,
            10.5,
            HAlign::Centre,
        );
        assert_eq!(
            ds,
            "font: italic bold Helvetica,sans-serif 10.5pt; text-align:center; color:#E52237"
        );
        assert_eq!(default_style_color(&ds), Some(Rgb::from_u8(229, 34, 55)));
        assert_eq!(
            default_style_color("font: 12pt Arial; COLOR: #f00"),
            Some(Rgb::RED)
        );
        assert_eq!(default_style_color("color:red"), None);
        assert_eq!(default_style_color("font: 12pt"), None);
    }

    #[test]
    fn rich_text_is_one_escaped_paragraph_per_line() {
        let rc = rich_text("a < b & c\r\nsecond\n");
        assert!(rc.starts_with("<?xml version=\"1.0\"?><body "));
        assert!(rc.ends_with("<p>a &lt; b &amp; c</p><p>second</p><p></p></body>"));
    }

    #[test]
    fn rotated_frames_map_onto_the_rectangle() {
        let rect = Rect::new(100.0, 200.0, 300.0, 260.0);
        for rotation in [0u16, 90, 180, 270] {
            let (frame, to) = rotated_frame(rect, rotation);
            let swapped = rotation % 180 == 90;
            assert_eq!(
                (frame.width(), frame.height()),
                if swapped {
                    (60.0, 200.0)
                } else {
                    (200.0, 60.0)
                }
            );
            assert_eq!(to.transform_rect_bbox(frame), rect, "{rotation}");
            // The frame's +x (the reading direction) points along the rotation.
            let dir = to * Point::new(frame.x1, frame.center().y) - to * frame.center();
            let want = match rotation {
                0 => (1.0, 0.0),
                90 => (0.0, 1.0),
                180 => (-1.0, 0.0),
                _ => (0.0, -1.0),
            };
            let unit = dir / dir.hypot();
            assert_eq!((unit.x.round(), unit.y.round()), want, "{rotation}");
        }
        assert_eq!(normalise_rotation(-90), 270);
        assert_eq!(normalise_rotation(450), 90);
        assert_eq!(normalise_rotation(45), 0);
    }

    #[test]
    fn a_callout_leaves_the_side_of_the_box_that_faces_the_tip() {
        let rect = Rect::new(100.0, 100.0, 200.0, 140.0);
        let left = callout_route(rect, Point::new(20.0, 60.0));
        assert_eq!(left.len(), 3);
        assert_eq!(left[0], Point::new(20.0, 60.0));
        assert_eq!(left[2], Point::new(100.0, 120.0));
        assert_eq!(left[1], Point::new(86.0, 120.0));
        let above = callout_route(rect, Point::new(150.0, 300.0));
        assert_eq!(above[2], Point::new(150.0, 140.0));
        assert_eq!(above[1], Point::new(150.0, 154.0));
        let near = callout_route(rect, Point::new(210.0, 120.0));
        assert_eq!(
            near,
            vec![Point::new(210.0, 120.0), Point::new(200.0, 120.0)]
        );
    }
}
