//! What a measurement paints: the dimension line with its endings and leader lines, the
//! outline of an area with its cutouts as holes, the arc of an angle, the circle of a radius,
//! the markers of a count, and the caption with the value.
//!
//! # Captions
//!
//! A caption is one line of text in the standard Helvetica. It is placed in page space but
//! reads upright in the *displayed* orientation: [`Measurement::rotation`] is the page's
//! `/Rotate`, and the text runs left to right on the page as shown (bottom to top for a line
//! that is vertical there). The caption of a line is centred on it; that of an area or volume
//! is centred on the outline's visual centre ([`pl_geom::polygon_label_point`]), which is
//! inside the outline and outside every cutout.

use pl_draw::kurbo::BezPath;
use pl_draw::shapes::{ellipse_path, polygon_path, polyline_ops};
use pl_draw::{
    measure_text, DrawList, DrawOp, Fill, FontStyle, Group, LineCap, LineEnding, LineJoin, PathOp,
    Rgb, Stroke, TextRun,
};
use pl_geom::{
    point_in_region, point_segment_distance, polygon_label_point, signed_angle_at, Affine, Point,
    Rect, Vec2,
};

use crate::measure::{CaptionPosition, CountSymbol, MeasureKind, Measurement};
use crate::model::{Annotation, Geometry, Style};

/// Where a measurement's caption is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Caption {
    pub text: String,
    /// The start of the baseline, in page space.
    pub origin: Point,
    /// The direction the text runs in, in page space: a unit vector.
    pub direction: Vec2,
    /// The advance width of the text, in page units.
    pub width: f64,
    /// The font size, in page units.
    pub size: f64,
}

impl Caption {
    /// The middle of the text: half way along it and half a capital's height up.
    pub fn centre(&self) -> Point {
        let cap = measure_text(&self.text, FontStyle::Regular, self.size)
            .map_or(0.7 * self.size, |m| m.cap_height);
        let up = Vec2::new(-self.direction.y, self.direction.x);
        self.origin + self.direction * (self.width / 2.0) + up * (cap / 2.0)
    }
}

/// The page-space direction that points right on a page shown with `/Rotate` `rotation`.
fn display_right(rotation: u16) -> Vec2 {
    match rotation {
        90 => Vec2::new(0.0, 1.0),
        180 => Vec2::new(-1.0, 0.0),
        270 => Vec2::new(0.0, -1.0),
        _ => Vec2::new(1.0, 0.0),
    }
}

fn left_of(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

/// `along` or its opposite, whichever reads left to right on the displayed page; for a line
/// that is vertical there, the one that reads bottom to top.
fn reading_direction(along: Vec2, rotation: u16) -> Vec2 {
    let right = display_right(rotation);
    let up = left_of(right);
    let h = along.dot(right);
    if h < -1e-9 || (h.abs() <= 1e-9 && along.dot(up) < 0.0) {
        -along
    } else {
        along
    }
}

/// A caption centred on `centre` along `direction`, its baseline `lift` above the centre.
fn caption_at(text: &str, size: f64, centre: Point, direction: Vec2, lift: f64) -> Option<Caption> {
    if text.is_empty() || !(size.is_finite() && size > 0.0) {
        return None;
    }
    let width = measure_text(text, FontStyle::Regular, size).ok()?.width;
    Some(Caption {
        text: text.to_string(),
        origin: centre - direction * (width / 2.0) + left_of(direction) * lift,
        direction,
        width,
        size,
    })
}

fn cap_height(text: &str, size: f64) -> f64 {
    measure_text(text, FontStyle::Regular, size).map_or(0.7 * size, |m| m.cap_height)
}

fn caption_op(c: &Caption, color: Rgb) -> Option<DrawOp> {
    let upright = c.direction == Vec2::new(1.0, 0.0);
    let origin = if upright { c.origin } else { Point::ZERO };
    let run = TextRun::new(c.text.clone(), origin, c.size, FontStyle::Regular, color).ok()?;
    if upright {
        return Some(DrawOp::Text(run));
    }
    let d = c.direction;
    let to = Affine::new([d.x, d.y, -d.y, d.x, c.origin.x, c.origin.y]);
    Some(DrawOp::Group(
        Group::new(vec![DrawOp::Text(run)]).with_transform(to),
    ))
}

fn stroke_of(style: &Style) -> Option<Stroke> {
    let color = style.stroke?;
    if !(style.width.is_finite() && style.width > 0.0) {
        return None;
    }
    let mut stroke = Stroke::new(style.width, color);
    if let Some(dash) = &style.dash {
        stroke = stroke.with_dash(pl_draw::Dash::new(dash.clone(), 0.0));
    }
    Some(stroke)
}

/// A dimension line as it is drawn: moved off the measured points by the leader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Dimension {
    /// The ends of the dimension line.
    pub a: Point,
    pub b: Point,
    /// The leader lines, from near each measured point to just past the dimension line.
    pub leaders: Option<[(Point, Point); 2]>,
    /// The unit direction from `a` to `b` and its left normal.
    pub along: Vec2,
    pub normal: Vec2,
}

