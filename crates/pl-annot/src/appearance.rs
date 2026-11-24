//! Appearance: one [`DrawList`] per annotation, in page space. The same list is painted on
//! screen, emitted as the `/AP /N` stream and rasterised by the tiny-skia oracle, so the three
//! cannot disagree.

use pl_draw::kurbo::BezPath;
use pl_draw::shapes::{
    cloud_path, ellipse_path, highlight_path, ink_path, inset_for_stroke, polygon_path,
    polyline_ops, rect_path, CloudStyle, Quad,
};
use pl_draw::{
    layout_text, Dash, DrawError, DrawList, DrawOp, Fill, Group, ImageOp, LineCap, LineEnding,
    LineJoin, PathOp, Rgb, ShapeStyle, Stroke, TextLayout,
};
use pl_geom::{Affine, Point, Rect, Vec2};

use crate::content::{rotated_frame, StampContent};
use crate::model::{AnnotKind, Annotation, Geometry, Style};

/// `/Rect` and the appearance box are rounded outwards to this grid, so they are exact in the
/// single-precision numbers a PDF holds and `/Rect` minus `/RD` gives back the shape exactly.
const RECT_GRID: f64 = 1.0 / 16.0;

/// Where an annotation sits on the page and what it paints there.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// `/Rect`, which is also the `/BBox` of the appearance stream.
    pub rect: Rect,
    /// `/RD` for a Square or Circle whose painted extent is larger than the shape (a cloud
    /// border) or a callout whose `/Rect` holds the leader too: left, bottom, right, top
    /// (the order of a rectangle, as PDFBox, MuPDF and Poppler read it).
    pub rect_differences: Option<[f64; 4]>,
    /// What is painted; empty for a kind with no appearance of its own.
    pub list: DrawList,
}

pub(crate) fn round_out(r: Rect) -> Rect {
    // Bounds that overshoot a grid line by rounding noise stay on that line.
    const SLACK: f64 = 1e-4;
    let down = |v: f64| ((v + SLACK) / RECT_GRID).floor() * RECT_GRID;
    let up = |v: f64| ((v - SLACK) / RECT_GRID).ceil() * RECT_GRID;
    let mut out = Rect::new(down(r.x0), down(r.y0), up(r.x1), up(r.y1));
    // A box with no area is not a usable /Rect.
    if out.width() <= 0.0 {
        out.x0 -= RECT_GRID;
        out.x1 += RECT_GRID;
    }
    if out.height() <= 0.0 {
        out.y0 -= RECT_GRID;
        out.y1 += RECT_GRID;
    }
    out
}

/// The scallop size of a cloud border of the given `/BE /I` intensity.
pub fn cloud_scallop(intensity: f64) -> f64 {
    5.0 + 5.0 * intensity.clamp(0.0, 10.0)
}

fn stroke_of(style: &Style) -> Option<Stroke> {
    let color = style.stroke?;
    if !(style.width.is_finite() && style.width > 0.0) {
        return None;
    }
    let mut stroke = Stroke::new(style.width, color);
    if let Some(dash) = &style.dash {
        stroke = stroke.with_dash(Dash::new(dash.clone(), 0.0));
    }
    Some(stroke)
}

fn shape_style(style: &Style, stroke: Option<Stroke>, filled: bool) -> ShapeStyle {
    ShapeStyle {
        stroke,
        fill: style.fill.filter(|_| filled).map(Fill::new),
        opacity: style.opacity,
        blend: style.blend,
    }
}

/// Points around the ellipse inscribed in `rect`, about `spacing` apart, anticlockwise.
fn ellipse_points(rect: Rect, spacing: f64) -> Vec<Point> {
    let (rx, ry) = (rect.width() / 2.0, rect.height() / 2.0);
    // Ramanujan's approximation of the circumference.
    let h = ((rx - ry) / (rx + ry).max(1e-9)).powi(2);
    let around =
        std::f64::consts::PI * (rx + ry) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt()));
    let n = ((around / spacing.max(0.5)).round() as usize).clamp(8, 720);
    let c = rect.center();
    (0..n)
        .map(|i| {
            let t = i as f64 / n as f64 * std::f64::consts::TAU;
            Point::new(c.x + rx * t.cos(), c.y + ry * t.sin())
        })
        .collect()
}

