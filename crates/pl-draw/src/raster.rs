//! CPU rasteriser: draws a [`DrawList`] into a tiny-skia pixmap.
//!
//! The pixmap's existing contents are the backdrop. Normal ops are laid over it; Multiply ops
//! are multiplied into it, which is how a highlighter is composited against a rendered tile.
//! Over a transparent backdrop a Multiply op has nothing to darken and paints as Normal, as the
//! PDF blend formulas say.

use kurbo::{Affine, BezPath, PathEl, Rect};
use pl_geom::PageGeometry;
use tiny_skia::{
    FillRule as SkFillRule, FilterQuality, IntSize, Paint, Path, PathBuilder, Pattern, Pixmap,
    PixmapPaint, Shader, SpreadMode, Transform,
};

use crate::list::{ops_bounds, DrawList, DrawOp, Group, ImageOp, PathOp, RgbaImage, TextRun};
use crate::style::{clamp_opacity, BlendMode, FillRule, LineCap, LineJoin, Rgb, Stroke};
use crate::text::text_outline;
use crate::DrawError;

/// The largest canvas side the convenience functions will allocate, in pixels.
pub const MAX_CANVAS_SIDE: u32 = 16_384;

/// Draws `list` onto `canvas`. `transform` maps page space to canvas pixels (y-down, origin at
/// the top-left pixel corner); for a whole page that is
/// `Affine::scale(px_per_unit) * PageGeometry::page_to_display()`.
///
/// Nothing is drawn when the list fails [`DrawList::validate`].
pub fn render(list: &DrawList, transform: Affine, canvas: &mut Pixmap) -> Result<(), DrawError> {
    list.validate()?;
    if !transform.as_coeffs().iter().all(|v| v.is_finite()) {
        return Err(DrawError::InvalidNumber);
    }
    draw_ops(&list.ops, transform, canvas)
}

/// Draws `list` onto a new canvas that is transparent, or filled with `background`.
pub fn render_new(
    list: &DrawList,
    transform: Affine,
    width: u32,
    height: u32,
    background: Option<Rgb>,
) -> Result<Pixmap, DrawError> {
    let mut canvas = new_canvas(width, height)?;
    if let Some(bg) = background {
        canvas.fill(color(bg, 1.0));
    }
    render(list, transform, &mut canvas)?;
    Ok(canvas)
}

