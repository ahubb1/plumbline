//! Measurements in the file: the `/Measure` dictionary and its number formats, the dimension
//! keys of a Line, and this crate's private part.
//!
//! A PDF real holds about seven digits. The factors of a measure dictionary are written as
//! reals, where other applications read them, and once more as exact strings in the private
//! dictionary, so that a measurement read back is the one that was written. The exact value
//! is believed only while the real beside it is still the one it rounds to: if another
//! application changes the scale, its number wins.

use pl_draw::shapes::LineEnding;
use pl_geom::{Point, Vec2, VolumeFormat, VolumeUnit};
use pl_pdf::{Dictionary, Object, PdfDocument};

use crate::measure::{
    CaptionPosition, CountSymbol, MeasureKind, MeasureSpec, Measurement, NumberFormat, NumberStyle,
    ScaleRef,
};
use crate::model::{AnnotKind, Annotation, Geometry};
use crate::objects::{encode_text, lookup, name, name_of, num, number_array, text_of};

/// A number written as a string, so that it reads back exactly.
fn exact(v: f64) -> Object {
    encode_text(&format!("{v}"))
}

fn read_exact(doc: &PdfDocument, dict: &Dictionary, key: &[u8]) -> Option<f64> {
    lookup(doc, dict, key)
        .and_then(text_of)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn number_format_dict(f: &NumberFormat) -> Dictionary {
    let mut d = Dictionary::new();
    d.set("Type", name("NumberFormat"));
    d.set("U", encode_text(&f.label));
    d.set("C", num(f.factor));
    d.set("D", Object::Integer(i64::from(f.precision)));
    d.set("F", name(f.style.pdf_name()));
    if f.fixed_denominator {
        d.set("FD", Object::Boolean(true));
    }
    d.set("RT", encode_text(&f.thousands));
    d.set("RD", encode_text(&f.decimal));
    d.set("PS", encode_text(&f.prefix_space));
    d.set("SS", encode_text(&f.suffix_space));
    d.set("O", name(if f.label_first { "P" } else { "S" }));
    d
}

fn formats_object(formats: &[NumberFormat]) -> Object {
    Object::Array(
        formats
            .iter()
            .map(|f| Object::Dictionary(number_format_dict(f)))
            .collect(),
    )
}

/// The `/Measure` dictionary of `spec`.
pub(crate) fn measure_dict(spec: &MeasureSpec) -> Dictionary {
    let mut d = Dictionary::new();
    d.set("Type", name("Measure"));
    d.set("Subtype", name("RL"));
    d.set("R", encode_text(&spec.ratio));
    d.set("X", formats_object(&spec.x));
    if let Some(y) = &spec.y {
        d.set("Y", formats_object(y));
    }
    d.set("D", formats_object(&spec.distance));
    // A dictionary read without /A (common for line dimensions) is not given an empty one:
    // ISO 32000 wants number formats in it, and the file had none.
    if !spec.area.is_empty() {
        d.set("A", formats_object(&spec.area));
    }
    if !spec.angle.is_empty() {
        d.set("T", formats_object(&spec.angle));
    }
    if !spec.slope.is_empty() {
        d.set("S", formats_object(&spec.slope));
    }
    if let Some(o) = spec.origin {
        d.set("O", Object::Array(vec![num(o.x), num(o.y)]));
    }
    if let Some(cyx) = spec.cyx.filter(|_| spec.y.is_some()) {
        d.set("CYX", num(cyx));
    }
    if !spec.volume.is_empty() {
        d.set("V", formats_object(&spec.volume));
    }
    if let Some(t) = spec.target_unit_conversion {
        d.set("TargetUnitConversion", num(t));
    }
    d
}

/// The `/V` a measure dictionary of a Volume of ours carries, so that Revu shows the volume
/// as this crate does: [`VolumeFormat`] in the cube of the units of `/X`. `None` when the
/// unit of `/X` is not one this crate knows.
pub(crate) fn volume_formats(spec: &MeasureSpec, volume: &VolumeFormat) -> Option<Vec<NumberFormat>> {
    let unit = spec.metres_per_unit()?;
    let factor = unit * unit * unit / volume.unit.cu_metres_per_unit();
    Some(vec![NumberFormat::decimal(
        volume.unit.abbreviation(),
        " ",
        factor,
        volume.places,
    )])
}

/// One number format dictionary, with the defaults ISO 32000 gives its optional keys.
fn read_number_format(doc: &PdfDocument, object: &Object) -> Option<NumberFormat> {
    let object = match object {
        Object::Reference(_) => doc.resolve(object)?,
        other => other,
    };
    let d = object.as_dict().ok()?;
    let text = |key: &[u8], default: &str| match lookup(doc, d, key) {
        Some(o) => text_of(o),
        None => Some(default.to_string()),
    };
    let factor = match lookup(doc, d, b"C") {
        Some(o) => pl_pdf::number(o)?,
        None => 1.0,
    };
    let precision = match lookup(doc, d, b"D") {
        Some(o) => pl_pdf::number(o)
            .filter(|v| *v >= 1.0 && *v <= 4e9)?
            .round() as u32,
        None => 100,
    };
    let style = match lookup(doc, d, b"F") {
        Some(o) => NumberStyle::from_pdf_name(&name_of(o)?)?,
        None => NumberStyle::Decimal,
    };
    let label_first = match lookup(doc, d, b"O") {
        Some(o) => match name_of(o)?.as_str() {
            "P" => true,
            "S" => false,
            _ => return None,
        },
        None => false,
    };
    (factor.is_finite() && factor > 0.0).then_some(())?;
    Some(NumberFormat {
        label: text(b"U", "")?,
        factor,
        precision,
        style,
        fixed_denominator: matches!(lookup(doc, d, b"FD"), Some(Object::Boolean(true))),
        thousands: text(b"RT", ",")?,
        decimal: text(b"RD", ".")?,
        prefix_space: text(b"PS", " ")?,
        suffix_space: text(b"SS", " ")?,
        label_first,
    })
}

fn read_formats(doc: &PdfDocument, dict: &Dictionary, key: &[u8]) -> Option<Vec<NumberFormat>> {
    match lookup(doc, dict, key) {
        Some(o) => o
            .as_array()
            .ok()?
            .iter()
            .map(|f| read_number_format(doc, f))
            .collect(),
        None => Some(Vec::new()),
    }
}

/// A rectilinear measure dictionary. `None` for any other measure subtype (a geospatial one)
/// and for one whose `/X` or `/D` cannot be read; such a dictionary is left alone.
pub(crate) fn read_spec(doc: &PdfDocument, dict: &Dictionary) -> Option<MeasureSpec> {
    match lookup(doc, dict, b"Subtype").and_then(name_of).as_deref() {
        None | Some("RL") => {}
        Some(_) => return None,
    }
    let x = read_formats(doc, dict, b"X")?;
    let distance = read_formats(doc, dict, b"D")?;
    if x.is_empty() || distance.is_empty() {
        return None;
    }
    let y = Some(read_formats(doc, dict, b"Y")?).filter(|y| !y.is_empty());
    let cyx = match lookup(doc, dict, b"CYX") {
        Some(o) => Some(pl_pdf::number(o).filter(|v| v.is_finite() && *v > 0.0)?),
        None => None,
    };
    Some(MeasureSpec {
        ratio: lookup(doc, dict, b"R")
            .and_then(text_of)
            .unwrap_or_default(),
        x,
        cyx: cyx.filter(|_| y.is_some()),
        y,
        distance,
        area: read_formats(doc, dict, b"A")?,
        angle: read_formats(doc, dict, b"T")?,
        slope: read_formats(doc, dict, b"S")?,
        origin: lookup(doc, dict, b"O")
            .and_then(|o| number_array(doc, o))
            .filter(|v| v.len() == 2)
            .map(|v| Point::new(v[0], v[1])),
        // A /V that cannot be read is left out rather than losing the whole dictionary.
        volume: read_formats(doc, dict, b"V").unwrap_or_default(),
        target_unit_conversion: lookup(doc, dict, b"TargetUnitConversion")
            .and_then(pl_pdf::number)
            .filter(|v| v.is_finite()),
    })
}

/// The factors of `spec` that a PDF real may not hold exactly, as exact strings.
pub(crate) fn set_exact_factors(d: &mut Dictionary, spec: &MeasureSpec) {
    d.set("X", exact(spec.x[0].factor));
    if let Some(y) = spec.y.as_ref().and_then(|y| y.first()) {
        d.set("Y", exact(y.factor));
    }
    if let Some(a) = spec.area.first() {
        d.set("A", exact(a.factor));
    }
    if let Some(cyx) = spec.cyx.filter(|_| spec.y.is_some()) {
        d.set("CYX", exact(cyx));
    }
}

/// Puts the exact factors kept in `private` back into a `spec` read from reals, each only
/// where the real is still the one the exact value is written as.
pub(crate) fn apply_exact_factors(doc: &PdfDocument, spec: &mut MeasureSpec, private: &Dictionary) {
    let believe = |read: &mut f64, key: &[u8]| {
        if let Some(e) = read_exact(doc, private, key) {
            if pl_pdf::number(&num(e)) == Some(*read) {
                *read = e;
            }
        }
    };
    if let Some(f) = spec.x.first_mut() {
        believe(&mut f.factor, b"X");
    }
    if let Some(f) = spec.y.as_mut().and_then(|y| y.first_mut()) {
        believe(&mut f.factor, b"Y");
    }
    if let Some(f) = spec.area.first_mut() {
        believe(&mut f.factor, b"A");
    }
    if let Some(cyx) = spec.cyx.as_mut() {
        believe(cyx, b"CYX");
    }
}

fn points_object(points: &[Point]) -> Object {
    Object::Array(points.iter().flat_map(|p| [num(p.x), num(p.y)]).collect())
}

/// This crate's own part of the private dictionary for a measurement: what the standard keys
/// cannot say.
pub(crate) fn measure_model(m: &Measurement) -> Dictionary {
    let mut d = Dictionary::new();
    d.set("Type", name("Measure"));
    d.set("Kind", name(m.kind.name()));
    if let ScaleRef::Viewport(viewport) = &m.scale {
        d.set("Viewport", encode_text(viewport));
    }
    if !m.kind.is_line() {
        // A line says it with /Cap.
        d.set("Caption", Object::Boolean(m.caption));
    }
    d.set("FontSize", exact(m.text_size));
    if m.rotation != 0 {
        d.set("Rotate", Object::Integer(i64::from(m.rotation)));
    }
    if !m.cutouts.is_empty() {
        d.set(
            "Cutouts",
            Object::Array(m.cutouts.iter().map(|ring| points_object(ring)).collect()),
        );
    }
    if let Some(depth) = m.depth {
        d.set("Depth", exact(depth));
    }
    d.set("VolumeUnit", encode_text(m.volume.unit.abbreviation()));
    d.set("VolumePlaces", Object::Integer(i64::from(m.volume.places)));
    d.set("Symbol", name(m.symbol.name()));
    d.set("SymbolSize", exact(m.symbol_size));
    if let Some(spec) = &m.spec {
        set_exact_factors(&mut d, spec);
        if m.kind == MeasureKind::Volume && spec.volume.is_empty() {
            // The /V in the file is made from the volume format above.
            d.set("OwnV", Object::Boolean(true));
        }
    }
    d
}

fn points(values: &[f64]) -> Vec<Point> {
    values
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Point::new(c[0], c[1]))
        .collect()
}