/// The outline a Square, Circle or Polygon is stroked along and filled inside, and whether the
/// stroke needs round joins (a cloud does).
fn closed_outline(a: &Annotation) -> Option<(BezPath, bool)> {
    let width = if a.style.stroke.is_some() {
        a.style.width.max(0.0)
    } else {
        0.0
    };
    let cloud = a.style.cloud.map(|i| CloudStyle::new(cloud_scallop(i)));
    match (&a.kind, &a.geometry) {
        (AnnotKind::Square, Geometry::Rect(frame)) => {
            let r = inset_for_stroke(*frame, width);
            Some(match cloud {
                Some(c) => (pl_draw::shapes::cloud_rect_path(r, &c), true),
                None => (rect_path(r), false),
            })
        }
        (AnnotKind::Circle, Geometry::Rect(frame)) => {
            let r = inset_for_stroke(*frame, width);
            Some(match cloud {
                Some(c) => (cloud_path(&ellipse_points(r, c.scallop), &c), true),
                None => (ellipse_path(r), false),
            })
        }
        (AnnotKind::Polygon, Geometry::Points(points)) => Some(match cloud {
            Some(c) if points.len() >= 3 => (cloud_path(points, &c), true),
            _ => (polygon_path(points), false),
        }),
        _ => None,
    }
}

/// The line drawn for one quad of an Underline, StrikeOut or Squiggly.
fn quad_rule(kind: &AnnotKind, quad: &Quad) -> (BezPath, f64) {
    let [bl, br, _, tl] = *quad;
    let up: Vec2 = tl - bl;
    let height = up.hypot();
    let along: Vec2 = br - bl;
    let length = along.hypot();
    let width = (height / 14.0).max(0.4);
    let mut path = BezPath::new();
    if length <= 0.0 || height <= 0.0 {
        return (path, width);
    }
    match kind {
        AnnotKind::StrikeOut => {
            path.move_to(bl + up * 0.42);
            path.line_to(br + up * 0.42);
        }
        AnnotKind::Squiggly => {
            let amplitude = 0.06;
            let steps = ((length / (height * 0.22)).round() as usize).clamp(2, 4000);
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let lift = if i % 2 == 0 {
                    0.07
                } else {
                    0.07 + 2.0 * amplitude
                };
                let p = bl + along * t + up * lift;
                if i == 0 {
                    path.move_to(p);
                } else {
                    path.line_to(p);
                }
            }
        }
        _ => {
            path.move_to(bl + up * 0.1);
            path.line_to(br + up * 0.1);
        }
    }
    (path, width)
}

fn darker(c: Rgb, factor: f32) -> Rgb {
    Rgb::new(c.r * factor, c.g * factor, c.b * factor)
}

/// The note icon: a sheet with a folded corner and three lines of writing, filling `rect`.
fn note_ops(rect: Rect, color: Rgb) -> Vec<DrawOp> {
    let size = rect.width().min(rect.height());
    if size <= 0.0 {
        return Vec::new();
    }
    let line = (size / 16.0).max(0.25);
    let r = rect.inset(-line / 2.0);
    let fold = size * 0.3;
    let ink = darker(color, 0.35);
    let outline = Stroke::new(line, ink).with_join(LineJoin::Round);

    let mut sheet = BezPath::new();
    sheet.move_to((r.x0, r.y0));
    sheet.line_to((r.x1, r.y0));
    sheet.line_to((r.x1, r.y1 - fold));
    sheet.line_to((r.x1 - fold, r.y1));
    sheet.line_to((r.x0, r.y1));
    sheet.close_path();

    let mut corner = BezPath::new();
    corner.move_to((r.x1 - fold, r.y1));
    corner.line_to((r.x1 - fold, r.y1 - fold));
    corner.line_to((r.x1, r.y1 - fold));

    let mut writing = BezPath::new();
    let margin = size * 0.2;
    for i in 0..3 {
        let y = r.y0 + size * (0.2 + 0.19 * i as f64);
        let right = if i == 2 {
            r.x1 - fold - line
        } else {
            r.x1 - margin
        };
        writing.move_to((r.x0 + margin, y));
        writing.line_to((right.max(r.x0 + margin), y));
    }
    vec![
        DrawOp::Path(PathOp::filled(sheet.clone(), Fill::new(color)).with_stroke(outline.clone())),
        DrawOp::Path(PathOp::stroked(corner, outline.clone())),
        DrawOp::Path(PathOp::stroked(
            writing,
            Stroke::new(line, ink).with_cap(LineCap::Round),
        )),
    ]
}

