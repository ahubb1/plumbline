//! Measurement commands: making a measurement on the scale that applies where it is, and the
//! changes a takeoff makes to one (new geometry, cutouts, a depth, count markers).
//!
//! Every function here is pure: it reads the annotations and the scales and returns the
//! [`Command`] to apply, or why the change cannot be made. Applying it is one undo step and is
//! journaled like any command. The quantity is never stored: [`crate::Editor`] works a
//! measurement's text out afresh from its geometry and scale whenever either changes.

use pl_annot::{
    AnnotId, AnnotSet, Annotation, CountSymbol, Geometry, MeasureKind, MeasureSpec, Origin,
    PdfDate, Quantities, ScaleRef,
};
use pl_geom::Point;

use crate::command::{Command, EditError};
use crate::scales::ScaleStore;

/// What to measure, in unrotated page space.
#[derive(Clone, Debug, PartialEq)]
pub enum MeasureShape {
    Length {
        a: Point,
        b: Point,
    },
    /// An open run of at least two points.
    Polylength(Vec<Point>),
    /// A closed outline of at least three points.
    Perimeter(Vec<Point>),
    Area {
        outline: Vec<Point>,
        cutouts: Vec<Vec<Point>>,
    },
    /// An area times `depth` metres.
    Volume {
        outline: Vec<Point>,
        cutouts: Vec<Vec<Point>>,
        depth: f64,
    },
    /// The angle at `vertex` between the rays to `a` and `b`.
    Angle {
        a: Point,
        vertex: Point,
        b: Point,
    },
    Radius {
        centre: Point,
        edge: Point,
    },
    Diameter {
        a: Point,
        b: Point,
    },
    /// Markers at `points` (none is allowed: markers are added later).
    Count {
        points: Vec<Point>,
        symbol: CountSymbol,
    },
}

impl MeasureShape {
    pub fn kind(&self) -> MeasureKind {
        match self {
            MeasureShape::Length { .. } => MeasureKind::Length,
            MeasureShape::Polylength(_) => MeasureKind::Polylength,
            MeasureShape::Perimeter(_) => MeasureKind::Perimeter,
            MeasureShape::Area { .. } => MeasureKind::Area,
            MeasureShape::Volume { .. } => MeasureKind::Volume,
            MeasureShape::Angle { .. } => MeasureKind::Angle,
            MeasureShape::Radius { .. } => MeasureKind::Radius,
            MeasureShape::Diameter { .. } => MeasureKind::Diameter,
            MeasureShape::Count { .. } => MeasureKind::Count,
        }
    }

    /// The points that decide which scale applies.
    fn points(&self) -> Vec<Point> {
        match self {
            MeasureShape::Length { a, b } | MeasureShape::Diameter { a, b } => vec![*a, *b],
            MeasureShape::Radius { centre, edge } => vec![*centre, *edge],
            MeasureShape::Polylength(p) | MeasureShape::Perimeter(p) => p.clone(),
            MeasureShape::Area { outline, .. } | MeasureShape::Volume { outline, .. } => {
                outline.clone()
            }
            MeasureShape::Angle { a, vertex, b } => vec![*a, *vertex, *b],
            MeasureShape::Count { points, .. } => points.clone(),
        }
    }
}

/// Every point of a geometry, in order (rectangles as their two corners).
pub fn geometry_points(g: &Geometry) -> Vec<Point> {
    match g {
        Geometry::Rect(r) => vec![Point::new(r.x0, r.y0), Point::new(r.x1, r.y1)],
        Geometry::Line { a, b } => vec![*a, *b],
        Geometry::Points(p) => p.clone(),
        Geometry::Ink(strokes) => strokes.iter().flatten().copied().collect(),
        Geometry::Quads(quads) => quads.iter().flatten().copied().collect(),
    }
}

fn finite(points: &[Point]) -> bool {
    points.iter().all(|p| p.x.is_finite() && p.y.is_finite())
}

fn shape_error(id: &AnnotId, reason: impl Into<String>) -> EditError {
    EditError::Unsupported {
        id: id.clone(),
        reason: reason.into(),
    }
}

