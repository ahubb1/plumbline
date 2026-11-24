//! Reading: annotation dictionaries from pl-pdf into the typed model. Nothing here fails on a
//! malformed dictionary; a key that cannot be read falls back to its default, and a modelled
//! subtype without usable geometry is kept as a plain rectangle that can be moved and deleted.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pl_draw::shapes::{LineEnding, Quad};
use pl_draw::{BlendMode, HAlign, Rgb, RgbaImage, StampScheme, StampSpec, VAlign};
use pl_geom::{Point, Rect};
use pl_pdf::{AnnotationEntry, Dictionary, Object, ObjectId, PdfDocument, Stream};

use crate::content::{
    default_style_color, normalise_rotation, parse_default_appearance, FreeText, FreeTextIntent,
    Stamp, StampContent,
};
use crate::model::{
    AnnotId, AnnotKind, Annotation, Baseline, Editable, Flags, Geometry, LinkTarget, Member, Meta,
    Style, COLUMN_DATA_KEY, MODEL_KEY, PRIVATE_KEY,
};
use crate::objects::{lookup, name_of, number_array, text_of, PdfDate};

/// References to other annotations, as object ids, until every annotation has its id.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RawLinks {
    pub popup: Option<ObjectId>,
    pub parent: Option<ObjectId>,
    pub reply_to: Option<ObjectId>,
}

fn reference(dict: &Dictionary, key: &[u8]) -> Option<ObjectId> {
    match dict.get(key).ok()? {
        Object::Reference(id) => Some(*id),
        _ => None,
    }
}

/// The id of an annotation without `/NM`: the same on every load of the same file. It is
/// written as the annotation's `/NM` the first time the annotation is modified.
fn derived_id(entry: &AnnotationEntry) -> AnnotId {
    match entry.id {
        Some((number, generation)) => AnnotId(format!("pl-{number}-{generation}")),
        None => AnnotId(format!("pl-p{}s{}", entry.page_index, entry.slot)),
    }
}

/// A `/C`-style colour array: grey, RGB or CMYK. An empty array (transparent) is `None`, and so
/// is a missing key; [`style`] gives a missing `/C` the kind's implied colour.
pub(crate) fn color(doc: &PdfDocument, dict: &Dictionary, key: &[u8]) -> Option<Rgb> {
    let v = number_array(doc, lookup(doc, dict, key)?)?;
    let c = |x: f64| x.clamp(0.0, 1.0) as f32;
    match v.as_slice() {
        [g] => Some(Rgb::new(c(*g), c(*g), c(*g))),
        [r, g, b] => Some(Rgb::new(c(*r), c(*g), c(*b))),
        [cy, m, y, k] => Some(Rgb::new(
            c((1.0 - cy) * (1.0 - k)),
            c((1.0 - m) * (1.0 - k)),
            c((1.0 - y) * (1.0 - k)),
        )),
        _ => None,
    }
}

fn points(values: &[f64]) -> Vec<Point> {
    values
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Point::new(c[0], c[1]))
        .collect()
}

fn crosses(a: Point, b: Point, c: Point, d: Point) -> bool {
    let side = |p: Point, q: Point, r: Point| (q - p).cross(r - p);
    side(a, b, c) * side(a, b, d) < 0.0 && side(c, d, a) * side(c, d, b) < 0.0
}

/// One `/QuadPoints` group as a ring.
///
/// Nearly every producer writes the four points as top-left, top-right, bottom-left,
/// bottom-right (the order Acrobat uses), not the anticlockwise ring the specification
/// describes. The common order is assumed; if that reading gives a self-crossing quad the
/// points were already a ring and are taken as they are.
pub(crate) fn quad_from_file(q: &[f64; 8]) -> Quad {
    let p = |i: usize| Point::new(q[2 * i], q[2 * i + 1]);
    let ring = [p(2), p(3), p(1), p(0)];
    if crosses(ring[0], ring[1], ring[2], ring[3]) || crosses(ring[1], ring[2], ring[3], ring[0]) {
        [p(0), p(1), p(2), p(3)]
    } else {
        ring
    }
}

/// A ring as the eight numbers written to `/QuadPoints` (top-left, top-right, bottom-left,
/// bottom-right).
pub(crate) fn quad_to_file(quad: &Quad) -> [f64; 8] {
    let [bl, br, tr, tl] = *quad;
    [tl.x, tl.y, tr.x, tr.y, bl.x, bl.y, br.x, br.y]
}

pub(crate) fn rect_of(doc: &PdfDocument, dict: &Dictionary) -> Rect {
    lookup(doc, dict, b"Rect")
        .and_then(|o| number_array(doc, o))
        .filter(|v| v.len() == 4)
        .map(|v| Rect::new(v[0], v[1], v[2], v[3]).abs())
        .unwrap_or(Rect::ZERO)
}

