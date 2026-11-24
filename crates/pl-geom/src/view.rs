//! The view transform: zoom, device pixel ratio and scroll offset; zoom-to-cursor and fit modes.

use kurbo::{Affine, Point, Rect, Size, Vec2};

use crate::page::PageGeometry;

/// Logical pixels per PDF point at 100% zoom. A logical pixel is 1/96 inch, so 100% shows the
/// page at its physical size on a correctly calibrated display.
pub const LOGICAL_PX_PER_POINT: f64 = 96.0 / 72.0;

/// The smallest zoom the UI offers (5%).
pub const MIN_ZOOM: f64 = 0.05;
/// The largest zoom the UI offers (6400%).
pub const MAX_ZOOM: f64 = 64.0;

/// Clamps a zoom factor to [`MIN_ZOOM`]..=[`MAX_ZOOM`]. A non-finite value becomes 1.
pub fn clamp_zoom(zoom: f64) -> f64 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// How one page is placed in a viewport.
///
/// View space is in **logical pixels** (egui points), y-down, with its origin at the viewport's
/// top-left corner. Device space is the same thing in physical pixels (`view * dpr`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewTransform {
    /// Zoom factor; 1.0 is 100% (one PDF point = 96/72 logical pixels).
    pub zoom: f64,
    /// Device pixel ratio: physical pixels per logical pixel.
    pub dpr: f64,
    /// Scroll offset in logical pixels: the position, measured from the top-left corner of the
    /// displayed page, of the viewport's top-left corner. Negative components put a margin before
    /// the page. In a multi-page layout, subtract the page's own offset before building this.
    pub scroll: Vec2,
}

impl Default for ViewTransform {
    fn default() -> Self {
        ViewTransform {
            zoom: 1.0,
            dpr: 1.0,
            scroll: Vec2::ZERO,
        }
    }
}

impl ViewTransform {
    pub fn new(zoom: f64, dpr: f64, scroll: Vec2) -> ViewTransform {
        ViewTransform { zoom, dpr, scroll }
    }

    /// Logical pixels per page user-space unit (includes `/UserUnit`).
    pub fn logical_px_per_unit(&self, page: &PageGeometry) -> f64 {
        self.zoom * page.user_unit() * LOGICAL_PX_PER_POINT
    }

    /// Physical pixels per page user-space unit (includes `/UserUnit` and the pixel ratio). This
    /// is the number to feed to [`crate::level_for_scale`].
    pub fn device_px_per_unit(&self, page: &PageGeometry) -> f64 {
        self.logical_px_per_unit(page) * self.dpr
    }

    /// Page space -> view space (logical pixels).
    pub fn page_to_view(&self, page: &PageGeometry) -> Affine {
        let s = self.logical_px_per_unit(page);
        Affine::translate(-self.scroll) * Affine::scale(s) * page.page_to_display()
    }

    /// View space (logical pixels) -> page space.
    pub fn view_to_page(&self, page: &PageGeometry) -> Affine {
        let s = self.logical_px_per_unit(page);
        page.display_to_page() * Affine::scale(1.0 / s) * Affine::translate(self.scroll)
    }

    /// Page space -> device space (physical pixels).
    pub fn page_to_device(&self, page: &PageGeometry) -> Affine {
        Affine::scale(self.dpr) * self.page_to_view(page)
    }

    /// Device space (physical pixels) -> page space.
    pub fn device_to_page(&self, page: &PageGeometry) -> Affine {
        self.view_to_page(page) * Affine::scale(1.0 / self.dpr)
    }

    /// Display space (user-space units, y-down) -> view space.
    pub fn display_to_view(&self, page: &PageGeometry) -> Affine {
        Affine::translate(-self.scroll) * Affine::scale(self.logical_px_per_unit(page))
    }

    /// The rectangle the whole displayed page occupies in view space.
    pub fn page_rect_in_view(&self, page: &PageGeometry) -> Rect {
        let s = self.logical_px_per_unit(page);
        let d = page.display_size();
        Rect::new(
            -self.scroll.x,
            -self.scroll.y,
            d.width * s - self.scroll.x,
            d.height * s - self.scroll.y,
        )
    }

