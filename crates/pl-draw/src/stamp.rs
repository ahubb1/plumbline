//! Stamp artwork: Plumbline's own set of review stamps, built as [`DrawList`]s.
//!
//! A stamp is a rounded double frame (a heavy outer rule and a fine inner one) around a bold
//! label, with an optional second line (who and when) under a short rule. It is drawn in one
//! ink colour with no fill, so the drawing under it stays readable. The design is this
//! project's own; no artwork is copied from another product.

use kurbo::{BezPath, Point, Rect, RoundedRect, Shape};

use crate::list::{DrawList, DrawOp, PathOp, TextRun};
use crate::style::{LineCap, LineJoin, Rgb, Stroke};
use crate::text::{measure_text, winansi_encode, FontStyle};
use crate::DrawError;

/// The ink a stamp is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StampScheme {
    /// Approval.
    #[default]
    Green,
    /// Rejection, void.
    Red,
    /// Information, review.
    Blue,
    /// Change, caution.
    Amber,
    /// Neutral: draft, superseded.
    Slate,
}

impl StampScheme {
    pub const ALL: [StampScheme; 5] = [
        StampScheme::Green,
        StampScheme::Red,
        StampScheme::Blue,
        StampScheme::Amber,
        StampScheme::Slate,
    ];

    /// The ink colour.
    pub fn ink(self) -> Rgb {
        match self {
            StampScheme::Green => Rgb::new(0.0, 0.5, 0.25),
            StampScheme::Red => Rgb::new(0.75, 0.125, 0.125),
            StampScheme::Blue => Rgb::new(0.125, 0.25, 0.625),
            StampScheme::Amber => Rgb::new(0.75, 0.4375, 0.0),
            StampScheme::Slate => Rgb::new(0.3125, 0.375, 0.4375),
        }
    }

    /// A stable name, for storing the scheme.
    pub fn name(self) -> &'static str {
        match self {
            StampScheme::Green => "Green",
            StampScheme::Red => "Red",
            StampScheme::Blue => "Blue",
            StampScheme::Amber => "Amber",
            StampScheme::Slate => "Slate",
        }
    }

    pub fn from_name(name: &str) -> Option<StampScheme> {
        StampScheme::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// What a stamp says.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StampSpec {
    /// The annotation `/Name` this stamp is stored under (`Approved`, `Draft`, ...).
    pub name: String,
    /// The main line, drawn bold. One line of WinAnsi text.
    pub label: String,
    /// The second line: author, date, a reference. One line of WinAnsi text.
    pub detail: Option<String>,
    pub scheme: StampScheme,
}

