//! The page canvas: one view's pages, drawn from cached tiles, and the input that moves it.

use egui::{
    pos2, Color32, CursorIcon, Id, Key, Mesh, MouseWheelUnit, PointerButton, Pos2, Role, Sense,
    Shape, Stroke, StrokeKind, TextureId, Ui, WidgetInfo,
};
use std::collections::HashSet;
use std::sync::Arc;

use pl_doc::pl_geom::{Affine, Point, Rect, Size, TileGrid, Vec2};
use pl_doc::pl_render::{PageText, Quad, Scheduler, TileKey};
use pl_doc::{char_at, select_between, select_line_at, select_word_at, Document};

use crate::app::{AppState, PendingText, RequestSignature, ViewId};
use crate::queries::Queries;
use crate::textures::THUMBNAIL_STREAM;
use crate::theme::Palette;

/// A page's text if it can be used right now: it is cached, and the page's real geometry is
/// known (text boxes are in the page's own coordinates). Otherwise whatever is missing is
/// asked for through the scheduler and `None` is returned; the canvas tries again on a later
/// frame. This never waits for the backend.
fn text_now(
    doc: &Document,
    queries: &mut Queries,
    scheduler: &Scheduler,
    page: u32,
) -> Option<Arc<PageText>> {
    let render = doc.render_id();
    if !doc.geometry_is_exact(page) {
        queries.want_info(scheduler, render, page);
    }
    let text = doc.cached_text(page);
    if text.is_none() {
        queries.want_text(scheduler, render, page);
    }
    text.filter(|_| doc.geometry_is_exact(page))
}

/// Logical pixels scrolled per wheel line.
pub const WHEEL_LINE_PX: f32 = 60.0;
/// Zoom factor per wheel line with Ctrl held.
pub const WHEEL_ZOOM_STEP: f64 = 1.2;
const SCROLLBAR_THICKNESS: f32 = 8.0;

fn to_pos(origin: Pos2, p: Point) -> Pos2 {
    pos2(origin.x + p.x as f32, origin.y + p.y as f32)
}

fn to_rect(origin: Pos2, r: Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        to_pos(origin, Point::new(r.x0, r.y0)),
        to_pos(origin, Point::new(r.x1, r.y1)),
    )
}

/// Paints a texture into `rect`, turned clockwise by `quarter_turns` quarter turns (the view
/// rotation: tiles are always rendered in the file's own orientation).
pub fn paint_texture(
    painter: &egui::Painter,
    texture: TextureId,
    rect: egui::Rect,
    quarter_turns: u8,
) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let uv = [
        pos2(0.0, 0.0),
        pos2(1.0, 0.0),
        pos2(1.0, 1.0),
        pos2(0.0, 1.0),
    ];
    let k = (quarter_turns % 4) as usize;
    let mut mesh = Mesh::with_texture(texture);
    for (i, corner) in corners.iter().enumerate() {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *corner,
            uv: uv[(i + 4 - k) % 4],
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(Shape::mesh(mesh));
}

fn paint_quads(
    painter: &egui::Painter,
    origin: Pos2,
    to_view: Affine,
    quads: &[Quad],
    fill: Color32,
    stroke: Stroke,
) {
    for quad in quads {
        let points: Vec<Pos2> = quad
            .iter()
            .map(|p| to_pos(origin, to_view * Point::new(p[0], p[1])))
            .collect();
        painter.add(Shape::convex_polygon(points, fill, stroke));
    }
}

