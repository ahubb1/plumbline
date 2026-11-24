//! [`ViewState`]: one view onto a document, with no UI types.
//!
//! # Spaces
//!
//! * **Page space** is pl-geom's: unrotated user space, y-up.
//! * **Document space** is the continuous layout: logical pixels, y-down, origin at the top-left
//!   of the laid-out content. Pages are stacked top to bottom with a gap between them and a
//!   margin around them, each centred horizontally ([`PageLayout`]).
//! * **View space** is the viewport: logical pixels, y-down, origin at its top-left corner.
//!   `view = document - scroll + origin`, where `origin` centres content that is smaller than
//!   the viewport.
//!
//! Page sizes scale with the zoom; the gap and margin do not.
//!
//! # View rotation
//!
//! The view can turn every page by a quarter-turn multiple without changing the file. The
//! renderer knows nothing about that: tiles are always rendered in the file's own orientation.
//! [`PageTiles`] therefore carries two descriptions of each visible page: the *screen* one
//! (rotated geometry, for placing things) and the *render* one (the file's geometry with an
//! equivalent view, for the scheduler and the tile cache). With a rotated view the painter must
//! turn each tile texture by [`ViewState::rotation`] when it draws it into
//! [`PageTiles::tile_screen_rect`].

use std::ops::Range;

use pl_geom::{
    clamp_zoom, fit_page_zoom, fit_width_zoom, visible_tiles, PageGeometry, Point, Rect, Rotation,
    Size, TileGrid, TileId, TileRange, Vec2, ViewTransform, LOGICAL_PX_PER_POINT,
};
use pl_render::{DocId, RenderOptions, ViewRequest};

/// Default gap between pages, in logical pixels.
pub const DEFAULT_PAGE_GAP: f64 = 12.0;
/// Default margin around the pages, in logical pixels.
pub const DEFAULT_PAGE_MARGIN: f64 = 12.0;

/// The zoom presets that zoom-in and zoom-out step through.
pub const ZOOM_STEPS: [f64; 19] = [
    0.05, 0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0,
    32.0, 64.0,
];

/// The next preset above (`up`) or below the given zoom.
pub fn zoom_step(zoom: f64, up: bool) -> f64 {
    let eps = zoom * 1e-9;
    let found = if up {
        ZOOM_STEPS.iter().copied().find(|z| *z > zoom + eps)
    } else {
        ZOOM_STEPS.iter().rev().copied().find(|z| *z < zoom - eps)
    };
    clamp_zoom(found.unwrap_or(zoom))
}

/// How the zoom follows the viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum FitMode {
    /// The zoom is whatever the user set.
    #[default]
    None,
    /// The current page's width fills the viewport.
    Width,
    /// The whole current page fits the viewport.
    Page,
}

/// The continuous vertical layout of a document's pages at one zoom, in document space.
#[derive(Clone, Debug, PartialEq)]
pub struct PageLayout {
    rects: Vec<Rect>,
    size: Size,
}

impl PageLayout {
    /// Lays the pages out top to bottom. `gap` and `margin` are logical pixels and do not scale
    /// with the zoom.
    pub fn new(pages: &[PageGeometry], zoom: f64, gap: f64, margin: f64) -> PageLayout {
        let scale = zoom * LOGICAL_PX_PER_POINT;
        let sizes: Vec<Size> = pages
            .iter()
            .map(|p| {
                let d = p.display_size_points();
                Size::new(d.width * scale, d.height * scale)
            })
            .collect();
        let widest = sizes.iter().map(|s| s.width).fold(0.0, f64::max);
        let width = widest + 2.0 * margin;
        let mut rects = Vec::with_capacity(sizes.len());
        let mut y = margin;
        for s in &sizes {
            let x = (width - s.width) / 2.0;
            rects.push(Rect::new(x, y, x + s.width, y + s.height));
            y += s.height + gap;
        }
        let height = if rects.is_empty() {
            2.0 * margin
        } else {
            y - gap + margin
        };
        PageLayout {
            rects,
            size: Size::new(width, height),
        }
    }

    pub fn page_count(&self) -> usize {
        self.rects.len()
    }

    /// The size of the whole laid-out content, margins included.
    pub fn content_size(&self) -> Size {
        self.size
    }

    /// Where a page sits in document space.
    pub fn page_rect(&self, page: usize) -> Option<Rect> {
        self.rects.get(page).copied()
    }

    /// The page at document height `y`, or the nearest one when `y` falls in a gap or margin.
    /// `None` only for an empty document.
    pub fn page_at_y(&self, y: f64) -> Option<usize> {
        if self.rects.is_empty() {
            return None;
        }
        let i = self.rects.partition_point(|r| r.y1 < y);
        if i >= self.rects.len() {
            return Some(self.rects.len() - 1);
        }
        if i > 0 && y < self.rects[i].y0 && y - self.rects[i - 1].y1 < self.rects[i].y0 - y {
            return Some(i - 1);
        }
        Some(i)
    }

    /// The pages that overlap the document-space band `y0..y1`.
    pub fn pages_in_band(&self, y0: f64, y1: f64) -> Range<usize> {
        let start = self.rects.partition_point(|r| r.y1 <= y0);
        let end = self.rects.partition_point(|r| r.y0 < y1);
        start..end.max(start)
    }
}

/// The result of a hit test: which page a view point is on, and where.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageHit {
    pub page: u32,
    /// The point in page space (unrotated user space, y-up).
    pub point: Point,
    /// Whether the point is on the page itself; `false` when it is in a gap or margin and the
    /// nearest page was used (the point then lies outside the page's crop box).
    pub inside: bool,
}

/// One visible page and the tiles it needs. See the module documentation for the two
/// descriptions it carries.
#[derive(Clone, Copy, Debug)]
pub struct PageTiles {
    pub page: u32,
    /// The page as shown (view rotation applied).
    pub screen_geometry: PageGeometry,
    /// Places the page on screen; `scroll` is snapped to whole device pixels.
    pub screen_view: ViewTransform,
    /// The page as the renderer knows it (the file's own rotation).
    pub render_geometry: PageGeometry,
    /// A view of `render_geometry` that shows exactly the visible part of the page.
    pub render_view: ViewTransform,
    /// The viewport that goes with `render_view`, in logical pixels.
    pub render_viewport: Size,
    /// The tile grid of `render_geometry` at the level this zoom needs.
    pub grid: TileGrid,
    /// The visible tiles in that grid.
    pub range: TileRange,
}