    /// The part of display space (user-space units, y-down) that a viewport of the given logical
    /// size shows. Not clipped to the page.
    pub fn visible_display_rect(&self, page: &PageGeometry, viewport: Size) -> Rect {
        let s = self.logical_px_per_unit(page);
        Rect::new(
            self.scroll.x / s,
            self.scroll.y / s,
            (self.scroll.x + viewport.width) / s,
            (self.scroll.y + viewport.height) / s,
        )
    }

    /// The page-space bounding rectangle of what a viewport of the given logical size shows. Not
    /// clipped to the page.
    pub fn visible_page_rect(&self, page: &PageGeometry, viewport: Size) -> Rect {
        page.display_to_page()
            .transform_rect_bbox(self.visible_display_rect(page, viewport))
    }

    /// Zoom-to-cursor: returns the view at `new_zoom` in which the page point under `cursor`
    /// (view space, logical pixels) is still under `cursor`.
    ///
    /// `new_zoom` is clamped with [`clamp_zoom`]; the invariant holds for the clamped value.
    pub fn zoom_about(&self, cursor: Point, new_zoom: f64) -> ViewTransform {
        let new_zoom = clamp_zoom(new_zoom);
        let ratio = new_zoom / self.zoom;
        // display_px = cursor + scroll; it scales by `ratio`, and must still sit at `cursor`.
        let scroll = (cursor.to_vec2() + self.scroll) * ratio - cursor.to_vec2();
        ViewTransform {
            zoom: new_zoom,
            dpr: self.dpr,
            scroll,
        }
    }

    /// The same view with its scroll offset moved to the nearest whole physical pixel, so that
    /// tiles land on the pixel grid and are not resampled.
    pub fn snapped_to_device_pixels(&self) -> ViewTransform {
        let snap = |v: f64| (v * self.dpr).round() / self.dpr;
        ViewTransform {
            scroll: Vec2::new(snap(self.scroll.x), snap(self.scroll.y)),
            ..*self
        }
    }

    /// The scroll offset that centres the page in a viewport of the given logical size at this
    /// zoom. Along an axis where the page is larger than the viewport the page is still centred
    /// (callers that want "start at the top-left" clamp the result to be non-negative).
    pub fn centred_scroll(&self, page: &PageGeometry, viewport: Size) -> Vec2 {
        let s = self.logical_px_per_unit(page);
        let d = page.display_size();
        Vec2::new(
            (d.width * s - viewport.width) / 2.0,
            (d.height * s - viewport.height) / 2.0,
        )
    }
}

/// The zoom at which the displayed page width fills a viewport of the given logical size, leaving
/// `margin` logical pixels on each side. The result is clamped with [`clamp_zoom`].
pub fn fit_width_zoom(page: &PageGeometry, viewport: Size, margin: f64) -> f64 {
    let d = page.display_size_points();
    let avail = (viewport.width - 2.0 * margin).max(1.0);
    clamp_zoom(avail / (d.width * LOGICAL_PX_PER_POINT))
}

