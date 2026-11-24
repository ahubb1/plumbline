//! pl-geom: geometry, transforms, tile maths, units and solvers (see docs/plan.md section 6).
//!
//! # The coordinate convention
//!
//! This crate owns the one coordinate convention used by every other crate:
//!
//! * **Page space** is *unrotated page user space*: the coordinates that appear in the PDF file
//!   itself (annotation `/Rect`, content streams, `/MediaBox`), y-up, in user-space units. A unit
//!   is one PDF point (1/72 inch) unless the page has a `/UserUnit`, in which case it is
//!   `user_unit` points.
//! * `/Rotate`, `/UserUnit` and the CropBox offset are applied **only** in the view transform
//!   ([`ViewTransform`]) and in the tile grid ([`TileGrid`]). Nothing stored in a model is ever
//!   rotated or offset.
//! * **Display space** is the page as shown: rotated by `/Rotate`, origin at the top-left corner
//!   of the visible (crop) box, y-down, still in user-space units.
//! * **View space** is display space scaled by the zoom and shifted by the scroll offset, in
//!   logical pixels (egui points), y-down. **Device space** is view space times the device pixel
//!   ratio, i.e. physical pixels.
//! * **Tile pixel space** is display space scaled by a power of two ([`level_scale`]).
//!
//! The geometric primitives are kurbo's, re-exported here so that other crates need not depend on
//! kurbo directly.

pub mod curve;
pub mod measure;
pub mod page;
pub mod quantity;
pub mod region;
pub mod scale;
pub mod solve;
pub mod tiles;
pub mod units;
pub mod view;

#[cfg(test)]
mod testutil;

pub use kurbo;
pub use kurbo::{Affine, Line, Point, Rect, Size, Vec2};

pub use curve::{
    arc_length, arc_through_three_points, circle_from_three_points, radius_from_three_points,
    slope_factor, slope_length, slope_length_from_angle, slope_length_from_pitch, Arc3,
};
pub use measure::{
    angle_at, closest_point_on_segment, line_intersection, point_in_polygon,
    point_segment_distance, polygon_area, polygon_centroid, polygon_perimeter, polygon_signed_area,
    polyline_length, segment_intersection, signed_angle_at, SegmentIntersection,
};
pub use page::{PageGeometry, Rotation};
pub use quantity::{
    format_angle, format_area, format_volume, AreaFormat, AreaUnit, VolumeFormat, VolumeUnit,
};
pub use region::{
    point_in_region, polygon_area_with_holes, polygon_label_point, region_edge_length,
    region_problem, region_signed_distance, ring_within, rings_overlap, RegionProblem,
};
pub use scale::{AxisScales, Scale, ScaleError, ScaleFamily, ScalePreset};
pub use solve::{
    affine_from_three_points, similarity_from_two_points, similarity_parts, solve_transform,
    SolveError,
};
pub use tiles::{
    level_for_scale, level_scale, visible_tiles, PixelRect, TileGrid, TileId, TileRange, MAX_LEVEL,
    MIN_LEVEL, TILE_SIZE,
};
pub use units::{
    convert, format_length, metres_to_points, parse_length, points_to_metres, to_fraction,
    Fraction, LengthFormat, LengthParseError, Precision, Unit, MAX_FRACTION_DENOMINATOR,
    METRES_PER_INCH, POINTS_PER_INCH,
};
pub use view::{
    clamp_zoom, fit_page_zoom, fit_width_zoom, ViewTransform, LOGICAL_PX_PER_POINT, MAX_ZOOM,
    MIN_ZOOM,
};
