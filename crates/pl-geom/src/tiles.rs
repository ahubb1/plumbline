//! Tile grid maths: 512 px tiles at power-of-two levels.
//!
//! A *level* `L` renders the page at `2^L` pixels per user-space unit. Tile pixel space is display
//! space (see [`PageGeometry::page_to_display`]) scaled by that factor: origin at the top-left
//! corner of the displayed page, y-down. Tile `(x, y)` covers pixels
//! `[x*512, (x+1)*512) x [y*512, (y+1)*512)`, clipped to the page's pixel extent, so tiles on the
//! right and bottom edges are usually smaller than 512 px.
//!
//! Because levels are defined per user-space unit, `/UserUnit` does not appear here; it enters
//! through [`ViewTransform::device_px_per_unit`] when a level is chosen for a zoom.

use kurbo::{Rect, Size};

use crate::page::PageGeometry;
use crate::view::ViewTransform;

/// Tile edge length in pixels.
pub const TILE_SIZE: u32 = 512;

/// The coarsest level: 1/256 px per unit.
pub const MIN_LEVEL: i8 = -8;
/// The finest level: 4096 px per unit.
pub const MAX_LEVEL: i8 = 12;

/// Slack, in pixels, used when deciding which tiles a floating-point rectangle touches, so that a
/// rectangle that ends on a tile boundary up to rounding error does not pull in the next tile.
const COVER_EPS: f64 = 1e-6;

/// Pixels per user-space unit at a level: `2^level`.
pub fn level_scale(level: i8) -> f64 {
    2f64.powi(level as i32)
}

/// The level to render for a given on-screen scale (physical pixels per user-space unit): the
/// smallest level whose scale is at least that, so tiles are only ever downscaled (by less than
/// 2x), never upscaled. Clamped to [`MIN_LEVEL`]..=[`MAX_LEVEL`].
pub fn level_for_scale(device_px_per_unit: f64) -> i8 {
    if !device_px_per_unit.is_finite() || device_px_per_unit <= 0.0 {
        return 0;
    }
    // The small bias keeps an exact power of two (up to rounding) on its own level.
    let level = (device_px_per_unit.log2() - 1e-9).ceil();
    level.clamp(MIN_LEVEL as f64, MAX_LEVEL as f64) as i8
}

/// One tile of one level. The page, document and revision parts of a cache key live elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub level: i8,
    pub x: u32,
    pub y: u32,
}

/// An integer rectangle in tile pixel space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    /// The same rectangle as a kurbo [`Rect`].
    pub fn to_rect(self) -> Rect {
        Rect::new(
            self.x as f64,
            self.y as f64,
            self.x as f64 + self.width as f64,
            self.y as f64 + self.height as f64,
        )
    }
}

/// The tile grid of one page at one level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileGrid {
    level: i8,
    width_px: u32,
    height_px: u32,
}

impl TileGrid {
    /// The grid for a page at a level (the level is clamped to the supported range).
    ///
    /// The pixel extent is the displayed page size times the level scale, rounded up to whole
    /// pixels and at least 1 x 1.
    pub fn new(page: &PageGeometry, level: i8) -> TileGrid {
        let level = level.clamp(MIN_LEVEL, MAX_LEVEL);
        let scale = level_scale(level);
        let d = page.display_size();
        // `as u32` saturates, so absurd page sizes cannot overflow.
        let px = |units: f64| ((units * scale - COVER_EPS).ceil().max(1.0)) as u32;
        TileGrid {
            level,
            width_px: px(d.width),
            height_px: px(d.height),
        }
    }

    pub fn level(&self) -> i8 {
        self.level
    }

    /// Pixels per user-space unit.
    pub fn scale(&self) -> f64 {
        level_scale(self.level)
    }

    /// Width of the whole page at this level, in pixels.
    pub fn width_px(&self) -> u32 {
        self.width_px
    }

    /// Height of the whole page at this level, in pixels.
    pub fn height_px(&self) -> u32 {
        self.height_px
    }

    /// Number of tile columns.
    pub fn cols(&self) -> u32 {
        self.width_px.div_ceil(TILE_SIZE)
    }

    /// Number of tile rows.
    pub fn rows(&self) -> u32 {
        self.height_px.div_ceil(TILE_SIZE)
    }

    /// Number of tiles in the grid.
    pub fn tile_count(&self) -> u64 {
        self.cols() as u64 * self.rows() as u64
    }