/// The leader and the box of a FreeText: everything but its text.
fn free_text_frame_ops(a: &Annotation) -> Vec<DrawOp> {
    let (Some(text), Geometry::Rect(frame)) = (&a.free_text, &a.geometry) else {
        return Vec::new();
    };
    let style = &a.style;
    let stroke = stroke_of(style);
    let mut ops = Vec::new();
    if let Some(stroke) = &stroke {
        if text.callout.len() >= 2 {
            // The ending is at the first point, the tip; the last point is at the box.
            ops.extend(polyline_ops(
                &text.callout,
                style.line_end,
                LineEnding::None,
                stroke,
                style.fill,
            ));
        }
    }
    let width = stroke.as_ref().map_or(0.0, |s| s.width);
    let r = inset_for_stroke(*frame, width);
    let cloud = style.cloud.map(|i| CloudStyle::new(cloud_scallop(i)));
    let (path, cloudy) = match cloud {
        Some(c) => (pl_draw::shapes::cloud_rect_path(r, &c), true),
        None => (rect_path(r), false),
    };
    let stroke = stroke.map(|s| {
        if cloudy {
            s.with_join(LineJoin::Round)
        } else {
            s
        }
    });
    if stroke.is_some() || style.fill.is_some() {
        ops.push(DrawOp::Path(PathOp {
            path,
            fill: style.fill.map(Fill::new),
            stroke,
            opacity: 1.0,
            blend: pl_draw::BlendMode::Normal,
        }));
    }
    ops
}

impl Annotation {
    /// Where the text of a modelled FreeText is laid out: the rectangle inside the border, in
    /// the frame the text is upright in, and the transform from that frame to page space (the
    /// identity unless the annotation is rotated). `None` for anything else.
    pub fn text_frame(&self) -> Option<(Rect, Affine)> {
        if self.kind != AnnotKind::FreeText {
            return None;
        }
        let (Some(text), Geometry::Rect(frame)) = (&self.free_text, &self.geometry) else {
            return None;
        };
        let border = stroke_of(&self.style).map_or(0.0, |s| s.width);
        // The border is drawn inside the frame; the text starts inside the border.
        let area = inset_for_stroke(*frame, 2.0 * border);
        Some(rotated_frame(area, text.rotation))
    }

    /// The text of a modelled FreeText laid out in its box: the lines, a caret position per
    /// character and what overflows, in the coordinates of [`Annotation::text_frame`]. `None`
    /// for anything else; an error when the text holds a character outside WinAnsi.
    pub fn text_layout(&self) -> Option<Result<TextLayout, DrawError>> {
        let (rect, _) = self.text_frame()?;
        let text = self.free_text.as_ref()?;
        if !self.geometry.is_finite() {
            return Some(Err(DrawError::InvalidNumber));
        }
        Some(layout_text(
            self.meta.contents.as_deref().unwrap_or(""),
            rect,
            &text.box_style(),
        ))
    }

