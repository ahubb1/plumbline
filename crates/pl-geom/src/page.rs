//! Page geometry: boxes, `/Rotate`, `/UserUnit`, and the page <-> display transform.

use kurbo::{Affine, Rect, Size};

/// A page rotation, clockwise, as stored in `/Rotate`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    /// All four rotations, in clockwise order.
    pub const ALL: [Rotation; 4] = [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270];

    /// Converts a `/Rotate` value. Any multiple of 90 is accepted (negative values and values
    /// beyond 360 are normalised); anything else returns `None`.
    pub fn from_degrees(degrees: i32) -> Option<Rotation> {
        match degrees.rem_euclid(360) {
            0 => Some(Rotation::R0),
            90 => Some(Rotation::R90),
            180 => Some(Rotation::R180),
            270 => Some(Rotation::R270),
            _ => None,
        }
    }

    /// The clockwise rotation in degrees: 0, 90, 180 or 270.
    pub fn degrees(self) -> i32 {
        match self {
            Rotation::R0 => 0,
            Rotation::R90 => 90,
            Rotation::R180 => 180,
            Rotation::R270 => 270,
        }
    }

    /// The number of clockwise quarter turns, 0..=3.
    pub fn quarter_turns(self) -> u8 {
        (self.degrees() / 90) as u8
    }

    /// This rotation followed by `other` (both clockwise).
    pub fn then(self, other: Rotation) -> Rotation {
        Rotation::ALL[((self.quarter_turns() + other.quarter_turns()) % 4) as usize]
    }

    /// The rotation that undoes this one.
    pub fn inverse(self) -> Rotation {
        Rotation::ALL[((4 - self.quarter_turns()) % 4) as usize]
    }

    /// True for 90 and 270, where the displayed page has width and height exchanged.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::R90 | Rotation::R270)
    }
}

/// The geometry of one page, as read from the file.
///
/// Invariants kept by [`PageGeometry::new`]: both boxes are normalised (`x0 <= x1`, `y0 <= y1`),
/// the crop box lies inside the media box and has positive area, and `user_unit` is finite and
/// positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeometry {
    media_box: Rect,
    crop_box: Rect,
    rotate: Rotation,
    user_unit: f64,
}

impl PageGeometry {
    /// Builds a page geometry.
    ///
    /// * `media_box` is normalised. A degenerate media box falls back to US Letter (612 x 792).
    /// * `crop_box`, when given, is normalised and clipped to the media box, as PDF requires. A
    ///   missing crop box, or one that does not overlap the media box, means "the media box".
    /// * A `user_unit` that is not finite and positive is replaced by 1.
    pub fn new(
        media_box: Rect,
        crop_box: Option<Rect>,
        rotate: Rotation,
        user_unit: f64,
    ) -> PageGeometry {
        let mut media = media_box.abs();
        let finite = [media.x0, media.y0, media.x1, media.y1]
            .iter()
            .all(|v| v.is_finite());
        if !finite || media.width() <= 0.0 || media.height() <= 0.0 {
            media = Rect::new(0.0, 0.0, 612.0, 792.0);
        }
        let crop = crop_box
            .map(|c| c.abs().intersect(media))
            .filter(|c| {
                c.width() > 0.0
                    && c.height() > 0.0
                    && c.width().is_finite()
                    && c.height().is_finite()
            })
            .unwrap_or(media);
        let user_unit = if user_unit.is_finite() && user_unit > 0.0 {
            user_unit
        } else {
            1.0
        };
        PageGeometry {
            media_box: media,
            crop_box: crop,
            rotate,
            user_unit,
        }
    }

    /// A plain page: media box `0 0 width height`, no crop box, no rotation, `UserUnit` 1.
    pub fn simple(width: f64, height: f64) -> PageGeometry {
        PageGeometry::new(Rect::new(0.0, 0.0, width, height), None, Rotation::R0, 1.0)
    }

    /// The normalised media box, in page space.
    pub fn media_box(&self) -> Rect {
        self.media_box
    }

    /// The effective crop box (the visible region), in page space. Equal to the media box when
    /// the page has no crop box.
    pub fn crop_box(&self) -> Rect {
        self.crop_box
    }

    /// The page's `/Rotate`.
    pub fn rotate(&self) -> Rotation {
        self.rotate
    }

    /// The page's `/UserUnit`: points per user-space unit.
    pub fn user_unit(&self) -> f64 {
        self.user_unit
    }

    /// The same page shown with an extra clockwise rotation (a view rotation that is not saved).
    pub fn rotated_by(&self, extra: Rotation) -> PageGeometry {
        PageGeometry {
            rotate: self.rotate.then(extra),
            ..*self
        }
    }