    /// Whether a tile id belongs to this grid.
    pub fn contains(&self, id: TileId) -> bool {
        id.level == self.level && id.x < self.cols() && id.y < self.rows()
    }

    /// Every tile of the grid.
    pub fn all(&self) -> TileRange {
        TileRange {
            level: self.level,
            x0: 0,
            y0: 0,
            x1: self.cols(),
            y1: self.rows(),
        }
    }

    /// The pixels a tile covers, clipped to the page extent. `x`/`y` are the origin offset to
    /// render the page at, `width`/`height` the bitmap size. `None` if the tile is not in the
    /// grid.
    pub fn tile_pixel_rect(&self, id: TileId) -> Option<PixelRect> {
        if !self.contains(id) {
            return None;
        }
        let x = id.x * TILE_SIZE;
        let y = id.y * TILE_SIZE;
        Some(PixelRect {
            x,
            y,
            width: TILE_SIZE.min(self.width_px - x),
            height: TILE_SIZE.min(self.height_px - y),
        })
    }

    /// The rectangle a tile covers in display space (user-space units, y-down, origin at the
    /// displayed page's top-left corner).
    pub fn tile_display_rect(&self, id: TileId) -> Option<Rect> {
        let r = self.tile_pixel_rect(id)?.to_rect();
        let s = self.scale();
        Some(Rect::new(r.x0 / s, r.y0 / s, r.x1 / s, r.y1 / s))
    }

    /// The rectangle a tile covers in page space (unrotated, y-up). Because the pixel extent is
    /// rounded up, an edge tile can extend past the crop box by less than one pixel.
    pub fn tile_page_rect(&self, page: &PageGeometry, id: TileId) -> Option<Rect> {
        let d = self.tile_display_rect(id)?;
        Some(page.display_to_page().transform_rect_bbox(d))
    }

    /// The rectangle a tile occupies in view space (logical pixels) for a given view.
    pub fn tile_view_rect(
        &self,
        page: &PageGeometry,
        view: &ViewTransform,
        id: TileId,
    ) -> Option<Rect> {
        let d = self.tile_display_rect(id)?;
        Some(view.display_to_view(page).transform_rect_bbox(d))
    }

    /// The tiles touched by a rectangle given in this level's pixel space. The rectangle is
    /// clipped to the page; a rectangle wholly outside the page gives an empty range. A
    /// zero-area rectangle inside the page (a point, or the bounds of a horizontal line) still
    /// yields the tile(s) it lies in.
    pub fn tiles_covering_pixels(&self, rect: Rect) -> TileRange {
        let empty = TileRange::empty(self.level);
        // Checked before `abs`, whose min/max would silently drop a NaN.
        if [rect.x0, rect.y0, rect.x1, rect.y1]
            .iter()
            .any(|v| v.is_nan())
        {
            return empty;
        }
        let r = rect.abs();
        let (w, h) = (self.width_px as f64, self.height_px as f64);
        if r.x1 < 0.0 || r.y1 < 0.0 || r.x0 > w || r.y0 > h {
            return empty;
        }
        let axis = |lo: f64, hi: f64, extent: f64, count: u32| -> (u32, u32) {
            let lo = lo.clamp(0.0, extent);
            let hi = hi.clamp(0.0, extent);
            let t = TILE_SIZE as f64;
            let first = (((lo + COVER_EPS) / t).floor() as u32).min(count - 1);
            let end = (((hi - COVER_EPS) / t).ceil() as u32).clamp(first + 1, count);
            (first, end)
        };
        let (x0, x1) = axis(r.x0, r.x1, w, self.cols());
        let (y0, y1) = axis(r.y0, r.y1, h, self.rows());
        TileRange {
            level: self.level,
            x0,
            y0,
            x1,
            y1,
        }
    }

    /// The tiles touched by a page-space rectangle (for example an annotation's `/Rect`, to
    /// invalidate the tiles under it).
    pub fn tiles_covering_page_rect(&self, page: &PageGeometry, rect: Rect) -> TileRange {
        let d = page.page_to_display().transform_rect_bbox(rect.abs());
        let s = self.scale();
        self.tiles_covering_pixels(Rect::new(d.x0 * s, d.y0 * s, d.x1 * s, d.y1 * s))
    }

