//! Legends: a table on the sheet that sums a quantity per group of markups (per subject, per
//! material, per space...), with a swatch that shows what each group looks like.
//!
//! A [`LegendDef`] says what the legend shows; [`LegendDef::evaluate`] works it out from a
//! [`Table`] as plain data ([`Legend`]); [`legend_draw_list`] lays that out as a pl-draw
//! [`DrawList`] that the application places on the page as an annotation's appearance. A
//! legend is refreshed by evaluating it again on the table built after the change and
//! replacing the drawing; nothing in it is stored apart from the definition.

use pl_draw::shapes::{ellipse_path, polyline_path, rect_path};
use pl_draw::{
    measure_text, DrawError, DrawList, DrawOp, Fill, FontStyle, PathOp, Rgb, Stroke, TextRun,
};
use pl_geom::{Affine, Point, Rect, Vec2};
use serde::{Deserialize, Serialize};

use crate::column::{ColumnId, ColumnType};
use crate::quantity::fixed;
use crate::rows::{Col, Table};
use crate::value::Value;
use crate::view::{Body, Filter, GroupKey, View, NO_VALUE_LABEL};
use pl_annot::MeasureKind;

/// What a legend shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegendDef {
    pub title: String,
    /// The column whose values make the legend's rows (Subject, a choice column, Space...).
    pub group_by: ColumnId,
    /// The column each row totals; `None` counts the markups.
    #[serde(default)]
    pub quantity: Option<ColumnId>,
    /// The unit written after each quantity; the column's own unit when `None`.
    #[serde(default)]
    pub unit: Option<String>,
    /// Which markups the legend covers (all when empty).
    #[serde(default)]
    pub filter: Filter,
    /// Whether a total line closes the legend.
    #[serde(default = "yes")]
    pub show_total: bool,
    /// Whether rows with nothing in the grouping column (`(none)`) are listed.
    #[serde(default)]
    pub show_ungrouped: bool,
}

fn yes() -> bool {
    true
}

impl LegendDef {
    pub fn new(
        title: &str,
        group_by: impl Into<ColumnId>,
        quantity: Option<ColumnId>,
    ) -> LegendDef {
        LegendDef {
            title: title.to_string(),
            group_by: group_by.into(),
            quantity,
            unit: None,
            filter: Filter::default(),
            show_total: true,
            show_ungrouped: false,
        }
    }
}

/// What a legend row's swatch draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwatchShape {
    /// A short line: lengths and other line markups.
    Line,
    /// A filled square: areas and volumes.
    Area,
    /// A marker: counts.
    Marker,
    /// An outlined square: anything else.
    Box,
}

/// How a legend row's markups look, from the first markup of the group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Swatch {
    pub shape: SwatchShape,
    pub colour: Option<[u8; 3]>,
    pub fill: Option<[u8; 3]>,
    /// The count marker's name (`Dot`, `Ring`, ...), for a count.
    pub symbol: Option<String>,
}

/// One row of an evaluated legend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegendRow {
    pub swatch: Swatch,
    pub label: String,
    /// Markups in the row.
    pub count: usize,
    /// The total of the quantity column over the row's markups (the count when the legend
    /// counts markups); `None` when none of them has a number there.
    pub quantity: Option<f64>,
    /// The quantity as written in the legend.
    pub text: String,
    pub unit: String,
}

/// A legend worked out from a table: plain data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Legend {
    pub title: String,
    pub rows: Vec<LegendRow>,
    /// The total over every row listed, when the definition asks for one.
    pub total: Option<LegendRow>,
}

fn swatch_of(table: &Table, row: usize) -> Swatch {
    let r = &table.rows[row];
    let shape = match r.measure.as_ref().map(|m| m.kind) {
        Some(MeasureKind::Count) => SwatchShape::Marker,
        Some(MeasureKind::Area | MeasureKind::Volume) => SwatchShape::Area,
        Some(_) => SwatchShape::Line,
        None => match r.kind {
            pl_annot::AnnotKind::Line
            | pl_annot::AnnotKind::PolyLine
            | pl_annot::AnnotKind::Ink => SwatchShape::Line,
            pl_annot::AnnotKind::Polygon if r.fill.is_some() => SwatchShape::Area,
            pl_annot::AnnotKind::Square | pl_annot::AnnotKind::Circle if r.fill.is_some() => {
                SwatchShape::Area
            }
            _ => SwatchShape::Box,
        },
    };
    Swatch {
        shape,
        colour: r.colour,
        fill: r.fill,
        symbol: r.measure.as_ref().and_then(|m| m.symbol.clone()),
    }
}