/// A thin scroll bar along one edge of the canvas. Returns the new scroll offset when the user
/// drags it.
#[allow(clippy::too_many_arguments)]
fn scrollbar(
    ui: &Ui,
    track: egui::Rect,
    vertical: bool,
    content: f64,
    viewport: f64,
    scroll: f64,
    id: Id,
    name: &str,
    color: Color32,
) -> Option<f64> {
    let range = content - viewport;
    if range <= 0.5 {
        return None;
    }
    let track_len = if vertical {
        track.height()
    } else {
        track.width()
    } as f64;
    let thumb_len = (track_len * viewport / content).clamp(28.0_f64.min(track_len), track_len);
    let travel = (track_len - thumb_len).max(1.0);
    let at = travel * (scroll / range).clamp(0.0, 1.0);
    let thumb = if vertical {
        egui::Rect::from_min_size(
            pos2(track.left(), track.top() + at as f32),
            egui::vec2(track.width(), thumb_len as f32),
        )
    } else {
        egui::Rect::from_min_size(
            pos2(track.left() + at as f32, track.top()),
            egui::vec2(thumb_len as f32, track.height()),
        )
    };
    let response = ui.interact(thumb, id, Sense::drag());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::ScrollBar, enabled, name));
    let active = response.hovered() || response.dragged();
    let fill = if active {
        color.gamma_multiply(1.6)
    } else {
        color
    };
    ui.painter()
        .rect_filled(thumb.shrink(1.5), SCROLLBAR_THICKNESS / 2.0, fill);
    if response.dragged() {
        let delta = response.drag_delta();
        let moved = if vertical { delta.y } else { delta.x } as f64;
        return Some(scroll + moved * range / travel);
    }
    None
}