/// A new measurement of `shape` on `page`, on the scale that applies where it is (the topmost
/// viewport holding all its points, else the page's scale, else the document's default; see
/// [`ScaleStore::resolve`]). An angle and a count need no scale; every other kind is refused
/// on a page with none. `rotation` is the page's `/Rotate`, so the caption reads upright.
pub fn new_measurement(
    scales: &ScaleStore,
    page: usize,
    shape: MeasureShape,
    rotation: i32,
    origin: &Origin,
) -> Result<Annotation, EditError> {
    if page >= scales.page_count() {
        return Err(EditError::NoPage {
            page,
            count: scales.page_count(),
        });
    }
    let points = shape.points();
    let kind = shape.kind();
    let bad = |why: &str| EditError::Scale(format!("a {} measurement {why}", kind.name()));
    if !finite(&points) {
        return Err(bad("needs finite coordinates"));
    }
    let least = match kind {
        MeasureKind::Polylength => 2,
        MeasureKind::Perimeter | MeasureKind::Area | MeasureKind::Volume => 3,
        _ => 0,
    };
    if points.len() < least {
        return Err(bad(&format!("needs at least {least} points")));
    }
    let resolved = scales.resolve(page, &points);
    let spec = |r: &Option<(ScaleRef, MeasureSpec)>| -> Result<MeasureSpec, EditError> {
        r.as_ref().map(|(_, s)| s.clone()).ok_or_else(|| {
            EditError::Scale(format!(
                "page {} has no scale where the {} is; set the page's scale or calibrate first",
                page + 1,
                kind.name().to_lowercase()
            ))
        })
    };
    let mut a = match shape {
        MeasureShape::Length { a, b } => {
            Annotation::measure_length(page, a, b, spec(&resolved)?, origin)
        }
        MeasureShape::Polylength(p) => {
            Annotation::measure_polylength(page, p, spec(&resolved)?, origin)
        }
        MeasureShape::Perimeter(p) => {
            Annotation::measure_perimeter(page, p, spec(&resolved)?, origin)
        }
        MeasureShape::Area { outline, cutouts } => {
            check_rings(&outline, &cutouts).map_err(|e| bad(&e))?;
            Annotation::measure_area(page, outline, cutouts, spec(&resolved)?, origin)
        }
        MeasureShape::Volume {
            outline,
            cutouts,
            depth,
        } => {
            check_rings(&outline, &cutouts).map_err(|e| bad(&e))?;
            check_depth(depth).map_err(|e| bad(&e))?;
            Annotation::measure_volume(page, outline, cutouts, depth, spec(&resolved)?, origin)
        }
        MeasureShape::Angle { a, vertex, b } => Annotation::measure_angle(
            page,
            a,
            vertex,
            b,
            resolved.as_ref().map(|(_, s)| s.clone()),
            origin,
        ),
        MeasureShape::Radius { centre, edge } => {
            Annotation::measure_radius(page, centre, edge, spec(&resolved)?, origin)
        }
        MeasureShape::Diameter { a, b } => {
            Annotation::measure_diameter(page, a, b, spec(&resolved)?, origin)
        }
        MeasureShape::Count { points, symbol } => Annotation::count(page, &points, symbol, origin),
    };
    // A count needs no scale for its value, but takes the one where it is like every other
    // kind: it is written as Revu's /BBMeasure, which every Revu count item carries.
    if let Some((scale, spec)) = resolved {
        a.apply_scale(scale, spec);
    }
    a.set_rotation(rotation);
    a.sync_measurement();
    Ok(a)
}

fn check_depth(depth: f64) -> Result<(), String> {
    if depth.is_finite() && depth > 0.0 {
        Ok(())
    } else {
        Err(format!("needs a depth above zero, not {depth}"))
    }
}

/// Every cutout has at least three points, lies wholly inside the outline (on its edge
/// counts), and overlaps no other cutout: otherwise the area less the cutouts is not the area
/// the measurement covers.
fn check_rings(outline: &[Point], cutouts: &[Vec<Point>]) -> Result<(), String> {
    match pl_geom::region_problem(outline, cutouts) {
        None => Ok(()),
        Some(pl_geom::RegionProblem::BadHole(_)) => {
            Err("needs cutouts of at least three finite points".into())
        }
        Some(pl_geom::RegionProblem::HoleOutside(_)) => {
            Err("needs its cutouts inside the outline".into())
        }
        Some(pl_geom::RegionProblem::HolesOverlap(_, _)) => {
            Err("needs cutouts that do not overlap each other".into())
        }
    }
}

/// The measurement with this id, as the set has it.
fn measurement<'a>(
    set: &'a AnnotSet,
    id: &AnnotId,
    kinds: &[MeasureKind],
    what: &'static str,
) -> Result<&'a Annotation, EditError> {
    let a = set.get(id).ok_or_else(|| EditError::NotFound(id.clone()))?;
    match a.measurement() {
        Some(m) if kinds.is_empty() || kinds.contains(&m.kind) => Ok(a),
        _ => Err(EditError::NotMeasurement {
            id: id.clone(),
            what,
        }),
    }
}

fn modified(mut a: Annotation, when: PdfDate) -> Command {
    a.touch(when);
    a.sync_measurement();
    Command::modify(a)
}