/// `/Rect` shrunk by `/RD`. The four differences are taken as left, bottom, right, top: the
/// order of a rectangle, which is how PDFBox, MuPDF and Poppler read and write the key
/// (ISO 32000 words it "left, top, right, bottom"; those implementations do not follow that).
pub(crate) fn frame_of(doc: &PdfDocument, dict: &Dictionary, rect: Rect) -> Rect {
    let rd = lookup(doc, dict, b"RD")
        .and_then(|o| number_array(doc, o))
        .filter(|v| v.len() == 4 && v.iter().all(|d| *d >= 0.0));
    match rd {
        Some(d) if d[0] + d[2] < rect.width() && d[1] + d[3] < rect.height() => Rect::new(
            rect.x0 + d[0],
            rect.y0 + d[1],
            rect.x1 - d[2],
            rect.y1 - d[3],
        ),
        _ => rect,
    }
}

/// True when the dictionary of a Line, PolyLine or Polygon says more about how it is drawn
/// than the model holds: leader lines or a caption (what a dimension tool writes), a
/// measurement, or a line ending this crate cannot draw. Regenerating the appearance of such an
/// annotation from the model would drop those parts while the keys stay in the dictionary, so it
/// is read as unmodelled: it keeps its own appearance and can be moved, described and deleted.
///
/// A dimension that `measure_io::read_measurement` can read (a rectilinear `/Measure` and
/// endings this crate draws) is read as a measurement afterwards, in `read_entry`, and is then
/// modelled after all.
fn beyond_the_model(doc: &PdfDocument, dict: &Dictionary, kind: &AnnotKind) -> bool {
    let number = |key: &[u8]| lookup(doc, dict, key).and_then(pl_pdf::number);
    let nonzero = |key: &[u8]| number(key).is_some_and(|v| v != 0.0);
    let intent = lookup(doc, dict, b"IT").and_then(name_of);
    // A /Measure, Revu's /BBMeasure or /MeasurementTypes, or an intent of a measurement
    // (Revu's `PolygonVolume`, `PolygonRadius`, `CircleDimension`, `PolyLineAngle` and
    // `PolygonCount` besides the standard dimensions).
    let measured = lookup(doc, dict, b"Measure").is_some()
        || lookup(doc, dict, b"BBMeasure").is_some()
        || lookup(doc, dict, b"MeasurementTypes").is_some()
        || intent.as_deref().is_some_and(|i| {
            matches!(
                i,
                "PolygonVolume"
                    | "PolygonRadius"
                    | "CircleDimension"
                    | "PolyLineAngle"
                    | "PolygonCount"
            )
        });
    let odd_ending = || match lookup(doc, dict, b"LE") {
        Some(Object::Array(pair)) => pair.iter().any(|o| {
            let o = match o {
                Object::Reference(_) => doc.resolve(o),
                other => Some(other),
            };
            o.and_then(name_of)
                .is_some_and(|n| LineEnding::from_pdf_name(&n).is_none())
        }),
        Some(Object::Name(n)) => LineEnding::from_pdf_name(&String::from_utf8_lossy(n)).is_none(),
        _ => false,
    };
    match kind {
        AnnotKind::Line => {
            nonzero(b"LL")
                || nonzero(b"LLE")
                || matches!(lookup(doc, dict, b"Cap"), Some(Object::Boolean(true)))
                || intent.as_deref() == Some("LineDimension")
                || measured
                || odd_ending()
        }
        AnnotKind::PolyLine => {
            intent.as_deref() == Some("PolyLineDimension") || measured || odd_ending()
        }
        AnnotKind::Polygon => intent.as_deref() == Some("PolygonDimension") || measured,
        AnnotKind::Circle => measured,
        _ => false,
    }
}