/// Shows one view's canvas in the space the `ui` has left, and handles its input.
pub fn show(app: &mut AppState, ui: &mut Ui, id: ViewId) {
    let ctx = ui.ctx().clone();
    let palette = Palette::of(ui.visuals());
    let rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    app.shown_this_frame.push(id);
    if let Some(dv) = app.views.get_mut(&id) {
        dv.canvas_id = Some(response.id);
    }
    let keyboard_free = app.keyboard_free(&ctx);
    if response.has_focus() {
        // With the focus on the page the arrow keys nudge the selection or scroll; they must
        // not also walk the focus off to a neighbouring control.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..Default::default()
                },
            );
        });
    }

    // The pointer over the page with a markup tool: the annotations will be needed.
    if ui.rect_contains_pointer(rect) && app.markup.tool.kind().is_some() {
        if let Some(doc) = app.views.get(&id).map(|v| v.doc) {
            app.want_annots(doc);
        }
    }
    let Some(dv) = app.views.get_mut(&id) else {
        return;
    };
    let enabled = ui.is_enabled();
    let name = format!("Page view, {}", dv.title);
    response.widget_info(|| WidgetInfo::labeled(Role::Document, enabled, &name));

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.canvas);
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    dv.rect = rect;
    dv.view
        .set_viewport(Size::new(rect.width() as f64, rect.height() as f64));
    dv.view.set_dpr(ctx.pixels_per_point() as f64);

    let origin = rect.min;
    let local = |p: Pos2| Point::new((p.x - origin.x) as f64, (p.y - origin.y) as f64);
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let pointer_inside = ui.rect_contains_pointer(rect);

    // ---- wheel: scroll, or zoom about the cursor with Ctrl ------------------------------------
    if pointer_inside {
        let wheel: Vec<(MouseWheelUnit, egui::Vec2, egui::Modifiers)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } => Some((*unit, *delta, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (unit, delta, modifiers) in wheel {
            let (lines, pixels) = match unit {
                MouseWheelUnit::Line => (delta, delta * WHEEL_LINE_PX),
                MouseWheelUnit::Point => (delta / WHEEL_LINE_PX, delta),
                MouseWheelUnit::Page => (delta * 4.0, delta * rect.height()),
            };
            if modifiers.command || modifiers.ctrl {
                let cursor = pointer.map(local).unwrap_or_else(|| {
                    Point::new(rect.width() as f64 / 2.0, rect.height() as f64 / 2.0)
                });
                let zoom = dv.view.zoom() * WHEEL_ZOOM_STEP.powf(lines.y as f64);
                dv.view.zoom_about(cursor, zoom);
            } else if modifiers.shift {
                dv.view
                    .scroll_by(Vec2::new(-(pixels.x + pixels.y) as f64, 0.0));
            } else {
                dv.view
                    .scroll_by(Vec2::new(-pixels.x as f64, -pixels.y as f64));
            }
        }
    }

    // ---- drag: pan with the middle button, Space or the Pan tool; otherwise the active tool ----
    // Space pans while it is held, unless a widget has the keyboard (Space presses a focused
    // button) or a tool gesture already has the pointer: that gesture keeps it until the
    // button is released, so its release is never lost.
    let space = ui.input(|i| i.key_down(Key::Space)) && keyboard_free && !dv.pointer_down;
    let pan_tool = app.markup.tool == crate::markup::Tool::Pan && !app.markup.snapshot_pick;
    let panning = response.dragged_by(PointerButton::Middle)
        || ((space || pan_tool) && response.dragged_by(PointerButton::Primary));
    let mut pressed = response.is_pointer_button_down_on() || response.clicked();
    if panning {
        let delta = response.drag_delta();
        dv.view
            .scroll_by(Vec2::new(-delta.x as f64, -delta.y as f64));
        ctx.set_cursor_icon(CursorIcon::Grabbing);
    } else if (space || pan_tool) && response.hovered() {
        ctx.set_cursor_icon(CursorIcon::Grab);
    }

    // ---- links: the Link tool's drag, following a link, the target under the pointer ---------
    let link_tool = !space && !panning && crate::links::canvas_links(app, ui, id, &response, rect);

    // ---- the markup tool: pointer events go to it, and it says whether text selection (the
    // Select tool's other job) may have this frame's pointer.
    let text_mode = !link_tool && pointer_phase(app, ui, id, &response, rect, space);
    let Some(dv) = app.views.get_mut(&id) else {
        return;
    };
    let show_handles = app.markup.tool == crate::markup::Tool::Select;

    // Pages on screen whose real geometry is known: only on these do page coordinates mean
    // what the file means by them (see `Document::pages`).
    let mut exact_pages: HashSet<u32> = HashSet::new();
    if let Ok(doc) = app.session.document(dv.doc) {
        let render_doc = doc.render_id();
        // While the document is being saved its display belongs to the save: what is cached
        // is shown, and nothing new is asked for until the save has been taken in.
        let saving = doc.is_saving();
        for page in dv.view.visible_pages() {
            if doc.geometry_is_exact(page) {
                exact_pages.insert(page);
            } else if !saving {
                app.queries.want_info(&app.scheduler, render_doc, page);
            }
        }
        // The tools that mark up the page's own text need it for the pages in view.
        if app.markup.tool.marks_text() && !saving {
            for page in dv.view.visible_pages() {
                text_now(doc, &mut app.queries, &app.scheduler, page);
            }
        }
        // Nothing below calls the backend: text comes from the document's cache, and what is
        // not there yet is requested through the scheduler (`text_now`).
        if !space && text_mode && !saving {
            if response.drag_started_by(PointerButton::Primary) {
                let start = ui.input(|i| i.pointer.press_origin()).or(pointer);
                dv.drag_anchor = start
                    .and_then(|p| dv.view.hit_test_nearest(local(p)))
                    .filter(|hit| doc.geometry_is_exact(hit.page))
                    .map(|hit| (hit.page, hit.point));
                dv.selection = None;
                dv.pending_text = None;
            }
            if response.dragged_by(PointerButton::Primary) {
                if let (Some((page, a)), Some(pos)) = (dv.drag_anchor, pointer) {
                    let b = dv
                        .view
                        .view_transform(page)
                        .zip(dv.view.page_geometry(page))
                        .map(|(view, geometry)| view.view_to_page(geometry) * local(pos));
                    let text = text_now(doc, &mut app.queries, &app.scheduler, page);
                    if let (Some(b), Some(text)) = (b, text) {
                        dv.selection = select_between(&text, a, b);
                    }
                }
            }
            if response.drag_stopped() {
                dv.drag_anchor = None;
            }
            let click_hit = response
                .interact_pointer_pos()
                .and_then(|p| dv.view.hit_test(local(p)))
                .filter(|hit| doc.geometry_is_exact(hit.page));
            let multi_click = if response.triple_clicked() {
                Some(true)
            } else if response.double_clicked() {
                Some(false)
            } else {
                None
            };
            if let Some(line) = multi_click {
                dv.selection = None;
                dv.pending_text = None;
                if let Some(hit) = click_hit {
                    let action = if line {
                        PendingText::Line {
                            page: hit.page,
                            point: hit.point,
                        }
                    } else {
                        PendingText::Word {
                            page: hit.page,
                            point: hit.point,
                        }
                    };
                    match text_now(doc, &mut app.queries, &app.scheduler, hit.page) {
                        Some(text) if line => dv.selection = select_line_at(&text, hit.point),
                        Some(text) => dv.selection = select_word_at(&text, hit.point),
                        // Applied when the text arrives.
                        None => dv.pending_text = Some(action),
                    }
                }
            } else if response.clicked() {
                dv.selection = None;
                dv.pending_text = None;
            }
        }

        // ---- hover: where the cursor is on the page ------------------------------------------
        dv.cursor = if pointer_inside {
            pointer.and_then(|p| dv.view.hit_test_nearest(local(p)))
        } else {
            None
        };
        if let Some(hit) = dv
            .cursor
            .filter(|h| h.inside && !space && !panning && !pan_tool && !saving)
        {
            let over_text = text_now(doc, &mut app.queries, &app.scheduler, hit.page)
                .is_some_and(|text| char_at(&text, hit.point).is_some());
            let tool_cursor = dv.tool_cursor.and_then(crate::markup::cursor_icon);
            if response.hovered() {
                if app.markup.snapshot_pick || !show_handles {
                    ctx.set_cursor_icon(CursorIcon::Crosshair);
                } else if let Some(icon) = tool_cursor {
                    ctx.set_cursor_icon(icon);
                } else if over_text {
                    ctx.set_cursor_icon(CursorIcon::Text);
                }
            }
        }

        // ---- pages and their tiles -----------------------------------------------------------
        let quarter_turns = dv.view.rotation().quarter_turns();
        for page in dv.view.visible_pages() {
            let Some(page_rect) = dv.view.page_rect_in_view(page) else {
                continue;
            };
            let page_rect = to_rect(origin, page_rect);
            painter.rect_filled(page_rect, 0.0, palette.paper);
            // The low-resolution stand-in until tiles arrive.
            if !saving {
                app.textures.request_thumb(&app.scheduler, render_doc, page);
            }
            if let Some(texture) = app.textures.thumb(render_doc, page) {
                paint_texture(&painter, texture, page_rect, quarter_turns);
            }
        }

        let tiles = dv.view.visible_tiles();
        let mut signature = RequestSignature {
            render_doc: render_doc.0,
            pages: Vec::with_capacity(tiles.len()),
        };
        for page_tiles in &tiles {
            // A page's tiles are keyed by that page's own content revision, so a change on
            // one page leaves every other page's tiles valid.
            let revision = doc.page_revision(page_tiles.page);
            signature
                .pages
                .push((page_tiles.page, revision, page_tiles.range));
            let Some(page_rect) = dv.view.page_rect_in_view(page_tiles.page) else {
                continue;
            };
            let page_painter = painter.with_clip_rect(to_rect(origin, page_rect).expand(0.5));
            let to_view = page_tiles
                .screen_view
                .page_to_view(&page_tiles.screen_geometry);
            let mut ready = Vec::new();
            for tile in page_tiles.range.iter() {
                let Some(tile_rect) = page_tiles.tile_screen_rect(tile) else {
                    continue;
                };
                let tile_rect = to_rect(origin, tile_rect);
                let key = TileKey::new(render_doc, page_tiles.page, revision, tile, 0);
                if let Some(texture) = app.textures.tile(&ctx, &app.cache, &key, app.frame) {
                    ready.push((texture, tile_rect));
                    continue;
                }
                // Not rendered yet: stretch the nearest coarser tile over this tile's area.
                let Some((coarse_key, bitmap)) = app.cache.find_ancestor(&key, 5) else {
                    continue;
                };
                let coarse_rect = TileGrid::new(&page_tiles.render_geometry, coarse_key.level)
                    .tile_page_rect(&page_tiles.render_geometry, coarse_key.tile())
                    .map(|r| to_rect(origin, to_view.transform_rect_bbox(r)));
                let texture = app
                    .textures
                    .tile_from(&ctx, &coarse_key, &bitmap, app.frame);
                if let (Some(texture), Some(coarse_rect)) = (texture, coarse_rect) {
                    paint_texture(
                        &page_painter.with_clip_rect(tile_rect),
                        texture,
                        coarse_rect,
                        quarter_turns,
                    );
                }
            }
            for (texture, tile_rect) in ready {
                paint_texture(&page_painter, texture, tile_rect, quarter_turns);
            }
        }
        // ---- markups: what pl-doc leaves to the interface, the preview, the selection ---------
        for page_tiles in &tiles {
            if !exact_pages.contains(&page_tiles.page) {
                continue;
            }
            let items = app.markup.overlay(doc, page_tiles.page);
            let marks = app.markup.selection_marks(doc, &dv.view);
            let mut page_canvas = crate::overlay::PageCanvas {
                ctx: &ctx,
                painter: &painter,
                origin,
                palette,
                cache: &app.cache,
                overlays: &mut app.overlays,
                frame: app.frame,
                show_handles,
                marks: &marks,
            };
            crate::overlay::paint_page(&mut page_canvas, doc, dv, page_tiles, &items);
        }
        if !saving && dv.requested.as_ref() != Some(&signature) {
            let generation = app.scheduler.advance(dv.stream);
            debug_assert_ne!(dv.stream, THUMBNAIL_STREAM);
            for page_tiles in &tiles {
                let revision = doc.page_revision(page_tiles.page);
                let request = page_tiles.view_request(render_doc, revision, 0, 1);
                app.scheduler.queue_view(dv.stream, generation, &request);
            }
            dv.requested = Some(signature);
        }
    }

    // ---- overlays: page edges, search hits, the selection ---------------------------------
    for page in dv.view.visible_pages() {
        let (Some(page_rect), Some(view), Some(geometry)) = (
            dv.view.page_rect_in_view(page),
            dv.view.view_transform(page),
            dv.view.page_geometry(page),
        ) else {
            continue;
        };
        let to_view = view.page_to_view(geometry);
        painter.rect_stroke(
            to_rect(origin, page_rect),
            0.0,
            Stroke::new(1.0, palette.page_edge),
            StrokeKind::Outside,
        );
        // Hit and selection quads are in page coordinates: drawn once those are the page's.
        if !exact_pages.contains(&page) {
            continue;
        }
        if let Some(search) = &dv.search {
            let current = search.current().map(|h| (h.page, h.char_start));
            for hit in search.hits_on_page(page) {
                let (fill, stroke) = if current == Some((hit.page, hit.char_start)) {
                    (
                        palette.hit_current,
                        Stroke::new(1.0, palette.hit_current_edge),
                    )
                } else {
                    (palette.hit, Stroke::NONE)
                };
                paint_quads(&painter, origin, to_view, &hit.quads, fill, stroke);
            }
        }
        if let Some(selection) = dv.selection.as_ref().filter(|s| s.page == page) {
            paint_quads(
                &painter,
                origin,
                to_view,
                &selection.quads,
                palette.selection,
                Stroke::NONE,
            );
        }
    }

    // ---- the region being picked for "Copy snapshot of region" --------------------------------
    if let Some((from, to)) = dv.region {
        let region = egui::Rect::from_two_pos(from, to).intersect(rect);
        painter.rect_filled(region, 0.0, palette.selection.gamma_multiply(0.5));
        painter.rect_stroke(
            region,
            0.0,
            Stroke::new(1.0, palette.accent),
            StrokeKind::Inside,
        );
    }

    // ---- the read-out of the measurement in progress, beside the pointer -------------------
    if let (Some(readout), Some(pos)) = (dv.readout.as_ref(), pointer.filter(|_| pointer_inside)) {
        crate::measure::paint_readout(&painter, pos, readout, &palette, ui.visuals());
    }

    // ---- scroll bars -------------------------------------------------------------------------
    let content = dv.view.layout().content_size();
    let viewport = dv.view.viewport();
    let scroll = dv.view.scroll();
    let t = SCROLLBAR_THICKNESS;
    let vertical_track = egui::Rect::from_min_max(
        pos2(rect.right() - t, rect.top()),
        pos2(rect.right(), rect.bottom() - t),
    );
    let horizontal_track = egui::Rect::from_min_max(
        pos2(rect.left(), rect.bottom() - t),
        pos2(rect.right() - t, rect.bottom()),
    );
    let bar_id = response.id;
    if let Some(y) = scrollbar(
        ui,
        vertical_track,
        true,
        content.height,
        viewport.height,
        scroll.y,
        bar_id.with("vertical"),
        "Vertical scroll bar",
        palette.scroll_thumb,
    ) {
        dv.view.set_scroll(Vec2::new(scroll.x, y));
    }
    if let Some(x) = scrollbar(
        ui,
        horizontal_track,
        false,
        content.width,
        viewport.width,
        scroll.x,
        bar_id.with("horizontal"),
        "Horizontal scroll bar",
        palette.scroll_thumb,
    ) {
        dv.view.set_scroll(Vec2::new(x, dv.view.scroll().y));
    }

    pressed |= response.dragged();
    if pressed {
        app.active = Some(id);
    }
    // The text being typed into a text box of this view.
    crate::textedit::show_editor(app, &ctx, id, rect);
}

