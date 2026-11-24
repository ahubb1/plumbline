//! The measurement steps of the JSON command file ([`crate::script`]).
//!
//! ```json
//! { "op": "set_scale", "page": 1, "scale": "1\" = 20'" },
//! { "op": "set_scale", "page": 1, "last_page": 3, "scale": "1:100",
//!   "format": { "length_unit": "m", "precision": { "decimal": 3 },
//!               "area_unit": "sq m", "area_places": 2 } },
//! { "op": "set_scale", "scale": null },
//! { "op": "default_scale", "scale": "1/4\" = 1'-0\"" },
//! { "op": "calibrate", "page": 1, "from": [72, 100], "to": [216, 100],
//!   "length": "40'", "unit": "ft" },
//! { "op": "add_viewport", "page": 1, "name": "Detail A", "bbox": [300, 300, 500, 500],
//!   "scale": "1\" = 5'" },
//! { "op": "edit_viewport", "page": 1, "name": "Detail A", "bbox": [300, 300, 520, 520] },
//! { "op": "remove_viewport", "page": 1, "name": "Detail A" },
//! { "op": "measure", "id": "m1", "kind": "Length", "page": 1, "points": [[72, 72], [144, 72]] },
//! { "op": "measure", "id": "slab", "kind": "Volume", "page": 1,
//!   "points": [[0, 0], [144, 0], [144, 72], [0, 72]],
//!   "cutouts": [[[10, 10], [20, 10], [20, 20]]], "depth": "6\"" },
//! { "op": "measure", "id": "c", "kind": "Count", "page": 1, "points": [[5, 5]],
//!   "symbol": "Ring" },
//! { "op": "cutout", "id": "slab", "points": [[30, 30], [40, 30], [40, 40]] },
//! { "op": "remove_cutout", "id": "slab", "index": 0 },
//! { "op": "depth", "id": "slab", "depth": "150 mm" },
//! { "op": "count", "id": "c", "add": [9, 9] },
//! { "op": "count", "id": "c", "remove": 0 },
//! { "op": "expect_measure", "id": "m1", "text": "20.00'", "length": 6.096 },
//! { "op": "expect_scale", "page": 1, "ratio": "1 in = 20 ft", "viewports": ["Detail A"] }
//! ```
//!
//! * `scale` is written as a person writes it (`1" = 20'`, `1:100`, `1/4" = 1'-0"`); `null`
//!   clears. Without `format`, values are shown the usual way for the scale's real unit.
//!   `set_scale` without `page` sets every page; with `last_page` a range (one undo step).
//! * `calibrate` makes the page's scale (or, with `viewport`, that viewport's) the one at which
//!   `from` to `to` is `length`; `unit` is the real unit the scale is stated in.
//! * `kind` of `measure` is `Length`, `Polylength`, `Perimeter`, `Area`, `Volume`, `Angle`
//!   (three points: a ray end, the vertex, the other ray end), `Radius` (centre, edge),
//!   `Diameter` or `Count`. Lengths such as `depth` are typed with their unit (`6"`,
//!   `150 mm`, `0.5`: metres).
//! * `expect_measure` checks the caption `text` exactly and the SI values (`length` in m,
//!   `area` in m², `volume` in m³, `angle` in degrees) within `tolerance` (relative,
//!   default 1e-9); `ratio` is the `/R` of its scale and `viewport` the viewport it was taken
//!   in (`""` for the page's own scale).

use pl_annot::{AnnotId, CountSymbol, Geometry, MeasureKind, Origin, ScaleRef};
use pl_geom::{parse_length, Point, Unit};
use serde::{Deserialize, Serialize};

use crate::measure_data::{unit_from_name, FormatData, ScaleSetting};
use crate::measure_ops::MeasureShape;
use crate::session::Document;

type StepResult = std::result::Result<String, String>;

fn text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn pt(p: &[f64; 2]) -> Point {
    Point::new(p[0], p[1])
}