fn rows_of(body: &Body) -> Vec<usize> {
    let mut out = Vec::new();
    fn walk(body: &Body, out: &mut Vec<usize>) {
        match body {
            Body::Rows(r) => out.extend(r),
            Body::Groups(g) => g.iter().for_each(|g| walk(&g.body, out)),
        }
    }
    walk(body, &mut out);
    out
}

impl LegendDef {
    /// The legend as it stands for `table`: one row per value of the grouping column among the
    /// markups the filter keeps, in the list's order of that column.
    pub fn evaluate(&self, table: &Table) -> Legend {
        let view = View::build(
            table,
            &self.filter,
            &[],
            &[GroupKey::asc(self.group_by.clone())],
        );
        let quantity_col = self.quantity.as_ref().map(|q| table.col(q));
        let unit = self.unit.clone().unwrap_or_else(|| match &self.quantity {
            None => "ea".to_string(),
            Some(q) => table.unit_label(q).unwrap_or("").to_string(),
        });
        let text = |n: f64| -> String {
            match &self.quantity {
                None => fixed(n, 0),
                Some(q @ ColumnId::Builtin(b)) => match b.quantity_kind() {
                    Some(k) => table.units.number_text(k, n),
                    None => match table.places(q) {
                        Some(p) => fixed(n, p),
                        None => Value::number(n).display(),
                    },
                },
                Some(ColumnId::Custom(id)) => match table.custom.iter().find(|c| c.id == *id) {
                    Some(c) if c.kind == ColumnType::Number || c.kind == ColumnType::Formula => {
                        c.display(&Value::number(n))
                    }
                    _ => Value::number(n).display(),
                },
            }
        };
        let sum = |rows: &[usize]| -> Option<f64> {
            match quantity_col {
                None => Some(rows.len() as f64),
                Some(col) => {
                    let mut any = false;
                    let mut total = 0.0;
                    for r in rows {
                        if let Some(n) = table.rows[*r].value(col).as_number() {
                            total += n;
                            any = true;
                        }
                    }
                    any.then_some(total)
                }
            }
        };
        let mut rows = Vec::new();
        let mut covered: Vec<usize> = Vec::new();
        if let Body::Groups(groups) = &view.body {
            for g in groups {
                if g.key.is_empty() && !self.show_ungrouped {
                    continue;
                }
                let members = rows_of(&g.body);
                let quantity = sum(&members);
                rows.push(LegendRow {
                    swatch: swatch_of(table, members[0]),
                    label: if g.key.is_empty() {
                        NO_VALUE_LABEL.to_string()
                    } else {
                        g.label.clone()
                    },
                    count: members.len(),
                    quantity,
                    text: quantity.map(text).unwrap_or_default(),
                    unit: unit.clone(),
                });
                covered.extend(members);
            }
        }
        let total = self.show_total.then(|| {
            let quantity = sum(&covered);
            LegendRow {
                swatch: Swatch {
                    shape: SwatchShape::Box,
                    colour: None,
                    fill: None,
                    symbol: None,
                },
                label: "Total".to_string(),
                count: covered.len(),
                quantity,
                text: quantity.map(text).unwrap_or_default(),
                unit: unit.clone(),
            }
        });
        Legend {
            title: self.title.clone(),
            rows,
            total,
        }
    }
}

