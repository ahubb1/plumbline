//! What a tool wants to snap to, and the Shift constraint in steps of any angle.
//!
//! Tools never search for snap points themselves: the caller looks near the pointer (pl-scene
//! for the drawing's content, the markups for their own points) and hands the best one in as
//! [`Pointer::snap`](crate::Pointer::snap). [`ToolKind::snap_kinds`](crate::ToolKind) says
//! which kinds of point are worth offering to a tool.

use pl_geom::{Point, Rotation, Vec2};

use crate::geom::{constrain_angle, to_display, to_page};

/// A kind of point a snap search can return. The names and priority are those of pl-scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnapKind {
    /// The end of a segment: a corner, a vertex.
    Endpoint,
    /// Where two segments cross.
    Intersection,
    /// The middle of a segment.
    Midpoint,
    /// The point of a segment nearest the pointer.
    Nearest,
}

impl SnapKind {
    /// Every kind, the one to prefer first.
    pub const PRIORITY: [SnapKind; 4] = [
        SnapKind::Endpoint,
        SnapKind::Intersection,
        SnapKind::Midpoint,
        SnapKind::Nearest,
    ];
}

/// A set of [`SnapKind`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SnapKinds {
    pub endpoint: bool,
    pub intersection: bool,
    pub midpoint: bool,
    pub nearest: bool,
}

impl SnapKinds {
    /// Nothing: the tool ignores snap points (freehand and text selection).
    pub const NONE: SnapKinds = SnapKinds {
        endpoint: false,
        intersection: false,
        midpoint: false,
        nearest: false,
    };
    /// Every kind.
    pub const ALL: SnapKinds = SnapKinds {
        endpoint: true,
        intersection: true,
        midpoint: true,
        nearest: true,
    };
    /// The defining points of the drawing: ends, crossings and middles.
    pub const POINTS: SnapKinds = SnapKinds {
        endpoint: true,
        intersection: true,
        midpoint: true,
        nearest: false,
    };
    /// Corners only: ends and crossings.
    pub const CORNERS: SnapKinds = SnapKinds {
        endpoint: true,
        intersection: true,
        midpoint: false,
        nearest: false,
    };

    pub fn contains(self, kind: SnapKind) -> bool {
        match kind {
            SnapKind::Endpoint => self.endpoint,
            SnapKind::Intersection => self.intersection,
            SnapKind::Midpoint => self.midpoint,
            SnapKind::Nearest => self.nearest,
        }
    }

    /// The kinds in the set, in [`SnapKind::PRIORITY`] order.
    pub fn kinds(self) -> Vec<SnapKind> {
        SnapKind::PRIORITY
            .into_iter()
            .filter(|k| self.contains(*k))
            .collect()
    }

    pub fn is_empty(self) -> bool {
        self == SnapKinds::NONE
    }
}

/// `p` moved onto the nearest direction from `anchor` that is a whole number of `step_degrees`
/// from the screen's horizontal, as the page is displayed with `rotation`, by perpendicular
/// projection.
///
/// A step of 45 degrees is [`constrain_angle`] exactly. A step of 90 is "ortho": the other
/// coordinate is exactly the anchor's. Directions on the screen's axes are always exact; others
/// are as exact as `cos` and `sin` are. A step that is not a finite number above zero leaves `p`
/// as it is.
pub fn constrain_step(anchor: Point, p: Point, rotation: Rotation, step_degrees: f64) -> Point {
    if !(step_degrees.is_finite() && step_degrees > 0.0) {
        return p;
    }
    if (step_degrees - 45.0).abs() < 1e-9 {
        return constrain_angle(anchor, p, rotation);
    }
    let d = to_display(rotation, p - anchor);
    if d.x == 0.0 && d.y == 0.0 {
        return anchor;
    }
    let c = if (step_degrees - 90.0).abs() < 1e-9 {
        if d.x.abs() >= d.y.abs() {
            Vec2::new(d.x, 0.0)
        } else {
            Vec2::new(0.0, d.y)
        }
    } else {
        let step = step_degrees.to_radians();
        let k = (d.y.atan2(d.x) / step).round();
        let degrees = (k * step_degrees).rem_euclid(360.0);
        // The screen's axes exactly; any other direction through cos and sin.
        let dir = if (degrees % 90.0).abs() < 1e-9 || (90.0 - degrees % 90.0).abs() < 1e-9 {
            match ((degrees / 90.0).round() as i64).rem_euclid(4) {
                0 => Vec2::new(1.0, 0.0),
                1 => Vec2::new(0.0, 1.0),
                2 => Vec2::new(-1.0, 0.0),
                _ => Vec2::new(0.0, -1.0),
            }
        } else {
            let t = k * step;
            Vec2::new(t.cos(), t.sin())
        };
        dir * d.dot(dir)
    };
    anchor + to_page(rotation, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn steps_of_45_are_the_eight_directions_and_90_is_ortho() {
        let o = pt(100.0, 100.0);
        for r in Rotation::ALL {
            for p in [pt(180.0, 110.0), pt(160.0, 140.0), pt(40.0, 20.0)] {
                assert_eq!(constrain_step(o, p, r, 45.0), constrain_angle(o, p, r));
            }
            // Ortho never goes diagonal: (60, 40) keeps its larger offset.
            assert_eq!(
                constrain_step(o, pt(160.0, 140.0), r, 90.0),
                pt(160.0, 100.0)
            );
            assert_eq!(constrain_step(o, pt(130.0, 40.0), r, 90.0), pt(100.0, 40.0));
            assert_eq!(constrain_step(o, o, r, 90.0), o);
        }
    }

    #[test]
    fn steps_of_15_project_onto_the_nearest_direction() {
        let o = pt(0.0, 0.0);
        // On an unrotated page screen y runs down, so page (cos 30, sin 30) is 30 degrees up
        // the screen. A point at 32 degrees goes to 30, its projection.
        let a = 32f64.to_radians();
        let got = constrain_step(o, pt(10.0 * a.cos(), 10.0 * a.sin()), Rotation::R0, 15.0);
        let want = 10.0 * (2f64.to_radians()).cos();
        let t = 30f64.to_radians();
        assert!((got - pt(want * t.cos(), want * t.sin())).hypot() < 1e-12);
        // Near an axis the result is exact.
        assert_eq!(
            constrain_step(o, pt(50.0, 3.0), Rotation::R0, 15.0),
            pt(50.0, 0.0)
        );
        assert_eq!(
            constrain_step(o, pt(3.0, -50.0), Rotation::R90, 15.0),
            pt(0.0, -50.0)
        );
        // A useless step changes nothing.
        assert_eq!(
            constrain_step(o, pt(3.0, 4.0), Rotation::R0, 0.0),
            pt(3.0, 4.0)
        );
    }

    #[test]
    fn snap_sets_list_their_kinds_in_priority_order() {
        assert_eq!(SnapKinds::ALL.kinds(), SnapKind::PRIORITY);
        assert_eq!(
            SnapKinds::CORNERS.kinds(),
            [SnapKind::Endpoint, SnapKind::Intersection]
        );
        assert!(SnapKinds::NONE.is_empty() && SnapKinds::NONE.kinds().is_empty());
        assert!(SnapKinds::POINTS.contains(SnapKind::Midpoint));
        assert!(!SnapKinds::POINTS.contains(SnapKind::Nearest));
    }
}