/// Gives this frame's pointer events on a canvas to the markup tool, in the order they arrived
/// (a test, or a slow frame, can carry a whole press, drag and release at once). Returns
/// whether text selection may use the pointer this frame: only with the Select tool, and only
/// when the press did not land on a markup, a handle or empty paper.
fn pointer_phase(
    app: &mut AppState,
    ui: &Ui,
    id: ViewId,
    response: &egui::Response,
    rect: egui::Rect,
    space: bool,
) -> bool {
    use crate::markup::Tool;
    use pl_tools::ToolInput;

    let tool = app.markup.tool;
    app.sync_tool(id);
    let origin = rect.min;
    let local = |p: Pos2| Point::new((p.x - origin.x) as f64, (p.y - origin.y) as f64);
    let events: Vec<egui::Event> = ui.input(|i| {
        i.events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    egui::Event::PointerMoved(_)
                        | egui::Event::PointerButton {
                            button: PointerButton::Primary,
                            ..
                        }
                )
            })
            .cloned()
            .collect()
    });
    let modifiers_now = ui.input(|i| i.modifiers);
    // The press reached this widget (and not a menu, a scroll bar or another pane over it).
    let owns_press = response.is_pointer_button_down_on()
        || response.clicked()
        || response.dragged()
        || response.drag_started()
        || response.drag_stopped();
    let hovering = response.hovered() || response.contains_pointer();

    // ---- picking a region to copy as an image takes the pointer from every tool -----------
    if app.markup.snapshot_pick {
        let mut finished = None;
        if let Some(dv) = app.views.get_mut(&id) {
            for event in &events {
                match event {
                    egui::Event::PointerButton {
                        pos, pressed: true, ..
                    } if owns_press && rect.contains(*pos) => dv.region = Some((*pos, *pos)),
                    egui::Event::PointerMoved(pos) => {
                        if let Some((_, to)) = dv.region.as_mut() {
                            *to = *pos;
                        }
                    }
                    egui::Event::PointerButton {
                        pos,
                        pressed: false,
                        ..
                    } => {
                        if let Some((from, _)) = dv.region.take() {
                            finished = Some(egui::Rect::from_two_pos(from, *pos));
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(region) = finished {
            app.markup.snapshot_pick = false;
            app.active = Some(id);
            let canvas = Palette::of(ui.visuals()).canvas;
            app.copy_snapshot(Some(region), canvas);
        }
        return false;
    }

    if tool.kind().is_none() || space {
        // Pan, or Space held: the canvas itself moves; no tool and no text selection.
        return !matches!(tool, Tool::Pan | Tool::Link);
    }

    let mut markup_used = false;
    let mut last_pos: Option<Pos2> = None;
    for event in events {
        if let egui::Event::PointerMoved(pos) | egui::Event::PointerButton { pos, .. } = &event {
            last_pos = Some(*pos);
        }
        match event {
            egui::Event::PointerMoved(pos) => {
                let (down, text_press) = match app.views.get(&id) {
                    Some(dv) => (dv.pointer_down, dv.text_press),
                    None => return false,
                };
                if down || (hovering && !text_press) {
                    if let Some(pointer) = app.pointer_at(id, local(pos), modifiers_now) {
                        app.tool_event(id, ToolInput::PointerMove(pointer));
                    }
                }
                markup_used |= down;
            }
            egui::Event::PointerButton {
                pos,
                pressed: true,
                modifiers,
                ..
            } => {
                if !owns_press || !rect.contains(pos) {
                    continue;
                }
                let Some(doc_id) = app.views.get(&id).map(|v| v.doc) else {
                    return false;
                };
                if app.text_edit_active() {
                    // A click on the page finishes the text being typed, and does no more.
                    app.commit_text_edit();
                    markup_used = true;
                    if let Some(dv) = app.views.get_mut(&id) {
                        dv.press_markup = true;
                        dv.pointer_down = false;
                        dv.text_press = false;
                    }
                    continue;
                }
                if app.doc_is_saving(doc_id) {
                    // Read-only until the save has finished.
                    continue;
                }
                // The first press on a document reads its annotations.
                app.ensure_annots(doc_id);
                let Some(pointer) = app.pointer_at(id, local(pos), modifiers) else {
                    continue;
                };
                let mut to_text = false;
                if tool == Tool::Select && !app.press_hits_markup(id, &pointer) {
                    // Over text the Select tool selects text; elsewhere it drags a box.
                    to_text = app
                        .session
                        .document(doc_id)
                        .ok()
                        .filter(|doc| doc.geometry_is_exact(pointer.page as u32))
                        .and_then(|doc| doc.cached_text(pointer.page as u32))
                        .is_some_and(|text| char_at(&text, pointer.point).is_some());
                }
                if to_text {
                    if let Ok(doc) = app.session.document_mut(doc_id) {
                        doc.clear_selection();
                    }
                }
                if let Some(dv) = app.views.get_mut(&id) {
                    dv.text_press = to_text;
                    dv.press_markup = !to_text;
                    dv.pointer_down = !to_text;
                    if !to_text {
                        dv.selection = None;
                        dv.pending_text = None;
                    }
                }
                if !to_text {
                    markup_used = true;
                    app.tool_event(id, ToolInput::PointerDown(pointer));
                }
            }
            egui::Event::PointerButton {
                pos,
                pressed: false,
                modifiers,
                ..
            } => {
                let down = app.views.get(&id).is_some_and(|dv| dv.pointer_down);
                if down {
                    markup_used = true;
                    if let Some(pointer) = app.pointer_at(id, local(pos), modifiers) {
                        app.tool_event(id, ToolInput::PointerUp(pointer));
                    }
                }
                if let Some(dv) = app.views.get_mut(&id) {
                    dv.pointer_down = false;
                    dv.text_press = false;
                }
            }
            _ => {}
        }
    }
    let press_markup = app.views.get(&id).is_some_and(|dv| dv.press_markup);
    // A third quick click counts as a triple-click for egui even when the first of the three
    // was somewhere else (the previous corner of a polygon); for a tool it is a double-click.
    if (response.double_clicked() || response.triple_clicked()) && press_markup {
        markup_used = true;
        if let Some(pos) = last_pos.or_else(|| response.interact_pointer_pos()) {
            if let Some(pointer) = app.pointer_at(id, local(pos), modifiers_now) {
                app.tool_event(id, ToolInput::DoubleClick(pointer));
            }
        }
    }
    let down = app.views.get(&id).is_some_and(|dv| dv.pointer_down);
    tool == Tool::Select && !markup_used && !down && !press_markup_click(response, press_markup)
}

/// A click (or the end of a drag) that belonged to the markup tool must not also be read as a
/// text click.
fn press_markup_click(response: &egui::Response, press_markup: bool) -> bool {
    press_markup && (response.clicked() || response.double_clicked() || response.triple_clicked())
}
