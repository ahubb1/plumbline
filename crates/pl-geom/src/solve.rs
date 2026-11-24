//! Solving for a transform from point pairs (calibration, compare alignment).

use std::fmt;

use kurbo::{Affine, Point, Vec2};

/// Relative tolerance for "these points coincide" / "these points are collinear".
const REL_EPS: f64 = 1e-12;

/// Why a transform could not be solved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolveError {
    /// Only one, two or three point pairs are supported.
    UnsupportedPairCount(usize),
    /// A coordinate is NaN or infinite.
    NonFinite,
    /// Two source points coincide (two-pair solve), or the three source points are collinear
    /// (three-pair solve): the transform is not determined.
    DegenerateSource,
    /// The destination points coincide or are collinear: the transform would collapse the plane
    /// and has no inverse.
    DegenerateDestination,
}

impl fmt::Display for SolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolveError::UnsupportedPairCount(n) => {
                write!(f, "a transform needs 1, 2 or 3 point pairs, got {n}")
            }
            SolveError::NonFinite => write!(f, "a point has a non-finite coordinate"),
            SolveError::DegenerateSource => {
                write!(f, "the source points coincide or lie on one line")
            }
            SolveError::DegenerateDestination => {
                write!(f, "the destination points coincide or lie on one line")
            }
        }
    }
}

impl std::error::Error for SolveError {}

fn all_finite(points: &[Point]) -> bool {
    points.iter().all(|p| p.x.is_finite() && p.y.is_finite())
}

/// The similarity (rotation, uniform scale and translation; no mirroring) that maps `src[0]` to
/// `dst[0]` and `src[1]` to `dst[1]` exactly.
pub fn similarity_from_two_points(src: [Point; 2], dst: [Point; 2]) -> Result<Affine, SolveError> {
    if !all_finite(&src) || !all_finite(&dst) {
        return Err(SolveError::NonFinite);
    }
    let s = src[1] - src[0];
    let d = dst[1] - dst[0];
    let s2 = s.hypot2();
    let d2 = d.hypot2();
    let src_mag = src[0].to_vec2().hypot2().max(src[1].to_vec2().hypot2());
    let dst_mag = dst[0].to_vec2().hypot2().max(dst[1].to_vec2().hypot2());
    if s2 <= REL_EPS * REL_EPS * src_mag || s2 == 0.0 {
        return Err(SolveError::DegenerateSource);
    }
    if d2 <= REL_EPS * REL_EPS * dst_mag || d2 == 0.0 {
        return Err(SolveError::DegenerateDestination);
    }
    // As complex numbers: z = d / s; the map is p -> z * (p - src0) + dst0.
    let a = (d.x * s.x + d.y * s.y) / s2;
    let b = (d.y * s.x - d.x * s.y) / s2;
    let tx = dst[0].x - (a * src[0].x - b * src[0].y);
    let ty = dst[0].y - (b * src[0].x + a * src[0].y);
    Ok(Affine::new([a, b, -b, a, tx, ty]))
}

/// The affine transform that maps each `src[i]` to `dst[i]` exactly. It may shear, scale
/// unevenly or mirror.
pub fn affine_from_three_points(src: [Point; 3], dst: [Point; 3]) -> Result<Affine, SolveError> {
    if !all_finite(&src) || !all_finite(&dst) {
        return Err(SolveError::NonFinite);
    }
    let (su, sv) = (src[1] - src[0], src[2] - src[0]);
    let (du, dv) = (dst[1] - dst[0], dst[2] - dst[0]);
    let collinear = |u: Vec2, v: Vec2| {
        let lens = (u.hypot2() * v.hypot2()).sqrt();
        lens == 0.0 || u.cross(v).abs() <= 1e-10 * lens
    };
    if collinear(su, sv) {
        return Err(SolveError::DegenerateSource);
    }
    if collinear(du, dv) {
        return Err(SolveError::DegenerateDestination);
    }
    // S maps the unit triangle (0,0),(1,0),(0,1) onto src; D maps it onto dst.
    let s = Affine::new([su.x, su.y, sv.x, sv.y, src[0].x, src[0].y]);
    let d = Affine::new([du.x, du.y, dv.x, dv.y, dst[0].x, dst[0].y]);
    Ok(d * s.inverse())
}

/// Solves for a transform from one, two or three `(source, destination)` pairs:
///
/// * one pair: a translation;
/// * two pairs: a similarity ([`similarity_from_two_points`]);
/// * three pairs: a full affine ([`affine_from_three_points`]).
pub fn solve_transform(pairs: &[(Point, Point)]) -> Result<Affine, SolveError> {
    match pairs {
        [(s, d)] => {
            if !all_finite(&[*s, *d]) {
                return Err(SolveError::NonFinite);
            }
            Ok(Affine::translate(*d - *s))
        }
        [(s0, d0), (s1, d1)] => similarity_from_two_points([*s0, *s1], [*d0, *d1]),
        [(s0, d0), (s1, d1), (s2, d2)] => {
            affine_from_three_points([*s0, *s1, *s2], [*d0, *d1, *d2])
        }
        other => Err(SolveError::UnsupportedPairCount(other.len())),
    }
}