fn page_index(doc: &Document, page: u32) -> std::result::Result<u32, String> {
    let count = doc.page_count();
    if page == 0 || page > count {
        return Err(format!(
            "page {page} is out of range: the document has {count} page{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    Ok(page - 1)
}

fn setting(
    scale: Option<&str>,
    format: Option<&FormatData>,
) -> std::result::Result<Option<ScaleSetting>, String> {
    let format = format.map(FormatData::to_format).transpose()?;
    scale
        .map(|s| ScaleSetting::parse(s, format.as_ref()))
        .transpose()
}

fn metres(text: &str) -> std::result::Result<f64, String> {
    parse_length(text, Unit::Metre).map_err(|e| format!("{text:?} is not a length: {e}"))
}

fn changed(out: &crate::Outcome) -> &'static str {
    if out.changed {
        ""
    } else {
        " (no change)"
    }
}

/// `set_scale`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetScaleSpec {
    /// From 1; absent for every page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// The last page of a range starting at `page`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_page: Option<u32>,
    /// `null` clears.
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FormatData>,
}

/// `default_scale`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultScaleSpec {
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FormatData>,
}

/// `calibrate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrateSpec {
    pub page: u32,
    pub from: [f64; 2],
    pub to: [f64; 2],
    /// The real length from `from` to `to`, with its unit (`40'`, `12.5 m`).
    pub length: String,
    /// The real unit the scale is stated in: `ft`, `ft-in`, `in`, `m`, `mm`, ...
    pub unit: String,
    /// Calibrate this viewport (which keeps its box) instead of the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FormatData>,
}

/// `add_viewport` and `edit_viewport`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewportSpec {
    pub page: u32,
    pub name: String,
    /// `[x0, y0, x1, y1]`; `edit_viewport` keeps the box when it is left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FormatData>,
}

/// `measure`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureStepSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: String,
    pub page: u32,
    #[serde(default)]
    pub points: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cutouts: Vec<Vec<[f64; 2]>>,
    /// A Volume's depth, with its unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<String>,
    /// A Count's marker: `Dot`, `Ring`, `Square`, `Diamond`, `Triangle`, `Hexagon`, `Star`,
    /// `Cross`, `Plus` or `Check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// `expect_measure`.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectMeasureSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<f64>,
}

/// `expect_scale`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectScaleSpec {
    pub page: u32,
    /// The `/R` of the page's own scale; `null` for none. Left out, not checked.
    #[serde(
        default,
        deserialize_with = "crate::script::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub ratio: Option<Option<String>>,
    /// The names of the page's viewports, bottom to top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewports: Option<Vec<String>>,
}

pub(crate) fn set_scale(doc: &mut Document, spec: &SetScaleSpec) -> StepResult {
    let scale = setting(spec.scale.as_deref(), spec.format.as_ref())?;
    let (first, last) = match (spec.page, spec.last_page) {
        (None, None) => (0, doc.page_count().saturating_sub(1)),
        (Some(p), None) => {
            let i = page_index(doc, p)?;
            (i, i)
        }
        (Some(p), Some(q)) => (page_index(doc, p)?, page_index(doc, q)?),
        (None, Some(_)) => return Err("\"last_page\" needs \"page\"".into()),
    };
    let out = if first == last {
        doc.set_page_scale(first, scale)
    } else {
        doc.apply_scale_to_pages(first, last, scale)
    }
    .map_err(text)?;
    Ok(format!(
        "pages {}-{}: {}{}",
        first + 1,
        last + 1,
        spec.scale.as_deref().unwrap_or("none"),
        changed(&out)
    ))
}

pub(crate) fn default_scale(doc: &mut Document, spec: &DefaultScaleSpec) -> StepResult {
    let scale = setting(spec.scale.as_deref(), spec.format.as_ref())?;
    let out = doc.set_default_scale(scale).map_err(text)?;
    Ok(format!(
        "{}{}",
        spec.scale.as_deref().unwrap_or("none"),
        changed(&out)
    ))
}