    /// What the annotation paints, in page space.
    ///
    /// Empty for an unmodelled annotation (it is displayed from its own appearance stream),
    /// for a Popup (viewers draw the window themselves), for a note that is a reply or a
    /// review status (it lives in the markups list, not on the page) and for anything with no
    /// colour. A FreeText whose text cannot be laid out ([`Annotation::validate`] says why) is
    /// drawn without it.
    pub fn draw_list(&self) -> DrawList {
        let mut list = DrawList::new();
        if !self.is_modelled() || !self.geometry.is_finite() {
            return list;
        }
        if let Some(m) = self.measurement() {
            return crate::measure_draw::draw_list(self, m);
        }
        let style = &self.style;
        match (&self.kind, &self.geometry) {
            (AnnotKind::FreeText, Geometry::Rect(_)) => {
                let mut ops = free_text_frame_ops(self);
                if let (Some(Ok(layout)), Some((area, to))) =
                    (self.text_layout(), self.text_frame())
                {
                    // Text that does not fit is cut off at the inside of the border, as
                    // Acrobat cuts it. (The appearance's /BBox would not do it: for a callout
                    // it also covers the leader, and the border is inside it.)
                    let clip = (!layout.overflow.is_none()).then_some(area);
                    let runs = layout.ops();
                    if runs.is_empty() {
                    } else if let Some(clip) = clip {
                        ops.push(DrawOp::Group(
                            Group::new(runs).with_transform(to).with_clip(clip),
                        ));
                    } else if to == Affine::IDENTITY {
                        ops.extend(runs);
                    } else {
                        ops.push(DrawOp::Group(Group::new(runs).with_transform(to)));
                    }
                }
                list.push_composite(ops, style.opacity, style.blend);
            }
            (AnnotKind::Stamp, Geometry::Rect(rect)) => {
                let Some(stamp) = &self.stamp else {
                    return list;
                };
                let (frame, to) = rotated_frame(*rect, stamp.rotation);
                let ops = match &stamp.content {
                    StampContent::Label(spec) => pl_draw::stamp_list(spec, frame)
                        .map(|l| l.ops)
                        .unwrap_or_default(),
                    StampContent::Image(image) => {
                        if frame.width() > 0.0 && frame.height() > 0.0 {
                            vec![DrawOp::Image(ImageOp::new(image.clone(), frame))]
                        } else {
                            Vec::new()
                        }
                    }
                };
                // Opacity goes on the content itself and the turn around that, so that a
                // translucent picture stays one image with a graphics state, turned or not.
                let mut content = DrawList::new();
                content.push_composite(ops, style.opacity, style.blend);
                if to == Affine::IDENTITY {
                    list.extend(content.ops);
                } else if !content.is_empty() {
                    list.push(DrawOp::Group(Group::new(content.ops).with_transform(to)));
                }
            }
            (AnnotKind::Square | AnnotKind::Circle | AnnotKind::Polygon, _) => {
                if let Some((path, cloudy)) = closed_outline(self) {
                    let stroke = stroke_of(style).map(|s| {
                        if cloudy {
                            s.with_join(LineJoin::Round)
                        } else {
                            s
                        }
                    });
                    list.push_shape(path, &shape_style(style, stroke, true));
                }
            }
            (AnnotKind::Line, Geometry::Line { a, b }) => {
                if let Some(stroke) = stroke_of(style) {
                    let ops = polyline_ops(
                        &[*a, *b],
                        style.line_start,
                        style.line_end,
                        &stroke,
                        style.fill,
                    );
                    list.push_composite(ops, style.opacity, style.blend);
                }
            }
            (AnnotKind::PolyLine, Geometry::Points(points)) => {
                if let Some(stroke) = stroke_of(style) {
                    let ops = polyline_ops(
                        points,
                        style.line_start,
                        style.line_end,
                        &stroke,
                        style.fill,
                    );
                    list.push_composite(ops, style.opacity, style.blend);
                }
            }
            (AnnotKind::Ink, Geometry::Ink(strokes)) => {
                if let Some(stroke) = stroke_of(style) {
                    let stroke = stroke.with_cap(LineCap::Round).with_join(LineJoin::Round);
                    // One path for all strokes, so that where two strokes cross a translucent
                    // pen does not darken twice.
                    let mut path = BezPath::new();
                    for points in strokes {
                        path.extend(ink_path(points));
                    }
                    list.push_shape(path, &shape_style(style, Some(stroke), false));
                }
            }
            (AnnotKind::Highlight, Geometry::Quads(quads)) => {
                if let Some(color) = style.stroke {
                    let op = PathOp::filled(highlight_path(quads), Fill::new(color))
                        .with_opacity(style.opacity)
                        .with_blend(style.blend);
                    list.push(DrawOp::Path(op));
                }
            }
            (
                AnnotKind::Underline | AnnotKind::StrikeOut | AnnotKind::Squiggly,
                Geometry::Quads(quads),
            ) => {
                if let Some(color) = style.stroke {
                    let ops: Vec<DrawOp> = quads
                        .iter()
                        .map(|q| quad_rule(&self.kind, q))
                        .filter(|(path, _)| !path.elements().is_empty())
                        .map(|(path, width)| {
                            DrawOp::Path(PathOp::stroked(
                                path,
                                Stroke::new(width, color).with_join(LineJoin::Round),
                            ))
                        })
                        .collect();
                    list.push_composite(ops, style.opacity, style.blend);
                }
            }
            (AnnotKind::Text, Geometry::Rect(rect)) => {
                // A reply or a review status has no place on the page.
                if self.meta.reply_to.is_none() {
                    let color = style.stroke.unwrap_or(Rgb::new(1.0, 0.9, 0.2));
                    list.push_composite(note_ops(*rect, color), style.opacity, style.blend);
                }
            }
            (AnnotKind::Link, Geometry::Rect(rect)) => {
                if let Some(stroke) = stroke_of(style) {
                    let path = rect_path(inset_for_stroke(*rect, stroke.width));
                    list.push_shape(path, &shape_style(style, Some(stroke), false));
                }
            }
            _ => {}
        }
        list
    }