    /// The tiles a viewport of the given logical size shows under `view`. The grid's level need
    /// not be the one [`level_for_scale`] would choose (placeholders use coarser levels).
    pub fn tiles_in_view(
        &self,
        page: &PageGeometry,
        view: &ViewTransform,
        viewport: Size,
    ) -> TileRange {
        let d = view.visible_display_rect(page, viewport);
        let s = self.scale();
        self.tiles_covering_pixels(Rect::new(d.x0 * s, d.y0 * s, d.x1 * s, d.y1 * s))
    }
}

/// Picks the level for a view and returns its grid together with the visible tiles.
pub fn visible_tiles(
    page: &PageGeometry,
    view: &ViewTransform,
    viewport: Size,
) -> (TileGrid, TileRange) {
    let grid = TileGrid::new(page, level_for_scale(view.device_px_per_unit(page)));
    let range = grid.tiles_in_view(page, view, viewport);
    (grid, range)
}

/// A rectangular block of tiles on one level: columns `x0..x1`, rows `y0..y1` (half-open).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileRange {
    pub level: i8,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl TileRange {
    /// An empty range on a level.
    pub fn empty(level: i8) -> TileRange {
        TileRange {
            level,
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    /// Number of tiles in the range.
    pub fn len(&self) -> usize {
        if self.is_empty() {
            0
        } else {
            (self.x1 - self.x0) as usize * (self.y1 - self.y0) as usize
        }
    }

    pub fn contains(&self, id: TileId) -> bool {
        id.level == self.level
            && id.x >= self.x0
            && id.x < self.x1
            && id.y >= self.y0
            && id.y < self.y1
    }

    /// The tiles in row-major order.
    pub fn iter(&self) -> impl Iterator<Item = TileId> {
        let r = *self;
        let rows = if r.is_empty() { 0..0 } else { r.y0..r.y1 };
        rows.flat_map(move |y| {
            (r.x0..r.x1).map(move |x| TileId {
                level: r.level,
                x,
                y,
            })
        })
    }

    /// The tiles ordered by distance of their centre from the centre of the range, nearest
    /// first (ties in row-major order). This is the render queue order for visible tiles.
    pub fn centre_out(&self) -> Vec<TileId> {
        let cx = (self.x0 as f64 + self.x1 as f64) / 2.0;
        let cy = (self.y0 as f64 + self.y1 as f64) / 2.0;
        let mut tiles: Vec<TileId> = self.iter().collect();
        let key = |t: &TileId| {
            let dx = t.x as f64 + 0.5 - cx;
            let dy = t.y as f64 + 0.5 - cy;
            dx * dx + dy * dy
        };
        // Stable sort: ties keep the row-major order of `iter`.
        tiles.sort_by(|a, b| key(a).total_cmp(&key(b)));
        tiles
    }

    /// The range grown by `ring` tiles on every side, clipped to the grid. An empty range stays
    /// empty.
    pub fn expanded(&self, ring: u32, grid: &TileGrid) -> TileRange {
        if self.is_empty() {
            return *self;
        }
        TileRange {
            level: self.level,
            x0: self.x0.saturating_sub(ring),
            y0: self.y0.saturating_sub(ring),
            x1: self.x1.saturating_add(ring).min(grid.cols()),
            y1: self.y1.saturating_add(ring).min(grid.rows()),
        }
    }

    /// The tiles of the prefetch ring: those in `self.expanded(ring, grid)` that are not in
    /// `self`, nearest first.
    pub fn prefetch_ring(&self, ring: u32, grid: &TileGrid) -> Vec<TileId> {
        let outer = self.expanded(ring, grid);
        let cx = (self.x0 as f64 + self.x1 as f64) / 2.0;
        let cy = (self.y0 as f64 + self.y1 as f64) / 2.0;
        let mut tiles: Vec<TileId> = outer.iter().filter(|t| !self.contains(*t)).collect();
        let key = |t: &TileId| {
            let dx = t.x as f64 + 0.5 - cx;
            let dy = t.y as f64 + 0.5 - cy;
            dx * dx + dy * dy
        };
        // Stable sort: ties keep the row-major order of `iter`.
        tiles.sort_by(|a, b| key(a).total_cmp(&key(b)));
        tiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Rotation;
    use crate::testutil::Rng;
    use kurbo::{Point, Vec2};

    fn id(level: i8, x: u32, y: u32) -> TileId {
        TileId { level, x, y }
    }

    #[test]
    fn level_scale_is_power_of_two() {
        assert_eq!(level_scale(0), 1.0);
        assert_eq!(level_scale(3), 8.0);
        assert_eq!(level_scale(-2), 0.25);
    }

    #[test]
    fn level_for_scale_never_upscales() {
        assert_eq!(level_for_scale(1.0), 0);
        assert_eq!(level_for_scale(1.0001), 1);
        assert_eq!(level_for_scale(2.0), 1);
        assert_eq!(level_for_scale(2.5), 2);
        assert_eq!(level_for_scale(4.0), 2);
        assert_eq!(level_for_scale(0.5), -1);
        assert_eq!(level_for_scale(0.3), -1);
        assert_eq!(level_for_scale(0.25), -2);
        // Powers of two reached through rounding stay on their level.
        assert_eq!(level_for_scale(4.0 * (1.0 + 1e-13)), 2);
        assert_eq!(level_for_scale(0.75 * (96.0 / 72.0)), 0);
        // Clamps and bad input.
        assert_eq!(level_for_scale(1e-9), MIN_LEVEL);
        assert_eq!(level_for_scale(1e9), MAX_LEVEL);
        assert_eq!(level_for_scale(0.0), 0);
        assert_eq!(level_for_scale(f64::NAN), 0);
    }

    #[test]
    fn prop_level_for_scale_bounds() {
        let mut rng = Rng::new(21);
        for _ in 0..2000 {
            let s = rng.log_range(0.01, 2000.0);
            let l = level_for_scale(s);
            assert!(level_scale(l) >= s * (1.0 - 1e-8), "scale {s} level {l}");
            assert!(level_scale(l) < 2.0 * s, "scale {s} level {l}");
        }
    }

    #[test]
    fn grid_dimensions() {
        // 612 x 792 at level 2 = 2448 x 3168 px -> 5 x 7 tiles.
        let page = PageGeometry::simple(612.0, 792.0);
        let g = TileGrid::new(&page, 2);
        assert_eq!((g.width_px(), g.height_px()), (2448, 3168));
        assert_eq!((g.cols(), g.rows()), (5, 7));
        assert_eq!(g.tile_count(), 35);
        assert_eq!(g.all().len(), 35);

        // Rotated: extents swap.
        let rot = page.rotated_by(Rotation::R90);
        let g = TileGrid::new(&rot, 2);
        assert_eq!((g.width_px(), g.height_px()), (3168, 2448));
        assert_eq!((g.cols(), g.rows()), (7, 5));

        // Exactly one tile, and a tiny page is still one pixel.
        let g = TileGrid::new(&PageGeometry::simple(512.0, 512.0), 0);
        assert_eq!((g.cols(), g.rows()), (1, 1));
        let g = TileGrid::new(&PageGeometry::simple(513.0, 512.0), 0);
        assert_eq!((g.cols(), g.rows()), (2, 1));
        let g = TileGrid::new(&PageGeometry::simple(10.0, 10.0), MIN_LEVEL);
        assert_eq!((g.width_px(), g.height_px()), (1, 1));

        // Levels are clamped.
        assert_eq!(TileGrid::new(&page, 100).level(), MAX_LEVEL);
        assert_eq!(TileGrid::new(&page, -100).level(), MIN_LEVEL);
    }

    #[test]
    fn edge_tiles_are_clipped() {
        let page = PageGeometry::simple(612.0, 792.0);
        let g = TileGrid::new(&page, 2); // 2448 x 3168
        assert_eq!(
            g.tile_pixel_rect(id(2, 0, 0)),
            Some(PixelRect {
                x: 0,
                y: 0,
                width: 512,
                height: 512
            })
        );
        assert_eq!(
            g.tile_pixel_rect(id(2, 4, 6)),
            Some(PixelRect {
                x: 2048,
                y: 3072,
                width: 400,
                height: 96
            })
        );
        assert_eq!(g.tile_pixel_rect(id(2, 5, 0)), None);
        assert_eq!(g.tile_pixel_rect(id(2, 0, 7)), None);
        assert_eq!(g.tile_pixel_rect(id(1, 0, 0)), None); // wrong level
    }

    #[test]
    fn tile_page_rect_unrotated() {
        let page = PageGeometry::new(
            Rect::new(0.0, 0.0, 1000.0, 1000.0),
            Some(Rect::new(100.0, 200.0, 900.0, 1000.0)),
            Rotation::R0,
            1.0,
        );
        // Level 1: 2 px per unit; crop is 800 x 800 units = 1600 px; a tile is 256 units.
        let g = TileGrid::new(&page, 1);
        assert_eq!((g.cols(), g.rows()), (4, 4));
        // Top-left tile: x from crop x0, y down from crop y1.
        assert_eq!(
            g.tile_page_rect(&page, id(1, 0, 0)),
            Some(Rect::new(100.0, 744.0, 356.0, 1000.0))
        );
        // Last tile is 64 px = 32 units wide and tall, at the bottom-right.
        assert_eq!(
            g.tile_page_rect(&page, id(1, 3, 3)),
            Some(Rect::new(868.0, 200.0, 900.0, 232.0))
        );
    }

    #[test]
    fn tile_page_rect_rotated_90() {
        let page = PageGeometry::new(
            Rect::new(0.0, 0.0, 1000.0, 1000.0),
            Some(Rect::new(100.0, 200.0, 612.0, 1000.0)), // 512 wide, 800 tall
            Rotation::R90,
            1.0,
        );
        // Displayed 800 wide x 512 tall. Level 0.
        let g = TileGrid::new(&page, 0);
        assert_eq!((g.cols(), g.rows()), (2, 1));
        // Display (dx, dy) -> page (x0 + dy, y0 + dx): the first tile is the bottom 512 units.
        assert_eq!(
            g.tile_page_rect(&page, id(0, 0, 0)),
            Some(Rect::new(100.0, 200.0, 612.0, 712.0))
        );
        assert_eq!(
            g.tile_page_rect(&page, id(0, 1, 0)),
            Some(Rect::new(100.0, 712.0, 612.0, 1000.0))
        );
    }

    /// Property: at every rotation the tiles' page rects tile the crop box: areas sum to the
    /// (pixel-rounded) page area, they do not overlap, and their union is the crop box grown by
    /// less than a pixel.
    #[test]
    fn prop_tiles_partition_the_page() {
        let mut rng = Rng::new(22);
        for rotate in Rotation::ALL {
            for _ in 0..40 {
                let w = rng.range(100.0, 3000.0);
                let h = rng.range(100.0, 3000.0);
                let ox = rng.range(5.0, 200.0);
                let oy = rng.range(5.0, 200.0);
                let page = PageGeometry::new(
                    Rect::new(0.0, 0.0, ox + w + 50.0, oy + h + 50.0),
                    Some(Rect::new(ox, oy, ox + w, oy + h)),
                    rotate,
                    *rng.pick(&[0.5, 2.0, 7.0]),
                );
                let level = *rng.pick(&[-2i8, -1, 0, 1, 2]);
                let g = TileGrid::new(&page, level);
                let s = g.scale();
                let rects: Vec<Rect> = g
                    .all()
                    .iter()
                    .map(|t| g.tile_page_rect(&page, t).unwrap())
                    .collect();
                assert_eq!(rects.len() as u64, g.tile_count());

                let area: f64 = rects.iter().map(|r| r.area()).sum();
                let want = g.width_px() as f64 * g.height_px() as f64 / (s * s);
                assert!(
                    (area - want).abs() < 1e-6 * want,
                    "{rotate:?} area {area} want {want}"
                );

                let union = rects.iter().skip(1).fold(rects[0], |a, b| a.union(*b));
                let c = page.crop_box();
                let slack = 1.0 / s + 1e-9;
                assert!(union.x0 <= c.x0 + 1e-9 && union.y0 <= c.y0 + 1e-9);
                assert!(union.x1 >= c.x1 - 1e-9 && union.y1 >= c.y1 - 1e-9);
                assert!(c.x0 - union.x0 < slack && c.y0 - union.y0 < slack);
                assert!(union.x1 - c.x1 < slack && union.y1 - c.y1 < slack);

                for (i, a) in rects.iter().enumerate() {
                    for b in rects.iter().skip(i + 1) {
                        let o = a.intersect(*b);
                        assert!(o.area() < 1e-6, "tiles overlap: {a:?} {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn covering_pixels_basic() {
        let page = PageGeometry::simple(2048.0, 1536.0);
        let g = TileGrid::new(&page, 0); // 4 x 3 tiles
        let r = g.tiles_covering_pixels(Rect::new(100.0, 100.0, 600.0, 300.0));
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (0, 0, 2, 1));
        // Ends exactly on a boundary: the neighbour is not included.
        let r = g.tiles_covering_pixels(Rect::new(0.0, 0.0, 512.0, 1024.0));
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (0, 0, 1, 2));
        // ...even with rounding error on either side.
        let r = g.tiles_covering_pixels(Rect::new(511.9999999, 0.0, 1024.0000001, 10.0));
        assert_eq!((r.x0, r.x1), (1, 2));
        // Larger than the page: everything.
        let r = g.tiles_covering_pixels(Rect::new(-1e6, -1e6, 1e6, 1e6));
        assert_eq!(r, g.all());
        // Outside the page: nothing.
        assert!(g
            .tiles_covering_pixels(Rect::new(3000.0, 0.0, 4000.0, 100.0))
            .is_empty());
        assert!(g
            .tiles_covering_pixels(Rect::new(0.0, -50.0, 100.0, -10.0))
            .is_empty());
        assert!(g
            .tiles_covering_pixels(Rect::new(f64::NAN, 0.0, 1.0, 1.0))
            .is_empty());
        // Reversed corners are normalised.
        let r = g.tiles_covering_pixels(Rect::new(600.0, 300.0, 100.0, 100.0));
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (0, 0, 2, 1));
    }

    #[test]
    fn covering_pixels_degenerate_rects_still_hit_a_tile() {
        let page = PageGeometry::simple(2048.0, 1536.0);
        let g = TileGrid::new(&page, 0);
        // A point.
        let r = g.tiles_covering_pixels(Rect::new(700.0, 700.0, 700.0, 700.0));
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (1, 1, 2, 2));
        // A horizontal line's bounds.
        let r = g.tiles_covering_pixels(Rect::new(100.0, 700.0, 1100.0, 700.0));
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (0, 1, 3, 2));
        // A point on the far corner maps to the last tile.
        let r = g.tiles_covering_pixels(Rect::new(2048.0, 1536.0, 2048.0, 1536.0));
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (3, 2, 4, 3));
    }

    /// Property: a tile is in the covering range exactly when its pixel rect overlaps the query
    /// (brute force over the whole grid).
    #[test]
    fn prop_covering_matches_brute_force() {
        let mut rng = Rng::new(23);
        let page = PageGeometry::simple(3000.0, 2100.0);
        let g = TileGrid::new(&page, 1); // 6000 x 4200 px
        for _ in 0..500 {
            let x0 = rng.range(-500.0, 6500.0);
            let y0 = rng.range(-500.0, 4700.0);
            let q = Rect::new(
                x0,
                y0,
                x0 + rng.range(1.0, 3000.0),
                y0 + rng.range(1.0, 3000.0),
            );
            let range = g.tiles_covering_pixels(q);
            for t in g.all().iter() {
                let tr = g.tile_pixel_rect(t).unwrap().to_rect();
                let overlap = tr.intersect(q);
                let hits = overlap.width() > 1e-3 && overlap.height() > 1e-3;
                let misses = overlap.width() <= 0.0 || overlap.height() <= 0.0;
                if hits {
                    assert!(range.contains(t), "{q:?} should include {t:?}");
                } else if misses && !range.is_empty() {
                    // Tiles with no overlap at all must be excluded.
                    let strictly_apart = tr.x1 <= q.x0 - 1e-3
                        || tr.x0 >= q.x1 + 1e-3
                        || tr.y1 <= q.y0 - 1e-3
                        || tr.y0 >= q.y1 + 1e-3;
                    if strictly_apart {
                        assert!(!range.contains(t), "{q:?} should exclude {t:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn covering_page_rect_respects_rotation_and_crop() {
        let media = Rect::new(0.0, 0.0, 3000.0, 3000.0);
        let crop = Rect::new(100.0, 200.0, 2148.0, 1736.0); // 2048 x 1536
                                                            // A 10 x 10 unit square near the page's top-left corner.
        let mark = Rect::new(110.0, 1716.0, 120.0, 1726.0);
        let expect = [
            (Rotation::R0, (0, 0)),   // top-left stays top-left: grid 4 x 3
            (Rotation::R90, (2, 0)),  // goes to top-right: grid 3 x 4
            (Rotation::R180, (3, 2)), // goes to bottom-right
            (Rotation::R270, (0, 3)), // goes to bottom-left
        ];
        for (rotate, (tx, ty)) in expect {
            let page = PageGeometry::new(media, Some(crop), rotate, 2.0);
            let g = TileGrid::new(&page, 0);
            let r = g.tiles_covering_page_rect(&page, mark);
            assert_eq!(
                (r.x0, r.y0, r.x1, r.y1),
                (tx, ty, tx + 1, ty + 1),
                "{rotate:?}"
            );
            // And the tile's page rect really contains the mark.
            let pr = g.tile_page_rect(&page, id(0, tx, ty)).unwrap();
            assert!(pr.contains_rect(mark), "{rotate:?}: {pr:?}");
        }
    }

    #[test]
    fn visible_tiles_picks_level_and_range() {
        // UserUnit 2: at 100% and dpr 1.5 a unit is 2 * 1.3333 * 1.5 = 4 device px -> level 2.
        let page = PageGeometry::new(Rect::new(0.0, 0.0, 1000.0, 1000.0), None, Rotation::R0, 2.0);
        let view = ViewTransform::new(1.0, 1.5, Vec2::ZERO);
        let (grid, range) = visible_tiles(&page, &view, Size::new(800.0, 600.0));
        assert_eq!(grid.level(), 2);
        assert_eq!((grid.width_px(), grid.height_px()), (4000, 4000));
        // 800 x 600 logical = 1200 x 900 device px = the same in level px (scale matches).
        assert_eq!((range.x0, range.y0, range.x1, range.y1), (0, 0, 3, 2));

        // Scrolled by exactly one tile (512 device px = 341.33 logical).
        let view = ViewTransform::new(1.0, 1.5, Vec2::new(512.0 / 1.5, 1024.0 / 1.5));
        let (_, range) = visible_tiles(&page, &view, Size::new(800.0, 600.0));
        assert_eq!((range.x0, range.y0, range.x1, range.y1), (1, 2, 4, 4));

        // Scrolled wholly past the page.
        let view = ViewTransform::new(1.0, 1.5, Vec2::new(1e6, 0.0));
        let (_, range) = visible_tiles(&page, &view, Size::new(800.0, 600.0));
        assert!(range.is_empty());
        assert_eq!(range.iter().count(), 0);
    }

    /// Property: every visible tile's view rect overlaps the viewport, the visible tiles cover
    /// the visible part of the page, and the chosen level is never upscaled on screen.
    #[test]
    fn prop_visible_tiles_cover_the_viewport() {
        let mut rng = Rng::new(24);
        let viewport = Size::new(1280.0, 800.0);
        let vp_rect = Rect::new(0.0, 0.0, viewport.width, viewport.height);
        for rotate in Rotation::ALL {
            for _ in 0..150 {
                let page = PageGeometry::new(
                    Rect::new(0.0, 0.0, 2600.0, 1800.0),
                    Some(Rect::new(40.0, 60.0, 2500.0, 1700.0)),
                    rotate,
                    *rng.pick(&[0.5, 2.0, 3.0]),
                );
                let zoom = rng.log_range(0.1, 16.0);
                let dpr = *rng.pick(&[1.0, 1.5, 2.0]);
                let mut view = ViewTransform::new(zoom, dpr, Vec2::ZERO);
                let pr = view.page_rect_in_view(&page);
                view.scroll = Vec2::new(
                    rng.range(-300.0, pr.width().max(1.0)),
                    rng.range(-300.0, pr.height().max(1.0)),
                );
                let (grid, range) = visible_tiles(&page, &view, viewport);
                assert!(grid.scale() >= view.device_px_per_unit(&page) * (1.0 - 1e-8));
                assert!(grid.scale() < 2.0 * view.device_px_per_unit(&page));

                let page_in_view = view.page_rect_in_view(&page).intersect(vp_rect);
                if page_in_view.width() <= 1e-6 || page_in_view.height() <= 1e-6 {
                    continue;
                }
                let mut covered = 0.0;
                for t in range.iter() {
                    let r = grid.tile_view_rect(&page, &view, t).unwrap();
                    let o = r.intersect(vp_rect);
                    assert!(
                        o.width() > -1e-6 && o.height() > -1e-6,
                        "{rotate:?}: tile {t:?} at {r:?} is off screen"
                    );
                    covered += o.intersect(page_in_view).area();
                }
                let want = page_in_view.area();
                assert!(
                    (covered - want).abs() < 1e-6 * want.max(1.0),
                    "{rotate:?}: covered {covered} of {want}"
                );
                // No tile outside the range is visible.
                for t in grid.all().iter().filter(|t| !range.contains(*t)) {
                    let r = grid.tile_view_rect(&page, &view, t).unwrap();
                    let o = r.intersect(vp_rect);
                    assert!(o.width() < 1e-3 || o.height() < 1e-3, "missed tile {t:?}");
                }
            }
        }
    }

    #[test]
    fn tile_view_rect_matches_page_transform() {
        let page = PageGeometry::new(
            Rect::new(0.0, 0.0, 2600.0, 1800.0),
            Some(Rect::new(40.0, 60.0, 2500.0, 1700.0)),
            Rotation::R270,
            2.0,
        );
        let view = ViewTransform::new(0.8, 2.0, Vec2::new(123.0, 45.0));
        let (grid, range) = visible_tiles(&page, &view, Size::new(1000.0, 700.0));
        for t in range.iter() {
            let via_page = view
                .page_to_view(&page)
                .transform_rect_bbox(grid.tile_page_rect(&page, t).unwrap());
            let direct = grid.tile_view_rect(&page, &view, t).unwrap();
            assert!((via_page.origin() - direct.origin()).hypot() < 1e-8);
            assert!(
                (via_page.x1 - direct.x1).abs() < 1e-8 && (via_page.y1 - direct.y1).abs() < 1e-8
            );
        }
    }

    #[test]
    fn range_iteration_and_len() {
        let r = TileRange {
            level: 3,
            x0: 2,
            y0: 5,
            x1: 4,
            y1: 7,
        };
        let v: Vec<_> = r.iter().collect();
        assert_eq!(v, vec![id(3, 2, 5), id(3, 3, 5), id(3, 2, 6), id(3, 3, 6)]);
        assert_eq!(r.len(), 4);
        assert!(r.contains(id(3, 3, 6)));
        assert!(!r.contains(id(3, 4, 6)));
        assert!(!r.contains(id(2, 3, 6)));
        assert_eq!(TileRange::empty(0).len(), 0);
        assert_eq!(TileRange::empty(0).iter().count(), 0);
        // Inverted bounds count as empty rather than underflowing.
        let bad = TileRange {
            level: 0,
            x0: 5,
            y0: 0,
            x1: 2,
            y1: 3,
        };
        assert!(bad.is_empty());
        assert_eq!(bad.len(), 0);
        assert_eq!(bad.iter().count(), 0);
    }

    #[test]
    fn centre_out_order() {
        let r = TileRange {
            level: 0,
            x0: 0,
            y0: 0,
            x1: 3,
            y1: 3,
        };
        let order = r.centre_out();
        assert_eq!(order.len(), 9);
        assert_eq!(order[0], id(0, 1, 1));
        // Edge neighbours next, corners last.
        let mid: Vec<_> = order[1..5].to_vec();
        assert_eq!(
            mid,
            vec![id(0, 1, 0), id(0, 0, 1), id(0, 2, 1), id(0, 1, 2)]
        );
        for c in &order[5..] {
            assert!(c.x != 1 && c.y != 1);
        }
        // Distances never decrease.
        let d = |t: &TileId| {
            let p = Point::new(t.x as f64 + 0.5, t.y as f64 + 0.5);
            p.distance(Point::new(1.5, 1.5))
        };
        assert!(order.windows(2).all(|w| d(&w[0]) <= d(&w[1])));
    }

    #[test]
    fn expanded_and_prefetch_ring() {
        let page = PageGeometry::simple(512.0 * 6.0, 512.0 * 4.0);
        let g = TileGrid::new(&page, 0); // 6 x 4
        let r = TileRange {
            level: 0,
            x0: 0,
            y0: 1,
            x1: 2,
            y1: 3,
        };
        let e = r.expanded(1, &g);
        assert_eq!((e.x0, e.y0, e.x1, e.y1), (0, 0, 3, 4));
        let ring = r.prefetch_ring(1, &g);
        assert_eq!(ring.len(), e.len() - r.len());
        assert!(ring.iter().all(|t| e.contains(*t) && !r.contains(*t)));
        // A huge ring is clipped to the grid.
        assert_eq!(r.expanded(u32::MAX, &g), g.all());
        // Empty stays empty.
        assert!(TileRange::empty(0).expanded(2, &g).is_empty());
        assert!(TileRange::empty(0).prefetch_ring(2, &g).is_empty());
    }
}