/// Draws `list` over a backdrop given as straight-alpha RGBA8 bytes (the layout of a rendered
/// tile) and returns the composite in the same layout. This is the Multiply path of the app:
/// the tile under a highlighter is the backdrop, the result is uploaded as the overlay.
pub fn render_over_rgba(
    list: &DrawList,
    transform: Affine,
    backdrop: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, DrawError> {
    let mut canvas = pixmap_from_rgba(backdrop, width, height)?;
    render(list, transform, &mut canvas)?;
    Ok(canvas.take_demultiplied())
}

/// A pixmap from straight-alpha RGBA8 bytes (tiny-skia stores premultiplied colour).
pub fn pixmap_from_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Pixmap, DrawError> {
    let mut canvas = new_canvas(width, height)?;
    if rgba.len() != canvas.data().len() {
        return Err(DrawError::ImageSize {
            width,
            height,
            actual: rgba.len(),
        });
    }
    for (dst, src) in canvas
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(rgba.as_chunks::<4>().0)
    {
        let a = src[3] as u32;
        let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        dst.copy_from_slice(&[pm(src[0]), pm(src[1]), pm(src[2]), src[3]]);
    }
    Ok(canvas)
}

/// The transform that draws a whole page at `px_per_unit` pixels per user-space unit, in display
/// orientation (`/Rotate` applied, the crop box's top-left corner at pixel (0, 0)): the layout
/// of a page rendered by `pl-render`.
pub fn page_transform(page: &PageGeometry, px_per_unit: f64) -> Affine {
    Affine::scale(px_per_unit) * page.page_to_display()
}

fn new_canvas(width: u32, height: u32) -> Result<Pixmap, DrawError> {
    if width == 0 || height == 0 || width > MAX_CANVAS_SIDE || height > MAX_CANVAS_SIDE {
        return Err(DrawError::CanvasSize { width, height });
    }
    Pixmap::new(width, height).ok_or(DrawError::CanvasSize { width, height })
}

fn color(rgb: Rgb, opacity: f64) -> tiny_skia::Color {
    let c = rgb.clamped();
    tiny_skia::Color::from_rgba(c.r, c.g, c.b, clamp_opacity(opacity) as f32)
        .unwrap_or(tiny_skia::Color::BLACK)
}

fn sk_blend(blend: BlendMode) -> tiny_skia::BlendMode {
    match blend {
        BlendMode::Normal => tiny_skia::BlendMode::SourceOver,
        BlendMode::Multiply => tiny_skia::BlendMode::Multiply,
    }
}

fn sk_transform(t: Affine) -> Transform {
    let c = t.as_coeffs();
    Transform::from_row(
        c[0] as f32,
        c[1] as f32,
        c[2] as f32,
        c[3] as f32,
        c[4] as f32,
        c[5] as f32,
    )
}

/// Converts a path, applying `transform` in f64 first so large page coordinates lose nothing.
fn sk_path(path: &BezPath, transform: Affine) -> Option<Path> {
    let mut b = PathBuilder::new();
    for el in path.elements() {
        match transform * *el {
            PathEl::MoveTo(p) => b.move_to(p.x as f32, p.y as f32),
            PathEl::LineTo(p) => b.line_to(p.x as f32, p.y as f32),
            PathEl::QuadTo(p1, p) => b.quad_to(p1.x as f32, p1.y as f32, p.x as f32, p.y as f32),
            PathEl::CurveTo(p1, p2, p) => b.cubic_to(
                p1.x as f32,
                p1.y as f32,
                p2.x as f32,
                p2.y as f32,
                p.x as f32,
                p.y as f32,
            ),
            PathEl::ClosePath => b.close(),
        }
    }
    b.finish()
}

fn solid_paint(rgb: Rgb, opacity: f64, blend: BlendMode) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(color(rgb, opacity)),
        blend_mode: sk_blend(blend),
        anti_alias: true,
        ..Paint::default()
    }
}

fn sk_stroke(stroke: &Stroke) -> tiny_skia::Stroke {
    tiny_skia::Stroke {
        width: stroke.width.max(0.0) as f32,
        miter_limit: stroke.miter_limit.max(1.0) as f32,
        line_cap: match stroke.cap {
            LineCap::Butt => tiny_skia::LineCap::Butt,
            LineCap::Round => tiny_skia::LineCap::Round,
            LineCap::Square => tiny_skia::LineCap::Square,
        },
        line_join: match stroke.join {
            LineJoin::Miter => tiny_skia::LineJoin::Miter,
            LineJoin::Round => tiny_skia::LineJoin::Round,
            LineJoin::Bevel => tiny_skia::LineJoin::Bevel,
        },
        dash: stroke.effective_dash().and_then(|(array, phase)| {
            tiny_skia::StrokeDash::new(array.iter().map(|v| *v as f32).collect(), phase as f32)
        }),
    }
}

fn draw_ops(ops: &[DrawOp], transform: Affine, canvas: &mut Pixmap) -> Result<(), DrawError> {
    for op in ops {
        match op {
            DrawOp::Path(p) => draw_path(p, transform, canvas),
            DrawOp::Text(t) => draw_text(t, transform, canvas)?,
            DrawOp::Image(i) => draw_image(i, transform, canvas),
            DrawOp::Group(g) => draw_group(g, transform, canvas)?,
        }
    }
    Ok(())
}