impl Legend {
    /// The legend as lines of text: the title, then `label: quantity unit` per row and the
    /// total. This is what an annotation's `/Contents` carries.
    pub fn lines(&self) -> Vec<String> {
        let row = |r: &LegendRow| {
            let q = [r.text.as_str(), r.unit.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            format!("{}: {q}", r.label)
        };
        std::iter::once(self.title.clone())
            .chain(self.rows.iter().map(row))
            .chain(self.total.iter().map(row))
            .collect()
    }

    /// [`Legend::lines`] joined with line breaks.
    pub fn contents(&self) -> String {
        self.lines().join("\n")
    }
}

/// How a legend is drawn. Sizes are in page units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegendStyle {
    pub text_size: f64,
    pub text: Rgb,
    pub border: Rgb,
    pub border_width: f64,
    /// The background; `None` leaves the sheet showing through.
    pub background: Option<Rgb>,
    /// The page's `/Rotate`: the legend is drawn to read upright on the displayed page.
    pub rotation: u16,
}

impl Default for LegendStyle {
    fn default() -> LegendStyle {
        LegendStyle {
            text_size: 9.0,
            text: Rgb::BLACK,
            border: Rgb::new(0.35, 0.35, 0.35),
            border_width: 0.75,
            background: Some(Rgb::WHITE),
            rotation: 0,
        }
    }
}

/// A legend laid out for the page.
#[derive(Debug, Clone, PartialEq)]
pub struct LegendDrawing {
    pub list: DrawList,
    /// The legend's box in page space (the annotation's `/Rect`).
    pub rect: Rect,
}

/// The page-space direction that reads to the right on a page shown with `/Rotate`.
fn display_right(rotation: u16) -> Vec2 {
    match rotation % 360 {
        90 => Vec2::new(0.0, 1.0),
        180 => Vec2::new(-1.0, 0.0),
        270 => Vec2::new(0.0, -1.0),
        _ => Vec2::new(1.0, 0.0),
    }
}

fn rgb(c: Option<[u8; 3]>) -> Option<Rgb> {
    c.map(|[r, g, b]| Rgb::from_u8(r, g, b))
}

fn swatch_ops(s: &Swatch, x: f64, centre_y: f64, size: f64) -> Vec<DrawOp> {
    let colour = rgb(s.colour).or(rgb(s.fill)).unwrap_or(Rgb::BLACK);
    let half = size / 2.0;
    let square = Rect::new(x, centre_y - half, x + size, centre_y + half);
    match s.shape {
        SwatchShape::Line => vec![DrawOp::Path(PathOp::stroked(
            polyline_path(&[Point::new(x, centre_y), Point::new(x + size, centre_y)]),
            Stroke::new((size * 0.2).max(0.5), colour),
        ))],
        SwatchShape::Area => {
            let fill = rgb(s.fill).unwrap_or(colour);
            vec![DrawOp::Path(
                PathOp::filled(rect_path(square), Fill::new(fill))
                    .with_stroke(Stroke::new(0.75, colour)),
            )]
        }
        SwatchShape::Marker => vec![DrawOp::Path(PathOp::filled(
            ellipse_path(square.inflate(-size * 0.1, -size * 0.1)),
            Fill::new(colour),
        ))],
        SwatchShape::Box => vec![DrawOp::Path(PathOp::stroked(
            rect_path(square),
            Stroke::new(0.75, colour),
        ))],
    }
}