fn geometry(doc: &PdfDocument, dict: &Dictionary, kind: &AnnotKind, rect: Rect) -> Geometry {
    if beyond_the_model(doc, dict, kind) {
        return Geometry::Rect(rect);
    }
    let array = |key: &[u8]| lookup(doc, dict, key).and_then(|o| number_array(doc, o));
    let read = match kind {
        AnnotKind::Square | AnnotKind::Circle => Some(Geometry::Rect(frame_of(doc, dict, rect))),
        AnnotKind::Line => array(b"L")
            .filter(|v| v.len() == 4)
            .map(|v| Geometry::Line {
                a: Point::new(v[0], v[1]),
                b: Point::new(v[2], v[3]),
            }),
        AnnotKind::PolyLine | AnnotKind::Polygon => array(b"Vertices")
            .filter(|v| v.len() >= 4 && v.len() % 2 == 0)
            .map(|v| Geometry::Points(points(&v))),
        AnnotKind::Ink => lookup(doc, dict, b"InkList")
            .and_then(|o| o.as_array().ok())
            .and_then(|strokes| {
                strokes
                    .iter()
                    .map(|s| {
                        let s = match s {
                            Object::Reference(_) => doc.resolve(s)?,
                            other => other,
                        };
                        number_array(doc, s)
                            .filter(|v| v.len() % 2 == 0)
                            .map(|v| points(&v))
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .filter(|strokes| strokes.iter().any(|s| !s.is_empty()))
            .map(Geometry::Ink),
        k if k.is_text_markup() => array(b"QuadPoints")
            .filter(|v| v.len() >= 8 && v.len() % 8 == 0)
            .map(|v| Geometry::Quads(v.as_chunks::<8>().0.iter().map(quad_from_file).collect())),
        _ => None,
    };
    read.unwrap_or(Geometry::Rect(rect))
}

fn style(doc: &PdfDocument, dict: &Dictionary, kind: &AnnotKind) -> Style {
    let bs = lookup(doc, dict, b"BS").and_then(|o| o.as_dict().ok());
    let border = lookup(doc, dict, b"Border").and_then(|o| o.as_array().ok());
    let number = |d: &Dictionary, key: &[u8]| lookup(doc, d, key).and_then(pl_pdf::number);

    // /BS wins over the older /Border array; with neither, the border is 1 unit wide.
    let mut width = 1.0;
    let mut dash = None;
    if let Some(bs) = bs {
        if let Some(w) = number(bs, b"W") {
            width = w;
        }
        if lookup(doc, bs, b"S").and_then(name_of).as_deref() == Some("D") {
            dash = Some(
                lookup(doc, bs, b"D")
                    .and_then(|o| number_array(doc, o))
                    .unwrap_or_else(|| vec![3.0]),
            );
        }
    } else if let Some(border) = border {
        if let Some(w) = border.get(2).and_then(pl_pdf::number) {
            width = w;
        }
        dash = border.get(3).and_then(|o| number_array(doc, o));
    }
    if !(width.is_finite() && width >= 0.0) {
        width = 1.0;
    }
    let dash = dash.filter(|d| {
        !d.is_empty() && d.iter().all(|v| v.is_finite() && *v >= 0.0) && d.iter().any(|v| *v > 0.0)
    });

    let ending = |o: &Object| name_of(o).and_then(|n| LineEnding::from_pdf_name(&n));
    let (line_start, line_end) = match lookup(doc, dict, b"LE") {
        Some(Object::Array(pair)) => (
            pair.first().and_then(ending).unwrap_or(LineEnding::None),
            pair.get(1).and_then(ending).unwrap_or(LineEnding::None),
        ),
        _ => (LineEnding::None, LineEnding::None),
    };

    let cloud = lookup(doc, dict, b"BE")
        .and_then(|o| o.as_dict().ok())
        .filter(|be| lookup(doc, be, b"S").and_then(name_of).as_deref() == Some("C"))
        .map(|be| {
            number(be, b"I")
                .filter(|i| i.is_finite())
                .unwrap_or(1.0)
                .clamp(0.0, 10.0)
        });

    let blend = match lookup(doc, dict, b"BM").and_then(name_of).as_deref() {
        Some("Multiply") => BlendMode::Multiply,
        Some(_) => BlendMode::Normal,
        None => kind.default_blend(),
    };

    Style {
        // No /C at all is not "no colour": viewers paint such an annotation in a default
        // colour. Only the empty array is transparent.
        stroke: match lookup(doc, dict, b"C") {
            Some(_) => color(doc, dict, b"C"),
            None => kind.implied_color(),
        },
        fill: color(doc, dict, b"IC"),
        width,
        dash,
        opacity: number(dict, b"CA")
            .filter(|v| v.is_finite())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0),
        line_start,
        line_end,
        cloud,
        blend,
    }
}

/// A number this crate wrote as a string so that it reads back exactly (a PDF real is single
/// precision).
fn exact_number(doc: &PdfDocument, dict: &Dictionary, key: &[u8]) -> Option<f64> {
    lookup(doc, dict, key)
        .and_then(text_of)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn rotation_of(doc: &PdfDocument, dict: &Dictionary) -> u16 {
    lookup(doc, dict, b"Rotate")
        .and_then(pl_pdf::number)
        .filter(|v| v.is_finite())
        .map_or(0, |v| normalise_rotation(v.round() as i64))
}

/// A FreeText dictionary read as a text box: the text side, the style and the box. `ours` is
/// the private model dictionary of a FreeText this crate wrote; without it the dictionary is
/// another application's and what the standard keys leave open is filled in the way Acrobat
/// reads them (the `/DA` colour is the border's, the `/DS` colour the text's).
fn free_text(
    doc: &PdfDocument,
    dict: &Dictionary,
    rect: Rect,
    ours: Option<&Dictionary>,
) -> Editable {
    let text = |key: &[u8]| lookup(doc, dict, key).and_then(text_of);
    let read = style(doc, dict, &AnnotKind::FreeText);
    let da = text(b"DA")
        .map(|s| parse_default_appearance(&s))
        .unwrap_or_default();
    let defaults = FreeText::default();

    let (stroke, color, width) = match ours {
        Some(model) => {
            // The exact text colour is in the model; /DS has it in eight bits. The
            // non-stroking colour of /DA is the text colour only for a box with no border.
            let exact = lookup(doc, model, b"TextColor")
                .and_then(text_of)
                .and_then(|s| {
                    let v: Vec<f32> = s
                        .split_whitespace()
                        .filter_map(|t| t.parse::<f32>().ok())
                        .filter(|v| v.is_finite())
                        .collect();
                    (v.len() == 3).then(|| Rgb::new(v[0], v[1], v[2]).clamped())
                });
            let ds = text(b"DS").and_then(|s| default_style_color(&s));
            (
                da.stroke,
                exact.or(ds).or(da.fill).unwrap_or(Rgb::BLACK),
                exact_number(doc, model, b"W").unwrap_or(read.width),
            )
        }
        None => {
            let border = da.stroke.or(da.fill).unwrap_or(Rgb::BLACK);
            let ds = text(b"DS").and_then(|s| default_style_color(&s));
            (
                (read.width > 0.0).then_some(border),
                ds.or(da.fill).unwrap_or(Rgb::BLACK),
                read.width,
            )
        }
    };

    let callout: Vec<Point> = lookup(doc, dict, b"CL")
        .and_then(|o| number_array(doc, o))
        .filter(|v| v.len() == 4 || v.len() == 6)
        .map(|v| points(&v))
        .unwrap_or_default();
    let mut intent = lookup(doc, dict, b"IT")
        .and_then(name_of)
        .map_or(FreeTextIntent::Box, |n| FreeTextIntent::from_pdf_name(&n));
    if !callout.is_empty() && intent == FreeTextIntent::Box {
        intent = FreeTextIntent::Callout;
    }
    // A FreeText has one line ending, a name; some producers write the pair a Line has.
    let ending = |o: &Object| name_of(o).and_then(|n| LineEnding::from_pdf_name(&n));
    let line_end = match lookup(doc, dict, b"LE") {
        Some(Object::Array(pair)) => pair.first().and_then(ending),
        Some(other) => ending(other),
        None => None,
    }
    .unwrap_or(LineEnding::None);

    let model_name = |key: &[u8]| ours.and_then(|m| lookup(doc, m, key)).and_then(name_of);
    let model_number = |key: &[u8]| ours.and_then(|m| exact_number(doc, m, key));
    let v_align = match model_name(b"VAlign").as_deref() {
        Some("Middle") => VAlign::Middle,
        Some("Bottom") => VAlign::Bottom,
        _ => VAlign::Top,
    };
    Editable {
        free_text: FreeText {
            intent,
            font: da.font.unwrap_or(defaults.font),
            size: da.size.unwrap_or(defaults.size),
            color,
            align: HAlign::from_quadding(
                lookup(doc, dict, b"Q")
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0),
            ),
            v_align,
            line_spacing: model_number(b"LineSpacing")
                .filter(|v| *v > 0.0)
                .unwrap_or(defaults.line_spacing),
            padding: model_number(b"Padding")
                .filter(|v| *v >= 0.0)
                .unwrap_or(defaults.padding),
            callout,
            rotation: rotation_of(doc, dict),
        },
        style: Style {
            stroke,
            // For a FreeText /C is the background.
            fill: color_of_background(doc, dict),
            width,
            line_start: LineEnding::None,
            line_end,
            blend: BlendMode::Normal,
            ..read
        },
        geometry: Geometry::Rect(frame_of(doc, dict, rect)),
    }
}

fn color_of_background(doc: &PdfDocument, dict: &Dictionary) -> Option<Rgb> {
    color(doc, dict, b"C")
}

fn stream_at<'a>(doc: &'a PdfDocument, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Stream> {
    match lookup(doc, dict, key)? {
        Object::Stream(s) => Some(s),
        _ => None,
    }
}

/// The samples of an 8-bit image stream in the given colour space, `components` bytes a pixel.
fn samples(
    doc: &PdfDocument,
    s: &Stream,
    space: &str,
    components: usize,
) -> Option<(u32, u32, Vec<u8>)> {
    const MAX_SIDE: i64 = 16_384;
    let int = |key: &[u8]| lookup(doc, &s.dict, key).and_then(|o| o.as_i64().ok());
    let (w, h) = (int(b"Width")?, int(b"Height")?);
    if !(1..=MAX_SIDE).contains(&w)
        || !(1..=MAX_SIDE).contains(&h)
        || int(b"BitsPerComponent")? != 8
    {
        return None;
    }
    if lookup(doc, &s.dict, b"ColorSpace")
        .and_then(name_of)
        .as_deref()
        != Some(space)
    {
        return None;
    }
    let want = w as usize * h as usize * components;
    let data = s.get_plain_content_with_limit(want + 64).ok()?;
    (data.len() == want).then_some((w as u32, h as u32, data))
}

/// The picture of an image stamp this crate wrote: the one image XObject of its appearance
/// stream (DeviceRGB, 8 bits) with its soft mask as the alpha channel.
fn stamp_image(doc: &PdfDocument, dict: &Dictionary) -> Option<Arc<RgbaImage>> {
    let ap = lookup(doc, dict, b"AP")?.as_dict().ok()?;
    let normal = stream_at(doc, ap, b"N")?;
    image_in_form(doc, normal, 0)
}

/// The first image XObject among a form's resources, looking into nested forms (a group that
/// was drawn as one object) a few levels deep.
fn image_in_form(doc: &PdfDocument, form: &Stream, depth: usize) -> Option<Arc<RgbaImage>> {
    let resources = lookup(doc, &form.dict, b"Resources")?.as_dict().ok()?;
    let xobjects = lookup(doc, resources, b"XObject")?.as_dict().ok()?;
    for (key, _) in xobjects.iter() {
        let Some(image) = stream_at(doc, xobjects, key) else {
            continue;
        };
        match lookup(doc, &image.dict, b"Subtype")
            .and_then(name_of)
            .as_deref()
        {
            Some("Image") => {}
            Some("Form") if depth < 3 => {
                if let Some(found) = image_in_form(doc, image, depth + 1) {
                    return Some(found);
                }
                continue;
            }
            _ => continue,
        }
        let (w, h, rgb) = samples(doc, image, "DeviceRGB", 3)?;
        let alpha = match stream_at(doc, &image.dict, b"SMask") {
            Some(mask) => {
                let (mw, mh, a) = samples(doc, mask, "DeviceGray", 1)?;
                if (mw, mh) != (w, h) {
                    return None;
                }
                Some(a)
            }
            None => None,
        };
        let mut pixels = Vec::with_capacity(rgb.len() / 3 * 4);
        for (i, p) in rgb.as_chunks::<3>().0.iter().enumerate() {
            pixels.extend_from_slice(p);
            pixels.push(alpha.as_ref().map_or(255, |a| a[i]));
        }
        return RgbaImage::new(w, h, pixels).ok().map(Arc::new);
    }
    None
}

/// The content of a Stamp this crate wrote, from its private model dictionary.
fn stamp(doc: &PdfDocument, dict: &Dictionary, model: &Dictionary) -> Option<Stamp> {
    let text = |key: &[u8]| lookup(doc, model, key).and_then(text_of);
    let content = match lookup(doc, model, b"Type").and_then(name_of).as_deref() {
        Some("Stamp") => StampContent::Label(StampSpec {
            name: text(b"Name")?,
            label: text(b"Label")?,
            detail: text(b"Detail"),
            scheme: lookup(doc, model, b"Scheme")
                .and_then(name_of)
                .and_then(|n| StampScheme::from_name(&n))
                .unwrap_or_default(),
        }),
        Some("ImageStamp") => StampContent::Image(stamp_image(doc, dict)?),
        _ => return None,
    };
    Some(Stamp {
        content,
        rotation: rotation_of(doc, dict),
    })
}

fn destination(doc: &PdfDocument, dest: &Object) -> LinkTarget {
    let Ok(array) = dest.as_array() else {
        return LinkTarget::Other;
    };
    let page = match array.first() {
        Some(Object::Reference(id)) => doc.page_index_of(*id),
        _ => None,
    };
    let Some(page) = page else {
        return LinkTarget::Other;
    };
    let fit = array.get(1).and_then(name_of);
    match fit.as_deref() {
        Some("XYZ") => {
            let n = |i: usize| array.get(i).and_then(pl_pdf::number);
            LinkTarget::Page {
                page,
                at: n(2).zip(n(3)).map(|(x, y)| Point::new(x, y)),
            }
        }
        Some("Fit") => LinkTarget::Page { page, at: None },
        _ => LinkTarget::Other,
    }
}

fn link_target(doc: &PdfDocument, dict: &Dictionary) -> Option<LinkTarget> {
    if let Some(dest) = lookup(doc, dict, b"Dest") {
        return Some(destination(doc, dest));
    }
    let action = lookup(doc, dict, b"A")?.as_dict().ok()?;
    let kind = lookup(doc, action, b"S").and_then(name_of);
    Some(match kind.as_deref() {
        Some("URI") => match lookup(doc, action, b"URI") {
            Some(Object::String(bytes, _)) => {
                LinkTarget::Uri(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => LinkTarget::Other,
        },
        Some("GoTo") => {
            lookup(doc, action, b"D").map_or(LinkTarget::Other, |dest| destination(doc, dest))
        }
        _ => LinkTarget::Other,
    })
}

/// Reads one `/Annots` entry. Links to other annotations come back as object ids.
pub(crate) fn read_entry(doc: &PdfDocument, entry: &AnnotationEntry) -> (Annotation, RawLinks) {
    let dict = &entry.dict;
    let kind = lookup(doc, dict, b"Subtype").and_then(name_of).map_or_else(
        || AnnotKind::Unknown(String::new()),
        |n| AnnotKind::from_pdf_name(&n),
    );
    let rect = rect_of(doc, dict);
    let text = |key: &[u8]| lookup(doc, dict, key).and_then(text_of);
    let date = |key: &[u8]| text(key).and_then(|s| PdfDate::parse(&s));

    let id = text(b"NM")
        .filter(|s| !s.is_empty())
        .map_or_else(|| derived_id(entry), AnnotId);
    let meta = Meta {
        author: text(b"T"),
        subject: text(b"Subj"),
        contents: text(b"Contents"),
        created: date(b"CreationDate"),
        modified: date(b"M"),
        flags: Flags(
            lookup(doc, dict, b"F")
                .and_then(|o| o.as_i64().ok())
                .map_or(0, |f| f as u32),
        ),
        layer: reference(dict, b"OC"),
        reply_to: None,
        reply_type: lookup(doc, dict, b"RT").and_then(name_of),
        state: text(b"State"),
        state_model: text(b"StateModel"),
    };
    let links = RawLinks {
        popup: reference(dict, b"Popup"),
        parent: reference(dict, b"Parent"),
        reply_to: reference(dict, b"IRT"),
    };
    let has_appearance = lookup(doc, dict, b"AP")
        .and_then(|o| o.as_dict().ok())
        .is_some_and(|ap| ap.has(b"N"));

    // The private dictionary: this crate's own part of it is read into the model and never
    // shown as application data.
    let private = lookup(doc, dict, PRIVATE_KEY.as_bytes()).and_then(|o| o.as_dict().ok());
    let own = private
        .and_then(|p| lookup(doc, p, MODEL_KEY.as_bytes()))
        .and_then(|o| o.as_dict().ok());
    let own_type = own.and_then(|m| lookup(doc, m, b"Type")).and_then(name_of);
    let app_data = private.and_then(|p| {
        let mut rest = p.clone();
        let had_own = rest.remove(MODEL_KEY.as_bytes()).is_some();
        (!(had_own && rest.is_empty())).then_some(rest)
    });

    let mut model = Annotation {
        id,
        page: entry.page_index,
        geometry: geometry(doc, dict, &kind, rect),
        style: style(doc, dict, &kind),
        meta,
        icon: lookup(doc, dict, b"Name").and_then(name_of),
        open: matches!(lookup(doc, dict, b"Open"), Some(Object::Boolean(true))),
        popup: None,
        parent: None,
        link: (kind == AnnotKind::Link)
            .then(|| link_target(doc, dict))
            .flatten(),
        app_data,
        free_text: None,
        stamp: None,
        measure: None,
        column_data: lookup(doc, dict, COLUMN_DATA_KEY.as_bytes())
            .and_then(|o| o.as_array().ok())
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| doc.resolve(e).cloned().unwrap_or(Object::Null))
                    .collect()
            }),
        kind,
        baseline: None,
    };
    let mut editable = None;
    if matches!(
        model.kind,
        AnnotKind::Line
            | AnnotKind::PolyLine
            | AnnotKind::Polygon
            | AnnotKind::Ink
            | AnnotKind::Circle
    ) {
        // A dimension (ours or another application's) that this crate can draw again is
        // modelled; one it cannot keeps the plain reading (unmodelled for a dimension).
        let rotation = || {
            doc.page(entry.page_index)
                .map_or(0, |p| normalise_rotation(i64::from(p.rotate.degrees())))
        };
        if let Some((mut m, g)) =
            crate::measure_io::read_measurement(doc, dict, &model.kind, own, rotation)
        {
            let plain = model.geometry.clone();
            let file_kind = model.kind.clone();
            if let Some(g) = g {
                model.geometry = g;
            }
            // The model holds each kind in its carrier: a perimeter is a closed PolyLine in
            // the file and a Polygon in the model, a radius or a diameter a Line, a count Ink.
            model.kind = m.kind.carrier();
            if own.is_none()
                && m.kind == crate::measure::MeasureKind::Count
                && model.style.width <= 0.0
                && m.symbol != crate::measure::CountSymbol::Dot
            {
                // Revu draws its outlined markers with no border width of its own; drawn
                // again here they would vanish.
                model.style.width = 1.0;
            }
            if matches!(
                m.kind,
                crate::measure::MeasureKind::Radius | crate::measure::MeasureKind::Diameter
            ) && file_kind != AnnotKind::Line
            {
                // Revu's Polygon radius and Circle diameter: /LE and /IC would describe the
                // shape, not the dimension line drawn. The line's endings and the fill of its
                // arrowheads are in our private part, else the kind's own (as Revu draws them).
                let (start, end) = m.kind.default_endings();
                let ends = own
                    .and_then(|p| lookup(doc, p, b"LE"))
                    .and_then(|o| o.as_array().ok())
                    .filter(|v| v.len() == 2)
                    .map(|v| {
                        let at = |i: usize| {
                            name_of(&v[i])
                                .and_then(|n| LineEnding::from_pdf_name(&n))
                                .unwrap_or(LineEnding::None)
                        };
                        (at(0), at(1))
                    });
                (model.style.line_start, model.style.line_end) = ends.unwrap_or((start, end));
                model.style.fill = match own.and_then(|p| lookup(doc, p, b"IC")) {
                    Some(_) => own.and_then(|p| color(doc, p, b"IC")),
                    None => model.style.stroke,
                };
                // So is the caption position (Inline, the default, when not said).
                if let Some(at) = own
                    .and_then(|p| lookup(doc, p, b"CP"))
                    .and_then(name_of)
                    .and_then(|n| crate::measure::CaptionPosition::from_pdf_name(&n))
                {
                    m.caption_position = at;
                }
            }
            model.measure = Some(m);
            if model.measurement().is_none() {
                // The geometry does not fit the kind (a count without strokes).
                model.measure = None;
                model.geometry = plain;
                model.kind = file_kind;
            }
        }
    }
    match model.kind {
        AnnotKind::FreeText => {
            let ours = own.filter(|_| own_type.as_deref() == Some("FreeText"));
            let read = free_text(doc, dict, rect, ours);
            if ours.is_some() {
                model.free_text = Some(read.free_text);
                model.style = read.style;
                model.geometry = read.geometry;
            } else {
                editable = Some(Box::new(read));
            }
        }
        AnnotKind::Stamp => {
            model.stamp = own.and_then(|m| stamp(doc, dict, m));
        }
        _ => {}
    }
    let annotation = Annotation {
        baseline: Some(Baseline {
            object: entry.id,
            page: entry.page_index,
            slot: entry.slot,
            dict: dict.clone(),
            has_appearance,
            model: Box::new(model.clone()),
            editable,
            members: Vec::new(),
        }),
        ..model
    };
    (annotation, links)
}

/// Reads every annotation of the pages in `pages`, in page order and `/Annots` order, and
/// resolves popup, parent and reply links to ids. A page that cannot be read contributes
/// nothing.
pub(crate) fn read_pages(doc: &PdfDocument, pages: impl Iterator<Item = usize>) -> Vec<Annotation> {
    let mut out: Vec<Annotation> = Vec::new();
    let mut links: Vec<RawLinks> = Vec::new();
    let mut seen: HashSet<AnnotId> = HashSet::new();
    let mut by_object: HashMap<ObjectId, AnnotId> = HashMap::new();
    for page in pages {
        let Ok(entries) = doc.annotations(page) else {
            continue;
        };
        for entry in &entries {
            let (mut annotation, raw) = read_entry(doc, entry);
            if !seen.insert(annotation.id.clone()) {
                // Two annotations with one /NM (a duplicated page, say): the later one gets a
                // distinct id for this session. Its /NM in the file is left alone.
                let mut n = 2;
                let id = loop {
                    let candidate = match entry.id {
                        Some((number, _)) if n == 2 => {
                            AnnotId(format!("{}~{number}", annotation.id))
                        }
                        _ => AnnotId(format!(
                            "{}~p{}s{}n{n}",
                            annotation.id, entry.page_index, entry.slot
                        )),
                    };
                    if seen.insert(candidate.clone()) {
                        break candidate;
                    }
                    n += 1;
                };
                set_id(&mut annotation, id);
            }
            if let Some(object) = entry.id {
                by_object.entry(object).or_insert(annotation.id.clone());
            }
            out.push(annotation);
            links.push(raw);
        }
    }
    merge_count_members(doc, &mut out, &mut links);
    for (annotation, raw) in out.iter_mut().zip(&links) {
        let find = |object: Option<ObjectId>| object.and_then(|o| by_object.get(&o).cloned());
        let (popup, parent, reply_to) = (find(raw.popup), find(raw.parent), find(raw.reply_to));
        if let Some(base) = annotation.baseline.as_mut() {
            base.model.popup = popup.clone();
            base.model.parent = parent.clone();
            base.model.meta.reply_to = reply_to.clone();
        }
        annotation.popup = popup;
        annotation.parent = parent;
        annotation.meta.reply_to = reply_to;
    }
    out
}

/// The group and the place in it of a further marker of a count of ours: its private part
/// names the count (`Group`, the first annotation's `/NM`) and its place (`Item`, from 1).
fn count_member(doc: &PdfDocument, a: &Annotation) -> Option<(AnnotId, i64)> {
    a.measurement()
        .filter(|m| m.kind == crate::measure::MeasureKind::Count)?;
    let dict = &a.baseline.as_ref()?.dict;
    let own = lookup(doc, dict, PRIVATE_KEY.as_bytes())
        .and_then(|o| o.as_dict().ok())
        .and_then(|p| lookup(doc, p, MODEL_KEY.as_bytes()))
        .and_then(|o| o.as_dict().ok())?;
    let group = lookup(doc, own, b"Group").and_then(text_of)?;
    let item = lookup(doc, own, b"Item").and_then(|o| o.as_i64().ok())?;
    (item >= 1).then_some((AnnotId(group), item))
}

/// A count of ours is written as one annotation per marker (see `MeasureKind::file_subtype`);
/// in the model it is one annotation again. Each further marker is folded into the count it
/// names, on the same page, in the order of its place; the annotations it came from are kept
/// in the count's baseline as [`Member`]s. A marker whose count is not there (another
/// application deleted it) stays a count of one.
fn merge_count_members(doc: &PdfDocument, out: &mut Vec<Annotation>, links: &mut Vec<RawLinks>) {
    let mut found: Vec<(usize, AnnotId, i64)> = Vec::new();
    for (i, a) in out.iter().enumerate() {
        if let Some((group, item)) = count_member(doc, a) {
            found.push((i, group, item));
        }
    }
    if found.is_empty() {
        return;
    }
    let heads: HashMap<(AnnotId, usize), usize> = out
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            a.measurement()
                .is_some_and(|m| m.kind == crate::measure::MeasureKind::Count)
                && count_member(doc, a).is_none()
        })
        .map(|(i, a)| ((a.id.clone(), a.page), i))
        .collect();
    let mut by_head: HashMap<usize, Vec<(i64, usize)>> = HashMap::new();
    for (i, group, item) in found {
        if let Some(head) = heads.get(&(group, out[i].page)) {
            by_head.entry(*head).or_default().push((item, i));
        }
    }
    let mut gone: HashSet<usize> = HashSet::new();
    for (head, mut members) in by_head {
        members.sort();
        for (_, i) in members {
            let member = &out[i];
            let (Some(base), Geometry::Ink(strokes)) = (&member.baseline, &member.geometry) else {
                continue;
            };
            let entry = Member {
                name: lookup(doc, &base.dict, b"NM").and_then(text_of),
                object: base.object,
                page: base.page,
                slot: base.slot,
                rect: rect_of(doc, &base.dict),
            };
            let strokes = strokes.clone();
            let h = &mut out[head];
            if let Geometry::Ink(own) = &mut h.geometry {
                own.extend(strokes.iter().cloned());
            }
            if let Some(hb) = h.baseline.as_mut() {
                if let Geometry::Ink(own) = &mut hb.model.geometry {
                    own.extend(strokes);
                }
                hb.members.push(entry);
            }
            gone.insert(i);
        }
    }
    let mut i = 0;
    out.retain(|_| {
        i += 1;
        !gone.contains(&(i - 1))
    });
    let mut i = 0;
    links.retain(|_| {
        i += 1;
        !gone.contains(&(i - 1))
    });
}

