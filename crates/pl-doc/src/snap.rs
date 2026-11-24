//! The snap service: one query per pointer position that combines the page's content
//! geometry (pl-scene) with the vertices of the markups on the page.
//!
//! Kinds are tried in priority order ([`SnapKind::PRIORITY`]) and the first kind with a
//! candidate inside the radius wins, with its closest candidate:
//!
//! 1. the defining points of a measurement (the ends of a length, the vertices of an area and
//!    its cutouts, the markers of a count),
//! 2. the vertices of other markups (corners, line ends, polyline vertices, ink stroke ends),
//! 3. content endpoints, 4. content intersections, 5. content midpoints, 6. the nearest point
//!    on content.
//!
//! The query never waits. The content scene is built on a worker thread the first time a page
//! is asked for ([`crate::Document::scene`]); until it is ready a query answers from the
//! markups alone, and says [`SnapOutcome::Pending`] when they have nothing either. A page with
//! no content snapping (a capped or failed build, an encrypted file) answers from the markups.

use pl_annot::{AnnotId, AnnotKind, AnnotSet, Annotation, Geometry, MeasureKind};
use pl_geom::Point;
use pl_scene::{Scene, SnapModes};

use crate::session::SceneState;

/// What a snap landed on, highest priority first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SnapKind {
    /// A defining point of a measurement.
    MeasureEndpoint,
    /// A vertex of a markup that is not a measurement.
    MarkupVertex,
    /// The end of a line of the drawing, or a corner.
    Endpoint,
    /// The crossing of two lines of the drawing.
    Intersection,
    /// The middle of a line of the drawing.
    Midpoint,
    /// The closest point on a line of the drawing.
    Nearest,
}

impl SnapKind {
    pub const PRIORITY: [SnapKind; 6] = [
        SnapKind::MeasureEndpoint,
        SnapKind::MarkupVertex,
        SnapKind::Endpoint,
        SnapKind::Intersection,
        SnapKind::Midpoint,
        SnapKind::Nearest,
    ];

    fn of_content(kind: pl_scene::SnapKind) -> SnapKind {
        match kind {
            pl_scene::SnapKind::Endpoint => SnapKind::Endpoint,
            pl_scene::SnapKind::Intersection => SnapKind::Intersection,
            pl_scene::SnapKind::Midpoint => SnapKind::Midpoint,
            pl_scene::SnapKind::Nearest => SnapKind::Nearest,
        }
    }

    /// A short name for the marker a user interface draws.
    pub fn name(self) -> &'static str {
        match self {
            SnapKind::MeasureEndpoint => "measurement point",
            SnapKind::MarkupVertex => "markup vertex",
            SnapKind::Endpoint => "endpoint",
            SnapKind::Intersection => "intersection",
            SnapKind::Midpoint => "midpoint",
            SnapKind::Nearest => "nearest",
        }
    }
}

/// Which sources a query may use.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapOptions {
    /// In page units (convert a pixel radius with the view's zoom and `/UserUnit`).
    pub radius: f64,
    /// The content kinds allowed; [`SnapModes::NONE`] turns content snapping off.
    pub content: SnapModes,
    pub measurements: bool,
    pub markups: bool,
    /// Whether the markers of counts are snapped to (with `measurements`). Off for the Count
    /// tool: a click snapped onto a marker would put a second one on it, a count of one more
    /// with nothing new to see.
    pub counts: bool,
    /// An annotation not to snap to: the one being drawn or dragged.
    pub exclude: Option<AnnotId>,
}

impl SnapOptions {
    /// Everything, within `radius` page units.
    pub fn all(radius: f64) -> SnapOptions {
        SnapOptions {
            radius,
            content: SnapModes::ALL,
            measurements: true,
            markups: true,
            counts: true,
            exclude: None,
        }
    }
}

/// One snap.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapTarget {
    /// The snapped point in page space. A vertex is the stored point, bit for bit.
    pub point: Point,
    pub kind: SnapKind,
    /// From the cursor, in page units.
    pub distance: f64,
    /// The markup snapped to, for the two markup kinds.
    pub annotation: Option<AnnotId>,
}

/// The answer to a snap query.
#[derive(Clone, Debug, PartialEq)]
pub enum SnapOutcome {
    Snapped(SnapTarget),
    /// Nothing is in range.
    Nothing,
    /// Nothing among the markups, and the content scene is still being built: ask again.
    Pending,
}

impl SnapOutcome {
    pub fn target(&self) -> Option<&SnapTarget> {
        match self {
            SnapOutcome::Snapped(t) => Some(t),
            _ => None,
        }
    }
}