pub(crate) fn calibrate(doc: &mut Document, spec: &CalibrateSpec) -> StepResult {
    let page = page_index(doc, spec.page)?;
    let unit = unit_from_name(&spec.unit).ok_or_else(|| format!("unknown unit {:?}", spec.unit))?;
    let real = metres(&spec.length)?;
    let format = spec
        .format
        .as_ref()
        .map(FormatData::to_format)
        .transpose()?;
    let scale = doc
        .calibrated_scale(page, pt(&spec.from), pt(&spec.to), real, unit)
        .map_err(text)?;
    let setting = ScaleSetting::new(&scale, format.as_ref());
    match &spec.viewport {
        None => {
            doc.set_page_scale(page, Some(setting)).map_err(text)?;
        }
        Some(name) => {
            let bbox = doc
                .scales()
                .map_err(text)?
                .page(page as usize)
                .and_then(|p| p.get(name))
                .map(|v| [v.bbox.x0, v.bbox.y0, v.bbox.x1, v.bbox.y1])
                .ok_or_else(|| format!("page {} has no viewport {name:?}", spec.page))?;
            doc.edit_viewport(page, name, bbox, Some(setting))
                .map_err(text)?;
        }
    }
    Ok(format!("page {}: {}", spec.page, scale.pdf_ratio()))
}

pub(crate) fn viewport(doc: &mut Document, spec: &ViewportSpec, add: bool) -> StepResult {
    let page = page_index(doc, spec.page)?;
    let scale = setting(spec.scale.as_deref(), spec.format.as_ref())?;
    let out = if add {
        let bbox = spec.bbox.ok_or("add_viewport needs \"bbox\"")?;
        doc.add_viewport(page, &spec.name, bbox, scale)
    } else {
        let bbox = match spec.bbox {
            Some(b) => b,
            None => doc
                .scales()
                .map_err(text)?
                .page(page as usize)
                .and_then(|p| p.get(&spec.name))
                .map(|v| [v.bbox.x0, v.bbox.y0, v.bbox.x1, v.bbox.y1])
                .ok_or_else(|| format!("page {} has no viewport {:?}", spec.page, spec.name))?,
        };
        doc.edit_viewport(page, &spec.name, bbox, scale)
    }
    .map_err(text)?;
    Ok(format!(
        "{:?} on page {}{}",
        spec.name,
        spec.page,
        changed(&out)
    ))
}

pub(crate) fn remove_viewport(doc: &mut Document, page: u32, name: &str) -> StepResult {
    let index = page_index(doc, page)?;
    doc.remove_viewport(index, name).map_err(text)?;
    Ok(format!("{name:?} from page {page}"))
}

fn shape(spec: &MeasureStepSpec) -> std::result::Result<MeasureShape, String> {
    let kind = MeasureKind::from_name(&spec.kind)
        .ok_or_else(|| format!("unknown measurement kind {:?}", spec.kind))?;
    let points: Vec<Point> = spec.points.iter().map(pt).collect();
    let cutouts: Vec<Vec<Point>> = spec
        .cutouts
        .iter()
        .map(|r| r.iter().map(pt).collect())
        .collect();
    let exactly = |n: usize| -> std::result::Result<(), String> {
        if points.len() == n {
            Ok(())
        } else {
            Err(format!("a {} needs exactly {n} points", spec.kind))
        }
    };
    Ok(match kind {
        MeasureKind::Length => {
            exactly(2)?;
            MeasureShape::Length {
                a: points[0],
                b: points[1],
            }
        }
        MeasureKind::Diameter => {
            exactly(2)?;
            MeasureShape::Diameter {
                a: points[0],
                b: points[1],
            }
        }
        MeasureKind::Radius => {
            exactly(2)?;
            MeasureShape::Radius {
                centre: points[0],
                edge: points[1],
            }
        }
        MeasureKind::Angle => {
            exactly(3)?;
            MeasureShape::Angle {
                a: points[0],
                vertex: points[1],
                b: points[2],
            }
        }
        MeasureKind::Polylength => MeasureShape::Polylength(points),
        MeasureKind::Perimeter => MeasureShape::Perimeter(points),
        MeasureKind::Area => MeasureShape::Area {
            outline: points,
            cutouts,
        },
        MeasureKind::Volume => MeasureShape::Volume {
            outline: points,
            cutouts,
            depth: metres(spec.depth.as_deref().ok_or("a Volume needs \"depth\"")?)?,
        },
        MeasureKind::Count => MeasureShape::Count {
            points,
            symbol: match &spec.symbol {
                Some(name) => CountSymbol::from_name(name)
                    .ok_or_else(|| format!("unknown count symbol {name:?}"))?,
                None => CountSymbol::default(),
            },
        },
    })
}

