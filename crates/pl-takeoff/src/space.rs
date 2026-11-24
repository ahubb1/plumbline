//! Spaces: named regions of a page (rooms, zones, floors). Each markup is in the space that
//! contains its anchor point, which is its [`crate::Builtin::Space`] column.
//!
//! A space is plain data ([`Space`]). The natural way to keep one in a document is a Polygon
//! (or Square, or Area measurement) marked with a name under the private key:
//!
//! ```text
//! /PlumblineData << /Space (Room 101) >>
//! ```
//!
//! ([`set_space_name`], [`Space::from_annotation`]). The list picks those up by itself.
//!
//! # Which space
//!
//! * A point **on the edge** of a space is in it (the region is closed), within
//!   [`EDGE_TOLERANCE`] page units.
//! * **Nested spaces:** when several spaces contain the point, the one with the smallest area
//!   wins (a room inside a floor); between equal areas, the later one in the list.
//! * A hole (an Area measurement's cutout) is not part of its space; its edge is.
//! * A markup on a page with no space containing its anchor has an empty Space.
//!
//! The **anchor** of a markup ([`anchor`]): the middle of a Line; the point halfway along a
//! PolyLine; the visual centre of a Polygon (always inside it, even for an L-shaped room); the
//! first marker of a Count; the centre of anything else's box. A space's own outline is in that
//! space.

use pl_annot::{decode_text, encode_text, AnnotId, AnnotKind, Annotation, Geometry};
use pl_geom::{
    point_in_polygon, point_segment_distance, polygon_area, polygon_label_point, Point, Rect,
};
use pl_pdf::Object;

/// The key inside `Annotation::app_data` that names a space.
pub const SPACE_KEY: &str = "Space";

/// How far outside a space's edge (page units) a point still counts as on the edge.
pub const EDGE_TOLERANCE: f64 = 1e-6;

/// A named region of one page.
#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    pub name: String,
    /// Zero-based page index.
    pub page: usize,
    /// The outline, in unrotated page space.
    pub outline: Vec<Point>,
    /// Holes in the region.
    pub holes: Vec<Vec<Point>>,
    /// The annotation that draws the space, when there is one.
    pub source: Option<AnnotId>,
    bounds: Rect,
    area: f64,
}

impl Space {
    pub fn new(name: &str, page: usize, outline: Vec<Point>, holes: Vec<Vec<Point>>) -> Space {
        let bounds = outline
            .iter()
            .fold(None::<Rect>, |r, p| {
                Some(r.map_or(Rect::from_points(*p, *p), |r| r.union_pt(*p)))
            })
            .unwrap_or(Rect::ZERO)
            .inflate(EDGE_TOLERANCE, EDGE_TOLERANCE);
        let area =
            (polygon_area(&outline) - holes.iter().map(|h| polygon_area(h)).sum::<f64>()).max(0.0);
        Space {
            name: name.trim().to_string(),
            page,
            outline,
            holes,
            source: None,
            bounds,
            area,
        }
    }

    /// The space an annotation is marked as ([`set_space_name`]): a Polygon, a Square or an
    /// Area or Volume measurement (whose cutouts are the space's holes).
    pub fn from_annotation(a: &Annotation) -> Option<Space> {
        let name = space_name(a)?;
        let (outline, holes) = match (&a.kind, &a.geometry) {
            (AnnotKind::Polygon, Geometry::Points(p)) if p.len() >= 3 => (
                p.clone(),
                a.measure
                    .as_ref()
                    .map(|m| m.cutouts.clone())
                    .unwrap_or_default(),
            ),
            (AnnotKind::Square, Geometry::Rect(r)) => (
                vec![
                    Point::new(r.x0, r.y0),
                    Point::new(r.x1, r.y0),
                    Point::new(r.x1, r.y1),
                    Point::new(r.x0, r.y1),
                ],
                Vec::new(),
            ),
            _ => return None,
        };
        let mut s = Space::new(&name, a.page, outline, holes);
        s.source = Some(a.id.clone());
        Some(s)
    }

