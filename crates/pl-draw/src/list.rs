//! [`DrawList`]: the plain-data description of what a markup looks like.

use std::sync::Arc;

use kurbo::{Affine, BezPath, Point, Rect, Shape};

use crate::style::{clamp_opacity, BlendMode, Fill, Rgb, ShapeStyle, Stroke};
use crate::text::{measure_text, winansi_encode, FontStyle};
use crate::DrawError;

/// An ordered list of drawing ops in page space (unrotated page user space, y-up). Later ops
/// paint over earlier ones.
///
/// It is plain data: the rasteriser ([`crate::raster`]), the appearance-stream emitter
/// ([`crate::emit`]) and any other painter all read the same list, so they cannot disagree
/// about what a markup is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawList {
    pub ops: Vec<DrawOp>,
}

/// One drawing op.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    Path(PathOp),
    Text(TextRun),
    Image(ImageOp),
    Group(Group),
}

/// A path that is filled, stroked or both (fill first).
///
/// `opacity` and `blend` apply to the fill and to the stroke separately: where a translucent
/// stroke overlaps its own fill, both show. To treat a shape as one object that is then made
/// translucent, put it in a [`Group`]; [`DrawList::push_shape`] does that when it matters.
#[derive(Clone, Debug, PartialEq)]
pub struct PathOp {
    pub path: BezPath,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    pub opacity: f64,
    pub blend: BlendMode,
}

impl PathOp {
    pub fn filled(path: BezPath, fill: Fill) -> PathOp {
        PathOp {
            path,
            fill: Some(fill),
            stroke: None,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }

    pub fn stroked(path: BezPath, stroke: Stroke) -> PathOp {
        PathOp {
            path,
            fill: None,
            stroke: Some(stroke),
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }

    pub fn with_fill(mut self, fill: Fill) -> PathOp {
        self.fill = Some(fill);
        self
    }

    pub fn with_stroke(mut self, stroke: Stroke) -> PathOp {
        self.stroke = Some(stroke);
        self
    }

    pub fn with_opacity(mut self, opacity: f64) -> PathOp {
        self.opacity = opacity;
        self
    }

    pub fn with_blend(mut self, blend: BlendMode) -> PathOp {
        self.blend = blend;
        self
    }
}

/// One line of text in one style. `origin` is the start of the baseline; the text runs along
/// +x. Rotated text goes in a [`Group`] with a transform.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub origin: Point,
    /// Font size in page units.
    pub size: f64,
    pub style: FontStyle,
    pub color: Rgb,
    pub opacity: f64,
    pub blend: BlendMode,
}

impl TextRun {
    /// Builds a run, rejecting text that `WinAnsiEncoding` cannot hold.
    pub fn new(
        text: impl Into<String>,
        origin: Point,
        size: f64,
        style: FontStyle,
        color: Rgb,
    ) -> Result<TextRun, DrawError> {
        let text = text.into();
        winansi_encode(&text)?;
        Ok(TextRun {
            text,
            origin,
            size,
            style,
            color,
            opacity: 1.0,
            blend: BlendMode::Normal,
        })
    }

    pub fn with_opacity(mut self, opacity: f64) -> TextRun {
        self.opacity = opacity;
        self
    }
}

/// An RGBA8 image with straight (not premultiplied) alpha: row-major, top row first.
#[derive(Clone, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl std::fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RgbaImage({} x {})", self.width, self.height)
    }
}

impl RgbaImage {
    /// Wraps pixel data. The image must not be empty and `pixels` must be exactly
    /// `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<RgbaImage, DrawError> {
        let expected = width as usize * height as usize * 4;
        if width == 0 || height == 0 || pixels.len() != expected {
            return Err(DrawError::ImageSize {
                width,
                height,
                actual: pixels.len(),
            });
        }
        Ok(RgbaImage {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Straight-alpha RGBA bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// True when every pixel is fully opaque.
    pub fn is_opaque(&self) -> bool {
        self.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255)
    }
}

/// An image stretched over `rect`, upright in page space: its top row lies along `rect.y1`.
/// The pixel data is shared, so cloning a list does not copy it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageOp {
    pub image: Arc<RgbaImage>,
    pub rect: Rect,
    pub opacity: f64,
    pub blend: BlendMode,
}