/// Gives a measurement new geometry (a dragged vertex, a moved line), and puts it on the
/// scale that applies at its new place. The geometry must be the kind's own: two points for
/// a Length, Radius or Diameter, a run for a Polylength, an outline for an Area, three points
/// for an Angle, markers for a Count.
pub fn set_geometry(
    set: &AnnotSet,
    scales: &ScaleStore,
    id: &AnnotId,
    geometry: Geometry,
    when: PdfDate,
) -> Result<Command, EditError> {
    let old = measurement(set, id, &[], "a measurement")?;
    let mut a = old.detached();
    a.geometry = geometry;
    let Some(m) = a.measurement() else {
        return Err(shape_error(
            id,
            "the new geometry does not fit the measurement's kind",
        ));
    };
    if matches!(m.kind, MeasureKind::Area | MeasureKind::Volume) {
        if let Geometry::Points(outline) = &a.geometry {
            check_rings(outline, &m.cutouts).map_err(|e| shape_error(id, format!("it {e}")))?;
        }
    }
    if m.kind != MeasureKind::Count {
        let points = geometry_points(&a.geometry);
        if let Some((scale, spec)) = scales.resolve(a.page, &points) {
            a.apply_scale(scale, spec);
        }
    }
    Ok(modified(a, when))
}

/// Cuts a ring out of an Area or Volume. The ring must lie inside the outline.
pub fn add_cutout(
    set: &AnnotSet,
    id: &AnnotId,
    ring: Vec<Point>,
    when: PdfDate,
) -> Result<Command, EditError> {
    let old = measurement(
        set,
        id,
        &[MeasureKind::Area, MeasureKind::Volume],
        "an area or volume",
    )?;
    let Geometry::Points(outline) = &old.geometry else {
        return Err(shape_error(id, "it has no outline"));
    };
    let mut cutouts = old
        .measure
        .as_ref()
        .map(|m| m.cutouts.clone())
        .unwrap_or_default();
    cutouts.push(ring);
    check_rings(outline, &cutouts).map_err(|e| shape_error(id, format!("a cutout {e}")))?;
    let mut a = old.detached();
    if let Some(m) = a.measure.as_mut() {
        m.cutouts = cutouts;
    }
    Ok(modified(a, when))
}

/// Takes the cutout at `index` off an Area or Volume.
pub fn remove_cutout(
    set: &AnnotSet,
    id: &AnnotId,
    index: usize,
    when: PdfDate,
) -> Result<Command, EditError> {
    let old = measurement(
        set,
        id,
        &[MeasureKind::Area, MeasureKind::Volume],
        "an area or volume",
    )?;
    let mut a = old.detached();
    let m = a.measure.as_mut().expect("a measurement");
    if index >= m.cutouts.len() {
        return Err(shape_error(
            id,
            format!("it has {} cutout(s), not {}", m.cutouts.len(), index + 1),
        ));
    }
    m.cutouts.remove(index);
    Ok(modified(a, when))
}

/// Sets the depth of a Volume, in metres. An Area given a depth becomes a Volume.
pub fn set_depth(
    set: &AnnotSet,
    id: &AnnotId,
    metres: f64,
    when: PdfDate,
) -> Result<Command, EditError> {
    let old = measurement(
        set,
        id,
        &[MeasureKind::Area, MeasureKind::Volume],
        "an area or volume",
    )?;
    check_depth(metres).map_err(|e| shape_error(id, format!("a volume {e}")))?;
    let mut a = old.detached();
    let m = a.measure.as_mut().expect("a measurement");
    if m.kind == MeasureKind::Area {
        m.kind = MeasureKind::Volume;
        if a.meta.subject.as_deref() == Some(MeasureKind::Area.name()) {
            a.meta.subject = Some(MeasureKind::Volume.name().to_string());
        }
    }
    if let Some(m) = a.measure.as_mut() {
        m.depth = Some(metres);
    }
    Ok(modified(a, when))
}

/// Adds a marker to a Count.
pub fn add_count_marker(
    set: &AnnotSet,
    id: &AnnotId,
    p: Point,
    when: PdfDate,
) -> Result<Command, EditError> {
    let old = measurement(set, id, &[MeasureKind::Count], "a count")?;
    let mut a = old.detached();
    if !a.add_count_point(p) {
        return Err(shape_error(id, "a marker needs finite coordinates"));
    }
    Ok(modified(a, when))
}