    /// The size of the page as displayed (crop box, after rotation), in user-space units.
    pub fn display_size(&self) -> Size {
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        if self.rotate.swaps_axes() {
            Size::new(h, w)
        } else {
            Size::new(w, h)
        }
    }

    /// The size of the page as displayed, in PDF points (1/72 inch): [`Self::display_size`] times
    /// the user unit. This is the physical paper size.
    pub fn display_size_points(&self) -> Size {
        let s = self.display_size();
        Size::new(s.width * self.user_unit, s.height * self.user_unit)
    }

    /// Page space -> display space.
    ///
    /// Display space is y-down with its origin at the top-left corner of the *displayed* crop box,
    /// in user-space units (no zoom, no user unit). The transform applies the CropBox offset, the
    /// y flip and `/Rotate`.
    pub fn page_to_display(&self) -> Affine {
        let c = self.crop_box;
        match self.rotate {
            // dx = x - x0, dy = y1 - y
            Rotation::R0 => Affine::new([1.0, 0.0, 0.0, -1.0, -c.x0, c.y1]),
            // dx = y - y0, dy = x - x0
            Rotation::R90 => Affine::new([0.0, 1.0, 1.0, 0.0, -c.y0, -c.x0]),
            // dx = x1 - x, dy = y - y0
            Rotation::R180 => Affine::new([-1.0, 0.0, 0.0, 1.0, c.x1, -c.y0]),
            // dx = y1 - y, dy = x1 - x
            Rotation::R270 => Affine::new([0.0, -1.0, -1.0, 0.0, c.y1, c.x1]),
        }
    }