impl ImageOp {
    pub fn new(image: Arc<RgbaImage>, rect: Rect) -> ImageOp {
        ImageOp {
            image,
            rect,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }
    }
}

/// Ops drawn under a transform, optionally as one translucent or blended object.
///
/// With `opacity == 1` and `BlendMode::Normal` a group is only a transform. Otherwise the
/// children are first painted together on their own, as if on a clear sheet (an isolated
/// transparency group), and the sheet is then laid over the backdrop with the group's opacity
/// and blend mode. Overlapping children therefore do not show through each other.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// Maps the children's coordinates to the parent's.
    pub transform: Affine,
    pub opacity: f64,
    pub blend: BlendMode,
    pub ops: Vec<DrawOp>,
    /// When set, nothing the children paint shows outside this rectangle (given in the
    /// children's own coordinates): a clipping path in the emitted stream.
    pub clip: Option<Rect>,
}

impl Group {
    pub fn new(ops: Vec<DrawOp>) -> Group {
        Group {
            transform: Affine::IDENTITY,
            opacity: 1.0,
            blend: BlendMode::Normal,
            ops,
            clip: None,
        }
    }

    /// Clips the children to `rect`, in their own coordinates.
    pub fn with_clip(mut self, rect: Rect) -> Group {
        self.clip = Some(rect.abs());
        self
    }

    pub fn with_transform(mut self, transform: Affine) -> Group {
        self.transform = transform;
        self
    }

    pub fn with_opacity(mut self, opacity: f64) -> Group {
        self.opacity = opacity;
        self
    }

    pub fn with_blend(mut self, blend: BlendMode) -> Group {
        self.blend = blend;
        self
    }

    /// True when the children must be painted together before compositing (see the type docs).
    pub fn is_layer(&self) -> bool {
        clamp_opacity(self.opacity) < 1.0 || self.blend != BlendMode::Normal
    }
}

/// Tolerance, in page units, for flattening while computing stroke outlines for bounds.
const BOUNDS_TOLERANCE: f64 = 0.05;

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn rect_is_finite(r: Rect) -> bool {
    r.x0.is_finite() && r.y0.is_finite() && r.x1.is_finite() && r.y1.is_finite()
}

/// The area a stroke covers: the exact outline of the undashed stroke, so miter spikes, caps and
/// round joins are all included.
pub fn stroke_bounds(path: &BezPath, stroke: &Stroke) -> Option<Rect> {
    if path.elements().is_empty() {
        return None;
    }
    let half = (stroke.width.max(0.0)) / 2.0;
    // The answer for a hairline, and for degenerate paths (a dot) where the outline is empty.
    let base = path.bounding_box().inflate(half, half);
    if half == 0.0 {
        return Some(base);
    }
    let cap = match stroke.cap {
        crate::LineCap::Butt => kurbo::Cap::Butt,
        crate::LineCap::Round => kurbo::Cap::Round,
        crate::LineCap::Square => kurbo::Cap::Square,
    };
    let join = match stroke.join {
        crate::LineJoin::Miter => kurbo::Join::Miter,
        crate::LineJoin::Round => kurbo::Join::Round,
        crate::LineJoin::Bevel => kurbo::Join::Bevel,
    };
    let style = kurbo::Stroke::new(stroke.width)
        .with_caps(cap)
        .with_join(join)
        .with_miter_limit(stroke.miter_limit.max(1.0));
    let outline = kurbo::stroke(
        path.iter(),
        &style,
        &kurbo::StrokeOpts::default(),
        BOUNDS_TOLERANCE,
    );
    let exact = (!outline.elements().is_empty())
        .then(|| outline.bounding_box())
        .filter(|r| rect_is_finite(*r));
    // A subpath of no length still paints with a round cap: a dot as wide as the stroke. The
    // stroker's outline leaves it out (or flattens it to nothing), so it is added here.
    let dots = if stroke.cap == crate::LineCap::Round {
        zero_length_subpaths(path).fold(None, |acc, p| {
            union(
                acc,
                Some(Rect::from_points(p, p).inflate(half, half)).filter(|r| rect_is_finite(*r)),
            )
        })
    } else {
        None
    };
    union(exact, dots).or(Some(base))
}

