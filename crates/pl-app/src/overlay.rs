//! What the canvas paints over the page tiles: the markups pl-doc leaves to the user interface
//! (new and edited ones), the preview of the gesture in progress, the selection's outline and
//! handles and the snap marker.
//!
//! Most markups go through [`crate::paint`] to the egui painter. Those the painter cannot draw
//! exactly (Multiply above all: the highlighter) are composited on the CPU with pl-draw's
//! tiny-skia rasteriser against the page tile under them, and the result is uploaded as a
//! texture that is painted over that tile ([`OverlayTiles`]). Text under a highlight therefore
//! stays as dark on screen as it will be in the saved file.
//!
//! [`compose_view`] builds the "Copy snapshot" image the same way, wholly on the CPU from
//! cached tiles, so it needs neither the GPU nor the render backend.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use egui::{Color32, ColorImage, Painter, Pos2, Stroke, StrokeKind, TextureHandle, TextureId};
use pl_doc::pl_annot::AnnotId;
use pl_doc::pl_draw::tiny_skia;
use pl_doc::pl_draw::{raster, DrawList};
use pl_doc::pl_geom::{Affine, Point, Rect, Vec2};
use pl_doc::pl_render::{TileCache, TileKey};
use pl_doc::{Document, OverlayItem, PageTiles};

use crate::app::{DocView, ViewId};
use crate::canvas::paint_texture;
use crate::paint::{needs_raster, paint_list};
use crate::theme::Palette;

/// The side of a selection handle, in logical pixels.
pub const HANDLE_SIZE: f32 = 7.0;
/// The side of the snap marker, in logical pixels.
pub const SNAP_MARK_SIZE: f32 = 11.0;

/// Tile-sized textures holding a page tile with the rastered markups composited onto it.
#[derive(Default)]
pub struct OverlayTiles {
    tiles: HashMap<(ViewId, TileKey), (u64, TextureHandle, u64)>,
}

impl OverlayTiles {
    /// The composite for a tile, made again whenever `signature` (what was composited) changes.
    fn get_or_make(
        &mut self,
        ctx: &egui::Context,
        view: ViewId,
        key: &TileKey,
        signature: u64,
        frame: u64,
        make: impl FnOnce() -> Option<ColorImage>,
    ) -> Option<TextureId> {
        if let Some((have, texture, used)) = self.tiles.get_mut(&(view, *key)) {
            if *have == signature {
                *used = frame;
                return Some(texture.id());
            }
        }
        let image = make()?;
        let name = format!("overlay-{}-{}-{}-{}", view.0, key.page, key.x, key.y);
        let texture = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
        let id = texture.id();
        self.tiles.insert((view, *key), (signature, texture, frame));
        Some(id)
    }

    /// Frees the composites that were not painted in `frame`.
    pub fn end_frame(&mut self, frame: u64) {
        self.tiles.retain(|_, (_, _, used)| *used >= frame);
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

fn to_pos(origin: Pos2, p: Point) -> Pos2 {
    Pos2::new(origin.x + p.x as f32, origin.y + p.y as f32)
}

fn to_rect(origin: Pos2, r: Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        to_pos(origin, Point::new(r.x0, r.y0)),
        to_pos(origin, Point::new(r.x1, r.y1)),
    )
}

/// Everything [`paint_page`] needs besides the page itself.
pub struct PageCanvas<'a> {
    pub ctx: &'a egui::Context,
    pub painter: &'a Painter,
    /// Where the view's top-left corner is on screen.
    pub origin: Pos2,
    pub palette: Palette,
    pub cache: &'a TileCache,
    pub overlays: &'a mut OverlayTiles,
    pub frame: u64,
    /// Whether the selection's outline and handles are shown (the Select tool is active).
    pub show_handles: bool,
    /// Which markups are selected on which page, with the handles.
    pub(crate) marks: &'a crate::markup::SelectionMarks,
}