impl PageTiles {
    /// Where a tile lands in view space (logical pixels).
    pub fn tile_screen_rect(&self, tile: TileId) -> Option<Rect> {
        let page_rect = self.grid.tile_page_rect(&self.render_geometry, tile)?;
        Some(
            self.screen_view
                .page_to_view(&self.screen_geometry)
                .transform_rect_bbox(page_rect),
        )
    }

    /// The scheduler request for this page.
    pub fn view_request(
        &self,
        doc: DocId,
        revision: u64,
        layer_hash: u64,
        prefetch_ring: u32,
    ) -> ViewRequest {
        ViewRequest {
            doc,
            page: self.page,
            revision,
            layer_hash,
            geometry: self.render_geometry,
            view: self.render_view,
            viewport: self.render_viewport,
            prefetch_ring,
            options: RenderOptions::default(),
        }
    }
}

/// One view onto a document: zoom, scroll, view rotation and fit mode over a continuous
/// vertical page layout.
#[derive(Clone, Debug)]
pub struct ViewState {
    base: Vec<PageGeometry>,
    pages: Vec<PageGeometry>,
    rotation: Rotation,
    zoom: f64,
    dpr: f64,
    scroll: Vec2,
    viewport: Size,
    fit: FitMode,
    gap: f64,
    margin: f64,
    layout: PageLayout,
}

impl ViewState {
    /// A view at 100% showing the top of the first page.
    pub fn new(pages: &[PageGeometry], viewport: Size, dpr: f64) -> ViewState {
        let mut v = ViewState {
            base: pages.to_vec(),
            pages: pages.to_vec(),
            rotation: Rotation::R0,
            zoom: 1.0,
            dpr: sane_dpr(dpr),
            scroll: Vec2::ZERO,
            viewport: sane_size(viewport),
            fit: FitMode::None,
            gap: DEFAULT_PAGE_GAP,
            margin: DEFAULT_PAGE_MARGIN,
            layout: PageLayout::new(&[], 1.0, 0.0, 0.0),
        };
        v.relayout();
        v
    }

    // ---- plain state -----------------------------------------------------------------------

    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    /// A page's geometry as shown, with the view rotation applied.
    pub fn page_geometry(&self, page: u32) -> Option<&PageGeometry> {
        self.pages.get(page as usize)
    }