/// The points of the subpaths that have at least one segment and no extent (`m x y l x y`, or a
/// closed point): the ones a round cap turns into a dot.
fn zero_length_subpaths(path: &BezPath) -> impl Iterator<Item = Point> + '_ {
    let mut out = Vec::new();
    // (start, has a segment, every point so far equals the start)
    let mut current: Option<(Point, bool, bool)> = None;
    let flush = |c: Option<(Point, bool, bool)>, out: &mut Vec<Point>| {
        if let Some((start, true, true)) = c {
            out.push(start);
        }
    };
    for el in path.elements() {
        match *el {
            kurbo::PathEl::MoveTo(p) => {
                flush(current.take(), &mut out);
                current = Some((p, false, true));
            }
            kurbo::PathEl::LineTo(p) => {
                if let Some(c) = current.as_mut() {
                    c.1 = true;
                    c.2 &= p == c.0;
                }
            }
            kurbo::PathEl::QuadTo(a, p) => {
                if let Some(c) = current.as_mut() {
                    c.1 = true;
                    c.2 &= a == c.0 && p == c.0;
                }
            }
            kurbo::PathEl::CurveTo(a, b, p) => {
                if let Some(c) = current.as_mut() {
                    c.1 = true;
                    c.2 &= a == c.0 && b == c.0 && p == c.0;
                }
            }
            kurbo::PathEl::ClosePath => {
                if let Some(c) = current.as_mut() {
                    c.1 = true;
                }
            }
        }
    }
    flush(current, &mut out);
    out.into_iter()
}

impl DrawOp {
    /// The area this op can paint, in the coordinates it is drawn in. `None` when it paints
    /// nothing. Stroke width, caps, joins and line endings are included; a dash pattern is
    /// ignored (the solid stroke covers it).
    pub fn bounds(&self) -> Option<Rect> {
        match self {
            DrawOp::Path(p) => {
                if p.path.elements().is_empty() {
                    return None;
                }
                let fill = p.fill.map(|_| p.path.bounding_box());
                let stroke = p.stroke.as_ref().and_then(|s| stroke_bounds(&p.path, s));
                union(fill, stroke).filter(|r| rect_is_finite(*r))
            }
            DrawOp::Text(t) => {
                let m = measure_text(&t.text, t.style, t.size).ok()?;
                if t.text.is_empty() {
                    return None;
                }
                // Oblique glyphs lean past their advance; a small margin covers the overhang.
                let pad = 0.25 * t.size.abs();
                Some(Rect::new(
                    t.origin.x - pad,
                    t.origin.y + m.descent,
                    t.origin.x + m.width + pad,
                    t.origin.y + m.ascent,
                ))
                .filter(|r| rect_is_finite(*r))
            }
            DrawOp::Image(i) => Some(i.rect.abs()).filter(|r| rect_is_finite(*r)),
            DrawOp::Group(g) => ops_bounds(&g.ops)
                .and_then(|r| match g.clip {
                    Some(clip) => {
                        let r = r.intersect(clip);
                        (r.width() > 0.0 && r.height() > 0.0).then_some(r)
                    }
                    None => Some(r),
                })
                .map(|r| g.transform.transform_rect_bbox(r))
                .filter(|r| rect_is_finite(*r)),
        }
    }