pub(crate) fn dimension(a: Point, b: Point, m: &Measurement) -> Option<Dimension> {
    let d = b - a;
    let len = d.hypot();
    if !(len.is_finite() && len > 1e-9) {
        return None;
    }
    let along = d / len;
    let normal = left_of(along);
    if m.leader == 0.0 || !m.leader.is_finite() {
        return Some(Dimension {
            a,
            b,
            leaders: None,
            along,
            normal,
        });
    }
    let sign = m.leader.signum();
    let gap = sign * m.leader_offset.max(0.0);
    let off = m.leader + gap;
    let past = off + sign * m.leader_extension.max(0.0);
    Some(Dimension {
        a: a + normal * off,
        b: b + normal * off,
        leaders: Some([
            (a + normal * gap, a + normal * past),
            (b + normal * gap, b + normal * past),
        ]),
        along,
        normal,
    })
}

/// The circle a Radius or Diameter measures: centre and radius.
pub(crate) fn measured_circle(a: Point, b: Point, kind: MeasureKind) -> Option<(Point, f64)> {
    match kind {
        MeasureKind::Radius => Some((a, a.distance(b))),
        MeasureKind::Diameter => Some((a.midpoint(b), a.distance(b) / 2.0)),
        _ => None,
    }
}

/// The longest segment of a run of points (the closing one included when `closed`).
fn longest_segment(points: &[Point], closed: bool) -> Option<(Point, Point)> {
    let n = points.len();
    let count = if closed { n } else { n.saturating_sub(1) };
    (0..count)
        .map(|i| (points[i], points[(i + 1) % n]))
        .filter(|(p, q)| p.distance(*q) > 1e-9)
        // The first of equally long segments, so that the choice does not flicker.
        .fold(None, |best: Option<(Point, Point)>, s| match best {
            Some(b) if b.0.distance(b.1) >= s.0.distance(s.1) - 1e-9 => Some(b),
            _ => Some(s),
        })
}

/// A caption above the middle of the segment from `p` to `q`.
fn caption_over(text: &str, m: &Measurement, p: Point, q: Point, width: f64) -> Option<Caption> {
    let along = (q - p) / p.distance(q);
    let reading = reading_direction(along, m.rotation);
    caption_at(
        text,
        m.text_size,
        p.midpoint(q),
        reading,
        width / 2.0 + 0.3 * m.text_size,
    )
}

fn symbol_path(symbol: CountSymbol, c: Point, size: f64, rotation: u16) -> BezPath {
    let h = size / 2.0;
    let right = display_right(rotation);
    let up = left_of(right);
    let at = |x: f64, y: f64| c + right * (x * h) + up * (y * h);
    let ring = |points: &[(f64, f64)], close: bool| {
        let mut path = BezPath::new();
        for (i, (x, y)) in points.iter().enumerate() {
            if i == 0 {
                path.move_to(at(*x, *y));
            } else {
                path.line_to(at(*x, *y));
            }
        }
        if close {
            path.close_path();
        }
        path
    };
    let around = |n: usize, radius: &dyn Fn(usize) -> f64| -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| {
                let t = std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * i as f64 / n as f64;
                (radius(i) * t.cos(), radius(i) * t.sin())
            })
            .collect()
    };
    match symbol {
        CountSymbol::Dot | CountSymbol::Ring => {
            ellipse_path(Rect::new(c.x - h, c.y - h, c.x + h, c.y + h))
        }
        CountSymbol::Square => ring(
            &[(-0.85, -0.85), (0.85, -0.85), (0.85, 0.85), (-0.85, 0.85)],
            true,
        ),
        CountSymbol::Diamond => ring(&[(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)], true),
        CountSymbol::Triangle => ring(&around(3, &|_| 1.0), true),
        CountSymbol::Hexagon => ring(&around(6, &|_| 1.0), true),
        CountSymbol::Star => ring(&around(10, &|i| if i % 2 == 0 { 1.0 } else { 0.42 }), true),
        CountSymbol::Cross => {
            let mut path = ring(&[(-0.8, -0.8), (0.8, 0.8)], false);
            path.extend(ring(&[(-0.8, 0.8), (0.8, -0.8)], false));
            path
        }
        CountSymbol::Plus => {
            let mut path = ring(&[(-1.0, 0.0), (1.0, 0.0)], false);
            path.extend(ring(&[(0.0, -1.0), (0.0, 1.0)], false));
            path
        }
        CountSymbol::Check => ring(&[(-0.9, 0.0), (-0.3, -0.7), (0.9, 0.8)], false),
    }
}