    /// Display space -> page space; the exact inverse of [`Self::page_to_display`].
    pub fn display_to_page(&self) -> Affine {
        let c = self.crop_box;
        match self.rotate {
            // x = dx + x0, y = y1 - dy
            Rotation::R0 => Affine::new([1.0, 0.0, 0.0, -1.0, c.x0, c.y1]),
            // x = dy + x0, y = dx + y0
            Rotation::R90 => Affine::new([0.0, 1.0, 1.0, 0.0, c.x0, c.y0]),
            // x = x1 - dx, y = dy + y0
            Rotation::R180 => Affine::new([-1.0, 0.0, 0.0, 1.0, c.x1, c.y0]),
            // x = x1 - dy, y = y1 - dx
            Rotation::R270 => Affine::new([0.0, -1.0, -1.0, 0.0, c.x1, c.y1]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Point;

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9
    }

    #[test]
    fn rotation_from_degrees_normalises() {
        assert_eq!(Rotation::from_degrees(0), Some(Rotation::R0));
        assert_eq!(Rotation::from_degrees(450), Some(Rotation::R90));
        assert_eq!(Rotation::from_degrees(-90), Some(Rotation::R270));
        assert_eq!(Rotation::from_degrees(-180), Some(Rotation::R180));
        assert_eq!(Rotation::from_degrees(720), Some(Rotation::R0));
        assert_eq!(Rotation::from_degrees(45), None);
        assert_eq!(Rotation::from_degrees(91), None);
    }

    #[test]
    fn rotation_compose_and_inverse() {
        for a in Rotation::ALL {
            assert_eq!(a.then(a.inverse()), Rotation::R0);
            for b in Rotation::ALL {
                assert_eq!(a.then(b).degrees(), (a.degrees() + b.degrees()) % 360);
            }
        }
        assert!(Rotation::R90.swaps_axes() && Rotation::R270.swaps_axes());
        assert!(!Rotation::R0.swaps_axes() && !Rotation::R180.swaps_axes());
    }

    #[test]
    fn new_normalises_and_clips() {
        // Media box given with swapped corners; crop box pokes outside.
        let g = PageGeometry::new(
            Rect::new(612.0, 792.0, 0.0, 0.0),
            Some(Rect::new(-50.0, 100.0, 300.0, 900.0)),
            Rotation::R0,
            1.0,
        );
        assert_eq!(g.media_box(), Rect::new(0.0, 0.0, 612.0, 792.0));
        assert_eq!(g.crop_box(), Rect::new(0.0, 100.0, 300.0, 792.0));
    }

    #[test]
    fn new_falls_back_on_bad_input() {
        let g = PageGeometry::new(
            Rect::new(0.0, 0.0, 100.0, 200.0),
            Some(Rect::new(500.0, 500.0, 600.0, 600.0)),
            Rotation::R0,
            0.0,
        );
        assert_eq!(g.crop_box(), g.media_box());
        assert_eq!(g.user_unit(), 1.0);

        let g = PageGeometry::new(Rect::new(5.0, 5.0, 5.0, 5.0), None, Rotation::R0, f64::NAN);
        assert_eq!(g.media_box(), Rect::new(0.0, 0.0, 612.0, 792.0));
        assert_eq!(g.user_unit(), 1.0);

        let g = PageGeometry::new(
            Rect::new(0.0, 0.0, f64::INFINITY, 10.0),
            None,
            Rotation::R0,
            -2.0,
        );
        assert_eq!(g.media_box(), Rect::new(0.0, 0.0, 612.0, 792.0));
    }

    #[test]
    fn display_size_swaps_for_quarter_turns() {
        let media = Rect::new(0.0, 0.0, 612.0, 792.0);
        let crop = Some(Rect::new(10.0, 20.0, 310.0, 420.0));
        for r in Rotation::ALL {
            let g = PageGeometry::new(media, crop, r, 2.5);
            let s = g.display_size();
            if r.swaps_axes() {
                assert_eq!((s.width, s.height), (400.0, 300.0));
            } else {
                assert_eq!((s.width, s.height), (300.0, 400.0));
            }
            let p = g.display_size_points();
            assert_eq!((p.width, p.height), (s.width * 2.5, s.height * 2.5));
        }
    }

    /// Where each corner of the crop box lands, per rotation. The crop box is 300 wide and 400
    /// tall, with a non-zero origin.
    #[test]
    fn corners_land_where_rotate_says() {
        let media = Rect::new(-20.0, -30.0, 700.0, 900.0);
        let crop = Rect::new(10.0, 20.0, 310.0, 420.0);
        let bl = Point::new(10.0, 20.0);
        let br = Point::new(310.0, 20.0);
        let tl = Point::new(10.0, 420.0);
        let tr = Point::new(310.0, 420.0);

        // (rotation, display position of [page bottom-left, bottom-right, top-left, top-right])
        let cases = [
            (
                Rotation::R0,
                [(0.0, 400.0), (300.0, 400.0), (0.0, 0.0), (300.0, 0.0)],
            ),
            // Clockwise quarter turn: the page's top edge becomes the right edge.
            (
                Rotation::R90,
                [(0.0, 0.0), (0.0, 300.0), (400.0, 0.0), (400.0, 300.0)],
            ),
            (
                Rotation::R180,
                [(300.0, 0.0), (0.0, 0.0), (300.0, 400.0), (0.0, 400.0)],
            ),
            // Three clockwise quarter turns: the page's top edge becomes the left edge.
            (
                Rotation::R270,
                [(400.0, 300.0), (400.0, 0.0), (0.0, 300.0), (0.0, 0.0)],
            ),
        ];
        for (r, expect) in cases {
            let g = PageGeometry::new(media, Some(crop), r, 1.0);
            let t = g.page_to_display();
            for (p, e) in [bl, br, tl, tr].into_iter().zip(expect) {
                let d = t * p;
                assert!(
                    close(d, Point::new(e.0, e.1)),
                    "{r:?}: {p:?} -> {d:?}, want {e:?}"
                );
            }
        }
    }

    #[test]
    fn display_to_page_is_exact_inverse() {
        let media = Rect::new(-20.0, -30.0, 700.0, 900.0);
        let crop = Rect::new(10.5, 20.25, 310.75, 420.125);
        for r in Rotation::ALL {
            let g = PageGeometry::new(media, Some(crop), r, 3.0);
            let f = g.page_to_display();
            let b = g.display_to_page();
            for p in [
                Point::new(0.0, 0.0),
                Point::new(123.456, 78.9),
                Point::new(-5.0, 1000.0),
            ] {
                assert!(close(b * (f * p), p));
                assert!(close(f * (b * p), p));
            }
            // A y-flip composed with a rotation always reverses orientation.
            assert_eq!(f.determinant(), -1.0);
            // The displayed crop box starts at the display origin.
            let bbox = f.transform_rect_bbox(g.crop_box());
            let s = g.display_size();
            assert!(close(Point::new(bbox.x0, bbox.y0), Point::ZERO));
            assert!(close(
                Point::new(bbox.x1, bbox.y1),
                Point::new(s.width, s.height)
            ));
        }
    }

    #[test]
    fn rotated_by_adds_view_rotation() {
        let g = PageGeometry::new(Rect::new(0.0, 0.0, 100.0, 200.0), None, Rotation::R270, 1.0);
        assert_eq!(g.rotated_by(Rotation::R90).rotate(), Rotation::R0);
        assert_eq!(g.rotated_by(Rotation::R180).rotate(), Rotation::R90);
    }
}