    /// True when this op, or anything inside it, uses a blend mode other than Normal.
    pub fn uses_blend(&self) -> bool {
        match self {
            DrawOp::Path(p) => p.blend != BlendMode::Normal,
            DrawOp::Text(t) => t.blend != BlendMode::Normal,
            DrawOp::Image(i) => i.blend != BlendMode::Normal,
            DrawOp::Group(g) => {
                g.blend != BlendMode::Normal || g.ops.iter().any(DrawOp::uses_blend)
            }
        }
    }
}

pub(crate) fn ops_bounds(ops: &[DrawOp]) -> Option<Rect> {
    ops.iter().fold(None, |acc, op| union(acc, op.bounds()))
}

impl DrawList {
    pub fn new() -> DrawList {
        DrawList::default()
    }

    pub fn from_ops(ops: Vec<DrawOp>) -> DrawList {
        DrawList { ops }
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn push(&mut self, op: DrawOp) -> &mut DrawList {
        self.ops.push(op);
        self
    }

    pub fn extend(&mut self, ops: impl IntoIterator<Item = DrawOp>) -> &mut DrawList {
        self.ops.extend(ops);
        self
    }

    /// Adds a path with a shape style, giving the look a viewer gives an annotation with `/CA`:
    /// the whole shape is one object. A stroked-and-filled shape that is translucent or blended
    /// goes in a group so the stroke does not show through its own fill; anything else is a
    /// single path op.
    pub fn push_shape(&mut self, path: BezPath, style: &ShapeStyle) -> &mut DrawList {
        if style.stroke.is_none() && style.fill.is_none() {
            return self;
        }
        let op = DrawOp::Path(PathOp {
            path,
            fill: style.fill,
            stroke: style.stroke.clone(),
            opacity: 1.0,
            blend: BlendMode::Normal,
        });
        self.push_composite(vec![op], style.opacity, style.blend)
    }

    /// Adds `ops` as one object with an opacity and blend mode. A lone op that is only filled or
    /// only stroked (or a text run, or an image) takes them directly; anything else is wrapped
    /// in a [`Group`]. Opaque Normal ops are added as they are.
    pub fn push_composite(
        &mut self,
        mut ops: Vec<DrawOp>,
        opacity: f64,
        blend: BlendMode,
    ) -> &mut DrawList {
        let opacity = clamp_opacity(opacity);
        if ops.is_empty() {
            return self;
        }
        if opacity >= 1.0 && blend == BlendMode::Normal {
            self.ops.extend(ops);
            return self;
        }
        if ops.len() == 1 {
            let direct = match &mut ops[0] {
                DrawOp::Path(p)
                    if p.fill.is_some() != p.stroke.is_some()
                        && p.opacity >= 1.0
                        && p.blend == BlendMode::Normal =>
                {
                    p.opacity = opacity;
                    p.blend = blend;
                    true
                }
                DrawOp::Text(t) if t.opacity >= 1.0 && t.blend == BlendMode::Normal => {
                    t.opacity = opacity;
                    t.blend = blend;
                    true
                }
                DrawOp::Image(i) if i.opacity >= 1.0 && i.blend == BlendMode::Normal => {
                    i.opacity = opacity;
                    i.blend = blend;
                    true
                }
                _ => false,
            };
            if direct {
                self.ops.extend(ops);
                return self;
            }
        }
        self.ops.push(DrawOp::Group(
            Group::new(ops).with_opacity(opacity).with_blend(blend),
        ));
        self
    }

    /// The same drawing under a transform (the ops are wrapped in one group).
    pub fn transformed(&self, transform: Affine) -> DrawList {
        DrawList {
            ops: vec![DrawOp::Group(
                Group::new(self.ops.clone()).with_transform(transform),
            )],
        }
    }

    /// The smallest rectangle that contains everything the list can paint, in page space, or
    /// `None` for a list that paints nothing. This is the natural `/Rect` and `/BBox`.
    pub fn bounds(&self) -> Option<Rect> {
        ops_bounds(&self.ops)
    }

    /// True when any op uses a blend mode other than Normal. Such a list cannot be drawn by a
    /// painter without blend modes; it is composited with [`crate::raster`] instead.
    pub fn uses_blend(&self) -> bool {
        self.ops.iter().any(DrawOp::uses_blend)
    }

    /// Checks what can be checked without drawing: every number is finite and every text run
    /// fits WinAnsi. The rasteriser and the emitter run this first.
    pub fn validate(&self) -> Result<(), DrawError> {
        validate_ops(&self.ops)
    }
}

fn finite(values: &[f64]) -> Result<(), DrawError> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(DrawError::InvalidNumber)
    }
}