pub(crate) fn measure(doc: &mut Document, spec: &MeasureStepSpec, origin: &Origin) -> StepResult {
    let page = page_index(doc, spec.page)?;
    let shape = shape(spec)?;
    let rotation = doc.load_geometry(page).map_err(text)?.rotate().degrees();
    let mut a = crate::measure_ops::new_measurement(
        doc.scales().map_err(text)?,
        page as usize,
        shape,
        rotation,
        origin,
    )
    .map_err(text)?;
    if let Some(wanted) = &spec.id {
        a.id = AnnotId(wanted.clone());
    }
    let id = a.id.clone();
    doc.apply(crate::Command::add(a)).map_err(text)?;
    let a = doc.annotation(&id).ok_or("the measurement vanished")?;
    Ok(format!(
        "{} {id} on page {}: {}",
        spec.kind,
        spec.page,
        a.measure_text().unwrap_or_default()
    ))
}

pub(crate) fn cutout(
    doc: &mut Document,
    id: &str,
    points: &[[f64; 2]],
    when: pl_annot::PdfDate,
) -> StepResult {
    let ring = points.iter().map(pt).collect();
    doc.add_cutout(&AnnotId(id.to_string()), ring, when)
        .map_err(text)?;
    value_line(doc, id)
}

pub(crate) fn remove_cutout(
    doc: &mut Document,
    id: &str,
    index: usize,
    when: pl_annot::PdfDate,
) -> StepResult {
    doc.remove_cutout(&AnnotId(id.to_string()), index, when)
        .map_err(text)?;
    value_line(doc, id)
}

pub(crate) fn depth(
    doc: &mut Document,
    id: &str,
    depth: &str,
    when: pl_annot::PdfDate,
) -> StepResult {
    doc.set_depth(&AnnotId(id.to_string()), metres(depth)?, when)
        .map_err(text)?;
    value_line(doc, id)
}

pub(crate) fn count(
    doc: &mut Document,
    id: &str,
    add: Option<[f64; 2]>,
    remove: Option<usize>,
    when: pl_annot::PdfDate,
) -> StepResult {
    let target = AnnotId(id.to_string());
    match (add, remove) {
        (Some(p), None) => doc.add_count_marker(&target, pt(&p), when),
        (None, Some(i)) => doc.remove_count_marker(&target, i, when),
        _ => return Err("count needs either \"add\" or \"remove\"".into()),
    }
    .map_err(text)?;
    value_line(doc, id)
}

/// `move_measure`: new points for a measurement (a dragged vertex).
pub(crate) fn reshape(
    doc: &mut Document,
    id: &str,
    points: &[[f64; 2]],
    when: pl_annot::PdfDate,
) -> StepResult {
    let target = AnnotId(id.to_string());
    let a = doc
        .annotation(&target)
        .ok_or_else(|| format!("there is no annotation {id}"))?;
    let points: Vec<Point> = points.iter().map(pt).collect();
    let geometry = match &a.geometry {
        Geometry::Line { .. } if points.len() == 2 => Geometry::Line {
            a: points[0],
            b: points[1],
        },
        Geometry::Ink(_) => Geometry::Ink(points.iter().map(|p| vec![*p, *p]).collect()),
        _ => Geometry::Points(points),
    };
    doc.set_measure_geometry(&target, geometry, when)
        .map_err(text)?;
    value_line(doc, id)
}