/// The points of an annotation a snap may land on, and whether they are a measurement's.
pub fn snap_points(a: &Annotation) -> (Vec<Point>, bool) {
    if let Some(m) = a.measurement() {
        let mut points = match (&a.geometry, m.kind) {
            (_, MeasureKind::Count) => a.count_points(),
            (Geometry::Line { a, b }, _) => vec![*a, *b],
            (Geometry::Points(p), _) => p.clone(),
            _ => Vec::new(),
        };
        points.extend(m.cutouts.iter().flatten().copied());
        return (points, true);
    }
    let points = match (&a.kind, &a.geometry) {
        (AnnotKind::Popup | AnnotKind::Link | AnnotKind::Text, _) => Vec::new(),
        (k, _) if k.is_text_markup() => Vec::new(),
        (AnnotKind::Circle, Geometry::Rect(r)) => {
            let c = r.center();
            vec![
                Point::new(r.x0, c.y),
                Point::new(r.x1, c.y),
                Point::new(c.x, r.y0),
                Point::new(c.x, r.y1),
            ]
        }
        (_, Geometry::Rect(r)) => vec![
            Point::new(r.x0, r.y0),
            Point::new(r.x1, r.y0),
            Point::new(r.x1, r.y1),
            Point::new(r.x0, r.y1),
        ],
        (_, Geometry::Line { a, b }) => vec![*a, *b],
        (_, Geometry::Points(p)) => p.clone(),
        (_, Geometry::Ink(strokes)) => strokes
            .iter()
            .flat_map(|s| s.first().into_iter().chain(s.last()))
            .copied()
            .collect(),
        (_, Geometry::Quads(_)) => Vec::new(),
    };
    (points, false)
}

/// The closest markup point of each of the two markup kinds within the radius.
fn markup_candidates(
    annotations: &AnnotSet,
    page: usize,
    cursor: Point,
    options: &SnapOptions,
) -> Vec<SnapTarget> {
    let mut best: [Option<SnapTarget>; 2] = [None, None];
    for a in annotations.on_page(page) {
        if a.meta.flags.is_hidden() || options.exclude.as_ref() == Some(&a.id) {
            continue;
        }
        let (points, measured) = snap_points(a);
        if (measured && !options.measurements) || (!measured && !options.markups) {
            continue;
        }
        if !options.counts && a.measurement().is_some_and(|m| m.kind == MeasureKind::Count) {
            continue;
        }
        let slot = usize::from(!measured);
        for p in points {
            let d = p.distance(cursor);
            if d <= options.radius && best[slot].as_ref().is_none_or(|b| d < b.distance) {
                best[slot] = Some(SnapTarget {
                    point: p,
                    kind: if measured {
                        SnapKind::MeasureEndpoint
                    } else {
                        SnapKind::MarkupVertex
                    },
                    distance: d,
                    annotation: Some(a.id.clone()),
                });
            }
        }
    }
    best.into_iter().flatten().collect()
}

/// The snap at `cursor` on `page` from the markups in `annotations` (if they are loaded) and
/// the page's content scene in whatever state it is.
pub fn snap(
    annotations: Option<&AnnotSet>,
    scene: &SceneState,
    page: usize,
    cursor: Point,
    options: &SnapOptions,
) -> SnapOutcome {
    if !(options.radius.is_finite()
        && options.radius >= 0.0
        && cursor.x.is_finite()
        && cursor.y.is_finite())
    {
        return SnapOutcome::Nothing;
    }
    let markup = match annotations {
        Some(set) => markup_candidates(set, page, cursor, options)
            .into_iter()
            .next(),
        None => None,
    };
    // A point of a measurement outranks every content snap: measurements chain on.
    if let Some(m) = markup.as_ref().filter(|m| m.kind == SnapKind::MeasureEndpoint) {
        return SnapOutcome::Snapped(m.clone());
    }
    let content = match scene {
        SceneState::Ready(scene) => content_candidates(scene, cursor, options)
            .into_iter()
            .min_by_key(|t| t.kind),
        SceneState::Pending => {
            return markup.map_or(SnapOutcome::Pending, SnapOutcome::Snapped);
        }
        SceneState::Unavailable(_) => None,
    };
    match (markup, content) {
        // The corner of a text box or a stamp gives way to an endpoint or an intersection of
        // the drawing that is closer: that is what is being measured.
        (Some(m), Some(c))
            if matches!(c.kind, SnapKind::Endpoint | SnapKind::Intersection)
                && c.distance < m.distance =>
        {
            SnapOutcome::Snapped(c)
        }
        (Some(m), _) => SnapOutcome::Snapped(m),
        (None, Some(c)) => SnapOutcome::Snapped(c),
        (None, None) if annotations.is_none() => SnapOutcome::Pending,
        (None, None) => SnapOutcome::Nothing,
    }
}