/// Paints one page's markups, preview and selection over its tiles.
pub fn paint_page(
    canvas: &mut PageCanvas<'_>,
    doc: &Document,
    dv: &DocView,
    page_tiles: &PageTiles,
    items: &[OverlayItem],
) {
    let page = page_tiles.page;
    let Some(page_rect) = dv.view.page_rect_in_view(page) else {
        return;
    };
    let clip = to_rect(canvas.origin, page_rect).expand(0.5);
    let painter = canvas
        .painter
        .with_clip_rect(clip.intersect(canvas.painter.clip_rect()));
    let to_view = page_tiles
        .screen_view
        .page_to_view(&page_tiles.screen_geometry);
    let editing: Vec<AnnotId> = dv.tool.editing();
    let preview = dv
        .preview_page
        .filter(|p| *p == page && !dv.preview.is_empty())
        .map(|_| &dv.preview);

    // In stacking order, with the preview on top. `None` is the preview's id.
    let layers: Vec<Layer<'_>> = items
        .iter()
        .filter(|item| !editing.contains(&item.id))
        .map(|item| (Some(&item.id), &item.list))
        .chain(preview.map(|list| (None, list)))
        .map(|(id, list)| Layer {
            id,
            list,
            raster: needs_raster(list),
            bounds: list.bounds(),
        })
        .collect();
    let stack: Vec<(bool, Option<Rect>)> = layers.iter().map(|l| (l.raster, l.bounds)).collect();

    // Where a markup that needs the rasteriser (Multiply: the highlighter) touches a tile,
    // that tile is composited on the CPU: the page tile, then every markup up to the topmost
    // such one, in stacking order. So a highlight drawn over a filled rectangle multiplies
    // with the rectangle, as it will in the saved file, and what lies above the highlight is
    // still painted above it. Each tile's composite depends only on the markups that touch
    // that tile, so drawing a highlighter stroke recomposites the tiles under the stroke and
    // no others.
    let tiles: Vec<TilePlace> = if layers.iter().any(|l| l.raster) {
        page_tiles
            .range
            .iter()
            .filter_map(|tile| {
                let page_rect = page_tiles
                    .grid
                    .tile_page_rect(&page_tiles.render_geometry, tile)?;
                let pixels = page_tiles.grid.tile_pixel_rect(tile)?;
                let screen = to_rect(canvas.origin, page_tiles.tile_screen_rect(tile)?);
                let members = composite_members(&stack, page_rect);
                Some(TilePlace {
                    tile,
                    page_rect,
                    origin: Vec2::new(pixels.x as f64, pixels.y as f64),
                    screen,
                    members,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    if tiles.iter().any(|t| !t.members.is_empty()) {
        let revision = doc.page_revision(page);
        let quarter_turns = dv.view.rotation().quarter_turns();
        let scale = page_tiles.grid.scale();
        let page_to_pixels = raster::page_transform(&page_tiles.render_geometry, scale);
        for place in tiles.iter().filter(|t| !t.members.is_empty()) {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            doc.edit_revision().hash(&mut hasher);
            doc.render_id().0.hash(&mut hasher);
            for i in &place.members {
                match layers[*i].id {
                    Some(id) => id.as_str().hash(&mut hasher),
                    // The preview changes with every pointer move; only the tiles it
                    // touches are made again for it.
                    None => (u64::MAX, dv.preview_serial).hash(&mut hasher),
                }
            }
            let signature = hasher.finish();
            let key = TileKey::new(doc.render_id(), page, revision, place.tile, 0);
            let base = canvas.cache.get(&key);
            let members: Vec<&DrawList> = place.members.iter().map(|i| layers[*i].list).collect();
            let texture = base.as_ref().and_then(|bitmap| {
                canvas.overlays.get_or_make(
                    canvas.ctx,
                    dv.id,
                    &key,
                    signature,
                    canvas.frame,
                    || {
                        let transform = Affine::translate(-place.origin) * page_to_pixels;
                        let pixmap = compose_tile(
                            &bitmap.pixels,
                            bitmap.width,
                            bitmap.height,
                            &members,
                            transform,
                        )?;
                        Some(ColorImage::from_rgba_premultiplied(
                            [bitmap.width as usize, bitmap.height as usize],
                            pixmap.data(),
                        ))
                    },
                )
            });
            match texture {
                Some(texture) => paint_texture(&painter, texture, place.screen, quarter_turns),
                None => {
                    // The tile is not rendered yet: the painter's approximation, for now.
                    let tile_painter = painter.with_clip_rect(place.screen.intersect(clip));
                    for list in &members {
                        paint_list(&tile_painter, canvas.origin, to_view, list);
                    }
                }
            }
        }
    }

    // Everything else goes to the painter, in order. A markup that is part of a composite in
    // some tiles is painted in the others only.
    for (i, layer) in layers.iter().enumerate() {
        if layer.raster {
            continue;
        }
        if !tiles.iter().any(|t| t.members.contains(&i)) {
            paint_list(&painter, canvas.origin, to_view, layer.list);
            continue;
        }
        for place in &tiles {
            let here = layer.bounds.is_some_and(|b| touches(b, place.page_rect));
            if here && !place.members.contains(&i) {
                let tile_painter = painter.with_clip_rect(place.screen.intersect(clip));
                paint_list(&tile_painter, canvas.origin, to_view, layer.list);
            }
        }
    }

    // ---- the selection ------------------------------------------------------------------------
    let accent = canvas.palette.accent;
    let selection = doc.selection();
    let free = canvas.painter;
    if canvas.show_handles && !selection.is_empty() {
        // Only this page's selected markups are looked at, not the whole selection.
        let here = canvas
            .marks
            .on_page
            .get(&page)
            .map_or(&[][..], Vec::as_slice);
        for annotation in here.iter().filter_map(|id| doc.annotation(id)) {
            if annotation.page != page as usize || editing.contains(&annotation.id) {
                continue;
            }
            let outline = to_rect(
                canvas.origin,
                to_view.transform_rect_bbox(annotation.bounds()),
            )
            .expand(3.0);
            for shape in egui::Shape::dashed_line(
                &[
                    outline.left_top(),
                    outline.right_top(),
                    outline.right_bottom(),
                    outline.left_bottom(),
                    outline.left_top(),
                ],
                Stroke::new(1.0, accent),
                4.0,
                3.0,
            ) {
                free.add(shape);
            }
        }
        let on_this_page = canvas.marks.first_page == Some(page);
        if on_this_page && editing.is_empty() {
            // Several markups selected: the box around them, whose handles resize them
            // together.
            if selection.len() > 1 {
                if let Some(bounds) = canvas.marks.group {
                    let outline =
                        to_rect(canvas.origin, to_view.transform_rect_bbox(bounds)).expand(1.0);
                    free.rect_stroke(
                        outline,
                        0.0,
                        Stroke::new(1.0, accent.gamma_multiply(0.7)),
                        StrokeKind::Middle,
                    );
                }
            }
            for handle in &canvas.marks.handles {
                let at = to_pos(canvas.origin, to_view * handle.at);
                let square = egui::Rect::from_center_size(at, egui::Vec2::splat(HANDLE_SIZE));
                free.rect(
                    square,
                    1.0,
                    Color32::WHITE,
                    Stroke::new(1.2, accent),
                    StrokeKind::Middle,
                );
            }
            // A selected measurement's own grips: the offset of its dimension line, the
            // place of its value, the corners of its cutouts. Drawn as small diamonds.
            if let [only] = selection {
                if let Some(a) = doc.annotation(only).filter(|a| a.page == page as usize) {
                    for handle in pl_tools::measure_handles(a) {
                        let at = to_pos(canvas.origin, to_view * handle.at);
                        let h = HANDLE_SIZE * 0.6;
                        free.add(egui::Shape::convex_polygon(
                            vec![
                                Pos2::new(at.x, at.y - h),
                                Pos2::new(at.x + h, at.y),
                                Pos2::new(at.x, at.y + h),
                                Pos2::new(at.x - h, at.y),
                            ],
                            Color32::WHITE,
                            Stroke::new(1.2, accent),
                        ));
                    }
                }
            }
        }
    }

    // ---- the snap marker ----------------------------------------------------------------------
    if let Some((_, point)) = dv.snap_mark.filter(|(p, _)| *p == page) {
        // A glyph of its own for each kind of point: a square for an endpoint, a triangle
        // for a midpoint, a cross for an intersection...
        let at = to_pos(canvas.origin, to_view * point);
        crate::measure::paint_snap_marker(free, at, dv.snap_kind, accent);
    }
}

/// One thing painted over a page: a markup, or (`id` is `None`) the preview of the gesture in
/// progress.
struct Layer<'a> {
    id: Option<&'a AnnotId>,
    list: &'a DrawList,
    /// The egui painter cannot draw it exactly (see [`needs_raster`]).
    raster: bool,
    bounds: Option<Rect>,
}

/// One visible tile of a page and what is composited onto it.
struct TilePlace {
    tile: pl_doc::pl_geom::TileId,
    /// The tile's area in page space.
    page_rect: Rect,
    /// The tile's top-left corner in the pixel grid of its level.
    origin: Vec2,
    screen: egui::Rect,
    /// Indices into the page's layers, ascending.
    members: Vec<usize>,
}

/// Which of a page's layers, given bottom to top as (needs the rasteriser, bounds), are
/// composited onto the tile covering `tile` (page space): none if no layer that needs the
/// rasteriser touches the tile; otherwise every layer touching the tile from the bottom of the
/// stack up to the topmost one that needs it. Layers above that one are left to the painter.
pub(crate) fn composite_members(stack: &[(bool, Option<Rect>)], tile: Rect) -> Vec<usize> {
    let here = |bounds: &Option<Rect>| bounds.is_some_and(|b| touches(b, tile));
    let Some(top) = stack
        .iter()
        .rposition(|(raster, bounds)| *raster && here(bounds))
    else {
        return Vec::new();
    };
    (0..=top).filter(|i| here(&stack[*i].1)).collect()
}

/// A page tile (straight RGBA8) with `lists` rendered onto it in order; `transform` takes
/// page space to the tile's pixels.
pub(crate) fn compose_tile(
    rgba: &[u8],
    width: u32,
    height: u32,
    lists: &[&DrawList],
    transform: Affine,
) -> Option<tiny_skia::Pixmap> {
    let mut pixmap = raster::pixmap_from_rgba(rgba, width, height).ok()?;
    for list in lists {
        raster::render(list, transform, &mut pixmap).ok()?;
    }
    Some(pixmap)
}

/// True when two rectangles share at least an edge (a zero-area overlap still counts: a
/// horizontal highlighter stroke has a flat bounding box only in theory, but be safe).
fn touches(a: Rect, b: Rect) -> bool {
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

/// Builds the image of what a view shows inside `region` (screen coordinates): the canvas, the
/// pages from their cached tiles and every markup the user interface paints, composited with
/// tiny-skia at device resolution. Returns width, height and straight RGBA8 pixels.
pub fn compose_view(
    dv: &DocView,
    doc: &Document,
    cache: &TileCache,
    region: egui::Rect,
    canvas_color: Color32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let dpr = dv.view.dpr().max(0.1);
    let width = ((region.width() as f64 * dpr).round() as u32).clamp(1, raster::MAX_CANVAS_SIDE);
    let height = ((region.height() as f64 * dpr).round() as u32).clamp(1, raster::MAX_CANVAS_SIDE);
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("the image is too large")?;
    pixmap.fill(tiny_skia::Color::from_rgba8(
        canvas_color.r(),
        canvas_color.g(),
        canvas_color.b(),
        255,
    ));
    // View space (logical pixels from the view's top-left corner) to image pixels.
    let offset = Vec2::new(
        (region.min.x - dv.rect.min.x) as f64,
        (region.min.y - dv.rect.min.y) as f64,
    );
    let view_to_image = Affine::scale(dpr) * Affine::translate(-offset);
    let skia = |a: Affine| {
        let c = a.as_coeffs();
        tiny_skia::Transform::from_row(
            c[0] as f32,
            c[1] as f32,
            c[2] as f32,
            c[3] as f32,
            c[4] as f32,
            c[5] as f32,
        )
    };
    let render = doc.render_id();
    let turns = (dv.view.rotation().quarter_turns() % 4) as usize;
    for page_tiles in dv.view.visible_tiles() {
        let page = page_tiles.page;
        if let Some(page_rect) = dv.view.page_rect_in_view(page) {
            let rect = view_to_image.transform_rect_bbox(page_rect);
            if let Some(rect) = tiny_skia::Rect::from_ltrb(
                rect.x0 as f32,
                rect.y0 as f32,
                rect.x1 as f32,
                rect.y1 as f32,
            ) {
                let mut paper = tiny_skia::Paint::default();
                paper.set_color(tiny_skia::Color::WHITE);
                pixmap.fill_rect(rect, &paper, tiny_skia::Transform::identity(), None);
            }
        }
        let revision = doc.page_revision(page);
        for tile in page_tiles.range.iter() {
            let key = TileKey::new(render, page, revision, tile, 0);
            let (Some(bitmap), Some(tile_view)) =
                (cache.get(&key), page_tiles.tile_screen_rect(tile))
            else {
                continue;
            };
            let Ok(source) = raster::pixmap_from_rgba(&bitmap.pixels, bitmap.width, bitmap.height)
            else {
                continue;
            };
            // The tile is in the file's orientation; the view may show it turned. Its
            // top-left corner lands on corner `turns` of its rectangle on screen, clockwise.
            let corners = [
                Point::new(tile_view.x0, tile_view.y0),
                Point::new(tile_view.x1, tile_view.y0),
                Point::new(tile_view.x1, tile_view.y1),
                Point::new(tile_view.x0, tile_view.y1),
            ];
            let o = corners[turns];
            let u = (corners[(turns + 1) % 4] - o) / bitmap.width.max(1) as f64;
            let v = (corners[(turns + 3) % 4] - o) / bitmap.height.max(1) as f64;
            let tile_to_view = Affine::new([u.x, u.y, v.x, v.y, o.x, o.y]);
            let paint = tiny_skia::PixmapPaint {
                quality: tiny_skia::FilterQuality::Bilinear,
                ..tiny_skia::PixmapPaint::default()
            };
            pixmap.draw_pixmap(
                0,
                0,
                source.as_ref(),
                &paint,
                skia(view_to_image * tile_to_view),
                None,
            );
        }
        if !doc.geometry_is_exact(page) {
            continue;
        }
        let to_view = page_tiles
            .screen_view
            .page_to_view(&page_tiles.screen_geometry);
        for item in doc.overlay(page) {
            raster::render(&item.list, view_to_image * to_view, &mut pixmap)
                .map_err(|e| e.to_string())?;
        }
    }
    // The image is opaque, so its premultiplied bytes are its straight bytes.
    Ok((width, height, pixmap.take()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_doc::pl_annot::{Annotation, BlendMode, Origin, PdfDate, Rgb};

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    /// A filled blue square over (10,10)-(40,40) and a yellow highlighter stroke across it.
    fn square_and_highlight() -> (DrawList, DrawList) {
        let mut square = Annotation::square(0, Rect::new(10.0, 10.0, 40.0, 40.0), &origin());
        square.style.stroke = None;
        square.style.fill = Some(Rgb::new(0.2, 0.4, 1.0));
        let mut stroke = Annotation::ink(
            0,
            vec![vec![Point::new(0.0, 25.0), Point::new(64.0, 25.0)]],
            &origin(),
        );
        stroke.style.stroke = Some(Rgb::new(1.0, 1.0, 0.0));
        stroke.style.width = 10.0;
        stroke.style.blend = BlendMode::Multiply;
        (square.draw_list(), stroke.draw_list())
    }

    fn pixel(pixmap: &tiny_skia::Pixmap, x: u32, y: u32) -> [u8; 3] {
        let c = pixmap.pixel(x, y).expect("inside");
        [c.red(), c.green(), c.blue()]
    }

    /// A 64 x 64 white tile whose pixels are page units, y down.
    fn white() -> Vec<u8> {
        vec![255; 64 * 64 * 4]
    }

    #[test]
    fn nothing_is_composited_where_no_multiply_markup_is() {
        let tile = Rect::new(0.0, 0.0, 64.0, 64.0);
        let far = Rect::new(200.0, 200.0, 240.0, 240.0);
        let near = Rect::new(10.0, 10.0, 40.0, 40.0);
        assert!(composite_members(&[], tile).is_empty());
        assert!(composite_members(&[(false, Some(near))], tile).is_empty());
        assert!(
            composite_members(&[(false, Some(near)), (true, Some(far))], tile).is_empty(),
            "a highlight elsewhere on the page leaves this tile alone"
        );
        assert!(composite_members(&[(true, None)], tile).is_empty());
    }

    #[test]
    fn a_tile_composites_everything_up_to_its_topmost_multiply_markup() {
        let tile = Rect::new(0.0, 0.0, 64.0, 64.0);
        let near = Rect::new(10.0, 10.0, 40.0, 40.0);
        let far = Rect::new(200.0, 200.0, 240.0, 240.0);
        // Bottom to top: a shape, a highlight, a shape elsewhere, a highlight elsewhere, a
        // shape on top.
        let stack = [
            (false, Some(near)),
            (true, Some(near)),
            (false, Some(far)),
            (true, Some(far)),
            (false, Some(near)),
        ];
        assert_eq!(composite_members(&stack, tile), [0, 1]);
        // The other tile composites its own two and neither of this tile's.
        assert_eq!(
            composite_members(&stack, Rect::new(192.0, 192.0, 256.0, 256.0)),
            [2, 3]
        );
        // A highlight at the bottom takes nothing with it.
        assert_eq!(
            composite_members(&[(true, Some(near)), (false, Some(near))], tile),
            [0]
        );
    }

    #[test]
    fn a_highlight_over_a_filled_square_multiplies_with_the_square() {
        let (square, highlight) = square_and_highlight();
        assert!(needs_raster(&highlight) && !needs_raster(&square));
        let stack = [(false, square.bounds()), (true, highlight.bounds())];
        let members = composite_members(&stack, Rect::new(0.0, 0.0, 64.0, 64.0));
        assert_eq!(
            members,
            [0, 1],
            "the square is composited under the highlight"
        );
        let lists = [&square, &highlight];
        let picked: Vec<&DrawList> = members.iter().map(|i| lists[*i]).collect();
        let tile = compose_tile(&white(), 64, 64, &picked, Affine::IDENTITY).unwrap();

        // On the square, under the stroke: blue times yellow, which has no blue left.
        let both = pixel(&tile, 25, 25);
        assert!(both[2] < 20, "multiplied with the square: {both:?}");
        assert!(both[0] > 30 && both[1] > 80, "{both:?}");
        // On the square, away from the stroke: the square's own blue.
        let plain = pixel(&tile, 25, 14);
        assert!(plain[2] > 240 && plain[0] < 70, "{plain:?}");
        // On bare paper under the stroke: yellow.
        let paper = pixel(&tile, 55, 25);
        assert!(
            paper[0] > 240 && paper[1] > 240 && paper[2] < 20,
            "{paper:?}"
        );

        // It is the picture a snapshot gets (everything rendered in stacking order).
        let mut reference = raster::pixmap_from_rgba(&white(), 64, 64).unwrap();
        for list in lists {
            raster::render(list, Affine::IDENTITY, &mut reference).unwrap();
        }
        assert_eq!(tile.data(), reference.data());

        // The other way up, the square covers the highlight.
        let stack = [(true, highlight.bounds()), (false, square.bounds())];
        let members = composite_members(&stack, Rect::new(0.0, 0.0, 64.0, 64.0));
        assert_eq!(
            members,
            [0],
            "the square above is painted over the composite"
        );
    }
}