    /// A page's geometry as the file has it.
    pub fn file_geometry(&self, page: u32) -> Option<&PageGeometry> {
        self.base.get(page as usize)
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    pub fn dpr(&self) -> f64 {
        self.dpr
    }

    /// The scroll offset in document space (logical pixels).
    pub fn scroll(&self) -> Vec2 {
        self.scroll
    }

    pub fn viewport(&self) -> Size {
        self.viewport
    }

    pub fn fit(&self) -> FitMode {
        self.fit
    }

    /// The extra clockwise rotation applied to every page in this view.
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    pub fn layout(&self) -> &PageLayout {
        &self.layout
    }

    /// The largest scroll offset on each axis.
    pub fn max_scroll(&self) -> Vec2 {
        let c = self.layout.content_size();
        Vec2::new(
            (c.width - self.viewport.width).max(0.0),
            (c.height - self.viewport.height).max(0.0),
        )
    }

    /// Where document-space (0, 0) sits in view space when the scroll is zero: non-zero only on
    /// an axis where the content is smaller than the viewport and is therefore centred.
    pub fn origin(&self) -> Vec2 {
        let c = self.layout.content_size();
        Vec2::new(
            ((self.viewport.width - c.width) / 2.0).max(0.0),
            ((self.viewport.height - c.height) / 2.0).max(0.0),
        )
    }

    // ---- changing the view -----------------------------------------------------------------

    /// Replaces the page list (after a page was inserted, deleted or rotated in the file),
    /// keeping the current page where possible.
    pub fn set_pages(&mut self, pages: &[PageGeometry]) {
        let keep = self.current_page();
        self.base = pages.to_vec();
        self.relayout();
        self.refit(keep);
    }

    /// Sets the viewport size in logical pixels. In a fit mode the zoom follows.
    pub fn set_viewport(&mut self, viewport: Size) {
        let viewport = sane_size(viewport);
        if viewport == self.viewport {
            return;
        }
        let keep = self.current_page();
        self.viewport = viewport;
        self.refit(keep);
    }

    pub fn set_dpr(&mut self, dpr: f64) {
        self.dpr = sane_dpr(dpr);
    }

    /// Sets the gap between pages and the margin around them (logical pixels).
    pub fn set_spacing(&mut self, gap: f64, margin: f64) {
        let keep = self.current_page();
        self.gap = gap.max(0.0);
        self.margin = margin.max(0.0);
        self.relayout();
        self.refit(keep);
    }

    /// Scrolls to a document-space offset, clamped to the content.
    pub fn set_scroll(&mut self, scroll: Vec2) {
        let max = self.max_scroll();
        let clamp = |v: f64, hi: f64| if v.is_finite() { v.clamp(0.0, hi) } else { 0.0 };
        self.scroll = Vec2::new(clamp(scroll.x, max.x), clamp(scroll.y, max.y));
    }

    /// Scrolls by a delta in logical pixels (positive moves down / right through the document).
    pub fn scroll_by(&mut self, delta: Vec2) {
        self.set_scroll(self.scroll + delta);
    }

    /// Sets the zoom, keeping the centre of the viewport fixed. Leaves any fit mode.
    pub fn set_zoom(&mut self, zoom: f64) {
        let centre = Point::new(self.viewport.width / 2.0, self.viewport.height / 2.0);
        self.zoom_about(centre, zoom);
    }

    /// Zoom-to-cursor: sets the zoom so that the page point under `cursor` (view space) stays
    /// under it. Leaves any fit mode. The invariant holds unless the scroll limits intervene
    /// (the content is smaller than the viewport, or the cursor is past the content's edge).
    pub fn zoom_about(&mut self, cursor: Point, zoom: f64) {
        self.fit = FitMode::None;
        self.rezoom(clamp_zoom(zoom), cursor);
    }

    /// Steps to the next zoom preset, about the viewport centre.
    pub fn zoom_in(&mut self) {
        self.set_zoom(zoom_step(self.zoom, true));
    }

    /// Steps to the previous zoom preset, about the viewport centre.
    pub fn zoom_out(&mut self) {
        self.set_zoom(zoom_step(self.zoom, false));
    }

    /// Sets the fit mode. `Width` and `Page` take effect at once and follow later viewport
    /// changes; `None` keeps the present zoom.
    pub fn set_fit(&mut self, fit: FitMode) {
        self.fit = fit;
        let keep = self.current_page();
        self.refit(keep);
    }

    /// Turns the view by a further clockwise rotation, keeping the current page.
    pub fn rotate_view(&mut self, extra: Rotation) {
        self.set_rotation(self.rotation.then(extra));
    }

    /// Sets the view rotation outright.
    pub fn set_rotation(&mut self, rotation: Rotation) {
        if rotation == self.rotation {
            return;
        }
        let keep = self.current_page();
        self.rotation = rotation;
        self.relayout();
        self.refit(keep);
        if self.fit == FitMode::None {
            if let Some(p) = keep {
                self.go_to_page(p);
            }
        }
    }

    /// Scrolls so that the top of a page (and its margin) is at the top of the viewport. In
    /// `FitMode::Page` and `FitMode::Width` the zoom is refitted to that page first, as pages
    /// may differ in size. The page index is clamped to the document.
    pub fn go_to_page(&mut self, page: u32) {
        if self.pages.is_empty() {
            return;
        }
        let page = page.min(self.page_count() - 1);
        if self.fit != FitMode::None {
            let zoom = self.fit_zoom(page);
            self.apply_zoom(zoom);
        }
        let rect = self.layout.rects[page as usize];
        self.set_scroll(Vec2::new(self.scroll.x, rect.y0 - self.margin));
        // Horizontally: a page that fits the viewport is centred in it (pages narrower than the
        // widest one would otherwise sit off to the side); a wider page keeps the present
        // scroll unless that leaves it wholly out of view.
        if rect.width() + 2.0 * self.margin <= self.viewport.width {
            let x = rect.center().x - self.viewport.width / 2.0;
            self.set_scroll(Vec2::new(x, self.scroll.y));
        } else if rect.x1 <= self.scroll.x || rect.x0 >= self.scroll.x + self.viewport.width {
            self.set_scroll(Vec2::new(rect.x0 - self.margin, self.scroll.y));
        }
    }

    pub fn next_page(&mut self) {
        if let Some(p) = self.current_page() {
            self.go_to_page(p.saturating_add(1));
        }
    }

    pub fn previous_page(&mut self) {
        if let Some(p) = self.current_page() {
            self.go_to_page(p.saturating_sub(1));
        }
    }

    /// Scrolls so that a page-space point lands at `anchor` in view space (as far as the scroll
    /// limits allow). Used for bookmark and link destinations.
    pub fn scroll_point_to(&mut self, page: u32, point: Point, anchor: Point) {
        let Some(view) = self.view_transform(page) else {
            return;
        };
        let now = view.page_to_view(&self.pages[page as usize]) * point;
        self.set_scroll(self.scroll + (now - anchor));
    }

    /// Goes to a destination: a page and, optionally, a point on it that is brought to the top
    /// of the viewport (a missing coordinate keeps the page's top or left edge).
    pub fn go_to_destination(&mut self, page: u32, x: Option<f64>, y: Option<f64>) {
        if self.pages.is_empty() {
            return;
        }
        let page = page.min(self.page_count() - 1);
        self.go_to_page(page);
        if x.is_none() && y.is_none() {
            return;
        }
        // The page's top-left corner in page space gives the defaults for a missing coordinate.
        let geometry = self.pages[page as usize];
        let top_left = geometry.display_to_page() * Point::ZERO;
        let target = Point::new(x.unwrap_or(top_left.x), y.unwrap_or(top_left.y));
        let Some(view) = self.view_transform(page) else {
            return;
        };
        let now = view.page_to_view(&geometry) * target;
        // Only scroll on the axes the destination names, in display terms: bring the point's
        // height to the top margin, and its x into view if it is not already.
        let mut delta = Vec2::new(0.0, now.y - self.margin);
        if now.x < 0.0 || now.x > self.viewport.width {
            delta.x = now.x - self.margin;
        }
        self.set_scroll(self.scroll + delta);
    }

    /// Makes a page-space rectangle visible (a search hit, a selection). If it is already
    /// wholly inside the viewport, less `padding` logical pixels, nothing moves; otherwise it
    /// is centred.
    pub fn reveal_rect(&mut self, page: u32, rect: Rect, padding: f64) {
        let Some(view) = self.view_transform(page) else {
            return;
        };
        let r = view
            .page_to_view(&self.pages[page as usize])
            .transform_rect_bbox(rect.abs());
        let inside = r.x0 >= padding
            && r.y0 >= padding
            && r.x1 <= self.viewport.width - padding
            && r.y1 <= self.viewport.height - padding;
        if inside {
            return;
        }
        let centre = Vec2::new(self.viewport.width / 2.0, self.viewport.height / 2.0);
        self.set_scroll(self.scroll + (r.center().to_vec2() - centre));
    }

    // ---- queries ---------------------------------------------------------------------------

    /// The transform that places one page in the viewport. Its `scroll` is this view's scroll
    /// with the page's own layout offset taken out, as pl-geom expects. Use it with
    /// [`page_geometry`](ViewState::page_geometry). It is not snapped to device pixels.
    pub fn view_transform(&self, page: u32) -> Option<ViewTransform> {
        let rect = self.layout.page_rect(page as usize)?;
        let scroll = self.scroll - self.origin() - Vec2::new(rect.x0, rect.y0);
        Some(ViewTransform::new(self.zoom, self.dpr, scroll))
    }

    /// The rectangle a page occupies in view space (it may lie partly or wholly off-screen).
    pub fn page_rect_in_view(&self, page: u32) -> Option<Rect> {
        let rect = self.layout.page_rect(page as usize)?;
        Some(rect + (self.origin() - self.scroll))
    }

    /// A page-space point's position in view space.
    pub fn page_to_view(&self, page: u32, point: Point) -> Option<Point> {
        let view = self.view_transform(page)?;
        Some(view.page_to_view(&self.pages[page as usize]) * point)
    }

    /// The page under a view-space point and the point in page space. `None` when the point is
    /// in a gap, a margin or beside a page.
    pub fn hit_test(&self, view_point: Point) -> Option<PageHit> {
        self.hit_test_nearest(view_point).filter(|h| h.inside)
    }

    /// As [`hit_test`](ViewState::hit_test), but a point off every page is resolved against the
    /// vertically nearest page, with `inside` false. `None` only for an empty document.
    pub fn hit_test_nearest(&self, view_point: Point) -> Option<PageHit> {
        let doc = view_point + (self.scroll - self.origin());
        let page = self.layout.page_at_y(doc.y)?;
        let rect = self.layout.rects[page];
        let view = self.view_transform(page as u32)?;
        Some(PageHit {
            page: page as u32,
            point: view.view_to_page(&self.pages[page]) * view_point,
            inside: doc.x >= rect.x0 && doc.x <= rect.x1 && doc.y >= rect.y0 && doc.y <= rect.y1,
        })
    }

    /// The pages that are at least partly inside the viewport.
    pub fn visible_pages(&self) -> Range<u32> {
        let top = self.scroll.y - self.origin().y;
        let band = self.layout.pages_in_band(top, top + self.viewport.height);
        band.start as u32..band.end as u32
    }

    /// The page that takes up the most of the viewport (the first of equals). When no page is
    /// visible at all, the nearest one. `None` for an empty document.
    pub fn current_page(&self) -> Option<u32> {
        let top = self.scroll.y - self.origin().y;
        let bottom = top + self.viewport.height;
        let mut best: Option<(f64, usize)> = None;
        for i in self.layout.pages_in_band(top, bottom) {
            let r = self.layout.rects[i];
            let shown = r.y1.min(bottom) - r.y0.max(top);
            if best.is_none_or(|(b, _)| shown > b + 1e-9) {
                best = Some((shown, i));
            }
        }
        match best {
            Some((_, i)) => Some(i as u32),
            None => self
                .layout
                .page_at_y((top + bottom) / 2.0)
                .map(|i| i as u32),
        }
    }

    /// The tiles each visible page needs, in page order. Pages whose visible part is empty are
    /// left out.
    pub fn visible_tiles(&self) -> Vec<PageTiles> {
        let mut out = Vec::new();
        for page in self.visible_pages() {
            let i = page as usize;
            let Some(view) = self.view_transform(page) else {
                continue;
            };
            let screen_view = view.snapped_to_device_pixels();
            let (render_view, render_viewport) = self.render_view(i, &screen_view);
            let (grid, range) = visible_tiles(&self.base[i], &render_view, render_viewport);
            if range.is_empty() {
                continue;
            }
            out.push(PageTiles {
                page,
                screen_geometry: self.pages[i],
                screen_view,
                render_geometry: self.base[i],
                render_view,
                render_viewport,
                grid,
                range,
            });
        }
        out
    }

    // ---- internals -------------------------------------------------------------------------

    /// The view of the file's own geometry that shows what `screen_view` shows of the rotated
    /// one. Quarter-turn rotations map rectangles to rectangles, so this is exact.
    fn render_view(&self, page: usize, screen_view: &ViewTransform) -> (ViewTransform, Size) {
        if self.rotation == Rotation::R0 {
            return (*screen_view, self.viewport);
        }
        let shown = &self.pages[page];
        let base = &self.base[page];
        let visible = screen_view.visible_display_rect(shown, self.viewport);
        let in_page = shown.display_to_page().transform_rect_bbox(visible);
        let r = base.page_to_display().transform_rect_bbox(in_page);
        let s = screen_view.logical_px_per_unit(base);
        (
            ViewTransform::new(
                screen_view.zoom,
                screen_view.dpr,
                Vec2::new(r.x0 * s, r.y0 * s),
            ),
            Size::new(r.width() * s, r.height() * s),
        )
    }

    fn relayout(&mut self) {
        self.pages = self
            .base
            .iter()
            .map(|p| p.rotated_by(self.rotation))
            .collect();
        self.layout = PageLayout::new(&self.pages, self.zoom, self.gap, self.margin);
        self.set_scroll(self.scroll);
    }

    fn apply_zoom(&mut self, zoom: f64) {
        self.zoom = zoom;
        self.layout = PageLayout::new(&self.pages, self.zoom, self.gap, self.margin);
        self.set_scroll(self.scroll);
    }

    /// Changes the zoom keeping the page point under `anchor` (view space) in place.
    fn rezoom(&mut self, zoom: f64, anchor: Point) {
        let hit = self.hit_test_nearest(anchor);
        self.apply_zoom(zoom);
        if let Some(hit) = hit {
            self.scroll_point_to(hit.page, hit.point, anchor);
        }
    }

    fn fit_zoom(&self, page: u32) -> f64 {
        let Some(geometry) = self.pages.get(page as usize) else {
            return self.zoom;
        };
        match self.fit {
            FitMode::None => self.zoom,
            FitMode::Width => fit_width_zoom(geometry, self.viewport, self.margin),
            FitMode::Page => fit_page_zoom(geometry, self.viewport, self.margin),
        }
    }

    /// Re-applies the fit mode (or just re-clamps the scroll) after something changed, keeping
    /// `keep` as the current page.
    fn refit(&mut self, keep: Option<u32>) {
        match (self.fit, keep) {
            (FitMode::None, _) | (_, None) => self.set_scroll(self.scroll),
            (FitMode::Page, Some(p)) => self.go_to_page(p),
            (FitMode::Width, Some(p)) => {
                // Keep the page point at the top-centre of the viewport where it is.
                let anchor = Point::new(self.viewport.width / 2.0, 0.0);
                let zoom = self.fit_zoom(p);
                self.rezoom(zoom, anchor);
            }
        }
    }
}

fn sane_dpr(dpr: f64) -> f64 {
    if dpr.is_finite() && dpr > 0.0 {
        dpr
    } else {
        1.0
    }
}

fn sane_size(size: Size) -> Size {
    let fix = |v: f64| if v.is_finite() && v >= 1.0 { v } else { 1.0 };
    Size::new(fix(size.width), fix(size.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER_W: f64 = 816.0; // 8.5 in at 96 px per inch
    const LETTER_H: f64 = 1056.0;

    fn letters(n: usize) -> Vec<PageGeometry> {
        vec![PageGeometry::simple(612.0, 792.0); n]
    }

    /// Pages that differ in every way the view must cope with.
    fn mixed() -> Vec<PageGeometry> {
        vec![
            PageGeometry::simple(612.0, 792.0),
            PageGeometry::new(Rect::new(0.0, 0.0, 612.0, 792.0), None, Rotation::R90, 1.0),
            PageGeometry::new(
                Rect::new(-100.0, 50.0, 500.0, 900.0),
                Some(Rect::new(-60.0, 80.0, 420.0, 700.0)),
                Rotation::R270,
                1.0,
            ),
            PageGeometry::new(Rect::new(0.0, 0.0, 306.0, 396.0), None, Rotation::R180, 2.0),
            PageGeometry::simple(2592.0, 1728.0),
        ]
    }

    fn close(a: Point, b: Point, tol: f64) -> bool {
        (a - b).hypot() <= tol
    }

    /// A small deterministic generator for the property-style tests.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.next()
        }
    }

    #[test]
    fn layout_stacks_pages_with_gaps_and_centres_them() {
        let pages = [
            PageGeometry::simple(612.0, 792.0),
            PageGeometry::simple(306.0, 396.0),
        ];
        let l = PageLayout::new(&pages, 1.0, 10.0, 20.0);
        assert_eq!(l.page_count(), 2);
        assert_eq!(
            l.page_rect(0).unwrap(),
            Rect::new(20.0, 20.0, 20.0 + LETTER_W, 20.0 + LETTER_H)
        );
        let second = l.page_rect(1).unwrap();
        assert_eq!(second.y0, 20.0 + LETTER_H + 10.0);
        assert_eq!(second.width(), LETTER_W / 2.0);
        assert_eq!(second.center().x, l.content_size().width / 2.0);
        assert_eq!(l.content_size().width, LETTER_W + 40.0);
        assert_eq!(l.content_size().height, second.y1 + 20.0);
        assert!(l.page_rect(2).is_none());

        // Pages scale with the zoom; the gap and margin do not.
        let l2 = PageLayout::new(&pages, 2.0, 10.0, 20.0);
        assert_eq!(l2.page_rect(0).unwrap().height(), 2.0 * LETTER_H);
        assert_eq!(l2.page_rect(1).unwrap().y0, 20.0 + 2.0 * LETTER_H + 10.0);
    }

    #[test]
    fn layout_uses_user_unit_and_rotation() {
        let pages = [
            PageGeometry::new(Rect::new(0.0, 0.0, 306.0, 396.0), None, Rotation::R0, 2.0),
            PageGeometry::new(Rect::new(0.0, 0.0, 612.0, 792.0), None, Rotation::R90, 1.0),
        ];
        let l = PageLayout::new(&pages, 1.0, 0.0, 0.0);
        let a = l.page_rect(0).unwrap();
        assert!((a.width() - LETTER_W).abs() < 1e-9 && (a.height() - LETTER_H).abs() < 1e-9);
        let b = l.page_rect(1).unwrap();
        assert!((b.width() - LETTER_H).abs() < 1e-9 && (b.height() - LETTER_W).abs() < 1e-9);
    }

    #[test]
    fn page_at_y_and_bands() {
        let l = PageLayout::new(&letters(3), 1.0, 20.0, 10.0);
        let r1 = l.page_rect(1).unwrap();
        assert_eq!(l.page_at_y(-500.0), Some(0));
        assert_eq!(l.page_at_y(10.0), Some(0));
        assert_eq!(l.page_at_y(r1.y0 + 1.0), Some(1));
        // In the 20 px gap before page 1: nearer page 0, then nearer page 1.
        assert_eq!(l.page_at_y(r1.y0 - 15.0), Some(0));
        assert_eq!(l.page_at_y(r1.y0 - 5.0), Some(1));
        assert_eq!(l.page_at_y(1e9), Some(2));
        assert_eq!(l.pages_in_band(0.0, 5.0), 0..0, "only the top margin");
        assert_eq!(l.pages_in_band(0.0, 11.0), 0..1);
        assert_eq!(l.pages_in_band(r1.y0 - 10.0, r1.y0 + 10.0), 1..2);
        assert_eq!(l.pages_in_band(r1.y0 - 30.0, r1.y1 + 30.0), 0..3);
        assert_eq!(
            l.pages_in_band(r1.y0 - 19.0, r1.y0 - 1.0),
            1..1,
            "inside a gap"
        );
        assert_eq!(PageLayout::new(&[], 1.0, 5.0, 5.0).page_at_y(0.0), None);
    }

    #[test]
    fn hit_test_round_trips_through_page_to_view_on_every_kind_of_page() {
        let pages = mixed();
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for rotation in Rotation::ALL {
            let mut v = ViewState::new(&pages, Size::new(1280.0, 800.0), 1.5);
            v.set_rotation(rotation);
            for _ in 0..300 {
                v.set_zoom(rng.range(0.1, 6.0));
                let max = v.max_scroll();
                v.set_scroll(Vec2::new(rng.range(0.0, max.x), rng.range(0.0, max.y)));
                let p = Point::new(rng.range(0.0, 1280.0), rng.range(0.0, 800.0));
                let Some(hit) = v.hit_test_nearest(p) else {
                    panic!("non-empty document");
                };
                let back = v.page_to_view(hit.page, hit.point).unwrap();
                assert!(close(back, p, 1e-6), "{rotation:?}: {p:?} -> {back:?}");
                // `inside` agrees with the page's rectangle on screen and with its crop box.
                let rect = v.page_rect_in_view(hit.page).unwrap();
                assert_eq!(hit.inside, rect.contains(p) || on_edge(rect, p));
                if hit.inside {
                    let crop = v
                        .file_geometry(hit.page)
                        .unwrap()
                        .crop_box()
                        .inflate(1e-6, 1e-6);
                    assert!(crop.contains(hit.point), "{:?} not in {crop:?}", hit.point);
                    assert_eq!(v.hit_test(p), Some(hit));
                } else {
                    assert_eq!(v.hit_test(p), None);
                }
            }
        }
    }

    fn on_edge(r: Rect, p: Point) -> bool {
        p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1
    }

    #[test]
    fn hit_test_finds_known_points_on_the_second_page() {
        let mut v = ViewState::new(&letters(3), Size::new(1000.0, 700.0), 1.0);
        v.go_to_page(1);
        assert_eq!(v.current_page(), Some(1));
        let rect = v.page_rect_in_view(1).unwrap();
        // The page is 816 px wide in a 1000 px viewport: content (840) is centred.
        assert!((rect.x0 - 92.0).abs() < 1e-9);
        assert!((rect.y0 - DEFAULT_PAGE_MARGIN).abs() < 1e-9);
        // Top-left corner of the page is page-space (0, 792).
        let hit = v.hit_test(Point::new(rect.x0, rect.y0)).unwrap();
        assert_eq!(hit.page, 1);
        assert!(close(hit.point, Point::new(0.0, 792.0), 1e-9));
        // One inch right and down: (72, 720).
        let hit = v
            .hit_test(Point::new(rect.x0 + 96.0, rect.y0 + 96.0))
            .unwrap();
        assert!(close(hit.point, Point::new(72.0, 720.0), 1e-9));
        // Left of the page: nothing, but the nearest page still answers.
        assert_eq!(v.hit_test(Point::new(rect.x0 - 5.0, 300.0)), None);
        let near = v
            .hit_test_nearest(Point::new(rect.x0 - 5.0, 300.0))
            .unwrap();
        assert!(!near.inside && near.page == 1 && near.point.x < 0.0);
        // In the gap above page 1.
        assert_eq!(v.hit_test(Point::new(500.0, 3.0)), None);
    }

    #[test]
    fn zoom_about_keeps_the_point_under_the_cursor() {
        let pages = mixed();
        let mut rng = Rng(42);
        let mut checked = 0;
        for rotation in Rotation::ALL {
            let mut v = ViewState::new(&pages, Size::new(1400.0, 900.0), 2.0);
            v.set_rotation(rotation);
            for _ in 0..400 {
                v.set_zoom(rng.range(0.6, 8.0));
                let max = v.max_scroll();
                v.set_scroll(Vec2::new(rng.range(0.0, max.x), rng.range(0.0, max.y)));
                let cursor = Point::new(rng.range(0.0, 1400.0), rng.range(0.0, 900.0));
                let before = v.hit_test_nearest(cursor).unwrap();
                let new_zoom = v.zoom() * rng.range(0.7, 1.6);
                v.zoom_about(cursor, new_zoom);
                // The invariant is only promised when the scroll limits did not intervene.
                let max = v.max_scroll();
                let s = v.scroll();
                let free = s.x > 1e-6 && s.y > 1e-6 && s.x < max.x - 1e-6 && s.y < max.y - 1e-6;
                if !free {
                    continue;
                }
                checked += 1;
                let after = v.page_to_view(before.page, before.point).unwrap();
                assert!(
                    close(after, cursor, 1e-6),
                    "{rotation:?}: {cursor:?} moved to {after:?}"
                );
                assert_eq!(v.fit(), FitMode::None);
            }
        }
        assert!(checked > 400, "only {checked} unclamped cases");
    }

    #[test]
    fn zoom_is_clamped_and_steps_follow_the_presets() {
        let mut v = ViewState::new(&letters(1), Size::new(800.0, 600.0), 1.0);
        v.set_zoom(1e9);
        assert_eq!(v.zoom(), pl_geom::MAX_ZOOM);
        v.set_zoom(0.0);
        assert_eq!(v.zoom(), pl_geom::MIN_ZOOM);
        v.set_zoom(f64::NAN);
        assert_eq!(v.zoom(), 1.0);
        v.zoom_in();
        assert_eq!(v.zoom(), 1.25);
        v.zoom_out();
        v.zoom_out();
        assert_eq!(v.zoom(), 0.75);
        assert_eq!(zoom_step(0.9, true), 1.0);
        assert_eq!(zoom_step(0.9, false), 0.75);
        assert_eq!(zoom_step(64.0, true), 64.0);
        assert_eq!(zoom_step(0.05, false), 0.05);
    }

    #[test]
    fn scroll_is_clamped_to_the_content() {
        let mut v = ViewState::new(&letters(3), Size::new(600.0, 400.0), 1.0);
        v.set_scroll(Vec2::new(-50.0, -50.0));
        assert_eq!(v.scroll(), Vec2::ZERO);
        v.set_scroll(Vec2::new(1e9, 1e9));
        assert_eq!(v.scroll(), v.max_scroll());
        let c = v.layout().content_size();
        assert_eq!(v.max_scroll(), Vec2::new(c.width - 600.0, c.height - 400.0));
        v.scroll_by(Vec2::new(0.0, -100.0));
        assert_eq!(v.scroll().y, v.max_scroll().y - 100.0);
        v.set_scroll(Vec2::new(f64::NAN, 5.0));
        assert_eq!(v.scroll(), Vec2::new(0.0, 5.0));

        // Content smaller than the viewport: no scrolling, and it is centred.
        let mut small = ViewState::new(&letters(1), Size::new(2000.0, 3000.0), 1.0);
        small.set_scroll(Vec2::new(40.0, 40.0));
        assert_eq!(small.scroll(), Vec2::ZERO);
        let r = small.page_rect_in_view(0).unwrap();
        assert!((r.center().x - 1000.0).abs() < 1e-9 && (r.center().y - 1500.0).abs() < 1e-9);
    }

    #[test]
    fn fit_width_and_fit_page_follow_the_viewport() {
        let mut v = ViewState::new(&letters(4), Size::new(1000.0, 700.0), 1.0);
        v.set_fit(FitMode::Width);
        let m = DEFAULT_PAGE_MARGIN;
        let w = v.page_rect_in_view(0).unwrap();
        assert!((w.x0 - m).abs() < 1e-6 && (w.x1 - (1000.0 - m)).abs() < 1e-6);
        v.set_viewport(Size::new(500.0, 700.0));
        let w = v.page_rect_in_view(0).unwrap();
        assert!((w.width() - (500.0 - 2.0 * m)).abs() < 1e-6);
        assert_eq!(v.fit(), FitMode::Width);

        v.set_viewport(Size::new(900.0, 700.0));
        v.go_to_page(2);
        v.set_fit(FitMode::Page);
        assert_eq!(v.current_page(), Some(2));
        let p = v.page_rect_in_view(2).unwrap();
        // The whole page is inside the viewport with the margin above and below.
        assert!((p.y0 - m).abs() < 1e-6, "{p:?}");
        assert!((p.y1 - (700.0 - m)).abs() < 1e-6, "{p:?}");
        assert!(p.x0 >= m - 1e-6 && p.x1 <= 900.0 - m + 1e-6);
        assert!((p.center().x - 450.0).abs() < 1e-6);
        assert_eq!(v.visible_pages(), 2..3);
        v.set_viewport(Size::new(1100.0, 400.0));
        let p = v.page_rect_in_view(2).unwrap();
        assert!((p.height() - (400.0 - 2.0 * m)).abs() < 1e-6);
        assert_eq!(v.current_page(), Some(2));

        // A manual zoom leaves the fit mode.
        v.set_zoom(1.0);
        assert_eq!(v.fit(), FitMode::None);
        v.set_viewport(Size::new(700.0, 500.0));
        assert_eq!(v.zoom(), 1.0);
    }

    #[test]
    fn fit_page_refits_for_pages_of_different_sizes() {
        let pages = mixed();
        let mut v = ViewState::new(&pages, Size::new(900.0, 700.0), 1.0);
        v.set_fit(FitMode::Page);
        for page in 0..pages.len() as u32 {
            v.go_to_page(page);
            assert_eq!(v.current_page(), Some(page));
            let r = v.page_rect_in_view(page).unwrap();
            let m = DEFAULT_PAGE_MARGIN - 1e-6;
            assert!(
                r.x0 >= m && r.y0 >= m && r.x1 <= 900.0 - m && r.y1 <= 700.0 - m,
                "page {page}: {r:?}"
            );
            // It touches the margin on at least one axis.
            let tight_w = (r.width() - (900.0 - 2.0 * DEFAULT_PAGE_MARGIN)).abs() < 1e-6;
            let tight_h = (r.height() - (700.0 - 2.0 * DEFAULT_PAGE_MARGIN)).abs() < 1e-6;
            assert!(tight_w || tight_h, "page {page}: {r:?}");
        }
    }

    #[test]
    fn page_navigation() {
        let mut v = ViewState::new(&letters(5), Size::new(900.0, 700.0), 1.0);
        assert_eq!(v.current_page(), Some(0));
        assert_eq!(v.visible_pages(), 0..1);
        v.next_page();
        v.next_page();
        assert_eq!(v.current_page(), Some(2));
        let r = v.page_rect_in_view(2).unwrap();
        assert!((r.y0 - DEFAULT_PAGE_MARGIN).abs() < 1e-9);
        v.previous_page();
        assert_eq!(v.current_page(), Some(1));
        v.go_to_page(99);
        assert_eq!(v.current_page(), Some(4), "clamped to the last page");
        v.go_to_page(0);
        v.previous_page();
        assert_eq!(v.current_page(), Some(0));
        // Scrolled so two pages show: the one with more of the viewport is current.
        let r1 = v.layout().page_rect(1).unwrap();
        v.set_scroll(Vec2::new(0.0, r1.y0 - 500.0));
        assert_eq!(v.visible_pages(), 0..2);
        assert_eq!(v.current_page(), Some(0));
        v.set_scroll(Vec2::new(0.0, r1.y0 - 200.0));
        assert_eq!(v.current_page(), Some(1));

        let mut empty = ViewState::new(&[], Size::new(900.0, 700.0), 1.0);
        empty.go_to_page(3);
        empty.next_page();
        empty.set_fit(FitMode::Page);
        empty.zoom_about(Point::new(10.0, 10.0), 2.0);
        assert_eq!(empty.current_page(), None);
        assert_eq!(empty.visible_pages(), 0..0);
        assert!(empty.hit_test_nearest(Point::ZERO).is_none());
        assert!(empty.visible_tiles().is_empty());
    }

    #[test]
    fn destinations_and_reveal() {
        let mut v = ViewState::new(&letters(6), Size::new(900.0, 500.0), 1.0);
        // A bookmark to page 3 at y = 400 pt: that height comes to the top margin.
        v.go_to_destination(3, Some(72.0), Some(400.0));
        let at = v.page_to_view(3, Point::new(72.0, 400.0)).unwrap();
        assert!((at.y - DEFAULT_PAGE_MARGIN).abs() < 1e-6, "{at:?}");
        assert!(at.x >= 0.0 && at.x <= 900.0);
        // Without coordinates it is the top of the page.
        v.go_to_destination(4, None, None);
        assert!((v.page_rect_in_view(4).unwrap().y0 - DEFAULT_PAGE_MARGIN).abs() < 1e-6);

        // Reveal: something off-screen is centred, something visible does not move.
        v.go_to_page(0);
        let target = Rect::new(100.0, 90.0, 200.0, 110.0); // near the bottom of page 0
        v.reveal_rect(0, target, 20.0);
        // (Only vertically: the content is narrower than the viewport, so x cannot scroll.)
        let c = v.page_to_view(0, target.center()).unwrap();
        assert!((c.y - 250.0).abs() < 1e-6, "{c:?}");
        assert_eq!(v.scroll().x, 0.0);
        let before = v.scroll();
        v.reveal_rect(0, target, 20.0);
        assert_eq!(v.scroll(), before);
        v.reveal_rect(77, target, 20.0);
        assert_eq!(v.scroll(), before, "an unknown page is ignored");
    }

    #[test]
    fn rotating_the_view_keeps_the_page_and_swaps_its_shape() {
        let mut v = ViewState::new(&letters(4), Size::new(900.0, 700.0), 1.0);
        v.go_to_page(2);
        v.rotate_view(Rotation::R90);
        assert_eq!(v.rotation(), Rotation::R90);
        assert_eq!(v.current_page(), Some(2));
        let r = v.page_rect_in_view(2).unwrap();
        assert!((r.width() - LETTER_H).abs() < 1e-9 && (r.height() - LETTER_W).abs() < 1e-9);
        // Turned 90 degrees clockwise, the page's bottom-left corner is at the top-left.
        let hit = v
            .hit_test(Point::new(r.x0.max(0.0) + 1.0, r.y0 + 1.0))
            .unwrap();
        assert!(hit.point.y < 2.0, "{hit:?}");
        v.rotate_view(Rotation::R270);
        assert_eq!(v.rotation(), Rotation::R0);
        assert_eq!(v.current_page(), Some(2));
        assert_eq!(v.page_geometry(2), v.file_geometry(2));
    }

    #[test]
    fn visible_tiles_cover_exactly_what_is_on_screen() {
        let pages = mixed();
        let mut rng = Rng(7);
        for rotation in Rotation::ALL {
            let mut v = ViewState::new(&pages, Size::new(1280.0, 800.0), 1.25);
            v.set_rotation(rotation);
            for _ in 0..120 {
                v.set_zoom(rng.range(0.15, 5.0));
                let max = v.max_scroll();
                v.set_scroll(Vec2::new(rng.range(0.0, max.x), rng.range(0.0, max.y)));
                let viewport = Rect::new(0.0, 0.0, 1280.0, 800.0);
                let tiles = v.visible_tiles();
                let listed: Vec<u32> = tiles.iter().map(|t| t.page).collect();
                for t in &tiles {
                    assert!(v.visible_pages().contains(&t.page));
                    // The level is the one this zoom needs on the file's geometry.
                    let want = pl_geom::level_for_scale(
                        t.render_view.device_px_per_unit(&t.render_geometry),
                    );
                    assert_eq!(t.grid.level(), want);
                    assert_eq!(t.range.level, want);
                    let on_screen = v.page_rect_in_view(t.page).unwrap().intersect(viewport);
                    // Every listed tile touches the viewport (with a pixel of slack for the
                    // device-pixel snap) ...
                    let mut union: Option<Rect> = None;
                    for id in t.range.iter() {
                        let r = t.tile_screen_rect(id).unwrap();
                        assert!(
                            r.x1 > -1.0 && r.y1 > -1.0 && r.x0 < 1281.0 && r.y0 < 801.0,
                            "{rotation:?} tile {id:?} at {r:?} is off-screen"
                        );
                        union = Some(union.map_or(r, |u| u.union(r)));
                    }
                    // ... and together they cover the visible part of the page.
                    let u = union.unwrap();
                    assert!(
                        u.x0 <= on_screen.x0 + 1.0
                            && u.y0 <= on_screen.y0 + 1.0
                            && u.x1 >= on_screen.x1 - 1.0
                            && u.y1 >= on_screen.y1 - 1.0,
                        "{rotation:?} page {}: tiles {u:?} do not cover {on_screen:?}",
                        t.page
                    );
                    // No tile outside the range touches the viewport.
                    for id in t.grid.all().iter().filter(|id| !t.range.contains(*id)) {
                        let r = t.tile_screen_rect(id).unwrap().intersect(viewport);
                        assert!(
                            r.width() < 1.0 || r.height() < 1.0,
                            "{rotation:?} tile {id:?} is visible but not listed"
                        );
                    }
                }
                // Every page with a real visible area is listed.
                for page in v.visible_pages() {
                    let r = v.page_rect_in_view(page).unwrap().intersect(viewport);
                    if r.width() > 1.0 && r.height() > 1.0 {
                        assert!(listed.contains(&page), "page {page} missing");
                    }
                }
            }
        }
    }

    #[test]
    fn unrotated_tiles_match_pl_geom_directly() {
        let mut v = ViewState::new(&letters(3), Size::new(1920.0, 1080.0), 1.0);
        v.set_zoom(4.0);
        v.go_to_page(1);
        let tiles = v.visible_tiles();
        assert_eq!(tiles.len(), 1);
        let t = &tiles[0];
        assert_eq!(t.page, 1);
        let view = v.view_transform(1).unwrap().snapped_to_device_pixels();
        let (grid, range) = visible_tiles(v.page_geometry(1).unwrap(), &view, v.viewport());
        assert_eq!((t.grid, t.range), (grid, range));
        // 400% at dpr 1 is 5.33 px per point: level 3.
        assert_eq!(t.grid.level(), 3);
        let req = t.view_request(DocId(9), 4, 5, 1);
        assert_eq!(
            (req.doc, req.page, req.revision, req.layer_hash),
            (DocId(9), 1, 4, 5)
        );
        assert_eq!(req.viewport, v.viewport());
    }

    #[test]
    fn set_pages_keeps_the_view_valid() {
        let mut v = ViewState::new(&letters(10), Size::new(900.0, 700.0), 1.0);
        v.go_to_page(8);
        v.set_pages(&letters(3));
        assert_eq!(v.page_count(), 3);
        assert_eq!(v.scroll(), v.max_scroll());
        assert_eq!(v.current_page(), Some(2));
        v.set_spacing(0.0, 0.0);
        assert_eq!(v.layout().content_size().width, LETTER_W);
    }
}