fn content_candidates(scene: &Scene, cursor: Point, options: &SnapOptions) -> Vec<SnapTarget> {
    scene
        .snap_candidates(cursor, options.radius, options.content)
        .into_iter()
        .map(|hit| SnapTarget {
            point: hit.point,
            kind: SnapKind::of_content(hit.kind),
            distance: hit.distance,
            annotation: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_annot::{CountSymbol, Origin, PdfDate};
    use pl_geom::Rect;
    use pl_scene::Segment;
    use std::sync::Arc;

    fn o() -> Origin {
        Origin::new("T", PdfDate::from_unix(0))
    }

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn cross() -> SceneState {
        // An X through (50, 50) and a separate line from (100, 0) to (200, 0).
        SceneState::Ready(Arc::new(Scene::from_segments(vec![
            Segment::line(p(0.0, 0.0), p(100.0, 100.0)),
            Segment::line(p(0.0, 100.0), p(100.0, 0.0)),
            Segment::line(p(100.0, 0.0), p(200.0, 0.0)),
        ])))
    }

    #[test]
    fn priority_is_measurements_markups_then_content() {
        let mut set = AnnotSet::new();
        let square = Annotation::square(0, Rect::new(48.0, 48.0, 80.0, 80.0), &o());
        let count = Annotation::count(0, &[p(53.0, 50.0)], CountSymbol::Dot, &o());
        set.add(square.clone()).unwrap();
        set.add(count.clone()).unwrap();
        let scene = cross();
        let opts = SnapOptions::all(5.0);

        // The count marker outranks the closer square corner and the exact intersection.
        let hit = snap(Some(&set), &scene, 0, p(50.0, 50.0), &opts);
        let t = hit.target().unwrap();
        assert_eq!(
            (t.kind, t.point),
            (SnapKind::MeasureEndpoint, p(53.0, 50.0))
        );
        assert_eq!(t.annotation.as_ref(), Some(&count.id));
        // Measurements off: the square's corner, where the drawing has nothing closer.
        let no_measures = SnapOptions {
            measurements: false,
            ..opts.clone()
        };
        let t = snap(Some(&set), &scene, 0, p(47.0, 49.0), &no_measures);
        let t = t.target().unwrap();
        assert_eq!((t.kind, t.point), (SnapKind::MarkupVertex, p(48.0, 48.0)));
        // The exact intersection is closer than the corner: it wins.
        let t = snap(Some(&set), &scene, 0, p(50.0, 50.0), &no_measures);
        let t = t.target().unwrap();
        assert_eq!((t.kind, t.point), (SnapKind::Intersection, p(50.0, 50.0)));
        // Counts off (the Count tool): the marker is not snapped to.
        let no_counts = SnapOptions {
            counts: false,
            ..opts.clone()
        };
        let t = snap(Some(&set), &scene, 0, p(53.5, 50.0), &no_counts);
        assert_ne!(t.target().unwrap().annotation.as_ref(), Some(&count.id));
        // Excluding it (it is being dragged): the drawing's intersection, exactly.
        let content = SnapOptions {
            exclude: Some(square.id.clone()),
            ..no_measures.clone()
        };
        let t = snap(Some(&set), &scene, 0, p(51.0, 49.0), &content);
        let t = t.target().unwrap();
        assert_eq!((t.kind, t.point), (SnapKind::Intersection, p(50.0, 50.0)));
        // An endpoint beats the midpoint and nearest near (100, 0).
        let t = snap(Some(&set), &scene, 0, p(101.0, 2.0), &content);
        assert_eq!(t.target().unwrap().point, p(100.0, 0.0));
        assert_eq!(t.target().unwrap().kind, SnapKind::Endpoint);
        let t = snap(Some(&set), &scene, 0, p(150.0, 1.0), &content);
        assert_eq!(t.target().unwrap().kind, SnapKind::Midpoint);
        let t = snap(Some(&set), &scene, 0, p(130.0, 1.0), &content);
        assert_eq!(
            (t.target().unwrap().kind, t.target().unwrap().point),
            (SnapKind::Nearest, p(130.0, 0.0))
        );
        assert_eq!(
            snap(Some(&set), &scene, 0, p(400.0, 400.0), &content),
            SnapOutcome::Nothing
        );
        // Another page has none of these markups.
        let t = snap(Some(&set), &scene, 1, p(50.0, 50.0), &opts);
        assert_eq!(t.target().unwrap().kind, SnapKind::Intersection);
    }

    #[test]
    fn a_scene_still_building_never_blocks_the_markups() {
        let mut set = AnnotSet::new();
        let line = Annotation::line(0, p(10.0, 10.0), p(90.0, 10.0), &o());
        set.add(line).unwrap();
        let opts = SnapOptions::all(3.0);
        let t = snap(Some(&set), &SceneState::Pending, 0, p(11.0, 11.0), &opts);
        assert_eq!(t.target().unwrap().point, p(10.0, 10.0));
        assert_eq!(
            snap(Some(&set), &SceneState::Pending, 0, p(50.0, 50.0), &opts),
            SnapOutcome::Pending
        );
        assert_eq!(
            snap(
                Some(&set),
                &SceneState::Unavailable(pl_scene::NoSnapReason::Encrypted),
                0,
                p(50.0, 50.0),
                &opts
            ),
            SnapOutcome::Nothing
        );
        assert_eq!(
            snap(None, &cross(), 0, p(300.0, 300.0), &opts),
            SnapOutcome::Pending,
            "markups not read yet"
        );
    }
}