fn draw_path(op: &PathOp, transform: Affine, canvas: &mut Pixmap) {
    let opacity = clamp_opacity(op.opacity);
    if opacity <= 0.0 {
        return;
    }
    if let Some(fill) = &op.fill {
        // Device-space geometry: the fill needs no further transform.
        if let Some(path) = sk_path(&op.path, transform) {
            let rule = match fill.rule {
                FillRule::NonZero => SkFillRule::Winding,
                FillRule::EvenOdd => SkFillRule::EvenOdd,
            };
            let paint = solid_paint(fill.color, opacity, op.blend);
            canvas.fill_path(&path, &paint, rule, Transform::identity(), None);
        }
    }
    if let Some(stroke) = &op.stroke {
        // The stroke is built in page space and transformed, so widths and dashes scale with
        // the view. The translation is folded into the path to keep f32 coordinates small.
        let c = transform.as_coeffs();
        let linear = Affine::new([c[0], c[1], c[2], c[3], 0.0, 0.0]);
        let det = linear.determinant();
        if det.abs() < 1e-18 {
            return;
        }
        let shift = linear.inverse() * kurbo::Vec2::new(c[4], c[5]).to_point();
        let local = Affine::translate(shift.to_vec2());
        if let Some(path) = sk_path(&op.path, local) {
            let paint = solid_paint(stroke.color, opacity, op.blend);
            canvas.stroke_path(
                &path,
                &paint,
                &sk_stroke(stroke),
                sk_transform(linear),
                None,
            );
        }
    }
}