/// Takes the marker at `index` off a Count.
pub fn remove_count_marker(
    set: &AnnotSet,
    id: &AnnotId,
    index: usize,
    when: PdfDate,
) -> Result<Command, EditError> {
    let old = measurement(set, id, &[MeasureKind::Count], "a count")?;
    let mut a = old.detached();
    let count = a.count_points().len();
    if !a.remove_count_point(index) {
        return Err(shape_error(
            id,
            format!("it has {count} marker(s), not {}", index + 1),
        ));
    }
    Ok(modified(a, when))
}

/// One measurement of a document, as a list or the command line shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasurementRow {
    pub id: AnnotId,
    pub page: usize,
    pub kind: MeasureKind,
    /// The value as its caption and `/Contents` show it.
    pub text: String,
    /// The value in SI units.
    pub quantities: Quantities,
    pub scale: ScaleRef,
    /// The scale as `/R` states it (`1 in = 20 ft`); `None` for a measurement with none.
    pub ratio: Option<String>,
}

/// Every measurement of the set, in stacking order. A count is one row however many
/// annotations carry its markers: the annotations of one [`Annotation::count_group`] on one
/// page (Bluebeam Revu writes one per marker, grouped with `/IRT` + `/RT /Group`) are the
/// row of the first of them, with the total of the group.
pub fn measurements(set: &AnnotSet) -> Vec<MeasurementRow> {
    let mut rows: Vec<MeasurementRow> = Vec::new();
    let mut groups: Vec<((usize, AnnotId), usize)> = Vec::new();
    for a in set.iter() {
        let Some(m) = a.measurement() else {
            continue;
        };
        let quantities = a.quantities().unwrap_or_default();
        let group = a
            .count_group()
            .filter(|_| !a.is_reply())
            .map(|g| (a.page, g));
        if let Some(key) = &group {
            if let Some(&(_, at)) = groups.iter().find(|(k, _)| k == key) {
                let row = &mut rows[at];
                let total = row.quantities.count.unwrap_or(0) + quantities.count.unwrap_or(0);
                row.quantities.count = Some(total);
                row.text = total.to_string();
                continue;
            }
            groups.push((key.clone(), rows.len()));
        }
        rows.push(MeasurementRow {
            id: a.id.clone(),
            page: a.page,
            kind: m.kind,
            text: a.measure_text().unwrap_or_default(),
            quantities,
            scale: m.scale.clone(),
            ratio: m.spec.as_ref().map(|s| s.ratio.clone()),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Editor;
    use crate::measure_data::ScaleSetting;
    use pl_geom::Rect;

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    /// One Letter page at 1" = 20' (one point is 20/72 ft).
    fn editor() -> Editor {
        let store = ScaleStore::blank(&[(Rect::new(0.0, 0.0, 612.0, 792.0), 1.0)]);
        let mut ed = Editor::with_scales(AnnotSet::new(), store, 1);
        ed.execute(Command::SetPageScale {
            page: 0,
            scale: Some(ScaleSetting::parse("1\" = 20'", None).unwrap()),
        })
        .unwrap();
        ed
    }

    const FT: f64 = 0.3048;

    /// A rectangle in feet at a page scale of 1" = 1' (72 points a foot).
    fn ft_rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point> {
        [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            .iter()
            .map(|(x, y)| p(x * 72.0, y * 72.0))
            .collect()
    }

    #[test]
    fn cutouts_that_overlap_repeat_contain_or_leave_the_outline_are_refused() {
        let store = ScaleStore::blank(&[(Rect::new(0.0, 0.0, 2000.0, 2000.0), 1.0)]);
        let mut ed = Editor::with_scales(AnnotSet::new(), store, 1);
        ed.execute(Command::SetPageScale {
            page: 0,
            scale: Some(ScaleSetting::parse("1\" = 1'", None).unwrap()),
        })
        .unwrap();
        let o = origin();
        let area = |outline: Vec<Point>, cutouts: Vec<Vec<Point>>| {
            new_measurement(
                ed.scales(),
                0,
                MeasureShape::Area { outline, cutouts },
                0,
                &o,
            )
        };
        let room = ft_rect(0.0, 0.0, 10.0, 10.0);
        let sq_ft = |a: &Annotation| a.quantities().unwrap().area.unwrap() / (FT * FT);
        // Two holes side by side are fine: 100 - 25 - 15.
        let ok = area(
            room.clone(),
            vec![ft_rect(1.0, 1.0, 6.0, 6.0), ft_rect(6.0, 1.0, 9.0, 6.0)],
        )
        .unwrap();
        assert!(close(sq_ft(&ok), 60.0));
        // (1) Overlapping holes.
        let err = area(
            room.clone(),
            vec![ft_rect(1.0, 1.0, 6.0, 6.0), ft_rect(4.0, 4.0, 9.0, 9.0)],
        )
        .unwrap_err();
        assert!(err.to_string().contains("overlap"), "{err}");
        // (2) The same hole twice.
        let hole = ft_rect(1.0, 1.0, 6.0, 6.0);
        assert!(area(room.clone(), vec![hole.clone(), hole.clone()]).is_err());
        // One hole holding another.
        assert!(area(
            room.clone(),
            vec![hole.clone(), ft_rect(2.0, 2.0, 3.0, 3.0)]
        )
        .is_err());
        // (3) A triangle whose corners are inside an L but whose long edge crosses the notch.
        let l: Vec<Point> = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 4.0),
            (4.0, 4.0),
            (4.0, 10.0),
            (0.0, 10.0),
        ]
        .iter()
        .map(|(x, y)| p(x * 72.0, y * 72.0))
        .collect();
        let triangle = vec![p(72.0, 72.0), p(9.0 * 72.0, 72.0), p(72.0, 9.0 * 72.0)];
        let err = area(l, vec![triangle]).unwrap_err();
        assert!(err.to_string().contains("inside the outline"), "{err}");

        // Adding a cutout checks it against the ones there already, either way round.
        let mut set = AnnotSet::new();
        let one = area(room.clone(), vec![hole.clone()]).unwrap();
        let id = set.add(one).unwrap();
        let when = PdfDate::from_unix(1_700_000_100);
        assert!(add_cutout(&set, &id, ft_rect(0.5, 0.5, 7.0, 7.0), when).is_err());
        assert!(add_cutout(&set, &id, ft_rect(2.0, 2.0, 3.0, 3.0), when).is_err());
        assert!(add_cutout(&set, &id, hole.clone(), when).is_err());
        assert!(add_cutout(&set, &id, ft_rect(7.0, 7.0, 9.0, 9.0), when).is_ok());
    }

    #[test]
    fn each_kind_gets_the_page_scale_and_its_analytic_value() {
        let ed = editor();
        let s = ed.scales();
        let o = origin();
        let make = |shape| new_measurement(s, 0, shape, 0, &o).unwrap();
        // 72 points is one inch of paper, 20 ft.
        let len = make(MeasureShape::Length {
            a: p(100.0, 100.0),
            b: p(172.0, 100.0),
        });
        assert!(close(len.quantities().unwrap().length.unwrap(), 20.0 * FT));
        assert_eq!(len.meta.contents.as_deref(), Some("20.00'"));
        // A 144 x 72 rectangle: 40 ft by 20 ft, less a 36 x 36 hole (10 ft square).
        let area = make(MeasureShape::Area {
            outline: vec![p(0.0, 0.0), p(144.0, 0.0), p(144.0, 72.0), p(0.0, 72.0)],
            cutouts: vec![vec![
                p(10.0, 10.0),
                p(46.0, 10.0),
                p(46.0, 46.0),
                p(10.0, 46.0),
            ]],
        });
        let q = area.quantities().unwrap();
        assert!(close(q.area.unwrap(), (800.0 - 100.0) * FT * FT));
        assert!(close(q.length.unwrap(), 120.0 * FT), "the outer ring only");
        let vol = make(MeasureShape::Volume {
            outline: vec![p(0.0, 0.0), p(144.0, 0.0), p(144.0, 72.0), p(0.0, 72.0)],
            cutouts: Vec::new(),
            depth: 0.5,
        });
        assert!(close(
            vol.quantities().unwrap().volume.unwrap(),
            800.0 * FT * FT * 0.5
        ));
        let angle = make(MeasureShape::Angle {
            a: p(10.0, 0.0),
            vertex: p(0.0, 0.0),
            b: p(0.0, 10.0),
        });
        assert!(close(
            angle.quantities().unwrap().angle.unwrap(),
            std::f64::consts::FRAC_PI_2
        ));
        let count = make(MeasureShape::Count {
            points: vec![p(1.0, 1.0), p(2.0, 2.0)],
            symbol: CountSymbol::Ring,
        });
        assert_eq!(count.quantities().unwrap().count, Some(2));
        assert_eq!(count.meta.contents.as_deref(), Some("2"));

        // Not enough points, a hole outside the outline, no scale.
        assert!(new_measurement(s, 0, MeasureShape::Perimeter(vec![p(0.0, 0.0)]), 0, &o).is_err());
        assert!(new_measurement(
            s,
            0,
            MeasureShape::Area {
                outline: vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)],
                cutouts: vec![vec![p(50.0, 50.0), p(60.0, 50.0), p(60.0, 60.0)]],
            },
            0,
            &o
        )
        .is_err());
        let bare = ScaleStore::blank(&[(Rect::new(0.0, 0.0, 612.0, 792.0), 1.0)]);
        let err = new_measurement(
            &bare,
            0,
            MeasureShape::Length {
                a: p(0.0, 0.0),
                b: p(1.0, 0.0),
            },
            0,
            &o,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no scale"), "{err}");
        // An angle needs none.
        assert!(new_measurement(
            &bare,
            0,
            MeasureShape::Angle {
                a: p(1.0, 0.0),
                vertex: p(0.0, 0.0),
                b: p(0.0, 1.0)
            },
            0,
            &o
        )
        .is_ok());
    }

    /// A page at 1" = 20' with viewport A at 1" = 10' over (300,500)-(500,700) and viewport B
    /// at 1" = 5' over (300,100)-(500,300).
    fn two_viewports() -> Editor {
        let mut ed = editor();
        for (name, bbox, scale) in [
            ("A", [300.0, 500.0, 500.0, 700.0], "1\" = 10'"),
            ("B", [300.0, 100.0, 500.0, 300.0], "1\" = 5'"),
        ] {
            ed.execute(Command::AddViewport {
                page: 0,
                name: name.into(),
                bbox,
                scale: Some(ScaleSetting::parse(scale, None).unwrap()),
            })
            .unwrap();
        }
        ed
    }

    fn feet_of(ed: &Editor, id: &AnnotId) -> f64 {
        ed.set()
            .get(id)
            .unwrap()
            .quantities()
            .unwrap()
            .length
            .unwrap()
            / FT
    }

    fn scale_of(ed: &Editor, id: &AnnotId) -> ScaleRef {
        ed.set()
            .get(id)
            .unwrap()
            .measurement()
            .unwrap()
            .scale
            .clone()
    }

    #[test]
    fn a_measurement_moved_or_pasted_takes_the_scale_where_it_lands() {
        let mut ed = two_viewports();
        let o = origin();
        let length = new_measurement(
            ed.scales(),
            0,
            MeasureShape::Length {
                a: p(320.0, 600.0),
                b: p(392.0, 600.0),
            },
            0,
            &o,
        )
        .unwrap();
        let id = length.id.clone();
        ed.execute(Command::add(length)).unwrap();
        assert!(close(feet_of(&ed, &id), 10.0));
        assert_eq!(scale_of(&ed, &id), ScaleRef::Viewport("A".into()));

        // Moved as the app moves it, by a plain change of geometry, wholly into B.
        let mut moved = ed.set().get(&id).unwrap().detached();
        moved.geometry = Geometry::Line {
            a: p(320.0, 200.0),
            b: p(392.0, 200.0),
        };
        ed.execute(Command::modify(moved)).unwrap();
        assert!(close(feet_of(&ed, &id), 5.0));
        assert_eq!(scale_of(&ed, &id), ScaleRef::Viewport("B".into()));
        let a = ed.set().get(&id).unwrap();
        assert_eq!(a.meta.contents, a.measure_value_text());
        // Moved half out of B: the page's scale.
        let mut moved = ed.set().get(&id).unwrap().detached();
        moved.geometry = Geometry::Line {
            a: p(450.0, 200.0),
            b: p(522.0, 200.0),
        };
        ed.execute(Command::modify(moved)).unwrap();
        assert!(close(feet_of(&ed, &id), 20.0));
        assert_eq!(scale_of(&ed, &id), ScaleRef::Page);
        // One undo step back to B.
        ed.undo().unwrap();
        assert!(close(feet_of(&ed, &id), 5.0));

        // A copy made at 1" = 20' added (pasted) into A is at A's scale.
        let mut copy = ed.set().get(&id).unwrap().detached();
        copy.id = AnnotId("pasted".into());
        copy.geometry = Geometry::Line {
            a: p(320.0, 600.0),
            b: p(392.0, 600.0),
        };
        ed.execute(Command::add(copy)).unwrap();
        let pasted = AnnotId("pasted".into());
        assert!(close(feet_of(&ed, &pasted), 10.0));
        assert_eq!(scale_of(&ed, &pasted), ScaleRef::Viewport("A".into()));

        // A change that leaves the geometry alone (its comment) does not touch the scale.
        let mut noted = ed.set().get(&pasted).unwrap().detached();
        noted.meta.subject = Some("Noted".into());
        ed.execute(Command::modify(noted)).unwrap();
        assert_eq!(scale_of(&ed, &pasted), ScaleRef::Viewport("A".into()));
    }

    #[test]
    fn a_measurement_keeps_its_own_units_while_its_scale_keeps_its_value() {
        let mut ed = editor();
        let o = origin();
        let mut length = new_measurement(
            ed.scales(),
            0,
            MeasureShape::Length {
                a: p(100.0, 100.0),
                b: p(172.0, 100.0),
            },
            0,
            &o,
        )
        .unwrap();
        // Shown in feet and inches, as a tool preset may ask, at the page's scale.
        let scale = pl_geom::Scale::parse("1\" = 20'").unwrap();
        let format = pl_annot::MeasureFormat::new(
            pl_geom::LengthFormat::architectural(16),
            pl_geom::AreaFormat::new(pl_geom::AreaUnit::SqYard, 1),
        );
        length.apply_scale(ScaleRef::Page, MeasureSpec::new(&scale, 1.0, &format));
        let id = length.id.clone();
        ed.execute(Command::add(length)).unwrap();
        let text = |ed: &Editor| ed.set().get(&id).unwrap().meta.contents.clone().unwrap();
        assert_eq!(text(&ed), "20'-0\"");
        // An unrelated viewport elsewhere, and one round it at the same scale: kept.
        ed.execute(Command::AddViewport {
            page: 0,
            name: "Far".into(),
            bbox: [400.0, 400.0, 500.0, 500.0],
            scale: Some(ScaleSetting::parse("1\" = 5'", None).unwrap()),
        })
        .unwrap();
        assert_eq!(text(&ed), "20'-0\"");
        ed.execute(Command::AddViewport {
            page: 0,
            name: "Same".into(),
            bbox: [50.0, 50.0, 300.0, 300.0],
            scale: Some(ScaleSetting::parse("1\" = 20'", None).unwrap()),
        })
        .unwrap();
        assert_eq!(text(&ed), "20'-0\"");
        assert_eq!(scale_of(&ed, &id), ScaleRef::Viewport("Same".into()));
        // A new value for its scale is taken, with that scale's presentation.
        ed.execute(Command::EditViewport {
            page: 0,
            name: "Same".into(),
            bbox: [50.0, 50.0, 300.0, 300.0],
            scale: Some(ScaleSetting::parse("1\" = 10'", None).unwrap()),
        })
        .unwrap();
        assert!(close(feet_of(&ed, &id), 10.0));
        assert_eq!(text(&ed), "10.00'");
    }

    /// Calibrating in feet and inches with no format of its own (as the command driver does)
    /// shows values in feet and inches.
    #[test]
    fn a_feet_and_inches_calibration_shows_feet_and_inches() {
        let store = ScaleStore::blank(&[(Rect::new(0.0, 0.0, 612.0, 792.0), 1.0)]);
        let mut ed = Editor::with_scales(AnnotSet::new(), store, 1);
        let scale = pl_geom::Scale::calibrate(
            p(0.0, 0.0),
            p(72.0, 0.0),
            10.0 * FT,
            1.0,
            pl_geom::Unit::FeetInches,
        )
        .unwrap();
        ed.execute(Command::SetPageScale {
            page: 0,
            scale: Some(ScaleSetting::new(&scale, None)),
        })
        .unwrap();
        let a = new_measurement(
            ed.scales(),
            0,
            MeasureShape::Length {
                a: p(0.0, 0.0),
                b: p(108.0, 0.0),
            },
            0,
            &origin(),
        )
        .unwrap();
        assert_eq!(a.meta.contents.as_deref(), Some("15'-0\""));
    }

    #[test]
    fn edits_recompute_the_value_and_undo_exactly() {
        let mut ed = editor();
        let o = origin();
        let when = PdfDate::from_unix(1_700_000_100);
        let square = vec![p(0.0, 0.0), p(72.0, 0.0), p(72.0, 72.0), p(0.0, 72.0)];
        let area = new_measurement(
            ed.scales(),
            0,
            MeasureShape::Area {
                outline: square.clone(),
                cutouts: Vec::new(),
            },
            0,
            &o,
        )
        .unwrap();
        let id = area.id.clone();
        ed.execute(Command::add(area)).unwrap();
        let value = |ed: &Editor| ed.set().get(&id).unwrap().quantities().unwrap();
        assert!(close(value(&ed).area.unwrap(), 400.0 * FT * FT));

        let hole = vec![p(0.0, 0.0), p(36.0, 0.0), p(36.0, 36.0), p(0.0, 36.0)];
        ed.execute(add_cutout(ed.set(), &id, hole, when).unwrap())
            .unwrap();
        assert!(close(value(&ed).area.unwrap(), 300.0 * FT * FT));
        ed.execute(set_depth(ed.set(), &id, 2.0, when).unwrap())
            .unwrap();
        let a = ed.set().get(&id).unwrap();
        assert_eq!(a.measurement().unwrap().kind, MeasureKind::Volume);
        assert!(close(value(&ed).volume.unwrap(), 300.0 * FT * FT * 2.0));
        // A vertex dragged: the outline doubles in width; the caption follows.
        let wide = Geometry::Points(vec![
            p(0.0, 0.0),
            p(144.0, 0.0),
            p(144.0, 72.0),
            p(0.0, 72.0),
        ]);
        ed.execute(set_geometry(ed.set(), ed.scales(), &id, wide, when).unwrap())
            .unwrap();
        assert!(close(value(&ed).area.unwrap(), 700.0 * FT * FT));
        let a = ed.set().get(&id).unwrap().clone();
        assert_eq!(a.meta.contents, a.measure_value_text());
        ed.execute(remove_cutout(ed.set(), &id, 0, when).unwrap())
            .unwrap();
        assert!(close(value(&ed).area.unwrap(), 800.0 * FT * FT));
        assert!(remove_cutout(ed.set(), &id, 0, when).is_err());
        // Wrong geometry for the kind is refused.
        assert!(set_geometry(
            ed.set(),
            ed.scales(),
            &id,
            Geometry::Line {
                a: p(0.0, 0.0),
                b: p(1.0, 1.0)
            },
            when
        )
        .is_err());
        for _ in 0..4 {
            ed.undo().unwrap();
        }
        assert!(close(value(&ed).area.unwrap(), 400.0 * FT * FT));
        assert_eq!(
            ed.set().get(&id).unwrap().measurement().unwrap().kind,
            MeasureKind::Area
        );

        // A plain modify of the geometry (a drag) recomputes the caption too.
        let mut dragged = ed.set().get(&id).unwrap().clone();
        dragged.geometry = Geometry::Points(vec![p(0.0, 0.0), p(36.0, 0.0), p(36.0, 72.0)]);
        ed.execute(Command::modify(dragged)).unwrap();
        let a = ed.set().get(&id).unwrap();
        assert!(close(value(&ed).area.unwrap(), 100.0 * FT * FT));
        assert_eq!(a.meta.contents, a.measure_value_text());

        // Counts.
        let count = new_measurement(
            ed.scales(),
            0,
            MeasureShape::Count {
                points: vec![p(5.0, 5.0)],
                symbol: CountSymbol::Dot,
            },
            0,
            &o,
        )
        .unwrap();
        // A count takes the page's scale too (written as Revu's /BBMeasure).
        let m = count.measurement().unwrap();
        assert_eq!(m.scale, ScaleRef::Page);
        assert!(m.spec.is_some());
        let cid = count.id.clone();
        ed.execute(Command::add(count)).unwrap();
        for x in [10.0, 20.0, 30.0] {
            ed.execute(add_count_marker(ed.set(), &cid, p(x, 5.0), when).unwrap())
                .unwrap();
        }
        let c = ed.set().get(&cid).unwrap();
        assert_eq!(c.quantities().unwrap().count, Some(4));
        assert_eq!(c.meta.contents.as_deref(), Some("4"));
        ed.execute(remove_count_marker(ed.set(), &cid, 0, when).unwrap())
            .unwrap();
        assert_eq!(ed.set().get(&cid).unwrap().count_points()[0], p(10.0, 5.0));
        assert!(remove_count_marker(ed.set(), &cid, 9, when).is_err());
        assert!(add_count_marker(ed.set(), &id, p(1.0, 1.0), when).is_err());
        assert_eq!(measurements(ed.set()).len(), 2);
    }

    #[test]
    fn a_revu_count_group_is_one_measurement_row_with_its_total() {
        let o = origin();
        let mut set = AnnotSet::new();
        let one = |x: f64| Annotation::count(0, &[p(x, 5.0)], CountSymbol::Dot, &o);
        let head = set.add(one(10.0)).unwrap();
        for x in [20.0, 30.0] {
            let mut a = one(x);
            a.meta.reply_to = Some(head.clone());
            a.meta.reply_type = Some("Group".into());
            set.add(a).unwrap();
        }
        // A count of its own that looks the same, and one of two markers.
        let single = set.add(one(40.0)).unwrap();
        let ours = set
            .add(Annotation::count(
                0,
                &[p(50.0, 5.0), p(60.0, 5.0)],
                CountSymbol::Dot,
                &o,
            ))
            .unwrap();
        let rows = measurements(&set);
        let got: Vec<(&AnnotId, &str, Option<usize>)> = rows
            .iter()
            .map(|r| (&r.id, r.text.as_str(), r.quantities.count))
            .collect();
        assert_eq!(
            got,
            [
                (&head, "3", Some(3)),
                (&single, "1", Some(1)),
                (&ours, "2", Some(2))
            ]
        );
    }
}