/// Splits a similarity into its uniform scale and its rotation in radians, in `(-pi, pi]`
/// (counter-clockwise positive in a y-up space). For a transform that is not a similarity the
/// scale is the square root of the absolute determinant (the area scale) and the rotation is
/// that of the x axis.
pub fn similarity_parts(t: Affine) -> (f64, f64) {
    let [a, b, _, _, _, _] = t.as_coeffs();
    (t.determinant().abs().sqrt(), b.atan2(a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;
    use std::f64::consts::PI;

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn near_pt(a: Point, b: Point, tol: f64) -> bool {
        a.distance(b) <= tol
    }

    #[test]
    fn one_pair_is_a_translation() {
        let t = solve_transform(&[(p(1.0, 2.0), p(4.0, -2.0))]).unwrap();
        assert_eq!(t.as_coeffs(), [1.0, 0.0, 0.0, 1.0, 3.0, -4.0]);
    }

    #[test]
    fn similarity_known_case() {
        // Rotate 90 degrees counter-clockwise, scale 2, then move by (10, 0).
        let t =
            similarity_from_two_points([p(0.0, 0.0), p(1.0, 0.0)], [p(10.0, 0.0), p(10.0, 2.0)])
                .unwrap();
        assert!(near_pt(t * p(0.0, 1.0), p(8.0, 0.0), 1e-12));
        assert!(near_pt(t * p(1.0, 1.0), p(8.0, 2.0), 1e-12));
        let (scale, rot) = similarity_parts(t);
        assert!((scale - 2.0).abs() < 1e-12);
        assert!((rot - PI / 2.0).abs() < 1e-12);
    }

    #[test]
    fn similarity_never_mirrors() {
        let t = similarity_from_two_points([p(0.0, 0.0), p(5.0, 0.0)], [p(0.0, 0.0), p(-5.0, 0.0)])
            .unwrap();
        // A half turn, not a reflection.
        assert!(t.determinant() > 0.0);
        assert!(near_pt(t * p(0.0, 1.0), p(0.0, -1.0), 1e-12));
    }

    /// Property: a random similarity is recovered from two of its point pairs, and then maps any
    /// third point correctly.
    #[test]
    fn prop_similarity_recovers_transform() {
        let mut rng = Rng::new(41);
        for _ in 0..500 {
            let k = rng.log_range(0.01, 100.0);
            let th = rng.range(-PI, PI);
            let truth = Affine::translate((rng.range(-1e3, 1e3), rng.range(-1e3, 1e3)))
                * Affine::rotate(th)
                * Affine::scale(k);
            let s0 = p(rng.range(-500.0, 500.0), rng.range(-500.0, 500.0));
            let s1 = s0 + Vec2::from_angle(rng.range(0.0, 2.0 * PI)) * rng.range(1.0, 500.0);
            let got = similarity_from_two_points([s0, s1], [truth * s0, truth * s1]).unwrap();

            let tol = 1e-8 * k.max(1.0) * 1e3;
            assert!(near_pt(got * s0, truth * s0, tol));
            assert!(near_pt(got * s1, truth * s1, tol));
            let q = p(rng.range(-500.0, 500.0), rng.range(-500.0, 500.0));
            assert!(
                near_pt(got * q, truth * q, tol),
                "{:?} vs {:?}",
                got * q,
                truth * q
            );

            let (scale, rot) = similarity_parts(got);
            assert!((scale - k).abs() < 1e-9 * k);
            let dth = (rot - th + PI).rem_euclid(2.0 * PI) - PI;
            assert!(dth.abs() < 1e-9);
            assert!(got.determinant() > 0.0);
        }
    }

    #[test]
    fn similarity_degenerate_inputs() {
        let a = p(3.0, 4.0);
        assert_eq!(
            similarity_from_two_points([a, a], [p(0.0, 0.0), p(1.0, 0.0)]),
            Err(SolveError::DegenerateSource)
        );
        assert_eq!(
            similarity_from_two_points([p(0.0, 0.0), p(1.0, 0.0)], [a, a]),
            Err(SolveError::DegenerateDestination)
        );
        assert_eq!(
            similarity_from_two_points([p(f64::NAN, 0.0), p(1.0, 0.0)], [a, p(0.0, 0.0)]),
            Err(SolveError::NonFinite)
        );
        // Coincident up to rounding at a large offset is still degenerate.
        let far = p(1e9, 1e9);
        let far2 = p(1e9 + 1e-7, 1e9);
        assert_eq!(
            similarity_from_two_points([far, far2], [p(0.0, 0.0), p(1.0, 0.0)]),
            Err(SolveError::DegenerateSource)
        );
    }

    #[test]
    fn affine_known_case() {
        // Unit triangle onto a sheared, mirrored one.
        let src = [p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0)];
        let dst = [p(5.0, 5.0), p(7.0, 5.0), p(6.0, 2.0)];
        let t = affine_from_three_points(src, dst).unwrap();
        assert_eq!(t.as_coeffs(), [2.0, 0.0, 1.0, -3.0, 5.0, 5.0]);
        assert!(t.determinant() < 0.0);
    }

    /// Property: a random invertible affine is recovered from three of its point pairs.
    #[test]
    fn prop_affine_recovers_transform() {
        let mut rng = Rng::new(42);
        let mut done = 0;
        while done < 500 {
            let truth = Affine::new([
                rng.range(-3.0, 3.0),
                rng.range(-3.0, 3.0),
                rng.range(-3.0, 3.0),
                rng.range(-3.0, 3.0),
                rng.range(-1e3, 1e3),
                rng.range(-1e3, 1e3),
            ]);
            if truth.determinant().abs() < 0.2 {
                continue;
            }
            let s0 = p(rng.range(-500.0, 500.0), rng.range(-500.0, 500.0));
            let th = rng.range(0.0, 2.0 * PI);
            let s1 = s0 + Vec2::from_angle(th) * rng.range(10.0, 300.0);
            let s2 = s0 + Vec2::from_angle(th + rng.range(0.3, PI - 0.3)) * rng.range(10.0, 300.0);
            let src = [s0, s1, s2];
            let dst = [truth * s0, truth * s1, truth * s2];
            let got = affine_from_three_points(src, dst).unwrap();
            for (s, d) in src.iter().zip(dst) {
                assert!(near_pt(got * *s, d, 1e-6), "{:?} vs {d:?}", got * *s);
            }
            let q = p(rng.range(-500.0, 500.0), rng.range(-500.0, 500.0));
            assert!(
                near_pt(got * q, truth * q, 1e-5),
                "{:?} vs {:?}",
                got * q,
                truth * q
            );
            done += 1;
        }
    }

    #[test]
    fn affine_degenerate_inputs() {
        let tri = [p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0)];
        let line = [p(0.0, 0.0), p(1.0, 1.0), p(5.0, 5.0)];
        let dup = [p(2.0, 2.0), p(2.0, 2.0), p(5.0, 1.0)];
        assert_eq!(
            affine_from_three_points(line, tri),
            Err(SolveError::DegenerateSource)
        );
        assert_eq!(
            affine_from_three_points(dup, tri),
            Err(SolveError::DegenerateSource)
        );
        assert_eq!(
            affine_from_three_points(tri, line),
            Err(SolveError::DegenerateDestination)
        );
        let bad = [p(0.0, f64::INFINITY), p(1.0, 0.0), p(0.0, 1.0)];
        assert_eq!(
            affine_from_three_points(tri, bad),
            Err(SolveError::NonFinite)
        );
    }

    #[test]
    fn solve_transform_dispatches_on_pair_count() {
        assert_eq!(
            solve_transform(&[]),
            Err(SolveError::UnsupportedPairCount(0))
        );
        let pair = (p(0.0, 0.0), p(1.0, 1.0));
        assert_eq!(
            solve_transform(&[pair; 4]),
            Err(SolveError::UnsupportedPairCount(4))
        );
        assert_eq!(
            solve_transform(&[(p(f64::NAN, 0.0), p(0.0, 0.0))]),
            Err(SolveError::NonFinite)
        );

        let two = [(p(0.0, 0.0), p(1.0, 1.0)), (p(1.0, 0.0), p(1.0, 3.0))];
        let t = solve_transform(&two).unwrap();
        assert!(near_pt(t * two[0].0, two[0].1, 1e-12));
        assert!(near_pt(t * two[1].0, two[1].1, 1e-12));

        let three = [
            (p(0.0, 0.0), p(1.0, 1.0)),
            (p(1.0, 0.0), p(1.0, 3.0)),
            (p(0.0, 1.0), p(-4.0, 0.5)),
        ];
        let t = solve_transform(&three).unwrap();
        for (s, d) in three {
            assert!(near_pt(t * s, d, 1e-12));
        }
    }

    #[test]
    fn errors_have_messages() {
        for e in [
            SolveError::UnsupportedPairCount(7),
            SolveError::NonFinite,
            SolveError::DegenerateSource,
            SolveError::DegenerateDestination,
        ] {
            assert!(!e.to_string().is_empty());
        }
    }
}