/// The outline of a count marker as the vertices of the small closed polygon a count is
/// written as (one per marker, as Revu writes them): the symbol's own corners, a circle as
/// 24 points, a line figure (a cross, a plus, a tick) as its points in drawing order.
pub(crate) fn symbol_outline(symbol: CountSymbol, c: Point, size: f64, rotation: u16) -> Vec<Point> {
    if matches!(symbol, CountSymbol::Dot | CountSymbol::Ring) {
        let h = size / 2.0;
        return (0..24)
            .map(|i| {
                let t = std::f64::consts::TAU * f64::from(i) / 24.0;
                Point::new(c.x + h * t.cos(), c.y + h * t.sin())
            })
            .collect();
    }
    symbol_path(symbol, c, size, rotation)
        .elements()
        .iter()
        .filter_map(|el| match el {
            pl_draw::kurbo::PathEl::MoveTo(p) | pl_draw::kurbo::PathEl::LineTo(p) => {
                Some(Point::new(p.x, p.y))
            }
            _ => None,
        })
        .collect()
}

/// What one marker of a count paints when the count is written as one annotation per marker:
/// its symbol, and on the last marker the caption with the whole count's value.
pub(crate) fn count_item_list(a: &Annotation, m: &Measurement, index: usize) -> DrawList {
    let points = a.count_points();
    let Some(at) = points.get(index).copied() else {
        return DrawList::new();
    };
    let mut one = a.clone();
    one.geometry = Geometry::Ink(vec![vec![at, at]]);
    let mut single = m.clone();
    single.caption = false;
    one.measure = Some(single.clone());
    let (mut ops, _) = parts(&one, &single);
    if index + 1 == points.len() {
        let color = a.style.stroke.unwrap_or(Rgb::BLACK);
        if let Some(op) = a.caption().as_ref().and_then(|c| caption_op(c, color)) {
            ops.push(op);
        }
    }
    let mut list = DrawList::new();
    list.push_composite(ops, a.style.opacity, a.style.blend);
    list
}