impl StampSpec {
    pub fn new(name: &str, label: &str, scheme: StampScheme) -> StampSpec {
        StampSpec {
            name: name.to_string(),
            label: label.to_string(),
            detail: None,
            scheme,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> StampSpec {
        self.detail = Some(detail.into());
        self
    }

    /// APPROVED, green. `/Name /Approved` is one of the standard stamp names.
    pub fn approved() -> StampSpec {
        StampSpec::new("Approved", "APPROVED", StampScheme::Green)
    }

    /// REVISED, amber.
    pub fn revised() -> StampSpec {
        StampSpec::new("Revised", "REVISED", StampScheme::Amber)
    }

    /// FOR REVIEW, blue. `/Name /ForComment` is the nearest standard stamp name.
    pub fn for_review() -> StampSpec {
        StampSpec::new("ForComment", "FOR REVIEW", StampScheme::Blue)
    }

    /// VOID, red.
    pub fn void() -> StampSpec {
        StampSpec::new("Void", "VOID", StampScheme::Red)
    }

    /// DRAFT, slate. `/Name /Draft` is one of the standard stamp names.
    pub fn draft() -> StampSpec {
        StampSpec::new("Draft", "DRAFT", StampScheme::Slate)
    }

    /// The built-in set, in menu order.
    pub fn standard_set() -> Vec<StampSpec> {
        vec![
            StampSpec::approved(),
            StampSpec::revised(),
            StampSpec::for_review(),
            StampSpec::void(),
            StampSpec::draft(),
        ]
    }

    /// The size the stamp is designed at, in page units, with the label 20 units tall: the
    /// size a stamp tool places it at.
    pub fn natural_size(&self) -> Result<(f64, f64), DrawError> {
        winansi_encode(&self.label)?;
        let label = measure_text(&self.label, FontStyle::Bold, 20.0)?.width;
        let detail = match &self.detail {
            Some(d) => measure_text(d, FontStyle::Regular, 20.0 * DETAIL_RATIO)?.width,
            None => 0.0,
        };
        let height = if self.detail.is_some() { 54.0 } else { 38.0 };
        // Frame, inner rule and side margins take 0.75 of the height in all.
        let width = (label.max(detail) + 0.75 * height).max(1.5 * height);
        Ok(((width * 4.0).ceil() / 4.0, height))
    }
}

/// Detail text size as a fraction of the label size.
const DETAIL_RATIO: f64 = 0.45;

fn rounded(rect: Rect, radius: f64) -> BezPath {
    let radius = radius.clamp(0.0, rect.width().min(rect.height()) / 2.0);
    RoundedRect::from_rect(rect, radius).to_path(0.01)
}

/// Draws `spec` filling `rect` (page space, upright along +x). The frame follows the rectangle;
/// the text is sized to fit it and is never stretched.
///
/// Fails when a line holds a character outside WinAnsi, or when a number is not finite. A
/// rectangle with no area gives an empty list.
pub fn stamp_list(spec: &StampSpec, rect: Rect) -> Result<DrawList, DrawError> {
    winansi_encode(&spec.label)?;
    if let Some(d) = &spec.detail {
        winansi_encode(d)?;
    }
    if ![rect.x0, rect.y0, rect.x1, rect.y1]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(DrawError::InvalidNumber);
    }
    let rect = rect.abs();
    let mut list = DrawList::new();
    let unit = rect.width().min(rect.height());
    if unit <= 0.0 {
        return Ok(list);
    }
    let ink = spec.scheme.ink();
    let detail = spec.detail.as_deref().filter(|d| !d.trim().is_empty());

    // Frame: a heavy outer rule drawn inside the rectangle, a fine inner rule within it.
    let heavy = (unit * 0.06).max(0.2);
    let fine = heavy / 3.0;
    let outer = rect.inset(-heavy / 2.0);
    let gap = heavy * 1.25;
    let inner = rect.inset(-(heavy + gap + fine / 2.0));
    let radius = unit * 0.2;
    list.push(DrawOp::Path(PathOp::stroked(
        rounded(outer, radius),
        Stroke::new(heavy, ink).with_join(LineJoin::Round),
    )));
    if inner.width() > 0.0 && inner.height() > 0.0 {
        list.push(DrawOp::Path(PathOp::stroked(
            rounded(inner, (radius - heavy - gap).max(0.0)),
            Stroke::new(fine, ink).with_join(LineJoin::Round),
        )));
    }

    // Text area: inside the inner rule with a margin.
    let margin = heavy + gap + fine + unit * 0.08;
    let area = rect.inset(-margin);
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return Ok(list);
    }
    // Shares of the text area's height: label alone, or label over rule over detail.
    let label_share = if detail.is_some() { 0.56 } else { 0.8 };
    let unit_label = measure_text(&spec.label, FontStyle::Bold, 1.0)?;
    let cap = unit_label.cap_height.max(0.1);
    let mut label_size = area.height() * label_share / cap;
    if unit_label.width > 0.0 {
        label_size = label_size.min(area.width() / unit_label.width);
    }
    let label_width = unit_label.width * label_size;
    let label_cap = cap * label_size;

    let mut detail_run = None;
    let mut block = label_cap;
    if let Some(d) = detail {
        let unit_detail = measure_text(d, FontStyle::Regular, 1.0)?;
        let mut size = label_size * DETAIL_RATIO;
        if unit_detail.width > 0.0 {
            size = size.min(area.width() / unit_detail.width);
        }
        let detail_cap = unit_detail.cap_height.max(0.1) * size;
        let rule_gap = label_cap * 0.3;
        block = label_cap + 2.0 * rule_gap + detail_cap;
        detail_run = Some((d, size, unit_detail.width * size, detail_cap, rule_gap));
    }
    // The block (label, rule, detail) is centred in the area.
    let top = area.center().y + block / 2.0;
    let label_baseline = top - label_cap;
    if !spec.label.is_empty() && label_size > 0.0 {
        list.push(DrawOp::Text(TextRun::new(
            spec.label.clone(),
            Point::new(area.center().x - label_width / 2.0, label_baseline),
            label_size,
            FontStyle::Bold,
            ink,
        )?));
    }
    if let Some((d, size, width, detail_cap, rule_gap)) = detail_run {
        let rule_y = label_baseline - rule_gap;
        let half = (label_width.max(width) / 2.0).min(area.width() / 2.0);
        let mut rule = BezPath::new();
        rule.move_to((area.center().x - half, rule_y));
        rule.line_to((area.center().x + half, rule_y));
        list.push(DrawOp::Path(PathOp::stroked(
            rule,
            Stroke::new(fine, ink).with_cap(LineCap::Butt),
        )));
        if size > 0.0 {
            list.push(DrawOp::Text(TextRun::new(
                d,
                Point::new(
                    area.center().x - width / 2.0,
                    rule_y - rule_gap - detail_cap,
                ),
                size,
                FontStyle::Regular,
                ink,
            )?));
        }
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(outer: Rect, inner: Rect) -> bool {
        let roomy = outer.inflate(1e-6, 1e-6);
        roomy.union(inner) == roomy
    }

    fn text_runs(list: &DrawList) -> Vec<&TextRun> {
        list.ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_standard_set_is_five_distinct_stamps() {
        let set = StampSpec::standard_set();
        let labels: Vec<&str> = set.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            ["APPROVED", "REVISED", "FOR REVIEW", "VOID", "DRAFT"]
        );
        let names: std::collections::HashSet<&str> = set.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 5);
        for scheme in StampScheme::ALL {
            assert_eq!(StampScheme::from_name(scheme.name()), Some(scheme));
        }
        assert_eq!(StampScheme::from_name("Pink"), None);
    }