/// Lays a legend out as a table: the title in bold, then one line per row (swatch, label,
/// quantity right-aligned, unit), then the total in bold, inside a bordered box whose top-left
/// corner (as the page is displayed) is `top_left`.
///
/// Fails with [`DrawError::NotWinAnsi`] when a label or the title holds a character the
/// standard fonts cannot write, and with [`DrawError::InvalidNumber`] for a size or position
/// that is not finite.
pub fn legend_draw_list(
    legend: &Legend,
    top_left: Point,
    style: &LegendStyle,
) -> Result<LegendDrawing, DrawError> {
    let s = style.text_size;
    if !(s.is_finite() && s > 0.0 && top_left.x.is_finite() && top_left.y.is_finite()) {
        return Err(DrawError::InvalidNumber);
    }
    let pad = 0.6 * s;
    let line = 1.6 * s;
    let swatch = s;
    let gap = 0.8 * s;
    let width = |t: &str, f: FontStyle| measure_text(t, f, s).map(|m| m.width);
    let cap = measure_text("H", FontStyle::Regular, s)?.cap_height;

    let all_rows: Vec<(&LegendRow, bool)> = legend
        .rows
        .iter()
        .map(|r| (r, false))
        .chain(legend.total.iter().map(|r| (r, true)))
        .collect();
    let mut label_w: f64 = 0.0;
    let mut qty_w: f64 = 0.0;
    let mut unit_w: f64 = 0.0;
    for (r, bold) in &all_rows {
        let f = if *bold {
            FontStyle::Bold
        } else {
            FontStyle::Regular
        };
        label_w = label_w.max(width(&r.label, f)?);
        qty_w = qty_w.max(width(&r.text, f)?);
        unit_w = unit_w.max(width(&r.unit, f)?);
    }
    let title_w = width(&legend.title, FontStyle::Bold)?;
    let body_w = swatch
        + gap
        + label_w
        + 2.0 * gap
        + qty_w
        + if unit_w > 0.0 { 0.4 * s + unit_w } else { 0.0 };
    let w = 2.0 * pad + body_w.max(title_w);
    let h = 2.0 * pad + line * (1 + all_rows.len()) as f64;

    // Laid out upright with the top-left corner at the origin, y up (so rows go to negative y).
    let mut ops: Vec<DrawOp> = Vec::new();
    let frame = Rect::new(0.0, -h, w, 0.0);
    let mut frame_op = PathOp::stroked(
        rect_path(frame),
        Stroke::new(style.border_width, style.border),
    );
    if let Some(bg) = style.background {
        frame_op = frame_op.with_fill(Fill::new(bg));
    }
    ops.push(DrawOp::Path(frame_op));
    let baseline = |i: usize| -pad - line * i as f64 - line / 2.0 - cap / 2.0;
    let text = |t: &str, x: f64, y: f64, f: FontStyle| -> Result<DrawOp, DrawError> {
        Ok(DrawOp::Text(TextRun::new(
            t,
            Point::new(x, y),
            s,
            f,
            style.text,
        )?))
    };
    ops.push(text(&legend.title, pad, baseline(0), FontStyle::Bold)?);
    let rule_y = -pad - line;
    ops.push(DrawOp::Path(PathOp::stroked(
        polyline_path(&[Point::new(pad, rule_y), Point::new(w - pad, rule_y)]),
        Stroke::new(style.border_width * 0.75, style.border),
    )));
    let label_x = pad + swatch + gap;
    let qty_right = label_x + label_w + 2.0 * gap + qty_w;
    for (i, (r, bold)) in all_rows.iter().enumerate() {
        let y = baseline(i + 1);
        let f = if *bold {
            FontStyle::Bold
        } else {
            FontStyle::Regular
        };
        if *bold {
            let top = -pad - line * (i + 1) as f64;
            ops.push(DrawOp::Path(PathOp::stroked(
                polyline_path(&[Point::new(pad, top), Point::new(w - pad, top)]),
                Stroke::new(style.border_width * 0.75, style.border),
            )));
        } else {
            ops.extend(swatch_ops(&r.swatch, pad, y + cap / 2.0, swatch));
        }
        if !r.label.is_empty() {
            ops.push(text(&r.label, label_x, y, f)?);
        }
        if !r.text.is_empty() {
            ops.push(text(&r.text, qty_right - width(&r.text, f)?, y, f)?);
        }
        if !r.unit.is_empty() {
            ops.push(text(&r.unit, qty_right + 0.4 * s, y, f)?);
        }
    }

    let right = display_right(style.rotation);
    let up = Vec2::new(-right.y, right.x);
    let transform = Affine::new([right.x, right.y, up.x, up.y, top_left.x, top_left.y]);
    let list = DrawList::from_ops(ops).transformed(transform);
    list.validate()?;
    let rect = transform.transform_rect_bbox(frame);
    Ok(LegendDrawing { list, rect })
}

/// The columns a legend can group by: every column but the number ones.
pub fn can_group_by(table: &Table, column: &ColumnId) -> bool {
    !matches!(table.col(column), Col::Missing)
        && table.column_type(column) != ColumnType::Number
        && table.column_type(column) != ColumnType::Formula
}