    /// `/Rect`, `/RD` and the draw list together.
    ///
    /// For Text, Link, Popup, Stamp and unmodelled annotations `/Rect` is the geometry itself.
    /// For the drawn kinds it is the box around what is painted (stroke widths, line endings,
    /// cloud scallops and a callout's leader included), rounded outwards to a sixteenth of a
    /// unit.
    pub fn placement(&self) -> Placement {
        let list = self.draw_list();
        let anchored = matches!(
            self.kind,
            AnnotKind::Text | AnnotKind::Link | AnnotKind::Popup | AnnotKind::Stamp
        ) || !self.is_modelled();
        if let (true, Geometry::Rect(rect)) = (anchored, &self.geometry) {
            return Placement {
                rect: *rect,
                rect_differences: None,
                list,
            };
        }
        let mut shape = self.geometry.bounds();
        if self.kind == AnnotKind::Ink && !list.is_empty() {
            // Every point of a pen stroke is covered by a disc of the pen's width. The draw
            // list's own bounds miss that disc for a stroke that is a single point.
            let half = self.style.width.max(0.0) / 2.0;
            shape = shape.inflate(half, half);
        }
        let painted = if self.kind == AnnotKind::FreeText {
            // The box and the leader decide /Rect; text that overflows the box is clipped to
            // the inside of its border ([`Annotation::draw_list`]) and never grows it.
            DrawList::from_ops(free_text_frame_ops(self)).bounds()
        } else {
            list.bounds()
        };
        let rect = round_out(painted.map_or(shape, |b| b.union(shape)));
        let rect_differences = match (&self.kind, &self.geometry) {
            (
                AnnotKind::Square | AnnotKind::Circle | AnnotKind::FreeText,
                Geometry::Rect(frame),
            ) => {
                let d = [
                    frame.x0 - rect.x0,
                    frame.y0 - rect.y0,
                    rect.x1 - frame.x1,
                    rect.y1 - frame.y1,
                ];
                d.iter().any(|v| *v > 1e-9).then_some(d.map(|v| v.max(0.0)))
            }
            _ => None,
        };
        Placement {
            rect,
            rect_differences,
            list,
        }
    }

    /// The box around everything the annotation paints or occupies: `/Rect` as a save would
    /// write it.
    pub fn bounds(&self) -> Rect {
        self.placement().rect
    }