fn set_id(annotation: &mut Annotation, id: AnnotId) {
    if let Some(base) = annotation.baseline.as_mut() {
        base.model.id = id.clone();
    }
    annotation.id = id;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_points_in_either_order_become_the_same_ring() {
        // Acrobat order: TL, TR, BL, BR.
        let common = [10.0, 30.0, 50.0, 30.0, 10.0, 20.0, 50.0, 20.0];
        let ring = quad_from_file(&common);
        let want = [
            Point::new(10.0, 20.0),
            Point::new(50.0, 20.0),
            Point::new(50.0, 30.0),
            Point::new(10.0, 30.0),
        ];
        assert_eq!(ring, want);
        assert_eq!(quad_to_file(&ring), common);
        // Specification order: an anticlockwise ring from the bottom-left.
        let spec = [10.0, 20.0, 50.0, 20.0, 50.0, 30.0, 10.0, 30.0];
        assert_eq!(quad_from_file(&spec), want);
        // A slanted quad in the common order.
        let slanted = [0.0, 10.0, 40.0, 30.0, 5.0, 0.0, 45.0, 20.0];
        let ring = quad_from_file(&slanted);
        assert_eq!(ring[0], Point::new(5.0, 0.0));
        assert_eq!(quad_to_file(&ring), slanted);
    }
}