fn validate_ops(ops: &[DrawOp]) -> Result<(), DrawError> {
    for op in ops {
        match op {
            DrawOp::Path(p) => {
                for el in p.path.elements() {
                    match *el {
                        kurbo::PathEl::MoveTo(a) | kurbo::PathEl::LineTo(a) => finite(&[a.x, a.y])?,
                        kurbo::PathEl::QuadTo(a, b) => finite(&[a.x, a.y, b.x, b.y])?,
                        kurbo::PathEl::CurveTo(a, b, c) => finite(&[a.x, a.y, b.x, b.y, c.x, c.y])?,
                        kurbo::PathEl::ClosePath => {}
                    }
                }
                if let Some(s) = &p.stroke {
                    finite(&[s.width, s.miter_limit])?;
                    if s.width < 0.0 {
                        return Err(DrawError::InvalidNumber);
                    }
                }
                // A NaN opacity is treated as opaque; an infinite one clamps.
            }
            DrawOp::Text(t) => {
                finite(&[t.origin.x, t.origin.y, t.size])?;
                winansi_encode(&t.text)?;
            }
            DrawOp::Image(i) => finite(&[i.rect.x0, i.rect.y0, i.rect.x1, i.rect.y1])?,
            DrawOp::Group(g) => {
                finite(&g.transform.as_coeffs())?;
                if let Some(c) = g.clip {
                    finite(&[c.x0, c.y0, c.x1, c.y1])?;
                }
                validate_ops(&g.ops)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::{polyline_path, rect_path};
    use crate::{LineCap, LineJoin};

    fn close(a: Rect, b: Rect, tol: f64) -> bool {
        (a.x0 - b.x0).abs() <= tol
            && (a.y0 - b.y0).abs() <= tol
            && (a.x1 - b.x1).abs() <= tol
            && (a.y1 - b.y1).abs() <= tol
    }

    #[test]
    fn empty_list_has_no_bounds() {
        assert_eq!(DrawList::new().bounds(), None);
        let mut l = DrawList::new();
        l.push(DrawOp::Path(PathOp::filled(
            BezPath::new(),
            Fill::new(Rgb::RED),
        )));
        assert_eq!(l.bounds(), None);
    }

    #[test]
    fn fill_bounds_are_the_path_box() {
        let mut l = DrawList::new();
        let r = Rect::new(10.0, 20.0, 110.0, 70.0);
        l.push(DrawOp::Path(PathOp::filled(
            rect_path(r),
            Fill::new(Rgb::RED),
        )));
        assert_eq!(l.bounds(), Some(r));
    }

    #[test]
    fn stroke_bounds_add_half_the_width() {
        let r = Rect::new(10.0, 20.0, 110.0, 70.0);
        let op = DrawOp::Path(PathOp::stroked(rect_path(r), Stroke::new(6.0, Rgb::BLACK)));
        // Right-angle miter corners reach exactly half a width past the path.
        assert!(close(op.bounds().unwrap(), r.inflate(3.0, 3.0), 1e-6));
    }

    #[test]
    fn caps_extend_the_bounds_along_the_line() {
        let line = polyline_path(&[Point::new(10.0, 10.0), Point::new(50.0, 10.0)]);
        let butt = stroke_bounds(&line, &Stroke::new(4.0, Rgb::BLACK)).unwrap();
        assert!(
            close(butt, Rect::new(10.0, 8.0, 50.0, 12.0), 1e-6),
            "{butt:?}"
        );
        for cap in [LineCap::Round, LineCap::Square] {
            let b = stroke_bounds(&line, &Stroke::new(4.0, Rgb::BLACK).with_cap(cap)).unwrap();
            assert!(
                close(b, Rect::new(8.0, 8.0, 52.0, 12.0), 0.06),
                "{cap:?} {b:?}"
            );
        }
    }

    #[test]
    fn a_sharp_miter_reaches_past_half_the_width_and_the_limit_cuts_it() {
        // A 30 degree corner at the origin, opening to the right.
        let a = 15f64.to_radians();
        let pts = [
            Point::new(40.0 * a.cos(), 40.0 * a.sin()),
            Point::ZERO,
            Point::new(40.0 * a.cos(), -40.0 * a.sin()),
        ];
        let path = polyline_path(&pts);
        let w = 4.0;
        // Miter length from the corner is (w / 2) / sin(half angle).
        let spike = (w / 2.0) / a.sin();
        let mitered = stroke_bounds(&path, &Stroke::new(w, Rgb::BLACK)).unwrap();
        assert!((mitered.x0 + spike).abs() < 0.01, "{mitered:?} vs {spike}");
        // With the limit below the ratio 1 / sin(half angle) = 3.86 the corner is bevelled.
        let limited =
            stroke_bounds(&path, &Stroke::new(w, Rgb::BLACK).with_miter_limit(2.0)).unwrap();
        assert!(
            limited.x0 > -w / 2.0 - 0.01 && limited.x0 < 0.0,
            "{limited:?}"
        );
        let round = stroke_bounds(
            &path,
            &Stroke::new(w, Rgb::BLACK).with_join(LineJoin::Round),
        )
        .unwrap();
        assert!((round.x0 + w / 2.0).abs() < 0.06, "{round:?}");
    }

    #[test]
    fn a_dot_has_bounds() {
        let p = Point::new(5.0, 5.0);
        let dot = polyline_path(&[p, p]);
        let b =
            stroke_bounds(&dot, &Stroke::new(4.0, Rgb::BLACK).with_cap(LineCap::Round)).unwrap();
        assert!(b.contains(Point::new(3.5, 5.0)) && b.contains(Point::new(5.0, 6.5)));
    }

    #[test]
    fn a_zero_length_round_capped_stroke_is_a_dot_as_wide_as_the_stroke() {
        let p = Point::new(40.0, 60.0);
        let round = Stroke::new(6.0, Rgb::BLACK).with_cap(LineCap::Round);
        let want = Rect::new(37.0, 57.0, 43.0, 63.0);
        // On its own.
        let dot = polyline_path(&[p, p]);
        let b = stroke_bounds(&dot, &round).unwrap();
        assert!(close(b, want, 1e-9), "{b:?}");
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::stroked(dot.clone(), round.clone())));
        assert!(close(list.bounds().unwrap(), want, 1e-9));
        // As one subpath among others: the dot still counts.
        let mut path = polyline_path(&[Point::new(0.0, 0.0), Point::new(10.0, 0.0)]);
        path.move_to(p);
        path.line_to(p);
        let b = stroke_bounds(&path, &round).unwrap();
        assert!(close(b, Rect::new(-3.0, -3.0, 43.0, 63.0), 0.06), "{b:?}");
        // A closed point is a dot too.
        let mut closed = BezPath::new();
        closed.move_to(p);
        closed.close_path();
        let b = stroke_bounds(&closed, &round).unwrap();
        assert!(close(b, want, 1e-9), "{b:?}");
        // A lone move paints nothing, and adds nothing to a path that has other parts.
        let mut lone = polyline_path(&[Point::new(0.0, 0.0), Point::new(10.0, 0.0)]);
        lone.move_to(p);
        let b = stroke_bounds(&lone, &round).unwrap();
        assert!(close(b, Rect::new(-3.0, -3.0, 13.0, 3.0), 0.06), "{b:?}");
        // The rasteriser agrees: the dot's pixels are inside the bounds and fill them.
        let pixmap = crate::render_new(&list, Affine::scale(4.0), 400, 400, None).unwrap();
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..400u32 {
            for x in 0..400u32 {
                if pixmap.pixel(x, y).unwrap().alpha() > 128 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        assert!(x1 > x0, "the dot is drawn");
        assert!((x1 - x0) as f64 >= 4.0 * 6.0 - 3.0 && (y1 - y0) as f64 >= 4.0 * 6.0 - 3.0);
        assert!(x0 as f64 >= 4.0 * 37.0 - 1.0 && x1 as f64 <= 4.0 * 43.0 + 1.0);
    }

    #[test]
    fn hairline_bounds_are_the_path_box() {
        let line = polyline_path(&[Point::new(0.0, 0.0), Point::new(10.0, 5.0)]);
        assert_eq!(
            stroke_bounds(&line, &Stroke::new(0.0, Rgb::BLACK)),
            Some(Rect::new(0.0, 0.0, 10.0, 5.0))
        );
    }

    #[test]
    fn group_bounds_follow_the_transform() {
        let r = Rect::new(0.0, 0.0, 10.0, 4.0);
        let inner = DrawOp::Path(PathOp::filled(rect_path(r), Fill::new(Rgb::RED)));
        let g = Group::new(vec![inner]).with_transform(
            Affine::translate((100.0, 50.0)) * Affine::rotate(std::f64::consts::FRAC_PI_2),
        );
        let b = DrawOp::Group(g.clone()).bounds().unwrap();
        assert!(close(b, Rect::new(96.0, 50.0, 100.0, 60.0), 1e-9), "{b:?}");
        // A clip (in the children's coordinates) bounds what the group can paint.
        let clipped = DrawOp::Group(g.clone().with_clip(Rect::new(0.0, 0.0, 5.0, 100.0)));
        let b = clipped.bounds().unwrap();
        assert!(close(b, Rect::new(96.0, 50.0, 100.0, 55.0), 1e-9), "{b:?}");
        let nothing = DrawOp::Group(g.with_clip(Rect::new(500.0, 500.0, 600.0, 600.0)));
        assert_eq!(nothing.bounds(), None);
    }

    #[test]
    fn text_and_image_bounds() {
        let t = TextRun::new(
            "Hello",
            Point::new(10.0, 100.0),
            12.0,
            FontStyle::Regular,
            Rgb::BLACK,
        )
        .unwrap();
        let m = measure_text("Hello", FontStyle::Regular, 12.0).unwrap();
        let b = DrawOp::Text(t).bounds().unwrap();
        assert!(b.x0 <= 10.0 && b.x1 >= 10.0 + m.width);
        assert!((b.y0 - (100.0 + m.descent)).abs() < 1e-9);
        assert!((b.y1 - (100.0 + m.ascent)).abs() < 1e-9);

        let img = Arc::new(RgbaImage::new(2, 1, vec![0; 8]).unwrap());
        let op = DrawOp::Image(ImageOp::new(img, Rect::new(5.0, 6.0, 25.0, 16.0)));
        assert_eq!(op.bounds(), Some(Rect::new(5.0, 6.0, 25.0, 16.0)));
    }

    #[test]
    fn image_size_is_checked() {
        assert!(RgbaImage::new(2, 2, vec![0; 16]).is_ok());
        assert_eq!(
            RgbaImage::new(2, 2, vec![0; 15]),
            Err(DrawError::ImageSize {
                width: 2,
                height: 2,
                actual: 15
            })
        );
        assert!(RgbaImage::new(0, 2, vec![]).is_err());
        assert!(RgbaImage::new(1, 1, vec![1, 2, 3, 255])
            .unwrap()
            .is_opaque());
        assert!(!RgbaImage::new(1, 1, vec![1, 2, 3, 254])
            .unwrap()
            .is_opaque());
    }

    #[test]
    fn push_shape_groups_only_when_fill_and_stroke_would_show_through() {
        let path = rect_path(Rect::new(0.0, 0.0, 10.0, 10.0));
        let both = ShapeStyle::stroked(Stroke::new(2.0, Rgb::RED)).with_fill(Fill::new(Rgb::BLUE));

        let mut opaque = DrawList::new();
        opaque.push_shape(path.clone(), &both);
        assert!(matches!(&opaque.ops[..], [DrawOp::Path(p)] if p.opacity == 1.0));

        let mut translucent = DrawList::new();
        translucent.push_shape(path.clone(), &both.clone().with_opacity(0.5));
        match &translucent.ops[..] {
            [DrawOp::Group(g)] => {
                assert_eq!(g.opacity, 0.5);
                assert!(g.is_layer());
                assert!(matches!(&g.ops[..], [DrawOp::Path(p)] if p.opacity == 1.0));
            }
            other => panic!("expected one group, got {other:?}"),
        }

        let mut stroke_only = DrawList::new();
        stroke_only.push_shape(
            path.clone(),
            &ShapeStyle::stroked(Stroke::new(2.0, Rgb::RED))
                .with_opacity(0.5)
                .with_blend(BlendMode::Multiply),
        );
        assert!(matches!(
            &stroke_only.ops[..],
            [DrawOp::Path(p)] if p.opacity == 0.5 && p.blend == BlendMode::Multiply
        ));
        assert!(stroke_only.uses_blend());
        assert!(!translucent.uses_blend());

        let mut nothing = DrawList::new();
        nothing.push_shape(
            path,
            &ShapeStyle {
                stroke: None,
                fill: None,
                opacity: 1.0,
                blend: BlendMode::Normal,
            },
        );
        assert!(nothing.is_empty());
    }

    #[test]
    fn validate_rejects_non_finite_numbers_and_foreign_text() {
        let mut l = DrawList::new();
        l.push(DrawOp::Path(PathOp::stroked(
            polyline_path(&[Point::ZERO, Point::new(f64::NAN, 1.0)]),
            Stroke::new(1.0, Rgb::BLACK),
        )));
        assert_eq!(l.validate(), Err(DrawError::InvalidNumber));

        let mut l = DrawList::new();
        l.push(DrawOp::Text(TextRun {
            text: "\u{4e2d}".into(),
            origin: Point::ZERO,
            size: 10.0,
            style: FontStyle::Regular,
            color: Rgb::BLACK,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }));
        assert!(matches!(
            l.validate(),
            Err(DrawError::NotWinAnsi { index: 0, .. })
        ));
        assert!(TextRun::new(
            "\u{4e2d}",
            Point::ZERO,
            10.0,
            FontStyle::Regular,
            Rgb::BLACK
        )
        .is_err());

        let mut l = DrawList::new();
        l.push(DrawOp::Group(
            Group::new(vec![]).with_transform(Affine::scale(f64::INFINITY)),
        ));
        assert_eq!(l.validate(), Err(DrawError::InvalidNumber));

        let mut l = DrawList::new();
        l.push(DrawOp::Path(PathOp::stroked(
            rect_path(Rect::new(0.0, 0.0, 1.0, 1.0)),
            Stroke::new(-1.0, Rgb::BLACK),
        )));
        assert_eq!(l.validate(), Err(DrawError::InvalidNumber));
    }
}