/// The ops and the caption of a measurement, before opacity.
fn parts(a: &Annotation, m: &Measurement) -> (Vec<DrawOp>, Option<Caption>) {
    let style = &a.style;
    let stroke = stroke_of(style);
    let width = stroke.as_ref().map_or(0.0, |s| s.width);
    let text = if m.caption {
        a.measure_text().unwrap_or_default()
    } else {
        String::new()
    };
    let size = m.text_size;
    let mut ops: Vec<DrawOp> = Vec::new();
    let mut caption = None;

    match (m.kind, &a.geometry) {
        (kind, Geometry::Line { a: p, b: q }) => {
            let Some(dim) = dimension(*p, *q, m) else {
                return (ops, None);
            };
            let middle = dim.a.midpoint(dim.b) + dim.along * m.caption_offset.x;
            let reading = reading_direction(dim.along, m.rotation);
            let mut gap = None;
            if !text.is_empty() {
                let lift = dim.normal.dot(left_of(reading)) * m.caption_offset.y;
                let above = caption_at(
                    &text,
                    size,
                    middle,
                    reading,
                    width / 2.0 + 0.3 * size + lift,
                );
                caption = match m.caption_position {
                    CaptionPosition::Top => above,
                    CaptionPosition::Inline => {
                        let inline = caption_at(
                            &text,
                            size,
                            middle,
                            reading,
                            lift - cap_height(&text, size) / 2.0,
                        );
                        let half = inline.as_ref().map_or(0.0, |c| c.width / 2.0) + 0.35 * size;
                        let room = dim.a.distance(dim.b) / 2.0
                            - m.caption_offset.x.abs()
                            - pl_draw::arrow_length(width);
                        // A caption that was moved off the line needs no gap in it; one the
                        // line is too short for goes above it instead.
                        if m.caption_offset.y != 0.0 {
                            inline
                        } else if half < room {
                            gap = Some((middle - dim.along * half, middle + dim.along * half));
                            inline
                        } else {
                            above
                        }
                    }
                };
            }
            if let Some(stroke) = &stroke {
                if let Some((centre, radius)) = measured_circle(*p, *q, kind) {
                    let r = Rect::new(
                        centre.x - radius,
                        centre.y - radius,
                        centre.x + radius,
                        centre.y + radius,
                    );
                    ops.push(DrawOp::Path(PathOp::stroked(
                        ellipse_path(r),
                        stroke.clone(),
                    )));
                }
                if let Some(leaders) = dim.leaders {
                    let mut path = BezPath::new();
                    for (from, to) in leaders {
                        path.move_to(from);
                        path.line_to(to);
                    }
                    ops.push(DrawOp::Path(PathOp::stroked(path, stroke.solid())));
                }
                match gap {
                    Some((g0, g1)) => {
                        ops.extend(polyline_ops(
                            &[dim.a, g0],
                            style.line_start,
                            LineEnding::None,
                            stroke,
                            style.fill,
                        ));
                        ops.extend(polyline_ops(
                            &[g1, dim.b],
                            LineEnding::None,
                            style.line_end,
                            stroke,
                            style.fill,
                        ));
                    }
                    None => ops.extend(polyline_ops(
                        &[dim.a, dim.b],
                        style.line_start,
                        style.line_end,
                        stroke,
                        style.fill,
                    )),
                }
            }
        }
        (MeasureKind::Polylength, Geometry::Points(points)) => {
            if let Some(stroke) = &stroke {
                ops.extend(polyline_ops(
                    points,
                    style.line_start,
                    style.line_end,
                    stroke,
                    style.fill,
                ));
            }
            if let Some((p, q)) = longest_segment(points, false) {
                caption = caption_over(&text, m, p, q, width);
            }
        }
        (
            MeasureKind::Perimeter | MeasureKind::Area | MeasureKind::Volume,
            Geometry::Points(points),
        ) => {
            let mut path = polygon_path(points);
            for ring in &m.cutouts {
                path.extend(polygon_path(ring));
            }
            // Even-odd: a cutout is a hole whichever way its ring runs.
            let fill = style.fill.map(Fill::even_odd);
            if fill.is_some() || stroke.is_some() {
                ops.push(DrawOp::Path(PathOp {
                    path,
                    fill,
                    stroke: stroke.clone().map(|s| s.with_join(LineJoin::Round)),
                    opacity: 1.0,
                    blend: pl_draw::BlendMode::Normal,
                }));
            }
            if m.kind == MeasureKind::Perimeter {
                if let Some((p, q)) = longest_segment(points, true) {
                    caption = caption_over(&text, m, p, q, width);
                }
            } else if let Some((centre, _)) = polygon_label_point(points, &m.cutouts, 0.25) {
                caption = caption_at(
                    &text,
                    size,
                    centre,
                    display_right(m.rotation),
                    -cap_height(&text, size) / 2.0,
                );
            }
        }
        (MeasureKind::Angle, Geometry::Points(points)) if points.len() == 3 => {
            let (p, vertex, q) = (points[0], points[1], points[2]);
            if let Some(stroke) = &stroke {
                ops.extend(polyline_ops(
                    points,
                    style.line_start,
                    style.line_end,
                    stroke,
                    style.fill,
                ));
            }
            let arms = vertex.distance(p).min(vertex.distance(q));
            if arms > 1e-9 {
                let radius = (arms / 2.0).min(28.0);
                let start = (p - vertex).atan2();
                let sweep = signed_angle_at(vertex, p, q);
                let on = |t: f64| vertex + Vec2::new(t.cos(), t.sin()) * radius;
                if let Some(stroke) = &stroke {
                    let steps = ((sweep.abs() / 5f64.to_radians()).ceil() as usize).max(1);
                    let mut arc = BezPath::new();
                    arc.move_to(on(start));
                    for i in 1..=steps {
                        arc.line_to(on(start + sweep * i as f64 / steps as f64));
                    }
                    ops.push(DrawOp::Path(PathOp::stroked(
                        arc,
                        stroke.solid().with_join(LineJoin::Round),
                    )));
                }
                let right = display_right(m.rotation);
                let bisector = Vec2::new((start + sweep / 2.0).cos(), (start + sweep / 2.0).sin());
                if let Some(c) = caption_at(&text, size, Point::ZERO, right, 0.0) {
                    // Far enough out that the text clears the arc whichever way it points.
                    let out = radius + size + c.width / 2.0 * bisector.dot(right).abs();
                    caption = caption_at(
                        &text,
                        size,
                        vertex + bisector * out,
                        right,
                        -cap_height(&text, size) / 2.0,
                    );
                }
            }
        }
        (MeasureKind::Count, _) => {
            let points = a.count_points();
            if let (Some(color), false) = (style.stroke, points.is_empty()) {
                let mut path = BezPath::new();
                for p in &points {
                    path.extend(symbol_path(m.symbol, *p, m.symbol_size, m.rotation));
                }
                if m.symbol == CountSymbol::Dot {
                    ops.push(DrawOp::Path(PathOp::filled(path, Fill::new(color))));
                } else if let Some(stroke) = &stroke {
                    ops.push(DrawOp::Path(PathOp::stroked(
                        path,
                        stroke
                            .solid()
                            .with_join(LineJoin::Round)
                            .with_cap(LineCap::Round),
                    )));
                }
            }
            if let Some(last) = points.last() {
                let right = display_right(m.rotation);
                if let Some(c) = caption_at(&text, size, Point::ZERO, right, 0.0) {
                    let centre = *last + right * (m.symbol_size / 2.0 + 0.4 * size + c.width / 2.0);
                    caption =
                        caption_at(&text, size, centre, right, -cap_height(&text, size) / 2.0);
                }
            }
        }
        _ => {}
    }
    (ops, caption)
}