/// The zoom at which the whole displayed page fits a viewport of the given logical size, leaving
/// `margin` logical pixels on every side. The result is clamped with [`clamp_zoom`].
pub fn fit_page_zoom(page: &PageGeometry, viewport: Size, margin: f64) -> f64 {
    let d = page.display_size_points();
    let avail_w = (viewport.width - 2.0 * margin).max(1.0);
    let avail_h = (viewport.height - 2.0 * margin).max(1.0);
    let zw = avail_w / (d.width * LOGICAL_PX_PER_POINT);
    let zh = avail_h / (d.height * LOGICAL_PX_PER_POINT);
    clamp_zoom(zw.min(zh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Rotation;
    use crate::testutil::Rng;

    fn random_page(rng: &mut Rng, rotate: Rotation) -> PageGeometry {
        // Media box with a non-zero origin, crop box strictly inside it with its own non-zero
        // offset, and a UserUnit that is never 1.
        let mx = rng.range(-500.0, 500.0);
        let my = rng.range(-500.0, 500.0);
        let mw = rng.range(200.0, 5000.0);
        let mh = rng.range(200.0, 5000.0);
        let media = Rect::new(mx, my, mx + mw, my + mh);
        let cx0 = mx + rng.range(1.0, mw * 0.3);
        let cy0 = my + rng.range(1.0, mh * 0.3);
        let cx1 = mx + mw - rng.range(1.0, mw * 0.3);
        let cy1 = my + mh - rng.range(1.0, mh * 0.3);
        let user_unit = *rng.pick(&[0.25, 0.5, 1.5, 2.0, 3.7, 10.0]);
        PageGeometry::new(
            media,
            Some(Rect::new(cx0, cy0, cx1, cy1)),
            rotate,
            user_unit,
        )
    }

    fn random_view(rng: &mut Rng) -> ViewTransform {
        ViewTransform {
            zoom: rng.log_range(MIN_ZOOM, MAX_ZOOM),
            dpr: *rng.pick(&[1.0, 1.25, 1.5, 2.0, 3.0]),
            scroll: Vec2::new(rng.range(-2000.0, 20000.0), rng.range(-2000.0, 20000.0)),
        }
    }

    /// Tolerance for a round trip, scaled to the magnitudes involved.
    fn tol(scale: f64) -> f64 {
        1e-9 * scale.max(1.0)
    }

    #[test]
    fn hundred_percent_is_physical_size() {
        let page = PageGeometry::simple(612.0, 792.0);
        let v = ViewTransform::default();
        assert!((v.logical_px_per_unit(&page) - 96.0 / 72.0).abs() < 1e-15);
        let r = v.page_rect_in_view(&page);
        assert!((r.width() - 816.0).abs() < 1e-9); // 8.5 in * 96
        assert!((r.height() - 1056.0).abs() < 1e-9); // 11 in * 96
    }

    #[test]
    fn user_unit_scales_the_view() {
        let a = PageGeometry::new(Rect::new(0.0, 0.0, 100.0, 100.0), None, Rotation::R0, 1.0);
        let b = PageGeometry::new(Rect::new(0.0, 0.0, 100.0, 100.0), None, Rotation::R0, 4.0);
        let v = ViewTransform::new(1.5, 2.0, Vec2::ZERO);
        assert!((v.logical_px_per_unit(&b) / v.logical_px_per_unit(&a) - 4.0).abs() < 1e-12);
        assert!((v.device_px_per_unit(&a) - 1.5 * 2.0 * 96.0 / 72.0).abs() < 1e-12);
    }

    #[test]
    fn top_left_of_displayed_page_is_at_minus_scroll() {
        let mut rng = Rng::new(11);
        for rotate in Rotation::ALL {
            for _ in 0..200 {
                let page = random_page(&mut rng, rotate);
                let view = random_view(&mut rng);
                let c = page.crop_box();
                // The page-space corner that /Rotate brings to the top-left of the display.
                let corner = match rotate {
                    Rotation::R0 => Point::new(c.x0, c.y1),
                    Rotation::R90 => Point::new(c.x0, c.y0),
                    Rotation::R180 => Point::new(c.x1, c.y0),
                    Rotation::R270 => Point::new(c.x1, c.y1),
                };
                let v = view.page_to_view(&page) * corner;
                let t = tol(view.scroll.hypot());
                assert!((v.x + view.scroll.x).abs() < t, "{rotate:?} {v:?}");
                assert!((v.y + view.scroll.y).abs() < t, "{rotate:?} {v:?}");

                // And the bounding box of the crop box is exactly page_rect_in_view.
                let bbox = view.page_to_view(&page).transform_rect_bbox(c);
                let want = view.page_rect_in_view(&page);
                let t = tol(want.x1.abs().max(want.y1.abs()).max(view.scroll.hypot()));
                assert!((bbox.x0 - want.x0).abs() < t && (bbox.y0 - want.y0).abs() < t);
                assert!((bbox.x1 - want.x1).abs() < t && (bbox.y1 - want.y1).abs() < t);
            }
        }
    }

    /// Property: view_to_page(page_to_view(p)) == p and the reverse, at all four rotations, with
    /// a non-zero CropBox origin and UserUnit != 1.
    #[test]
    fn prop_page_view_round_trip() {
        let mut rng = Rng::new(1);
        for rotate in Rotation::ALL {
            for _ in 0..1000 {
                let page = random_page(&mut rng, rotate);
                assert!(page.crop_box().x0 != 0.0 && page.crop_box().y0 != 0.0);
                assert!(page.user_unit() != 1.0);
                let view = random_view(&mut rng);
                let fwd = view.page_to_view(&page);
                let back = view.view_to_page(&page);

                let m = page.media_box();
                let p = Point::new(rng.range(m.x0, m.x1), rng.range(m.y0, m.y1));
                let q = back * (fwd * p);
                // Page-space error budget: the view coordinate can be large (scroll up to 2e4,
                // page up to 5e3 units at 64x), so allow a relative error on that magnitude
                // divided by the scale.
                let s = view.logical_px_per_unit(&page);
                let v = fwd * p;
                let page_tol =
                    1e-11 * (v.to_vec2().hypot() + view.scroll.hypot()).max(1.0) / s + 1e-9;
                assert!(
                    (q - p).hypot() < page_tol,
                    "{rotate:?}: {p:?} -> {q:?} (tol {page_tol})"
                );

                let w = Point::new(rng.range(-500.0, 4000.0), rng.range(-500.0, 3000.0));
                let w2 = fwd * (back * w);
                let view_tol = 1e-11 * (w.to_vec2().hypot() + view.scroll.hypot()).max(1.0) + 1e-9;
                assert!(
                    (w2 - w).hypot() < view_tol,
                    "{rotate:?}: {w:?} -> {w2:?} (tol {view_tol})"
                );
            }
        }
    }

    /// Property: the device transform is the view transform times dpr, and round-trips too.
    #[test]
    fn prop_device_round_trip() {
        let mut rng = Rng::new(2);
        for rotate in Rotation::ALL {
            for _ in 0..500 {
                let page = random_page(&mut rng, rotate);
                let view = random_view(&mut rng);
                let c = page.crop_box();
                let p = Point::new(rng.range(c.x0, c.x1), rng.range(c.y0, c.y1));
                let v = view.page_to_view(&page) * p;
                let d = view.page_to_device(&page) * p;
                let mag = d.to_vec2().hypot().max(1.0);
                assert!((d.x - v.x * view.dpr).abs() < 1e-11 * mag);
                assert!((d.y - v.y * view.dpr).abs() < 1e-11 * mag);
                let q = view.device_to_page(&page) * d;
                let s = view.device_px_per_unit(&page);
                let page_tol = 1e-11 * (mag + view.scroll.hypot() * view.dpr) / s + 1e-9;
                assert!((q - p).hypot() < page_tol, "{rotate:?}: {p:?} -> {q:?}");
            }
        }
    }

    /// Property: the transform is a similarity with the expected scale and reversed orientation
    /// (page y-up, view y-down), whatever the rotation.
    #[test]
    fn prop_scale_and_orientation() {
        let mut rng = Rng::new(3);
        for rotate in Rotation::ALL {
            for _ in 0..300 {
                let page = random_page(&mut rng, rotate);
                let view = random_view(&mut rng);
                let t = view.page_to_view(&page);
                let s = view.logical_px_per_unit(&page);
                assert!((t.determinant() + s * s).abs() < 1e-9 * s * s);
                let a = Point::new(rng.range(-100.0, 100.0), rng.range(-100.0, 100.0));
                let b = Point::new(rng.range(-100.0, 100.0), rng.range(-100.0, 100.0));
                let d_page = (b - a).hypot();
                let d_view = (t * b - t * a).hypot();
                assert!((d_view - d_page * s).abs() < 1e-9 * (d_page * s).max(1.0));
            }
        }
    }

    /// Property: the direction "up the page" maps to the screen direction /Rotate implies.
    #[test]
    fn page_up_points_where_rotate_says() {
        let mut rng = Rng::new(4);
        for (rotate, want) in [
            (Rotation::R0, Vec2::new(0.0, -1.0)),   // up
            (Rotation::R90, Vec2::new(1.0, 0.0)),   // right
            (Rotation::R180, Vec2::new(0.0, 1.0)),  // down
            (Rotation::R270, Vec2::new(-1.0, 0.0)), // left
        ] {
            let page = random_page(&mut rng, rotate);
            let view = random_view(&mut rng);
            let t = view.page_to_view(&page);
            let c = page.crop_box().center();
            let dir = (t * (c + Vec2::new(0.0, 1.0)) - t * c).normalize();
            assert!((dir - want).hypot() < 1e-6, "{rotate:?}: {dir:?}");
        }
    }

    /// The M1 acceptance invariant: the page point under the cursor stays under the cursor,
    /// within 0.5 px, across a zoom step. (It actually holds to floating-point precision.)
    #[test]
    fn prop_zoom_to_cursor_keeps_point_fixed() {
        let mut rng = Rng::new(5);
        let mut worst: f64 = 0.0;
        for rotate in Rotation::ALL {
            for _ in 0..1000 {
                let page = random_page(&mut rng, rotate);
                let view = random_view(&mut rng);
                let cursor = Point::new(rng.range(0.0, 3840.0), rng.range(0.0, 2160.0));
                // Includes requests beyond the clamp range.
                let new_zoom = rng.log_range(MIN_ZOOM / 4.0, MAX_ZOOM * 4.0);

                let anchor = view.view_to_page(&page) * cursor;
                let zoomed = view.zoom_about(cursor, new_zoom);
                assert!(zoomed.zoom >= MIN_ZOOM && zoomed.zoom <= MAX_ZOOM);
                assert_eq!(zoomed.dpr, view.dpr);

                let after = zoomed.page_to_view(&page) * anchor;
                let err_logical = (after - cursor).hypot();
                let err_device = err_logical * view.dpr;
                worst = worst.max(err_device);
                assert!(
                    err_device < 0.5,
                    "{rotate:?}: cursor {cursor:?} moved to {after:?} ({err_device} px)"
                );
            }
        }
        // Far tighter than the acceptance bound in practice.
        assert!(worst < 1e-3, "worst zoom-to-cursor error {worst} px");
    }

    /// A long run of wheel steps in and out must not drift either.
    #[test]
    fn zoom_to_cursor_does_not_drift_over_many_steps() {
        let page = PageGeometry::new(
            Rect::new(0.0, 0.0, 2592.0, 1728.0),
            Some(Rect::new(36.0, 48.0, 2500.0, 1700.0)),
            Rotation::R90,
            2.0,
        );
        let cursor = Point::new(733.0, 411.0);
        let mut view = ViewTransform::new(1.0, 1.5, Vec2::new(300.0, 200.0));
        let anchor = view.view_to_page(&page) * cursor;
        for i in 0..400 {
            let factor = if (i / 25) % 2 == 0 { 1.1 } else { 1.0 / 1.1 };
            view = view.zoom_about(cursor, view.zoom * factor);
            let now = view.page_to_view(&page) * anchor;
            assert!((now - cursor).hypot() * view.dpr < 0.5, "step {i}");
        }
    }

    #[test]
    fn zoom_about_identity_when_zoom_unchanged() {
        let v = ViewTransform::new(2.0, 1.0, Vec2::new(10.0, 20.0));
        let z = v.zoom_about(Point::new(100.0, 100.0), 2.0);
        assert_eq!(z, v);
    }

    #[test]
    fn clamp_zoom_handles_bad_values() {
        assert_eq!(clamp_zoom(0.0), MIN_ZOOM);
        assert_eq!(clamp_zoom(1e9), MAX_ZOOM);
        assert_eq!(clamp_zoom(f64::NAN), 1.0);
        assert_eq!(clamp_zoom(f64::INFINITY), 1.0);
        assert_eq!(clamp_zoom(1.25), 1.25);
    }

    #[test]
    fn fit_width_fills_the_viewport_width() {
        let viewport = Size::new(1200.0, 700.0);
        for rotate in Rotation::ALL {
            let page = PageGeometry::new(
                Rect::new(0.0, 0.0, 612.0, 792.0),
                Some(Rect::new(12.0, 24.0, 600.0, 780.0)),
                rotate,
                2.0,
            );
            let zoom = fit_width_zoom(&page, viewport, 8.0);
            let view = ViewTransform::new(zoom, 1.0, Vec2::ZERO);
            let r = view.page_rect_in_view(&page);
            assert!((r.width() - (1200.0 - 16.0)).abs() < 1e-9, "{rotate:?}");
        }
    }

    #[test]
    fn fit_page_fits_both_axes_and_touches_one() {
        let viewport = Size::new(1200.0, 700.0);
        for rotate in Rotation::ALL {
            let page = PageGeometry::new(
                Rect::new(0.0, 0.0, 612.0, 792.0),
                Some(Rect::new(12.0, 24.0, 600.0, 780.0)),
                rotate,
                0.5,
            );
            let zoom = fit_page_zoom(&page, viewport, 10.0);
            let view = ViewTransform::new(zoom, 1.0, Vec2::ZERO);
            let r = view.page_rect_in_view(&page);
            assert!(r.width() <= 1180.0 + 1e-9 && r.height() <= 680.0 + 1e-9);
            let touches_w = (r.width() - 1180.0).abs() < 1e-9;
            let touches_h = (r.height() - 680.0).abs() < 1e-9;
            assert!(touches_w || touches_h, "{rotate:?}");
            assert!(fit_page_zoom(&page, viewport, 10.0) <= fit_width_zoom(&page, viewport, 10.0));
        }
    }

    #[test]
    fn fit_is_clamped_for_degenerate_viewports() {
        let page = PageGeometry::simple(612.0, 792.0);
        assert_eq!(fit_width_zoom(&page, Size::new(0.0, 0.0), 50.0), MIN_ZOOM);
        assert_eq!(fit_page_zoom(&page, Size::new(1e9, 1e9), 0.0), MAX_ZOOM);
    }

    #[test]
    fn centred_scroll_centres_the_page() {
        let page = PageGeometry::simple(612.0, 792.0);
        let viewport = Size::new(1000.0, 2000.0);
        let mut view = ViewTransform::new(1.0, 1.0, Vec2::ZERO);
        view.scroll = view.centred_scroll(&page, viewport);
        let r = view.page_rect_in_view(&page);
        assert!((r.x0 - (1000.0 - r.x1)).abs() < 1e-9);
        assert!((r.y0 - (2000.0 - r.y1)).abs() < 1e-9);
        assert!(view.scroll.x < 0.0 && view.scroll.y < 0.0); // page smaller than viewport
    }

    #[test]
    fn visible_page_rect_matches_corner_mapping() {
        let mut rng = Rng::new(6);
        let viewport = Size::new(1280.0, 720.0);
        for rotate in Rotation::ALL {
            let page = random_page(&mut rng, rotate);
            let view = random_view(&mut rng);
            let r = view.visible_page_rect(&page, viewport);
            let back = view.view_to_page(&page);
            let a = back * Point::new(0.0, 0.0);
            let b = back * Point::new(viewport.width, viewport.height);
            let want = Rect::from_points(a, b);
            let t = 1e-9 * (want.x1.abs() + want.y1.abs() + 1.0);
            assert!((r.x0 - want.x0).abs() < t && (r.y0 - want.y0).abs() < t);
            assert!((r.x1 - want.x1).abs() < t && (r.y1 - want.y1).abs() < t);
        }
    }

    #[test]
    fn snapping_puts_scroll_on_device_pixels() {
        let v = ViewTransform::new(1.3, 1.5, Vec2::new(10.4, -3.21));
        let s = v.snapped_to_device_pixels();
        for c in [s.scroll.x, s.scroll.y] {
            assert!(((c * 1.5) - (c * 1.5).round()).abs() < 1e-9);
        }
        assert!((s.scroll.x - v.scroll.x).abs() <= 0.5 / 1.5 + 1e-12);
        assert!((s.scroll.y - v.scroll.y).abs() <= 0.5 / 1.5 + 1e-12);
    }
}