    /// The outline used for hit-testing a closed shape.
    pub(crate) fn outline(&self) -> Option<BezPath> {
        closed_outline(self).map(|(path, _)| path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Origin;
    use crate::objects::PdfDate;

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        let roomy = outer.inflate(1e-6, 1e-6);
        roomy.union(inner) == roomy
    }

    #[test]
    fn a_plain_rectangle_is_placed_on_its_frame() {
        let frame = Rect::new(100.0, 200.0, 180.5, 260.25);
        let mut a = Annotation::square(0, frame, &origin());
        a.style.width = 3.0;
        let p = a.placement();
        assert_eq!(p.rect, frame, "the border is drawn inside the frame");
        assert_eq!(p.rect_differences, None);
        assert!(inside(p.rect, p.list.bounds().unwrap()));
        assert_eq!(a.bounds(), frame);
    }

    #[test]
    fn a_cloud_border_grows_the_rect_and_records_the_difference() {
        let frame = Rect::new(100.0, 200.0, 260.0, 300.0);
        for kind in [AnnotKind::Square, AnnotKind::Circle] {
            let mut a = Annotation::new(kind, 0, Geometry::Rect(frame), &origin());
            a.style.cloud = Some(1.0);
            let p = a.placement();
            assert!(p.rect.x0 < frame.x0 && p.rect.y1 > frame.y1, "{:?}", a.kind);
            let d = p.rect_differences.expect("a cloud needs /RD");
            // Left, top, right, bottom: /Rect shrunk by them is the frame again, exactly.
            let back = Rect::new(
                p.rect.x0 + d[0],
                p.rect.y0 + d[3],
                p.rect.x1 - d[2],
                p.rect.y1 - d[1],
            );
            assert_eq!(back, frame);
            assert!(inside(p.rect, p.list.bounds().unwrap()));
            // Everything is on the sixteenth grid.
            for v in [p.rect.x0, p.rect.y0, p.rect.x1, p.rect.y1] {
                assert_eq!((v * 16.0).fract(), 0.0);
            }
        }
        assert!(cloud_scallop(2.0) > cloud_scallop(1.0));
        assert_eq!(cloud_scallop(-5.0), cloud_scallop(0.0));
    }

    #[test]
    fn line_endings_and_width_are_inside_the_rect() {
        let mut a = Annotation::line(
            0,
            Point::new(50.0, 50.0),
            Point::new(150.0, 50.0),
            &origin(),
        );
        a.style.width = 4.0;
        let plain = a.placement().rect;
        assert_eq!(plain, Rect::new(50.0, 48.0, 150.0, 52.0));
        a.style.line_end = pl_draw::shapes::LineEnding::ClosedArrow;
        let arrow = a.placement();
        assert!(
            arrow.rect.height() > plain.height(),
            "the arrowhead is wider than the line"
        );
        assert!(inside(arrow.rect, arrow.list.bounds().unwrap()));
        assert_eq!(arrow.rect_differences, None);
    }

    #[test]
    fn nothing_is_drawn_without_a_colour_or_a_width() {
        let frame = Rect::new(0.0, 0.0, 50.0, 50.0);
        let mut a = Annotation::square(0, frame, &origin());
        a.style.stroke = None;
        assert!(a.draw_list().is_empty());
        assert_eq!(a.placement().rect, frame);
        a.style.fill = Some(Rgb::BLUE);
        assert_eq!(
            a.draw_list().bounds(),
            Some(frame),
            "a fill alone covers the frame"
        );
        a.style.fill = None;
        a.style.stroke = Some(Rgb::RED);
        a.style.width = 0.0;
        assert!(a.draw_list().is_empty());

        let line = {
            let mut l = Annotation::line(0, Point::new(1.0, 1.0), Point::new(1.0, 1.0), &origin());
            l.style.stroke = None;
            l
        };
        assert!(line.draw_list().is_empty());
        let r = line.placement().rect;
        assert!(
            r.width() > 0.0 && r.height() > 0.0,
            "a degenerate /Rect is given some area"
        );
    }

    #[test]
    fn the_note_icon_stays_inside_its_rect_and_popups_draw_nothing() {
        let rect = Rect::new(300.0, 300.0, 318.0, 318.0);
        let mut note = Annotation::note(0, rect, "n", &origin());
        let p = note.placement();
        assert_eq!(p.rect, rect);
        assert!(
            inside(rect, p.list.bounds().unwrap()),
            "{:?}",
            p.list.bounds()
        );
        assert!(p.list.ops.len() >= 3);
        let popup = Annotation::popup_for(&mut note, Rect::new(0.0, 0.0, 100.0, 50.0), &origin());
        assert!(popup.draw_list().is_empty());
        assert_eq!(popup.placement().rect, Rect::new(0.0, 0.0, 100.0, 50.0));
    }

    #[test]
    fn text_rules_follow_their_quads() {
        let quad = [
            Point::new(10.0, 10.0),
            Point::new(110.0, 10.0),
            Point::new(110.0, 24.0),
            Point::new(10.0, 24.0),
        ];
        let mut y = Vec::new();
        for kind in [
            AnnotKind::Underline,
            AnnotKind::Squiggly,
            AnnotKind::StrikeOut,
        ] {
            let a = Annotation::text_markup(kind, 0, vec![quad], &origin());
            let b = a.draw_list().bounds().unwrap();
            assert!(
                b.x0 >= 9.0 && b.x1 <= 111.0 && b.y0 >= 10.0 && b.y1 <= 24.0,
                "{b:?}"
            );
            assert!(b.width() > 99.0);
            y.push(b.center().y);
            assert!(inside(a.placement().rect, b));
        }
        assert!(
            y[0] < y[2] && y[1] < y[2],
            "underline and squiggle sit below the strikeout"
        );
        let h = Annotation::text_markup(AnnotKind::Highlight, 0, vec![quad], &origin());
        assert_eq!(
            h.draw_list().bounds(),
            Some(Rect::new(10.0, 10.0, 110.0, 24.0))
        );
        assert!(h.draw_list().uses_blend(), "a highlight multiplies");
    }

    #[test]
    fn a_single_point_pen_stroke_is_covered_by_the_rect() {
        let mut dot = Annotation::ink(0, vec![vec![Point::new(40.0, 40.0)]], &origin());
        dot.style.width = 6.0;
        let r = dot.placement().rect;
        assert_eq!(r, Rect::new(37.0, 37.0, 43.0, 43.0));
    }

    #[test]
    fn a_text_box_is_placed_on_its_frame_and_a_callout_grows_the_rect() {
        let frame = Rect::new(100.0, 200.0, 260.0, 260.0);
        let mut a = Annotation::free_text(0, frame, "Check this\nwall", &origin());
        a.style.width = 2.0;
        assert!(a.is_modelled());
        let p = a.placement();
        assert_eq!((p.rect, p.rect_differences), (frame, None));
        // Border, then one run per line.
        assert_eq!(p.list.ops.len(), 3);
        let (area, to) = a.text_frame().unwrap();
        assert_eq!(area, Rect::new(102.0, 202.0, 258.0, 258.0));
        assert_eq!(to, Affine::IDENTITY);
        let layout = a.text_layout().unwrap().unwrap();
        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.lines[0].origin.x, 105.0, "border 2 plus padding 3");

        let c = Annotation::callout(0, frame, Point::new(20.0, 120.0), "Here", &origin());
        let p = c.placement();
        assert!(
            p.rect.x0 <= 20.0 && p.rect.y0 <= 120.0 && p.rect.x1 == 260.0,
            "{:?}",
            p.rect
        );
        let d = p.rect_differences.expect("a callout needs /RD");
        // Left, bottom, right, top: the order PDFBox, MuPDF and Poppler read.
        let back = Rect::new(
            p.rect.x0 + d[0],
            p.rect.y0 + d[1],
            p.rect.x1 - d[2],
            p.rect.y1 - d[3],
        );
        assert_eq!(back, frame);
        // A tip below the box: the room for the leader is the second number, the bottom.
        let below = Annotation::callout(0, frame, Point::new(180.0, 40.0), "Here", &origin());
        let d = below.placement().rect_differences.expect("/RD");
        assert!(d[1] > 100.0 && d[3] == 0.0, "{d:?}");
        assert!(inside(
            p.rect,
            DrawList::from_ops(free_text_frame_ops(&c))
                .bounds()
                .unwrap()
        ));

        // Overflowing text does not grow the rectangle.
        let mut tall = Annotation::free_text(0, frame, "a\nb\nc\nd\ne\nf\ng\nh", &origin());
        assert_eq!(tall.placement().rect, frame);
        assert!(!tall.text_layout().unwrap().unwrap().overflow.is_none());
        // Text outside WinAnsi: the box is drawn without it and validate says why.
        tall.meta.contents = Some("ok \u{4e2d}".into());
        assert_eq!(tall.draw_list().ops.len(), 1);
        assert_eq!(
            tall.validate(),
            Err(DrawError::NotWinAnsi {
                ch: '\u{4e2d}',
                index: 3
            })
        );
    }