/// What a measurement paints, in page space.
pub(crate) fn draw_list(a: &Annotation, m: &Measurement) -> DrawList {
    let (mut ops, caption) = parts(a, m);
    let color = a.style.stroke.unwrap_or(Rgb::BLACK);
    if let Some(op) = caption.as_ref().and_then(|c| caption_op(c, color)) {
        ops.push(op);
    }
    let mut list = DrawList::new();
    list.push_composite(ops, a.style.opacity, a.style.blend);
    list
}

/// Whether `p` is on a measurement, within `reach` of its lines (`tolerance` of its filled
/// parts and markers). `None` for a kind that is hit the way its carrier is.
pub(crate) fn hit(
    a: &Annotation,
    m: &Measurement,
    p: Point,
    reach: f64,
    tolerance: f64,
) -> Option<bool> {
    match (m.kind, &a.geometry) {
        (kind, Geometry::Line { a: from, b: to }) => {
            let Some(dim) = dimension(*from, *to, m) else {
                return Some(from.distance(p) <= reach);
            };
            let on_line = point_segment_distance(p, dim.a, dim.b) <= reach;
            let on_leader = dim.leaders.is_some_and(|l| {
                l.iter()
                    .any(|(s, e)| point_segment_distance(p, *s, *e) <= reach)
            });
            let on_circle = measured_circle(*from, *to, kind)
                .is_some_and(|(centre, radius)| (centre.distance(p) - radius).abs() <= reach);
            Some(on_line || on_leader || on_circle)
        }
        (MeasureKind::Area | MeasureKind::Volume, Geometry::Points(points)) => {
            let ring_hit = |ring: &[Point]| {
                let n = ring.len();
                (0..n).any(|i| point_segment_distance(p, ring[i], ring[(i + 1) % n]) <= reach)
            };
            let inside = a.style.fill.is_some() && point_in_region(p, points, &m.cutouts);
            Some(inside || ring_hit(points) || m.cutouts.iter().any(|r| ring_hit(r)))
        }
        (MeasureKind::Count, _) => Some(
            a.count_points()
                .iter()
                .any(|c| c.distance(p) <= m.symbol_size / 2.0 + tolerance),
        ),
        _ => None,
    }
}

impl Annotation {
    /// Where the caption of a measurement is drawn; `None` when it has none (not a
    /// measurement, the caption is turned off, or the text cannot be drawn).
    pub fn caption(&self) -> Option<Caption> {
        let m = self.measurement()?;
        if !self.geometry.is_finite() {
            return None;
        }
        parts(self, m).1
    }