/// True when every `/LE` name is an ending this crate draws.
fn endings_are_known(doc: &PdfDocument, dict: &Dictionary) -> bool {
    match lookup(doc, dict, b"LE") {
        Some(Object::Array(pair)) => pair.iter().all(|o| {
            let o = match o {
                Object::Reference(_) => doc.resolve(o),
                other => Some(other),
            };
            o.and_then(name_of)
                .is_some_and(|n| LineEnding::from_pdf_name(&n).is_some())
        }),
        Some(Object::Name(n)) => LineEnding::from_pdf_name(&String::from_utf8_lossy(n)).is_some(),
        Some(_) => false,
        None => true,
    }
}

/// Reads the measurement an annotation dictionary holds, and its geometry in the model.
///
/// `kind` is the file's `/Subtype`. `own` is this crate's private dictionary, when the
/// annotation has one. Without it the standard keys and Bluebeam Revu's intents decide:
///
/// * a Line with `/IT /LineDimension` is a Length;
/// * a PolyLine with `/IT /PolyLineDimension` is a Polylength, or a Perimeter when its
///   vertices end where they start;
/// * a Polygon with `/IT /PolygonDimension` is an Area, `/PolygonVolume` a Volume (its depth
///   0: Revu writes no depth key for it), `/PolygonRadius` a Radius (three vertices, the
///   centre in the middle) and `/PolygonCount` one marker of a Count (scale under
///   `/BBMeasure`);
/// * a Circle with `/IT /CircleDimension` is a Diameter (the circle is `/Rect` less `/RD`);
/// * a PolyLine with `/IT /PolyLineAngle` is an Angle at its middle vertex.
///
/// Each needs a readable rectilinear `/Measure` (`/BBMeasure` for a count) when its value
/// depends on the scale. `None` for anything else, and for a dimension with a key this crate
/// could not draw again (an unknown line ending or caption position): that one keeps its own
/// appearance.
///
/// The geometry is `None` for the kinds whose geometry is read the ordinary way (the
/// `/InkList` of a count written by an earlier version).
pub(crate) fn read_measurement(
    doc: &PdfDocument,
    dict: &Dictionary,
    kind: &AnnotKind,
    own: Option<&Dictionary>,
    page_rotation: impl FnOnce() -> u16,
) -> Option<(Measurement, Option<Geometry>)> {
    let ours =
        own.filter(|m| lookup(doc, m, b"Type").and_then(name_of).as_deref() == Some("Measure"));
    let intent = lookup(doc, dict, b"IT").and_then(name_of);
    let spec_at = |d: &Dictionary, key: &[u8]| {
        lookup(doc, d, key)
            .and_then(|o| o.as_dict().ok())
            .and_then(|d| read_spec(doc, d))
    };
    let stated = ours
        .and_then(|m| lookup(doc, m, b"Kind"))
        .and_then(name_of)
        .and_then(|n| MeasureKind::from_name(&n));
    let closed = || {
        lookup(doc, dict, b"Vertices")
            .and_then(|o| number_array(doc, o))
            .is_some_and(|v| v.len() >= 8 && v.len() % 2 == 0 && closes(&points(&v)))
    };
    let measure_kind = match (stated, kind) {
        // Ours: the kind's subtype now, or the carrier an earlier version wrote it as.
        (Some(k), subtype) if k.carrier() == *subtype || k.file_subtype() == *subtype => k,
        (Some(_), _) => return None,
        // Another application's perimeter: a polyline dimension that ends where it starts.
        (None, AnnotKind::PolyLine) if intent.as_deref() == Some("PolyLineDimension") => {
            if closed() {
                MeasureKind::Perimeter
            } else {
                MeasureKind::Polylength
            }
        }
        (None, subtype) => MeasureKind::from_intent(intent.as_deref()?, subtype)?,
    };
    let mut spec = spec_at(dict, b"Measure").or_else(|| spec_at(dict, b"BBMeasure"));
    if spec.is_none() {
        // An angle or a count of an earlier version kept its scale in the private part.
        spec = ours.and_then(|own| spec_at(own, b"Measure"));
    }
    if measure_kind.is_scaled() && spec.is_none() {
        return None;
    }
    if let (Some(spec), Some(private)) = (spec.as_mut(), ours) {
        apply_exact_factors(doc, spec, private);
        if matches!(lookup(doc, private, b"OwnV"), Some(Object::Boolean(true))) {
            // Made from the volume format, which is read from the private part.
            spec.volume.clear();
        }
    }

    let array = |key: &[u8]| lookup(doc, dict, key).and_then(|o| number_array(doc, o));
    let number = |key: &[u8]| {
        lookup(doc, dict, key)
            .and_then(pl_pdf::number)
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
    };
    let vertices = || array(b"Vertices").filter(|v| v.len() % 2 == 0);
    let geometry = match (measure_kind, kind) {
        (_, AnnotKind::Line) => {
            let l = array(b"L").filter(|v| v.len() == 4)?;
            if !endings_are_known(doc, dict) {
                return None;
            }
            Some(Geometry::Line {
                a: Point::new(l[0], l[1]),
                b: Point::new(l[2], l[3]),
            })
        }
        // Revu's radius: a point on the circle, the centre, another point on the circle.
        (MeasureKind::Radius, AnnotKind::Polygon) => {
            let v = vertices().filter(|v| v.len() == 6)?;
            if !endings_are_known(doc, dict) {
                return None;
            }
            Some(Geometry::Line {
                a: Point::new(v[2], v[3]),
                b: Point::new(v[0], v[1]),
            })
        }
        (MeasureKind::Diameter, AnnotKind::Circle) => {
            let frame = crate::read::frame_of(doc, dict, crate::read::rect_of(doc, dict));
            if !(frame.width() > 0.0 && frame.height() > 0.0) || !endings_are_known(doc, dict) {
                return None;
            }
            let centre = frame.center();
            let radius = frame.width().max(frame.height()) / 2.0;
            // Ours keeps the line it was drawn along; it is believed while it still fits the
            // circle (another application may have moved or resized it).
            let stored = ours
                .and_then(|p| lookup(doc, p, b"Line"))
                .and_then(|o| number_array(doc, o))
                .filter(|v| v.len() == 4)
                .map(|v| (Point::new(v[0], v[1]), Point::new(v[2], v[3])))
                .filter(|(a, b)| {
                    let slack = 0.5f64.max(radius * 0.01);
                    a.midpoint(*b).distance(centre) <= slack
                        && (a.distance(*b) / 2.0 - radius).abs() <= slack
                });
            let (a, b) = stored.unwrap_or((
                Point::new(centre.x - radius, centre.y),
                Point::new(centre.x + radius, centre.y),
            ));
            Some(Geometry::Line { a, b })
        }
        (MeasureKind::Count, AnnotKind::Polygon) => {
            // One marker: where it was put (ours), else the middle of the symbol's outline.
            let outline = vertices().filter(|v| v.len() >= 2).map(|v| points(&v));
            let middle = outline.as_ref().map(|p| bounds_of(p));
            let at = ours
                .and_then(|p| lookup(doc, p, b"At"))
                .and_then(|o| exact_point(doc, o))
                .filter(|p| middle.is_none_or(|b| b.inflate(1.0, 1.0).contains(*p)));
            let marker = at.or(middle.map(|b| b.center()));
            Some(Geometry::Ink(
                marker.map(|p| vec![p, p]).into_iter().collect(),
            ))
        }
        (_, AnnotKind::PolyLine | AnnotKind::Polygon) => {
            let mut v = vertices()?;
            // The repeated first vertex of a closed perimeter is not a vertex of the model.
            if measure_kind == MeasureKind::Perimeter && closes(&points(&v)) {
                v.truncate(v.len() - 2);
            }
            let enough = match measure_kind {
                MeasureKind::Polylength => v.len() >= 4,
                MeasureKind::Angle => v.len() == 6,
                MeasureKind::Perimeter | MeasureKind::Area | MeasureKind::Volume => v.len() >= 6,
                _ => false,
            };
            if !enough || !endings_are_known(doc, dict) {
                return None;
            }
            Some(Geometry::Points(points(&v)))
        }
        (MeasureKind::Count, AnnotKind::Ink) => None,
        _ => return None,
    };

    let mut m = Measurement::new(measure_kind, spec);
    if measure_kind.is_line() {
        m.leader = number(b"LL");
        m.leader_extension = number(b"LLE").max(0.0);
        m.leader_offset = number(b"LLO").max(0.0);
        m.caption = matches!(lookup(doc, dict, b"Cap"), Some(Object::Boolean(true)));
        m.caption_position = match lookup(doc, dict, b"CP") {
            Some(o) => CaptionPosition::from_pdf_name(&name_of(o)?)?,
            // The PDF default is Inline.
            None if m.caption => CaptionPosition::Inline,
            None => CaptionPosition::default(),
        };
        if let Some(co) = array(b"CO").filter(|v| v.len() == 2) {
            m.caption_offset = Vec2::new(co[0], co[1]);
        }
    } else {
        // Another application's polyline or polygon dimension draws its value when it says
        // so with /Cap (Revu does); one that does not draws none.
        m.caption = match ours {
            Some(p) => !matches!(lookup(doc, p, b"Caption"), Some(Object::Boolean(false))),
            None => matches!(lookup(doc, dict, b"Cap"), Some(Object::Boolean(true))),
        };
    }
    match ours {
        Some(p) => {
            if let Some(v) = lookup(doc, p, b"Viewport").and_then(text_of) {
                m.scale = ScaleRef::Viewport(v);
            }
            if let Some(size) = read_exact(doc, p, b"FontSize").filter(|v| *v > 0.0) {
                m.text_size = size;
            }
            m.rotation = lookup(doc, p, b"Rotate")
                .and_then(pl_pdf::number)
                .map_or(0, |v| crate::content::normalise_rotation(v.round() as i64));
            m.cutouts = lookup(doc, p, b"Cutouts")
                .and_then(|o| o.as_array().ok())
                .map(|rings| {
                    rings
                        .iter()
                        .filter_map(|ring| {
                            let ring = match ring {
                                Object::Reference(_) => doc.resolve(ring)?,
                                other => other,
                            };
                            number_array(doc, ring)
                                .filter(|v| v.len() >= 6 && v.len() % 2 == 0)
                                .map(|v| points(&v))
                        })
                        .collect()
                })
                .unwrap_or_default();
            m.depth = read_exact(doc, p, b"Depth");
            let unit = lookup(doc, p, b"VolumeUnit")
                .and_then(text_of)
                .and_then(|s| VolumeUnit::from_abbreviation(&s));
            let places = lookup(doc, p, b"VolumePlaces")
                .and_then(|o| o.as_i64().ok())
                .map(|v| v.clamp(0, 12) as u8);
            m.volume = VolumeFormat::new(
                unit.unwrap_or(m.volume.unit),
                places.unwrap_or(m.volume.places),
            );
            if let Some(symbol) = lookup(doc, p, b"Symbol")
                .and_then(name_of)
                .and_then(|n| CountSymbol::from_name(&n))
            {
                m.symbol = symbol;
            }
            if let Some(size) = read_exact(doc, p, b"SymbolSize").filter(|v| *v > 0.0) {
                m.symbol_size = size;
            }
        }
        None => {
            m.rotation = page_rotation();
            match measure_kind {
                // Revu writes no depth for a volume it shows as 0 (and no other depth key has
                // been seen): without one the depth is 0, as Revu's value says.
                MeasureKind::Volume => m.depth = Some(0.0),
                MeasureKind::Count => {
                    m.symbol = match lookup(doc, dict, b"CountStyle").and_then(name_of).as_deref()
                    {
                        Some("Square") => CountSymbol::Square,
                        Some("Checkmark") => CountSymbol::Check,
                        Some("Diamond") => CountSymbol::Diamond,
                        Some("Triangle") => CountSymbol::Triangle,
                        Some("Star") => CountSymbol::Star,
                        Some("Cross") => CountSymbol::Cross,
                        Some("Circle") if color_set(doc, dict, b"IC") => CountSymbol::Dot,
                        _ => CountSymbol::Ring,
                    };
                    if let Some(v) = vertices().filter(|v| v.len() >= 4) {
                        let b = bounds_of(&points(&v));
                        let size = b.width().max(b.height());
                        if size > 0.0 && size.is_finite() {
                            m.symbol_size = size;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Some((m, geometry))
}

/// The measure dictionary under `key` of an annotation dictionary, read as
/// [`read_measurement`] reads it (exact factors of ours put back).
pub(crate) fn file_spec(doc: &PdfDocument, dict: &Dictionary, key: &[u8]) -> Option<MeasureSpec> {
    let mut spec = lookup(doc, dict, key)
        .and_then(|o| o.as_dict().ok())
        .and_then(|d| read_spec(doc, d))?;
    let private = lookup(doc, dict, crate::model::PRIVATE_KEY.as_bytes())
        .and_then(|o| o.as_dict().ok())
        .and_then(|p| lookup(doc, p, crate::model::MODEL_KEY.as_bytes()))
        .and_then(|o| o.as_dict().ok())
        .filter(|m| lookup(doc, m, b"Type").and_then(name_of).as_deref() == Some("Measure"));
    if let Some(private) = private {
        apply_exact_factors(doc, &mut spec, private);
        if matches!(lookup(doc, private, b"OwnV"), Some(Object::Boolean(true))) {
            spec.volume.clear();
        }
    }
    Some(spec)
}

/// True when `key` is a colour array with a colour in it.
fn color_set(doc: &PdfDocument, dict: &Dictionary, key: &[u8]) -> bool {
    lookup(doc, dict, key)
        .and_then(|o| number_array(doc, o))
        .is_some_and(|v| !v.is_empty())
}

/// The box around some points.
pub(crate) fn bounds_of(points: &[Point]) -> pl_geom::Rect {
    let first = points.first().copied().unwrap_or(Point::ZERO);
    points
        .iter()
        .fold(pl_geom::Rect::from_points(first, first), |r, p| r.union_pt(*p))
}

/// A point this crate wrote as two exact strings.
fn exact_point(doc: &PdfDocument, o: &Object) -> Option<Point> {
    let o = match o {
        Object::Reference(_) => doc.resolve(o)?,
        other => other,
    };
    let v: Vec<f64> = o
        .as_array()
        .ok()?
        .iter()
        .map(|e| text_of(e).and_then(|s| s.trim().parse::<f64>().ok()))
        .collect::<Option<Vec<f64>>>()?;
    (v.len() == 2 && v.iter().all(|x| x.is_finite())).then(|| Point::new(v[0], v[1]))
}

/// `p` as two exact strings.
pub(crate) fn exact_point_object(p: Point) -> Object {
    Object::Array(vec![exact(p.x), exact(p.y)])
}

/// True when a run of vertices ends on its first vertex (and has more than that one point).
fn closes(points: &[Point]) -> bool {
    match (points.first(), points.last()) {
        (Some(a), Some(b)) if points.len() > 1 => {
            let size = a.x.abs().max(a.y.abs()).max(1.0);
            a.distance(*b) <= 1e-9 * size
        }
        _ => false,
    }
}

/// The value of one of the measurement keys of `a`'s dictionary; `None` means the key is
/// absent. Only called for the keys in [`MEASURE_KEYS`].
pub(crate) fn measure_key(a: &Annotation, key: &str) -> Option<Object> {
    let m = a.measurement()?;
    let line = m.kind.is_line();
    let file_dict = |spec: &MeasureSpec| {
        let mut spec = spec.clone();
        if m.kind == MeasureKind::Volume && spec.volume.is_empty() {
            spec.volume = volume_formats(&spec, &m.volume).unwrap_or_default();
        }
        Object::Dictionary(measure_dict(&spec))
    };
    match key {
        "IT" => m.kind.intent().map(name),
        "MeasurementTypes" => Some(Object::Integer(m.kind.measurement_types())),
        // A count's scale is under Revu's key, where a reader that knows /Measure does not
        // take the marker's outline for an area.
        "Measure" => m
            .spec
            .as_ref()
            .filter(|_| m.kind != MeasureKind::Count)
            .map(file_dict),
        "BBMeasure" => m
            .spec
            .as_ref()
            .filter(|_| m.kind == MeasureKind::Count)
            .map(file_dict),
        "CountStyle" if m.kind == MeasureKind::Count => Some(name(match m.symbol {
            CountSymbol::Dot | CountSymbol::Ring => "Circle",
            CountSymbol::Square => "Square",
            CountSymbol::Check => "Checkmark",
            CountSymbol::Diamond => "Diamond",
            CountSymbol::Triangle => "Triangle",
            CountSymbol::Star => "Star",
            CountSymbol::Cross => "Cross",
            CountSymbol::Hexagon => "Hexagon",
            CountSymbol::Plus => "Plus",
        })),
        "NumCounts" if m.kind == MeasureKind::Count => {
            Some(Object::Integer(a.count_points().len() as i64))
        }
        "LL" if line => (m.leader != 0.0).then(|| num(m.leader)),
        "LLE" if line => {
            (m.leader != 0.0 && m.leader_extension != 0.0).then(|| num(m.leader_extension))
        }
        "LLO" if line => (m.leader_offset != 0.0).then(|| num(m.leader_offset)),
        // Revu writes /Cap on every measurement that shows its value.
        "Cap" => m.caption.then_some(Object::Boolean(true)),
        "CP" if line => m.caption.then(|| name(m.caption_position.pdf_name())),
        "CO" if line => (m.caption && m.caption_offset != Vec2::ZERO)
            .then(|| Object::Array(vec![num(m.caption_offset.x), num(m.caption_offset.y)])),
        _ => None,
    }
}

/// The keys that say what a measurement is and how its dimension is drawn. They are written
/// together: a key whose value comes out as the file has it is left alone by the save.
pub(crate) const MEASURE_KEYS: [&str; 12] = [
    "IT",
    "MeasurementTypes",
    "Measure",
    "BBMeasure",
    "CountStyle",
    "NumCounts",
    "LL",
    "LLE",
    "LLO",
    "Cap",
    "CP",
    "CO",
];

/// Bluebeam Revu's own keys that this crate writes. On a measurement of another application
/// that has them, they are left as the file has them: they are that application's account
/// of the markup. (`/NumCounts`, the total of a count's group, is not among them: the save
/// writes the group's total on every annotation of a group that changed.)
pub(crate) const REVU_KEYS: [&str; 3] = ["MeasurementTypes", "BBMeasure", "CountStyle"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::MeasureFormat;
    use pl_geom::{AreaFormat, AreaUnit, LengthFormat, Scale, Unit};

    fn doc() -> PdfDocument {
        PdfDocument::from_bytes(pl_testkit::plain().bytes, None).unwrap()
    }

    fn spec(scale: &str, user_unit: f64, length: LengthFormat, area: AreaUnit) -> MeasureSpec {
        MeasureSpec::new(
            &Scale::parse(scale).unwrap(),
            user_unit,
            &MeasureFormat::new(length, AreaFormat::new(area, 2)),
        )
    }

    fn real(d: &Dictionary, key: &str) -> f64 {
        pl_pdf::number(d.get(key.as_bytes()).unwrap()).unwrap()
    }

    fn first<'a>(d: &'a Dictionary, key: &str) -> &'a Dictionary {
        d.get(key.as_bytes()).unwrap().as_array().unwrap()[0]
            .as_dict()
            .unwrap()
    }

    #[test]
    fn the_measure_dictionary_is_golden() {
        // 1 in = 20 ft shown in decimal feet: 20/72 ft per unit, as a PDF real.
        let s = spec(
            "1 in = 20 ft",
            1.0,
            LengthFormat::decimal(Unit::Foot, 2),
            AreaUnit::SqFoot,
        );
        let d = measure_dict(&s);
        assert_eq!(d.get(b"Type").unwrap(), &name("Measure"));
        assert_eq!(d.get(b"Subtype").unwrap(), &name("RL"));
        assert_eq!(d.get(b"R").unwrap(), &encode_text("1 in = 20 ft"));
        let x = first(&d, "X");
        assert_eq!(x.get(b"C").unwrap(), &Object::Real(0.277_777_8));
        assert_eq!(real(x, "C"), 0.2777778);
        assert_eq!(x.get(b"U").unwrap(), &encode_text("'"));
        assert_eq!(x.get(b"D").unwrap(), &Object::Integer(100));
        assert_eq!(x.get(b"F").unwrap(), &name("D"));
        assert_eq!(x.get(b"RT").unwrap(), &encode_text(""));
        assert_eq!(x.get(b"PS").unwrap(), &encode_text(""));
        assert_eq!(x.get(b"O").unwrap(), &name("S"));
        assert_eq!(first(&d, "D").get(b"C").unwrap(), &Object::Integer(1));
        let a = first(&d, "A");
        assert_eq!(a.get(b"C").unwrap(), &Object::Integer(1));
        assert_eq!(a.get(b"U").unwrap(), &encode_text("sq ft"));
        assert_eq!(a.get(b"PS").unwrap(), &encode_text(" "));
        assert_eq!(first(&d, "T").get(b"D").unwrap(), &Object::Integer(10));
        assert!(!d.has(b"Y") && !d.has(b"CYX") && !d.has(b"S") && !d.has(b"O"));

        // The same scale on a /UserUnit 2 page: twice the factor.
        let s = spec(
            "1 in = 20 ft",
            2.0,
            LengthFormat::decimal(Unit::Foot, 2),
            AreaUnit::SqFoot,
        );
        assert_eq!(real(first(&measure_dict(&s), "X"), "C"), 0.5555556);

        // 1/4" = 1'-0" in feet and inches to a sixteenth, areas in square yards.
        let s = spec(
            "1/4\" = 1'-0\"",
            1.0,
            LengthFormat::architectural(16),
            AreaUnit::SqYard,
        );
        let d = measure_dict(&s);
        assert_eq!(d.get(b"R").unwrap(), &encode_text("0.25 in = 1 ft"));
        let x = d.get(b"X").unwrap().as_array().unwrap();
        assert_eq!(x.len(), 2);
        let (feet, inches) = (x[0].as_dict().unwrap(), x[1].as_dict().unwrap());
        assert_eq!(real(feet, "C"), 0.055555556);
        assert_eq!(feet.get(b"SS").unwrap(), &encode_text("-"));
        assert_eq!(inches.get(b"C").unwrap(), &Object::Integer(12));
        assert_eq!(inches.get(b"D").unwrap(), &Object::Integer(16));
        assert_eq!(inches.get(b"F").unwrap(), &name("F"));
        assert_eq!(inches.get(b"U").unwrap(), &encode_text("\""));
        assert_eq!(real(first(&d, "A"), "C"), 0.11111111);

        // 1:100 in metres, areas in hectares.
        let s = spec(
            "1:100",
            1.0,
            LengthFormat::decimal(Unit::Metre, 3),
            AreaUnit::Hectare,
        );
        let d = measure_dict(&s);
        assert_eq!(d.get(b"R").unwrap(), &encode_text("1 mm = 100 mm"));
        assert_eq!(real(first(&d, "X"), "C"), 0.035277776);
        assert_eq!(first(&d, "X").get(b"D").unwrap(), &Object::Integer(1000));
        assert_eq!(first(&d, "X").get(b"U").unwrap(), &encode_text("m"));
        assert_eq!(real(first(&d, "A"), "C"), 0.0001);
        // 1:50 in millimetres: 50 * 25.4 / 72 mm per unit.
        let s = spec(
            "1:50",
            1.0,
            LengthFormat::decimal(Unit::Millimetre, 0),
            AreaUnit::SqMetre,
        );
        let d = measure_dict(&s);
        assert_eq!(real(first(&d, "X"), "C"), 17.63889);
        assert_eq!(real(first(&d, "A"), "C"), 0.000001);
    }

    #[test]
    fn a_measure_dictionary_reads_back_and_exact_factors_are_restored() {
        let doc = doc();
        let s = spec(
            "1 in = 20 ft",
            1.0,
            LengthFormat::architectural(8),
            AreaUnit::Acre,
        );
        let d = measure_dict(&s);
        let mut read = read_spec(&doc, &d).unwrap();
        // Everything but the two factors a real cannot hold.
        assert_ne!(read, s);
        assert_eq!(read.x[0].factor, 0.2777778);
        assert_eq!(read.x[1], s.x[1]);
        assert_eq!(read.distance, s.distance);
        assert_eq!(read.ratio, s.ratio);
        let mut private = Dictionary::new();
        set_exact_factors(&mut private, &s);
        apply_exact_factors(&doc, &mut read, &private);
        assert_eq!(read, s);

        // A factor another application changed is not overruled by the stale exact one.
        let mut changed = read_spec(&doc, &d).unwrap();
        changed.x[0].factor = 0.5;
        apply_exact_factors(&doc, &mut changed, &private);
        assert_eq!(changed.x[0].factor, 0.5);
        assert_eq!(changed.area[0].factor, s.area[0].factor);

        // Two axes.
        let format = MeasureFormat::new(
            LengthFormat::decimal(Unit::Metre, 2),
            AreaFormat::new(AreaUnit::SqMetre, 1),
        );
        let s = MeasureSpec::with_axes(
            &Scale::ratio(100.0).unwrap(),
            &Scale::ratio(10.0).unwrap(),
            1.0,
            &format,
        );
        let d = measure_dict(&s);
        assert!(d.has(b"Y"));
        assert_eq!(d.get(b"CYX").unwrap(), &Object::Integer(1));
        let mut read = read_spec(&doc, &d).unwrap();
        let mut private = Dictionary::new();
        set_exact_factors(&mut private, &s);
        apply_exact_factors(&doc, &mut read, &private);
        assert_eq!(read, s);
    }

    #[test]
    fn foreign_number_formats_get_the_specification_defaults() {
        let doc = doc();
        // What Acrobat writes for 1 in = 4 ft, more or less: bare /U and /C.
        let mut nf = Dictionary::new();
        nf.set("U", encode_text("ft"));
        nf.set("C", Object::Real(0.055_555_6));
        let mut area = Dictionary::new();
        area.set("U", encode_text("sq ft"));
        area.set("C", Object::Integer(1));
        area.set("D", Object::Integer(10));
        let mut d = Dictionary::new();
        d.set("Type", name("Measure"));
        d.set("R", encode_text("1 in = 4 ft"));
        d.set("X", Object::Array(vec![Object::Dictionary(nf.clone())]));
        let mut dist = nf.clone();
        dist.set("C", Object::Integer(1));
        d.set("D", Object::Array(vec![Object::Dictionary(dist)]));
        d.set("A", Object::Array(vec![Object::Dictionary(area)]));
        let s = read_spec(&doc, &d).unwrap();
        let f = &s.distance[0];
        assert_eq!((f.precision, f.style), (100, NumberStyle::Decimal));
        assert_eq!((f.thousands.as_str(), f.decimal.as_str()), (",", "."));
        assert_eq!(
            (f.prefix_space.as_str(), f.suffix_space.as_str()),
            (" ", " ")
        );
        assert!(!f.label_first && !f.fixed_denominator);
        assert_eq!(s.format_distance(1234.5), "1,234.50 ft");
        assert_eq!(s.format_area(12.34), "12.3 sq ft");
        assert_eq!(s.metres_per_unit(), Some(0.3048));
        assert!((s.scale(1.0).unwrap().factor() - 48.0).abs() < 1e-4);
        assert_eq!(s.scale(1.0).unwrap().pdf_ratio(), "1 in = 4 ft");

        // Not rectilinear, or without a usable /X or /D: not read.
        let mut geo = d.clone();
        geo.set("Subtype", name("GEO"));
        assert_eq!(read_spec(&doc, &geo), None);
        let mut no_x = d.clone();
        no_x.remove(b"X");
        assert_eq!(read_spec(&doc, &no_x), None);
        let mut bad = d.clone();
        let mut zero = nf.clone();
        zero.set("C", Object::Integer(0));
        bad.set("X", Object::Array(vec![Object::Dictionary(zero)]));
        assert_eq!(read_spec(&doc, &bad), None);
        let mut odd = d.clone();
        let mut style = nf;
        style.set("F", name("Q"));
        odd.set("D", Object::Array(vec![Object::Dictionary(style)]));
        assert_eq!(read_spec(&doc, &odd), None);
    }
}