fn value_line(doc: &Document, id: &str) -> StepResult {
    let a = doc
        .annotation(&AnnotId(id.to_string()))
        .ok_or_else(|| format!("there is no annotation {id}"))?;
    Ok(format!("{id}: {}", a.measure_text().unwrap_or_default()))
}

pub(crate) fn expect_measure(doc: &mut Document, spec: &ExpectMeasureSpec) -> StepResult {
    let id = AnnotId(spec.id.clone());
    let set = doc.annots().map_err(text)?.set();
    let a = set
        .get(&id)
        .ok_or_else(|| format!("there is no annotation {id}"))?;
    let m = a
        .measurement()
        .ok_or_else(|| format!("annotation {id} is not a measurement"))?;
    let q = a.quantities().unwrap_or_default();
    let tol = spec.tolerance.unwrap_or(1e-9);
    let mut wrong = Vec::new();
    let mut number = |what: &str, found: Option<f64>, want: Option<f64>| {
        if let Some(want) = want {
            match found {
                Some(v) if (v - want).abs() <= tol * want.abs().max(1.0) => {}
                other => wrong.push(format!("{what} is {other:?}, expected {want}")),
            }
        }
    };
    number("length", q.length, spec.length);
    number("area", q.area, spec.area);
    number("volume", q.volume, spec.volume);
    number("angle", q.angle.map(f64::to_degrees), spec.angle);
    if let Some(kind) = &spec.kind {
        if m.kind.name() != kind {
            wrong.push(format!("kind is {}, expected {kind}", m.kind.name()));
        }
    }
    if let Some(count) = spec.count {
        if q.count != Some(count) {
            wrong.push(format!("count is {:?}, expected {count}", q.count));
        }
    }
    let shown = a.measure_text().unwrap_or_default();
    if let Some(want) = &spec.text {
        if &shown != want {
            wrong.push(format!("text is {shown:?}, expected {want:?}"));
        }
        // /Contents is the bare value (no R or diameter sign: Revu's convention).
        if a.meta.contents != a.measure_value_text() {
            wrong.push(format!(
                "contents is {:?}, expected {:?}",
                a.meta.contents,
                a.measure_value_text()
            ));
        }
    }
    if let Some(want) = &spec.ratio {
        let found = m.spec.as_ref().map(|s| s.ratio.clone()).unwrap_or_default();
        if &found != want {
            wrong.push(format!("scale is {found:?}, expected {want:?}"));
        }
    }
    if let Some(want) = &spec.viewport {
        let found = match &m.scale {
            ScaleRef::Page => String::new(),
            ScaleRef::Viewport(name) => name.clone(),
        };
        if &found != want {
            wrong.push(format!("viewport is {found:?}, expected {want:?}"));
        }
    }
    if wrong.is_empty() {
        Ok(format!("{id} is {shown}"))
    } else {
        Err(format!("measurement {id}: {}", wrong.join("; ")))
    }
}

pub(crate) fn expect_scale(doc: &mut Document, spec: &ExpectScaleSpec) -> StepResult {
    let page = page_index(doc, spec.page)?;
    let scales = doc.scales().map_err(text)?;
    let vps = scales
        .page(page as usize)
        .ok_or_else(|| format!("page {} has no scale state", spec.page))?;
    let mut wrong = Vec::new();
    let ratio = vps.page_scale.as_ref().map(|s| s.ratio.clone());
    if let Some(want) = &spec.ratio {
        if &ratio != want {
            wrong.push(format!("the page scale is {ratio:?}, expected {want:?}"));
        }
    }
    let names: Vec<String> = vps.viewports.iter().map(|v| v.name.clone()).collect();
    if let Some(want) = &spec.viewports {
        if &names != want {
            wrong.push(format!("the viewports are {names:?}, expected {want:?}"));
        }
    }
    if wrong.is_empty() {
        Ok(format!(
            "page {}: {}, {} viewport(s)",
            spec.page,
            ratio.as_deref().unwrap_or("no scale"),
            names.len()
        ))
    } else {
        Err(wrong.join("; "))
    }
}