    #[test]
    fn a_stamp_stays_inside_its_rectangle_at_its_natural_size_and_others() {
        for spec in StampSpec::standard_set() {
            for detail in [None, Some("A. Reviewer  2026-11-24")] {
                let spec = match detail {
                    Some(d) => spec.clone().with_detail(d),
                    None => spec.clone(),
                };
                let (w, h) = spec.natural_size().unwrap();
                for rect in [
                    Rect::new(100.0, 200.0, 100.0 + w, 200.0 + h),
                    Rect::new(0.0, 0.0, 3.0 * w, 1.5 * h),
                    Rect::new(10.0, 10.0, 10.0 + 0.4 * w, 10.0 + h),
                    Rect::new(10.0, 10.0, 40.0, 150.0),
                ] {
                    let list = stamp_list(&spec, rect).unwrap();
                    list.validate().unwrap();
                    let runs = text_runs(&list);
                    assert_eq!(runs.len(), 1 + usize::from(detail.is_some()), "{rect:?}");
                    assert_eq!(runs[0].style, FontStyle::Bold);
                    assert_eq!(runs[0].color, spec.scheme.ink());
                    // The frame is inside the rectangle and reaches its edges.
                    let frame = list.ops[0].bounds().unwrap();
                    assert!(inside(rect, frame), "{frame:?} in {rect:?}");
                    assert!((frame.x0 - rect.x0).abs() < 0.01 && (frame.y1 - rect.y1).abs() < 0.01);
                    // Each line of text is inside the rectangle, measured by its advance.
                    for run in runs {
                        let m = measure_text(&run.text, run.style, run.size).unwrap();
                        let b = Rect::new(
                            run.origin.x,
                            run.origin.y,
                            run.origin.x + m.width,
                            run.origin.y + m.cap_height,
                        );
                        assert!(inside(rect, b), "{:?}: {b:?} in {rect:?}", run.text);
                    }
                }
            }
        }
    }

    #[test]
    fn the_label_is_twenty_units_tall_at_the_natural_size() {
        let spec = StampSpec::approved();
        let (w, h) = spec.natural_size().unwrap();
        let list = stamp_list(&spec, Rect::new(0.0, 0.0, w, h)).unwrap();
        let label = text_runs(&list)[0];
        assert!(
            (label.size - 20.0).abs() < 6.0,
            "label size {} at {w} x {h}",
            label.size
        );
        // Centred.
        let m = measure_text(&label.text, label.style, label.size).unwrap();
        assert!((label.origin.x + m.width / 2.0 - w / 2.0).abs() < 1e-6);
        // With a second line the label moves up and the detail sits under a rule.
        let two = spec.clone().with_detail("Checked by QA");
        let (w2, h2) = two.natural_size().unwrap();
        assert!(h2 > h);
        let list = stamp_list(&two, Rect::new(0.0, 0.0, w2, h2)).unwrap();
        let runs = text_runs(&list);
        assert!(runs[0].origin.y > runs[1].origin.y);
        assert!(runs[1].size < runs[0].size);
        assert_eq!(runs[1].style, FontStyle::Regular);
        assert_eq!(list.ops.len(), 5, "two frames, label, rule, detail");
    }

    #[test]
    fn schemes_differ_only_in_ink() {
        let rect = Rect::new(0.0, 0.0, 160.0, 40.0);
        let mut green = StampSpec::approved();
        let a = stamp_list(&green, rect).unwrap();
        green.scheme = StampScheme::Red;
        let b = stamp_list(&green, rect).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.bounds(), b.bounds());
        let inks: std::collections::HashSet<[u8; 3]> =
            StampScheme::ALL.iter().map(|s| s.ink().to_u8()).collect();
        assert_eq!(inks.len(), 5);
    }

    #[test]
    fn bad_input_is_refused_or_draws_nothing() {
        let rect = Rect::new(0.0, 0.0, 100.0, 40.0);
        let bad = StampSpec::new("X", "\u{627}\u{639}", StampScheme::Red);
        assert!(matches!(
            stamp_list(&bad, rect),
            Err(DrawError::NotWinAnsi { index: 0, .. })
        ));
        assert!(bad.natural_size().is_err());
        let bad_detail = StampSpec::approved().with_detail("ok \u{4e2d}");
        assert!(matches!(
            stamp_list(&bad_detail, rect),
            Err(DrawError::NotWinAnsi { index: 3, .. })
        ));
        assert!(
            stamp_list(&StampSpec::draft(), Rect::new(0.0, 0.0, 0.0, 40.0))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            stamp_list(&StampSpec::draft(), Rect::new(0.0, 0.0, f64::NAN, 40.0)),
            Err(DrawError::InvalidNumber)
        );
        // A blank detail is the same as none.
        let blank = StampSpec::draft().with_detail("  ");
        assert_eq!(
            stamp_list(&blank, rect).unwrap(),
            stamp_list(&StampSpec::draft(), rect).unwrap()
        );
    }
}