    /// The ends of the dimension line of a Length, Radius or Diameter as it is drawn: the
    /// measured points moved off by the leader (`/LL` and `/LLO`). `None` for anything else.
    pub fn dimension_line(&self) -> Option<(Point, Point)> {
        let m = self.measurement()?;
        match &self.geometry {
            Geometry::Line { a, b } => dimension(*a, *b, m).map(|d| (d.a, d.b)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::{MeasureFormat, MeasureSpec};
    use crate::model::Origin;
    use crate::objects::PdfDate;
    use pl_geom::{AreaFormat, AreaUnit, LengthFormat, Scale, Unit};

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn spec() -> MeasureSpec {
        MeasureSpec::new(
            &Scale::engineering(20.0).unwrap(),
            1.0,
            &MeasureFormat::new(
                LengthFormat::decimal(Unit::Foot, 2),
                AreaFormat::new(AreaUnit::SqFoot, 2),
            ),
        )
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        let roomy = outer.inflate(1e-6, 1e-6);
        roomy.union(inner) == roomy
    }

    #[test]
    fn captions_read_upright_as_the_page_is_displayed() {
        let right = Vec2::new(1.0, 0.0);
        let up = Vec2::new(0.0, 1.0);
        // On an unrotated page: left to right, and bottom to top for a vertical line.
        assert_eq!(reading_direction(right, 0), right);
        assert_eq!(reading_direction(-right, 0), right);
        assert_eq!(reading_direction(up, 0), up);
        assert_eq!(reading_direction(-up, 0), up);
        // On a page turned a quarter clockwise, page-up is display-right.
        assert_eq!(reading_direction(-up, 90), up);
        assert_eq!(
            reading_direction(right, 90),
            -right,
            "page-left is display-up"
        );
        assert_eq!(reading_direction(right, 180), -right);
        assert_eq!(reading_direction(up, 270), -up);
        let slant = Vec2::new(-0.6, 0.8);
        assert_eq!(reading_direction(slant, 0), -slant);
        assert_eq!(reading_direction(slant, 90), slant);

        for rotation in [0u16, 90, 180, 270] {
            for (a, b) in [
                (p(100.0, 100.0), p(300.0, 100.0)),
                (p(300.0, 100.0), p(100.0, 100.0)),
                (p(100.0, 100.0), p(100.0, 300.0)),
                (p(100.0, 300.0), p(180.0, 120.0)),
            ] {
                let mut m = Annotation::measure_length(0, a, b, spec(), &origin());
                m.set_rotation(i32::from(rotation));
                let c = m.caption().unwrap();
                let shown = c.direction.dot(display_right(rotation));
                let rising = c.direction.dot(left_of(display_right(rotation)));
                assert!(
                    shown > 1e-9 || (shown.abs() <= 1e-9 && rising > 0.0),
                    "rotation {rotation}, {a:?} to {b:?}: {:?}",
                    c.direction
                );
                // Along the line, centred on it, and on the display-up side of it.
                let along = (b - a) / a.distance(b);
                assert!(c.direction.cross(along).abs() < 1e-9);
                let off = c.centre() - a.midpoint(b);
                assert!(off.dot(c.direction).abs() < 1e-6, "{off:?}");
                assert!(off.dot(left_of(c.direction)) > 0.0);
                assert_eq!(c.text, m.measure_text().unwrap());
            }
        }
    }

    #[test]
    fn a_leader_moves_the_dimension_line_off_the_points() {
        let (a, b) = (p(100.0, 100.0), p(300.0, 100.0));
        let mut m = Annotation::measure_length(0, a, b, spec(), &origin());
        assert_eq!(m.dimension_line(), Some((a, b)));
        let flat = m.bounds();
        {
            let measure = m.measure.as_mut().unwrap();
            measure.leader = 30.0;
            measure.leader_extension = 6.0;
            measure.leader_offset = 4.0;
        }
        // To the left of a line that runs right is up the page.
        assert_eq!(m.dimension_line(), Some((p(100.0, 134.0), p(300.0, 134.0))));
        let dim = dimension(a, b, m.measure.as_ref().unwrap()).unwrap();
        assert_eq!(
            dim.leaders,
            Some([
                (p(100.0, 104.0), p(100.0, 140.0)),
                (p(300.0, 104.0), p(300.0, 140.0))
            ])
        );
        let rect = m.bounds();
        assert!(rect.y1 > flat.y1 + 30.0 && rect.y0 >= 100.0, "{rect:?}");
        assert!(inside(rect, m.draw_list().bounds().unwrap()));
        // The value does not change with the leader.
        assert_eq!(m.measure_text().as_deref(), Some("55.56'"));
        assert!(m.hit_test(p(200.0, 134.0), 1.0), "on the dimension line");
        assert!(m.hit_test(p(100.0, 120.0), 1.0), "on a leader");
        assert!(
            !m.hit_test(p(200.0, 100.0), 1.0),
            "between the measured points"
        );
        // A negative leader goes the other way.
        m.measure.as_mut().unwrap().leader = -30.0;
        assert_eq!(m.dimension_line(), Some((p(100.0, 66.0), p(300.0, 66.0))));
        let dim = dimension(a, b, m.measure.as_ref().unwrap()).unwrap();
        assert_eq!(dim.leaders.unwrap()[0], (p(100.0, 96.0), p(100.0, 60.0)));
        // The caption sits over the moved line.
        let c = m.caption().unwrap();
        assert!((c.centre().x - 200.0).abs() < 1e-6 && c.centre().y > 66.0);
    }

    #[test]
    fn an_inline_caption_breaks_the_line_and_a_short_line_keeps_it_above() {
        let (a, b) = (p(100.0, 100.0), p(300.0, 100.0));
        let mut m = Annotation::measure_length(0, a, b, spec(), &origin());
        let top = m.caption().unwrap();
        assert!(top.origin.y > 100.0, "above the line");
        let whole = m.draw_list().ops.len();
        m.measure.as_mut().unwrap().caption_position = CaptionPosition::Inline;
        let inline = m.caption().unwrap();
        assert!(
            (inline.centre().y - 100.0).abs() < 1e-6,
            "{:?}",
            inline.centre()
        );
        assert!((inline.centre().x - 200.0).abs() < 1e-6);
        assert!(
            m.draw_list().ops.len() > whole,
            "the line is drawn in two pieces"
        );
        // Moved along the line by /CO.
        m.measure.as_mut().unwrap().caption_offset = Vec2::new(40.0, 0.0);
        assert!((m.caption().unwrap().centre().x - 240.0).abs() < 1e-6);
        // Too short for its text: the caption goes above and the line stays whole.
        let mut short = Annotation::measure_length(0, a, p(120.0, 100.0), spec(), &origin());
        short.measure.as_mut().unwrap().caption_position = CaptionPosition::Inline;
        assert!(short.caption().unwrap().origin.y > 100.0);
        // No caption at all.
        m.measure.as_mut().unwrap().caption = false;
        assert_eq!(m.caption(), None);
        assert!(!m.draw_list().is_empty());
    }

    #[test]
    fn an_area_caption_sits_at_the_visual_centre_inside_the_outline() {
        // An L whose centroid is near the inner corner; a hole in the wide arm.
        let outline = vec![
            p(100.0, 100.0),
            p(400.0, 100.0),
            p(400.0, 200.0),
            p(200.0, 200.0),
            p(200.0, 400.0),
            p(100.0, 400.0),
        ];
        let hole = vec![
            p(300.0, 120.0),
            p(380.0, 120.0),
            p(380.0, 180.0),
            p(300.0, 180.0),
        ];
        for rotation in [0u16, 90, 180, 270] {
            let mut m =
                Annotation::measure_area(0, outline.clone(), vec![hole.clone()], spec(), &origin());
            m.set_rotation(i32::from(rotation));
            m.style.fill = Some(Rgb::new(0.5, 0.75, 1.0));
            let c = m.caption().unwrap();
            assert_eq!(c.direction, display_right(rotation));
            let centre = c.centre();
            assert!(
                point_in_region(centre, &outline, std::slice::from_ref(&hole)),
                "{centre:?}"
            );
            let (want, _) =
                polygon_label_point(&outline, std::slice::from_ref(&hole), 0.25).unwrap();
            assert!((centre - want).hypot() < 1e-6);
            // One path with the hole in it, filled even-odd, then the text.
            let list = m.draw_list();
            let DrawOp::Path(path) = &list.ops[0] else {
                panic!("the outline comes first");
            };
            assert_eq!(path.fill.unwrap().rule, pl_draw::FillRule::EvenOdd);
            assert_eq!(
                path.path
                    .elements()
                    .iter()
                    .filter(|e| matches!(e, pl_draw::kurbo::PathEl::MoveTo(_)))
                    .count(),
                2
            );
            // Filled: hit inside, but not in the hole.
            assert!(m.hit_test(p(150.0, 300.0), 1.0));
            assert!(!m.hit_test(p(340.0, 150.0), 1.0));
            assert!(m.hit_test(p(300.0, 150.0), 1.0), "on the hole's edge");
        }
    }

    #[test]
    fn radius_diameter_angle_and_count_draw_their_figures() {
        let og = origin();
        // A radius draws its whole circle.
        let m = Annotation::measure_radius(0, p(200.0, 200.0), p(250.0, 200.0), spec(), &og);
        let b = m.bounds();
        assert!(
            b.x0 <= 150.0 && b.x1 >= 250.0 && b.y0 <= 150.0 && b.y1 >= 250.0,
            "{b:?}"
        );
        assert!(m.hit_test(p(200.0, 250.0), 1.0), "on the circle");
        assert!(m.hit_test(p(225.0, 200.0), 1.0), "on the radius line");
        assert!(!m.hit_test(p(180.0, 180.0), 1.0));
        let m = Annotation::measure_diameter(0, p(150.0, 200.0), p(250.0, 200.0), spec(), &og);
        assert!(m.hit_test(p(200.0, 150.0), 1.0), "on the circle");
        let b = m.bounds();
        assert!(b.y0 <= 150.0 && b.y1 >= 250.0);

        // An angle: the caption is on the bisector, beyond the arc.
        let v = p(200.0, 200.0);
        let m = Annotation::measure_angle(0, p(300.0, 200.0), v, p(200.0, 300.0), None, &og);
        let c = m.caption().unwrap();
        let off = c.centre() - v;
        assert!(
            (off.x - off.y).abs() < 1e-6 && off.x > 28.0 / 2f64.sqrt(),
            "{off:?}"
        );
        assert_eq!(c.text, "90.0\u{b0}");
        assert_eq!(m.draw_list().ops.len(), 3, "arms, arc, text");

        // A count draws every marker in one path, and its /Rect holds them all.
        let points = [p(100.0, 100.0), p(160.0, 130.0), p(220.0, 90.0)];
        for symbol in CountSymbol::ALL {
            let mut m = Annotation::count(0, &points, symbol, &og);
            m.measure.as_mut().unwrap().symbol_size = 12.0;
            let list = m.draw_list();
            assert_eq!(list.ops.len(), 1, "{symbol:?}");
            let DrawOp::Path(path) = &list.ops[0] else {
                panic!("markers are one path");
            };
            assert_eq!(path.fill.is_some(), symbol == CountSymbol::Dot);
            let b = m.bounds();
            // Every marker reaches at least half its half-size below its point (a triangle's
            // base is at -h/2) and its full half-size above or beside it.
            assert!(
                b.x0 <= 95.0 && b.x1 >= 225.0 && b.y0 <= 87.0 && b.y1 >= 134.0,
                "{symbol:?} {b:?}"
            );
            assert!(inside(b, list.bounds().unwrap()), "{symbol:?}");
            assert!(m.hit_test(p(162.0, 131.0), 1.0));
            assert!(!m.hit_test(p(130.0, 115.0), 1.0), "between two markers");
            assert_eq!(m.caption(), None);
            // With its caption on: the number beside the last marker.
            m.measure.as_mut().unwrap().caption = true;
            let c = m.caption().unwrap();
            assert_eq!(c.text, "3");
            assert!(c.origin.x > 226.0 && (c.centre().y - 90.0).abs() < 1e-6);
        }
        // A triangle points up on the page as displayed.
        let tip = |rotation: u16| {
            let path = symbol_path(CountSymbol::Triangle, p(0.0, 0.0), 10.0, rotation);
            match path.elements()[0] {
                pl_draw::kurbo::PathEl::MoveTo(q) => q,
                _ => panic!("a path starts with a move"),
            }
        };
        assert!((tip(0) - p(0.0, 5.0)).hypot() < 1e-9);
        assert!((tip(90) - p(-5.0, 0.0)).hypot() < 1e-9);
        assert!((tip(180) - p(0.0, -5.0)).hypot() < 1e-9);
    }

    #[test]
    fn perimeter_and_polylength_captions_follow_their_longest_segment() {
        let og = origin();
        let run = vec![
            p(100.0, 100.0),
            p(100.0, 160.0),
            p(300.0, 160.0),
            p(300.0, 120.0),
        ];
        let m = Annotation::measure_polylength(0, run.clone(), spec(), &og);
        let c = m.caption().unwrap();
        assert_eq!(c.direction, Vec2::new(1.0, 0.0));
        assert!((c.centre().x - 200.0).abs() < 1e-6 && c.centre().y > 160.0);
        // Closed, the longest edge is still the top one; the bottom one is as long but later.
        let m = Annotation::measure_perimeter(
            0,
            vec![
                p(100.0, 100.0),
                p(100.0, 160.0),
                p(300.0, 160.0),
                p(300.0, 100.0),
            ],
            spec(),
            &og,
        );
        let c = m.caption().unwrap();
        assert!((c.centre().x - 200.0).abs() < 1e-6 && c.centre().y > 160.0);
        assert!(m.style.fill.is_none());
        // Without a colour the caption is still drawn, in black.
        let mut bare = m.clone();
        bare.style.stroke = None;
        assert_eq!(bare.draw_list().ops.len(), 1);
    }
}