    #[test]
    fn rotated_text_is_laid_out_in_a_turned_frame() {
        let frame = Rect::new(100.0, 200.0, 160.0, 400.0);
        let mut a = Annotation::typewriter(0, frame, "Reads upward", &origin());
        a.set_rotation(90);
        assert_eq!(a.rotation(), 90);
        let (area, to) = a.text_frame().unwrap();
        assert_eq!((area.width(), area.height()), (200.0, 60.0));
        assert_eq!(to.transform_rect_bbox(area), frame);
        let layout = a.text_layout().unwrap().unwrap();
        assert_eq!(layout.lines.len(), 1, "200 units are enough for one line");
        // In page space the baseline runs up the page from the bottom; the first line is the
        // leftmost, so that on a page shown turned a quarter clockwise it is the top one.
        let start = to * layout.lines[0].origin;
        let end = to * (layout.lines[0].origin + Vec2::new(layout.lines[0].width, 0.0));
        assert!((end.x - start.x).abs() < 1e-9 && end.y > start.y);
        assert!(start.x < 130.0 && start.y < 210.0, "{start:?}");
        assert_eq!(a.placement().rect, frame);
        assert!(matches!(&a.draw_list().ops[..], [DrawOp::Group(_)]));
    }

    #[test]
    fn stamps_fill_their_rect_and_foreign_ones_draw_nothing() {
        let rect = Rect::new(50.0, 60.0, 210.0, 100.0);
        let s = Annotation::stamp(0, rect, pl_draw::StampSpec::approved(), &origin());
        assert!(s.is_modelled());
        assert_eq!(s.icon.as_deref(), Some("Approved"));
        let p = s.placement();
        assert_eq!((p.rect, p.rect_differences), (rect, None));
        assert!(inside(rect, p.list.ops[0].bounds().unwrap()));
        let image = std::sync::Arc::new(pl_draw::RgbaImage::new(2, 1, vec![255; 8]).unwrap());
        let mut i = Annotation::image_stamp(0, rect, image, &origin());
        assert_eq!(i.draw_list().bounds(), Some(rect));
        i.set_rotation(270);
        assert_eq!(i.draw_list().bounds(), Some(rect));
        let foreign = Annotation::new(AnnotKind::Stamp, 0, Geometry::Rect(rect), &origin());
        assert!(!foreign.is_modelled() && foreign.draw_list().is_empty());
    }

    #[test]
    fn replies_draw_nothing() {
        let rect = Rect::new(300.0, 300.0, 318.0, 318.0);
        let note = Annotation::note(0, rect, "n", &origin());
        let reply = Annotation::reply(&note, "agreed", &origin());
        assert!(reply.draw_list().is_empty());
        assert!(!note.draw_list().is_empty());
    }

    #[test]
    fn unmodelled_annotations_have_no_draw_list() {
        let a = Annotation::new(
            AnnotKind::FreeText,
            0,
            Geometry::Rect(Rect::new(0.0, 0.0, 10.0, 10.0)),
            &origin(),
        );
        assert!(a.draw_list().is_empty());
        assert_eq!(a.bounds(), Rect::new(0.0, 0.0, 10.0, 10.0));
        // A modelled kind with the wrong geometry is not drawn either.
        let b = Annotation::new(
            AnnotKind::Line,
            0,
            Geometry::Rect(Rect::new(0.0, 0.0, 10.0, 10.0)),
            &origin(),
        );
        assert!(!b.is_modelled() && b.draw_list().is_empty());
    }
}