fn draw_text(run: &TextRun, transform: Affine, canvas: &mut Pixmap) -> Result<(), DrawError> {
    let opacity = clamp_opacity(run.opacity);
    let outline = text_outline(&run.text, run.style, run.size, run.origin)?;
    if opacity <= 0.0 {
        return Ok(());
    }
    if let Some(path) = sk_path(&outline, transform) {
        let paint = solid_paint(run.color, opacity, run.blend);
        canvas.fill_path(
            &path,
            &paint,
            SkFillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    Ok(())
}

fn image_pixmap(image: &RgbaImage) -> Option<Pixmap> {
    let size = IntSize::from_wh(image.width(), image.height())?;
    let mut data = image.pixels().to_vec();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        for c in &mut px[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(data, size)
}

fn draw_image(op: &ImageOp, transform: Affine, canvas: &mut Pixmap) {
    let opacity = clamp_opacity(op.opacity);
    let rect = op.rect.abs();
    if opacity <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let Some(source) = image_pixmap(&op.image) else {
        return;
    };
    let (w, h) = (source.width() as f64, source.height() as f64);
    // Image pixels (y-down, top row first) -> page: the top row lies along rect.y1.
    let image_to_page = Affine::new([
        rect.width() / w,
        0.0,
        0.0,
        -rect.height() / h,
        rect.x0,
        rect.y1,
    ]);
    let image_to_device = transform * image_to_page;
    // An enlarged image is drawn with hard pixel edges, as PDF viewers draw an image XObject
    // that has no /Interpolate (the emitter sets none); a reduced one is smoothed.
    let quality = if image_to_device.determinant().abs() >= 1.0 {
        FilterQuality::Nearest
    } else {
        FilterQuality::Bilinear
    };
    let paint = Paint {
        shader: Pattern::new(
            source.as_ref(),
            SpreadMode::Pad,
            quality,
            opacity as f32,
            Transform::identity(),
        ),
        blend_mode: sk_blend(op.blend),
        anti_alias: true,
        ..Paint::default()
    };
    if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
        canvas.fill_rect(r, &paint, sk_transform(image_to_device), None);
    }
}

fn draw_group(group: &Group, transform: Affine, canvas: &mut Pixmap) -> Result<(), DrawError> {
    let Some(clip) = group.clip else {
        return draw_group_unclipped(group, transform, canvas);
    };
    // The group is painted on a copy of the canvas (so blends inside it still see what is
    // beneath) and the copy is taken over only where the clip covers.
    let inner = transform * group.transform;
    let (w, h) = (canvas.width(), canvas.height());
    let shape = tiny_skia::Rect::from_ltrb(
        clip.x0 as f32,
        clip.y0 as f32,
        clip.x1 as f32,
        clip.y1 as f32,
    )
    .map(tiny_skia::PathBuilder::from_rect);
    let (Some(shape), Some(mut mask)) = (shape, tiny_skia::Mask::new(w, h)) else {
        return Ok(());
    };
    mask.fill_path(
        &shape,
        tiny_skia::FillRule::Winding,
        true,
        sk_transform(inner),
    );
    let mut painted = canvas.clone();
    draw_group_unclipped(group, transform, &mut painted)?;
    let cover = mask.data();
    let from = painted.data();
    let to = canvas.data_mut();
    for (i, m) in cover.iter().copied().enumerate() {
        if m == 0 {
            continue;
        }
        let at = i * 4;
        if m == 255 {
            to[at..at + 4].copy_from_slice(&from[at..at + 4]);
            continue;
        }
        let m = u32::from(m);
        for c in 0..4 {
            let mixed = u32::from(from[at + c]) * m + u32::from(to[at + c]) * (255 - m);
            to[at + c] = ((mixed + 127) / 255) as u8;
        }
    }
    Ok(())
}

fn draw_group_unclipped(
    group: &Group,
    transform: Affine,
    canvas: &mut Pixmap,
) -> Result<(), DrawError> {
    let inner = transform * group.transform;
    if !group.is_layer() {
        return draw_ops(&group.ops, inner, canvas);
    }
    let opacity = clamp_opacity(group.opacity);
    if opacity <= 0.0 {
        return Ok(());
    }
    // The layer only needs to cover the part of the group that lands on the canvas.
    let Some(bounds) = ops_bounds(&group.ops) else {
        return Ok(());
    };
    let device = inner.transform_rect_bbox(bounds).inflate(2.0, 2.0);
    let visible = device.intersect(Rect::new(
        0.0,
        0.0,
        canvas.width() as f64,
        canvas.height() as f64,
    ));
    if visible.width() <= 0.0 || visible.height() <= 0.0 {
        return Ok(());
    }
    let x0 = visible.x0.floor();
    let y0 = visible.y0.floor();
    let w = (visible.x1.ceil() - x0) as u32;
    let h = (visible.y1.ceil() - y0) as u32;
    let Some(mut layer) = Pixmap::new(w, h) else {
        return Ok(());
    };
    draw_ops(
        &group.ops,
        Affine::translate((-x0, -y0)) * inner,
        &mut layer,
    )?;
    canvas.draw_pixmap(
        x0 as i32,
        y0 as i32,
        layer.as_ref(),
        &PixmapPaint {
            opacity: opacity as f32,
            blend_mode: sk_blend(group.blend),
            quality: FilterQuality::Nearest,
        },
        Transform::identity(),
        None,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::{polyline_path, rect_path};
    use crate::style::Fill;
    use kurbo::Point;
    use std::sync::Arc;

    fn px(canvas: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let p = canvas.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    fn fill_op(rect: Rect, color: Rgb) -> PathOp {
        PathOp::filled(rect_path(rect), Fill::new(color))
    }

    /// Page 20 x 10 units, y-up, drawn at 2 px per unit.
    fn page_ts() -> Affine {
        Affine::scale(2.0) * Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, 10.0])
    }

    #[test]
    fn a_clipped_group_paints_only_inside_its_clip() {
        // A red fill over the whole page, clipped to its lower-left quarter, on white.
        let mut list = DrawList::new();
        list.push(DrawOp::Path(fill_op(
            Rect::new(0.0, 0.0, 20.0, 10.0),
            Rgb::WHITE,
        )));
        let red = DrawOp::Path(fill_op(Rect::new(0.0, 0.0, 20.0, 10.0), Rgb::RED));
        list.push(DrawOp::Group(
            Group::new(vec![red.clone()]).with_clip(Rect::new(0.0, 0.0, 10.0, 5.0)),
        ));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        assert_eq!(px(&canvas, 5, 15), [255, 0, 0, 255], "inside the clip");
        assert_eq!(px(&canvas, 25, 15), [255, 255, 255, 255], "right of it");
        assert_eq!(px(&canvas, 5, 5), [255, 255, 255, 255], "above it");
        // The clip is in the group's own coordinates: it moves with the group's transform.
        let mut list = DrawList::new();
        list.push(DrawOp::Group(
            Group::new(vec![red])
                .with_transform(Affine::translate((10.0, 0.0)))
                .with_clip(Rect::new(0.0, 0.0, 5.0, 5.0)),
        ));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        assert_eq!(px(&canvas, 25, 15), [255, 0, 0, 255]);
        assert_eq!(px(&canvas, 5, 15)[3], 0, "nothing where the group is not");
        assert_eq!(px(&canvas, 35, 15)[3], 0, "nothing past the clip");
        // A blend inside a clipped group still acts on what is beneath it.
        let mut list = DrawList::new();
        list.push(DrawOp::Path(fill_op(
            Rect::new(0.0, 0.0, 20.0, 10.0),
            Rgb::new(1.0, 1.0, 0.0),
        )));
        let cyan = fill_op(Rect::new(0.0, 0.0, 20.0, 10.0), Rgb::new(0.0, 1.0, 1.0))
            .with_blend(BlendMode::Multiply);
        list.push(DrawOp::Group(
            Group::new(vec![DrawOp::Path(cyan)]).with_clip(Rect::new(0.0, 0.0, 10.0, 10.0)),
        ));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        assert_eq!(px(&canvas, 5, 10), [0, 255, 0, 255], "yellow times cyan");
        assert_eq!(px(&canvas, 30, 10), [255, 255, 0, 255]);
    }

    #[test]
    fn a_fill_lands_where_the_transform_puts_it() {
        let mut list = DrawList::new();
        // Lower-left quarter of the page.
        list.push(DrawOp::Path(fill_op(
            Rect::new(0.0, 0.0, 10.0, 5.0),
            Rgb::RED,
        )));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        assert_eq!(
            px(&canvas, 5, 15),
            [255, 0, 0, 255],
            "bottom-left in y-down pixels"
        );
        assert_eq!(px(&canvas, 5, 5), [0, 0, 0, 0]);
        assert_eq!(px(&canvas, 25, 15), [0, 0, 0, 0]);
        // Pixel-aligned edges are exact.
        assert_eq!(px(&canvas, 19, 10), [255, 0, 0, 255]);
        assert_eq!(px(&canvas, 20, 10), [0, 0, 0, 0]);
        assert_eq!(px(&canvas, 19, 9), [0, 0, 0, 0]);
    }

    #[test]
    fn opacity_blends_over_the_backdrop() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            fill_op(Rect::new(0.0, 0.0, 20.0, 10.0), Rgb::BLACK).with_opacity(0.5),
        ));
        let canvas = render_new(&list, page_ts(), 40, 20, Some(Rgb::WHITE)).unwrap();
        let p = px(&canvas, 10, 10);
        assert!((p[0] as i32 - 128).abs() <= 1 && p[3] == 255, "{p:?}");
    }

    #[test]
    fn multiply_keeps_dark_ink_dark_and_tints_paper() {
        // Backdrop: white paper with a black bar across the middle.
        let mut backdrop = DrawList::new();
        backdrop.push(DrawOp::Path(fill_op(
            Rect::new(0.0, 4.0, 20.0, 6.0),
            Rgb::BLACK,
        )));
        let mut canvas = render_new(&backdrop, page_ts(), 40, 20, Some(Rgb::WHITE)).unwrap();

        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            fill_op(Rect::new(0.0, 0.0, 10.0, 10.0), Rgb::YELLOW).with_blend(BlendMode::Multiply),
        ));
        render(&list, page_ts(), &mut canvas).unwrap();
        assert_eq!(
            px(&canvas, 5, 2),
            [255, 255, 0, 255],
            "paper takes the highlight colour"
        );
        assert_eq!(
            px(&canvas, 5, 10),
            [0, 0, 0, 255],
            "ink under the highlight stays black"
        );
        assert_eq!(
            px(&canvas, 30, 2),
            [255, 255, 255, 255],
            "outside is untouched"
        );

        // The same mark drawn Normal would have covered the ink.
        let mut normal = render_new(&backdrop, page_ts(), 40, 20, Some(Rgb::WHITE)).unwrap();
        let mut list = DrawList::new();
        list.push(DrawOp::Path(fill_op(
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Rgb::YELLOW,
        )));
        render(&list, page_ts(), &mut normal).unwrap();
        assert_eq!(px(&normal, 5, 10), [255, 255, 0, 255]);
    }

    #[test]
    fn multiply_with_opacity_and_a_coloured_backdrop() {
        let grey = Rgb::from_u8(200, 200, 200);
        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            fill_op(Rect::new(0.0, 0.0, 20.0, 10.0), Rgb::from_u8(255, 128, 0))
                .with_opacity(0.5)
                .with_blend(BlendMode::Multiply),
        ));
        let canvas = render_new(&list, page_ts(), 40, 20, Some(grey)).unwrap();
        // B(cb, cs) = cb * cs; result = cb + alpha * (B - cb).
        let expect = |cb: f64, cs: f64| (cb + 0.5 * (cb * cs - cb)) * 255.0;
        let p = px(&canvas, 20, 10);
        let b = 200.0 / 255.0;
        assert!((p[0] as f64 - expect(b, 1.0)).abs() <= 1.5, "{p:?}");
        assert!(
            (p[1] as f64 - expect(b, 128.0 / 255.0)).abs() <= 1.5,
            "{p:?}"
        );
        assert!((p[2] as f64 - expect(b, 0.0)).abs() <= 1.5, "{p:?}");
    }

    #[test]
    fn multiply_over_nothing_paints_as_normal() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(
            fill_op(Rect::new(0.0, 0.0, 20.0, 10.0), Rgb::YELLOW).with_blend(BlendMode::Multiply),
        ));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        assert_eq!(px(&canvas, 20, 10), [255, 255, 0, 255]);
    }

    #[test]
    fn a_group_is_composited_as_one_object() {
        // Two overlapping black squares at 50%: separately they darken the overlap twice;
        // as a group the overlap is the same grey as the rest.
        let a = fill_op(Rect::new(0.0, 0.0, 12.0, 10.0), Rgb::BLACK);
        let b = fill_op(Rect::new(8.0, 0.0, 20.0, 10.0), Rgb::BLACK);

        let mut separate = DrawList::new();
        separate.push(DrawOp::Path(a.clone().with_opacity(0.5)));
        separate.push(DrawOp::Path(b.clone().with_opacity(0.5)));
        let canvas = render_new(&separate, page_ts(), 40, 20, Some(Rgb::WHITE)).unwrap();
        assert!((px(&canvas, 4, 10)[0] as i32 - 128).abs() <= 1);
        assert!((px(&canvas, 20, 10)[0] as i32 - 64).abs() <= 1);

        let mut grouped = DrawList::new();
        grouped.push_composite(
            vec![DrawOp::Path(a), DrawOp::Path(b)],
            0.5,
            BlendMode::Normal,
        );
        let canvas = render_new(&grouped, page_ts(), 40, 20, Some(Rgb::WHITE)).unwrap();
        for x in [4, 20, 36] {
            assert!((px(&canvas, x, 10)[0] as i32 - 128).abs() <= 1, "x={x}");
        }
    }

    #[test]
    fn a_multiply_group_multiplies_as_a_whole() {
        let mut backdrop = DrawList::new();
        backdrop.push(DrawOp::Path(fill_op(
            Rect::new(0.0, 4.0, 20.0, 6.0),
            Rgb::BLACK,
        )));
        let mut canvas = render_new(&backdrop, page_ts(), 40, 20, Some(Rgb::WHITE)).unwrap();
        let mut list = DrawList::new();
        list.push_composite(
            vec![
                DrawOp::Path(fill_op(Rect::new(0.0, 0.0, 12.0, 10.0), Rgb::YELLOW)),
                DrawOp::Path(fill_op(Rect::new(8.0, 0.0, 20.0, 10.0), Rgb::YELLOW)),
            ],
            1.0,
            BlendMode::Multiply,
        );
        render(&list, page_ts(), &mut canvas).unwrap();
        assert_eq!(px(&canvas, 20, 2), [255, 255, 0, 255]);
        assert_eq!(px(&canvas, 20, 10), [0, 0, 0, 255]);
    }

    #[test]
    fn group_transform_moves_and_turns_children() {
        let mut list = DrawList::new();
        // A 4 x 2 bar at the origin, turned a quarter turn anticlockwise and moved to (10, 2):
        // it then covers x 8..10, y 2..6 in page space.
        list.push(DrawOp::Group(
            Group::new(vec![DrawOp::Path(fill_op(
                Rect::new(0.0, 0.0, 4.0, 2.0),
                Rgb::BLUE,
            ))])
            .with_transform(
                Affine::translate((10.0, 2.0)) * Affine::rotate(std::f64::consts::FRAC_PI_2),
            ),
        ));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        // Page (9, 4) -> pixel (18, 12).
        assert_eq!(px(&canvas, 18, 12), [0, 0, 255, 255]);
        assert_eq!(px(&canvas, 22, 12), [0, 0, 0, 0]);
        assert_eq!(px(&canvas, 18, 18), [0, 0, 0, 0]);
    }

    #[test]
    fn stroke_width_scales_with_the_view() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::stroked(
            polyline_path(&[Point::new(0.0, 5.0), Point::new(20.0, 5.0)]),
            Stroke::new(2.0, Rgb::BLACK),
        )));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        // 2 units wide at 2 px per unit: rows 8..12 are covered.
        let column: Vec<u8> = (0..20).map(|y| px(&canvas, 20, y)[3]).collect();
        assert_eq!(
            column.iter().filter(|a| **a == 255).count(),
            4,
            "{column:?}"
        );
        assert_eq!(column[8..12], [255; 4]);
    }

    #[test]
    fn dashes_leave_gaps() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(PathOp::stroked(
            polyline_path(&[Point::new(0.0, 5.0), Point::new(20.0, 5.0)]),
            Stroke::new(2.0, Rgb::BLACK).with_dash(crate::Dash::new([4.0, 2.0], 0.0)),
        )));
        let canvas = render_new(&list, page_ts(), 40, 20, None).unwrap();
        let row: Vec<bool> = (0..40).map(|x| px(&canvas, x, 10)[3] == 255).collect();
        // 4 units on (8 px), 2 off (4 px), repeating from the start of the line.
        let expected: Vec<bool> = (0..40).map(|x| x % 12 < 8).collect();
        assert_eq!(row, expected);
    }

    #[test]
    fn an_image_is_upright_with_its_top_row_at_the_top() {
        // 2 x 2: red, green on top; blue, half-transparent white below.
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 128,
        ];
        let image = Arc::new(RgbaImage::new(2, 2, pixels).unwrap());
        let mut list = DrawList::new();
        list.push(DrawOp::Image(ImageOp::new(
            image,
            Rect::new(0.0, 0.0, 20.0, 10.0),
        )));
        let canvas = render_new(&list, page_ts(), 40, 20, Some(Rgb::BLACK)).unwrap();
        assert_eq!(px(&canvas, 3, 2), [255, 0, 0, 255], "top-left");
        assert_eq!(px(&canvas, 36, 2), [0, 255, 0, 255], "top-right");
        assert_eq!(px(&canvas, 3, 17), [0, 0, 255, 255], "bottom-left");
        let p = px(&canvas, 36, 17);
        assert!(
            (p[0] as i32 - 128).abs() <= 2,
            "half-transparent white over black: {p:?}"
        );
    }

    #[test]
    fn text_is_drawn_upright_on_its_baseline() {
        let mut list = DrawList::new();
        list.push(DrawOp::Text(
            TextRun::new(
                "H",
                Point::new(2.0, 2.0),
                8.0,
                crate::FontStyle::Bold,
                Rgb::BLACK,
            )
            .unwrap(),
        ));
        let ts = Affine::scale(4.0) * Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, 10.0]);
        let canvas = render_new(&list, ts, 80, 40, None).unwrap();
        let ink: Vec<(u32, u32)> = (0..40)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .filter(|(x, y)| px(&canvas, *x, *y)[3] > 128)
            .collect();
        assert!(!ink.is_empty());
        let top = ink.iter().map(|p| p.1).min().unwrap();
        let bottom = ink.iter().map(|p| p.1).max().unwrap();
        // Baseline at page y = 2 -> pixel row 32; cap height about 0.72 * 8 units = 23 px.
        assert!((bottom as i32 - 31).abs() <= 1, "bottom {bottom}");
        assert!((top as i32 - 9).abs() <= 2, "top {top}");
        // An H has ink at both stems and none between them above the crossbar.
        let row = top + 2;
        let xs: Vec<u32> = ink.iter().filter(|p| p.1 == row).map(|p| p.0).collect();
        let (lo, hi) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
        assert!(!xs.contains(&((lo + hi) / 2)), "the top of an H is open");
    }

    #[test]
    fn invalid_input_draws_nothing() {
        let mut list = DrawList::new();
        list.push(DrawOp::Path(fill_op(
            Rect::new(0.0, 0.0, 20.0, 10.0),
            Rgb::RED,
        )));
        list.push(DrawOp::Text(TextRun {
            text: "\u{4e2d}".into(),
            origin: Point::ZERO,
            size: 5.0,
            style: crate::FontStyle::Regular,
            color: Rgb::BLACK,
            opacity: 1.0,
            blend: BlendMode::Normal,
        }));
        let mut canvas = Pixmap::new(40, 20).unwrap();
        assert!(matches!(
            render(&list, page_ts(), &mut canvas),
            Err(DrawError::NotWinAnsi { .. })
        ));
        assert!(
            canvas.data().iter().all(|b| *b == 0),
            "the valid op was not drawn either"
        );
        assert_eq!(
            render(&DrawList::new(), Affine::scale(f64::NAN), &mut canvas),
            Err(DrawError::InvalidNumber)
        );
        assert!(matches!(
            render_new(&list, page_ts(), 0, 10, None),
            Err(DrawError::CanvasSize { .. })
        ));
        assert!(matches!(
            render_new(&list, page_ts(), MAX_CANVAS_SIDE + 1, 10, None),
            Err(DrawError::CanvasSize { .. })
        ));
    }

    #[test]
    fn render_over_rgba_round_trips_the_backdrop_layout() {
        let backdrop: Vec<u8> = std::iter::repeat_n([200u8, 200, 200, 255], 40 * 20)
            .flatten()
            .collect();
        let mut list = DrawList::new();
        list.push(crate::shapes::highlight_op(
            &[crate::shapes::rect_quad(Rect::new(0.0, 0.0, 10.0, 10.0))],
            Rgb::YELLOW,
            1.0,
        ));
        let out = render_over_rgba(&list, page_ts(), &backdrop, 40, 20).unwrap();
        assert_eq!(out.len(), backdrop.len());
        assert_eq!(&out[..4], &[200, 200, 0, 255]);
        let last = out.len() - 4;
        assert_eq!(&out[last..], &[200, 200, 200, 255]);
        assert!(matches!(
            render_over_rgba(&list, page_ts(), &backdrop[1..], 40, 20),
            Err(DrawError::ImageSize { .. })
        ));
    }

    #[test]
    fn far_away_coordinates_keep_their_precision() {
        // A hairline-thin gap between two fills, 14,000 units from the origin, viewed at 8x.
        let x = 14_000.0;
        let mut list = DrawList::new();
        list.push(DrawOp::Path(fill_op(
            Rect::new(x, x, x + 1.0, x + 2.0),
            Rgb::BLACK,
        )));
        list.push(DrawOp::Path(PathOp::stroked(
            polyline_path(&[Point::new(x + 2.0, x), Point::new(x + 2.0, x + 2.0)]),
            Stroke::new(0.5, Rgb::BLACK),
        )));
        let ts = Affine::scale(8.0) * Affine::new([1.0, 0.0, 0.0, -1.0, -x, x + 2.0]);
        let canvas = render_new(&list, ts, 32, 16, None).unwrap();
        let row: Vec<u8> = (0..32).map(|px_x| px(&canvas, px_x, 8)[3]).collect();
        assert_eq!(row[..8], [255; 8], "{row:?}");
        assert_eq!(row[8..14], [0; 6], "{row:?}");
        assert_eq!(row[14..18], [255; 4], "{row:?}");
        assert_eq!(row[18..], [0; 14], "{row:?}");
    }
}