    /// The net area of the region, in square page units.
    pub fn area(&self) -> f64 {
        self.area
    }

    /// Whether `p` is in the region, edges included.
    pub fn contains(&self, p: Point) -> bool {
        if !self.bounds.contains(p) || self.outline.len() < 3 {
            return false;
        }
        let on_edge = |ring: &[Point]| {
            let n = ring.len();
            (0..n).any(|i| point_segment_distance(p, ring[i], ring[(i + 1) % n]) <= EDGE_TOLERANCE)
        };
        if on_edge(&self.outline) || self.holes.iter().any(|h| on_edge(h)) {
            return true;
        }
        point_in_polygon(p, &self.outline) && !self.holes.iter().any(|h| point_in_polygon(p, h))
    }
}

/// The name an annotation is marked with as a space.
pub fn space_name(a: &Annotation) -> Option<String> {
    let name = match a.app_data.as_ref()?.get(SPACE_KEY.as_bytes()).ok()? {
        Object::String(bytes, _) => decode_text(bytes),
        _ => return None,
    };
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Marks an annotation as a space with this name (`None` or a blank name unmarks it). Returns
/// whether the annotation changed.
pub fn set_space_name(a: &mut Annotation, name: Option<&str>) -> bool {
    let before = a.app_data.clone();
    let mut data = a.app_data.take().unwrap_or_default();
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => data.set(SPACE_KEY, encode_text(n)),
        None => {
            data.remove(SPACE_KEY.as_bytes());
        }
    }
    a.app_data = (!data.is_empty()).then_some(data);
    a.app_data != before
}

/// Every space marked among `annotations`, in input order.
pub fn spaces_in<'a>(annotations: impl IntoIterator<Item = &'a Annotation>) -> Vec<Space> {
    annotations
        .into_iter()
        .filter_map(Space::from_annotation)
        .collect()
}

/// The point of a markup that decides its space; see the module documentation.
pub fn anchor(a: &Annotation) -> Option<Point> {
    let p = match &a.geometry {
        Geometry::Line { a, b } => a.midpoint(*b),
        Geometry::Points(points) if a.kind == AnnotKind::Polygon && points.len() >= 3 => {
            let holes = a
                .measure
                .as_ref()
                .map(|m| m.cutouts.clone())
                .unwrap_or_default();
            polygon_label_point(points, &holes, 0.0).map(|(p, _)| p)?
        }
        Geometry::Points(points) => halfway(points)?,
        Geometry::Ink(strokes) if a.measurement().is_some() => {
            *strokes.iter().find_map(|s| s.first())?
        }
        g => g.bounds().center(),
    };
    (p.x.is_finite() && p.y.is_finite()).then_some(p)
}

/// The point halfway along a run of segments.
fn halfway(points: &[Point]) -> Option<Point> {
    let first = *points.first()?;
    let total: f64 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut left = total / 2.0;
    for w in points.windows(2) {
        let d = w[0].distance(w[1]);
        if d >= left && d > 0.0 {
            return Some(w[0].lerp(w[1], left / d));
        }
        left -= d;
    }
    Some(first)
}

/// The space a point of page `page` is in: the smallest containing one (see the module
/// documentation). `own` is the annotation the point belongs to: a space drawn by it is
/// always its own.
pub fn space_at<'s>(
    spaces: &'s [Space],
    page: usize,
    p: Point,
    own: Option<&AnnotId>,
) -> Option<&'s Space> {
    if let Some(id) = own {
        if let Some(s) = spaces
            .iter()
            .find(|s| s.page == page && s.source.as_ref() == Some(id))
        {
            return Some(s);
        }
    }
    let mut best: Option<&Space> = None;
    for s in spaces.iter().filter(|s| s.page == page) {
        if s.contains(p) && best.is_none_or(|b| s.area <= b.area) {
            best = Some(s);
        }
    }
    best
}
