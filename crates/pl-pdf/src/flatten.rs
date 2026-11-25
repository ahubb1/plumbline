//! Flatten: burn annotations into the page content (ISO 32000-1 12.5.5).
//!
//! Each chosen annotation's normal appearance (`/AP /N`, the `/AS` state of a state dictionary)
//! is placed as a form XObject: its `/BBox` is transformed by its `/Matrix`, the bounding box of
//! the result is mapped onto `/Rect` by a scale and translation `A`, and the page content draws
//! it with `q A cm /Name Do Q`. Annotation opacity (`/CA`) is applied through an ExtGState to a
//! transparency group that wraps the appearance, so overlapping parts of one appearance are not
//! blended with each other. An annotation in an optional-content layer (`/OC`) is drawn inside
//! `/OC ... BDC ... EMC`, so it stays in its layer.
//!
//! The page's existing content streams are not touched: the new content is a `q` stream before
//! them and a `Q` + drawing stream after, so nothing the page leaves on the graphics state stack
//! changes where the appearances land.
//!
//! **Which flags flatten.** For [`FlattenTarget::Screen`] (the default) an annotation flattens
//! when it is shown on screen: not `Hidden` (bit 2) and not `NoView` (bit 6). For
//! [`FlattenTarget::Print`] it flattens when it prints: `Print` (bit 3) set and not `Hidden`.
//! Annotations that do not qualify are left as they are, as live annotations. Popups are never
//! drawn; the popups of flattened annotations are removed with them. Links are left live
//! (they have no appearance to keep). Widgets (form fields) flatten only when
//! [`FlattenOptions::include_form_fields`] is set; their fields then leave the form. An
//! annotation that is `NoRotate` on a rotated page is skipped (its appearance turns with the
//! window, not the page, and has no fixed place in the content). `NoZoom` annotations are
//! flattened at their size at 100 %.

use std::collections::{BTreeSet, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::dest::deref;
use crate::document::{annots_array, number, rect_from_object, PdfDocument};
use crate::error::Result;
use crate::pages::{null_references, prune_fields};

/// Which annotations count as visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FlattenTarget {
    /// What the screen shows: not `Hidden`, not `NoView`.
    #[default]
    Screen,
    /// What prints: `Print` set, not `Hidden`.
    Print,
}

/// Which annotations to flatten.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlattenOptions {
    /// Only these pages (zero-based); `None` for every page.
    pub pages: Option<BTreeSet<usize>>,
    /// Only these annotation objects; `None` for every annotation that qualifies.
    pub ids: Option<BTreeSet<ObjectId>>,
    pub target: FlattenTarget,
    /// Flatten widgets too (and take their fields out of the form).
    pub include_form_fields: bool,
}

/// Why an annotation was left as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlattenSkip {
    /// `Hidden`, or `NoView` for the screen, or not `Print` for print.
    NotShown,
    /// No `/AP /N` (or no stream for the current `/AS`).
    NoAppearance,
    /// The appearance has no usable `/BBox`, or `/Rect` is empty.
    Degenerate,
    /// A link: left live.
    Link,
    /// A widget, and form fields were not included.
    FormField,
    /// `NoRotate` on a page with `/Rotate`.
    NoRotateOnRotatedPage,
}

/// One annotation that was burnt in.
#[derive(Debug, Clone, PartialEq)]
pub struct Flattened {
    pub page_index: usize,
    pub id: Option<ObjectId>,
    /// `/Subtype`, for example `Square`.
    pub subtype: String,
}

/// What a flatten did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlattenReport {
    pub flattened: Vec<Flattened>,
    /// Popups removed with their parents.
    pub popups_removed: usize,
    /// Annotations left in place, with the reason. (Annotations outside the chosen pages or ids
    /// are not listed.)
    pub skipped: Vec<(usize, Option<ObjectId>, FlattenSkip)>,
    /// Form fields removed because their widgets were flattened.
    pub fields_removed: Vec<String>,
    /// Other references to flattened annotations (a reply's `/IRT`) set to `null`.
    pub refs_dropped: usize,
    /// Signatures the full rewrite will invalidate.
    pub signatures_invalidated: Vec<String>,
}

const HIDDEN: i64 = 1 << 1;
const PRINT: i64 = 1 << 2;
const NO_ROTATE: i64 = 1 << 4;
const NO_VIEW: i64 = 1 << 5;

/// Affine matrix `[a b c d e f]` (PDF order).
type Matrix = [f64; 6];

fn apply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn matrix_of(doc: &Document, dict: &Dictionary) -> Matrix {
    let Some(Object::Array(items)) = dict.get(b"Matrix").ok().and_then(|o| deref(doc, o)) else {
        return [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    };
    let v: Vec<f64> = items
        .iter()
        .filter_map(|o| deref(doc, o).and_then(number))
        .collect();
    if v.len() == 6 {
        [v[0], v[1], v[2], v[3], v[4], v[5]]
    } else {
        [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
    }
}

/// The matrix `A` of ISO 32000-1 12.5.5 that maps the transformed appearance box onto `rect`.
pub(crate) fn placement(bbox: [f64; 4], matrix: &Matrix, rect: [f64; 4]) -> Option<Matrix> {
    let corners = [
        apply(matrix, bbox[0], bbox[1]),
        apply(matrix, bbox[2], bbox[1]),
        apply(matrix, bbox[0], bbox[3]),
        apply(matrix, bbox[2], bbox[3]),
    ];
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let (rw, rh) = (rect[2] - rect[0], rect[3] - rect[1]);
    if !(w > 0.0 && h > 0.0) || !(rw > 0.0 && rh > 0.0) {
        return None;
    }
    let sx = rw / w;
    let sy = rh / h;
    Some([sx, 0.0, 0.0, sy, rect[0] - x0 * sx, rect[1] - y0 * sy])
}

fn fmt(v: f64) -> String {
    let mut s = format!("{v:.6}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// The normal appearance stream of an annotation (the `/AS` state of a state dictionary).
fn normal_appearance(doc: &Document, annot: &Dictionary) -> Option<ObjectId> {
    let ap = deref(doc, annot.get(b"AP").ok()?)?.as_dict().ok()?;
    let n = ap.get(b"N").ok()?;
    match n {
        Object::Reference(id) => match doc.get_object(*id).ok()? {
            Object::Stream(_) => Some(*id),
            Object::Dictionary(states) => state_stream(doc, annot, states),
            _ => None,
        },
        Object::Dictionary(states) => state_stream(doc, annot, states),
        _ => None,
    }
}

fn state_stream(doc: &Document, annot: &Dictionary, states: &Dictionary) -> Option<ObjectId> {
    let state = annot
        .get(b"AS")
        .ok()
        .and_then(|o| deref(doc, o))?
        .as_name()
        .ok()?;
    let id = states.get(state).ok()?.as_reference().ok()?;
    matches!(doc.get_object(id).ok()?, Object::Stream(_)).then_some(id)
}

/// A resource name not yet used in `dict`.
fn fresh_name(dict: &Dictionary, prefix: &str, counter: &mut usize) -> String {
    loop {
        *counter += 1;
        let n = format!("{prefix}{counter}");
        if !dict.has(n.as_bytes()) {
            return n;
        }
    }
}

/// A sub-dictionary of resources (`/XObject`...) as a direct dictionary, taken from `res`.
fn sub_dict(doc: &Document, res: &Dictionary, key: &[u8]) -> Dictionary {
    res.get(key)
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .cloned()
        .unwrap_or_default()
}

impl PdfDocument {
    /// Burn the chosen annotations into their pages' content. See the module documentation for
    /// which annotations qualify. The result is saved with the full rewrite
    /// ([`crate::PdfFile::save_restructured`]).
    pub fn flatten(&self, options: &FlattenOptions) -> Result<(PdfDocument, FlattenReport)> {
        let mut doc = self.doc.clone();
        let mut report = FlattenReport {
            signatures_invalidated: crate::signatures::detect(&doc).descriptions(),
            ..FlattenReport::default()
        };
        let mut dead: HashSet<ObjectId> = HashSet::new();
        for (page_index, page) in self.pages.iter().enumerate() {
            if options
                .pages
                .as_ref()
                .is_some_and(|p| !p.contains(&page_index))
            {
                continue;
            }
            flatten_page(
                &mut doc,
                *page,
                page_index,
                self,
                options,
                &mut report,
                &mut dead,
            )?;
        }
        if !dead.is_empty() {
            let live: HashSet<ObjectId> = self
                .pages
                .iter()
                .flat_map(|p| crate::copy::page_annotation_ids(&doc, *p))
                .collect();
            report.fields_removed = prune_fields(&mut doc, &live, &dead);
            report.refs_dropped = null_references(&mut doc, &dead);
        }
        Ok((PdfDocument::derived(self.bytes.clone(), doc)?, report))
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten_page(
    doc: &mut Document,
    page: ObjectId,
    page_index: usize,
    source: &PdfDocument,
    options: &FlattenOptions,
    report: &mut FlattenReport,
    dead: &mut HashSet<ObjectId>,
) -> Result<()> {
    let rotated = source.page(page_index)?.rotate.degrees() != 0;
    let entries: Vec<Object> = annots_array(doc, page)
        .map(<[Object]>::to_vec)
        .unwrap_or_default();
    if entries.is_empty() {
        return Ok(());
    }
    // Resources as direct dictionaries this page owns from now on (taken from a /Pages
    // ancestor when the page inherits them).
    let page_dict = doc.get_dictionary(page).cloned().unwrap_or_default();
    let mut resources = crate::copy::inherited_raw(doc, page, b"Resources")
        .as_ref()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .cloned()
        .unwrap_or_default();
    let mut xobjects = sub_dict(doc, &resources, b"XObject");
    let mut gstates = sub_dict(doc, &resources, b"ExtGState");
    let mut properties = sub_dict(doc, &resources, b"Properties");
    let mut counter = 0;
    let mut draw = String::new();
    let mut remove_slots: BTreeSet<usize> = BTreeSet::new();
    let mut flattened_ids: Vec<ObjectId> = Vec::new();

    for (slot, entry) in entries.iter().enumerate() {
        let id = entry.as_reference().ok();
        let Some(annot) = deref(doc, entry).and_then(|o| o.as_dict().ok()).cloned() else {
            continue;
        };
        if let (Some(filter), Some(id)) = (&options.ids, id) {
            if !filter.contains(&id) {
                continue;
            }
        } else if options.ids.is_some() {
            // A direct annotation has no id, so an id filter never picks it.
            continue;
        }
        let subtype = annot
            .get(b"Subtype")
            .and_then(Object::as_name)
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let skip = |r: &mut FlattenReport, why| r.skipped.push((page_index, id, why));
        match subtype.as_str() {
            "Popup" => continue,
            "Link" => {
                skip(report, FlattenSkip::Link);
                continue;
            }
            "Widget" if !options.include_form_fields => {
                skip(report, FlattenSkip::FormField);
                continue;
            }
            _ => {}
        }
        let flags = annot
            .get(b"F")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(number)
            .map_or(0, |f| f as i64);
        let shown = match options.target {
            FlattenTarget::Screen => flags & (HIDDEN | NO_VIEW) == 0,
            FlattenTarget::Print => flags & PRINT != 0 && flags & HIDDEN == 0,
        };
        if !shown {
            skip(report, FlattenSkip::NotShown);
            continue;
        }
        if rotated && flags & NO_ROTATE != 0 {
            skip(report, FlattenSkip::NoRotateOnRotatedPage);
            continue;
        }
        let Some(ap) = normal_appearance(doc, &annot) else {
            skip(report, FlattenSkip::NoAppearance);
            continue;
        };
        let Some(rect) = annot
            .get(b"Rect")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(rect_from_object)
        else {
            skip(report, FlattenSkip::Degenerate);
            continue;
        };
        let Some(Object::Stream(stream)) = doc.objects.get(&ap) else {
            skip(report, FlattenSkip::NoAppearance);
            continue;
        };
        let bbox = stream
            .dict
            .get(b"BBox")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(rect_from_object);
        let matrix = matrix_of(doc, &stream.dict);
        let placed = bbox.and_then(|b| {
            placement(
                [b.x0, b.y0, b.x1, b.y1],
                &matrix,
                [rect.x0, rect.y0, rect.x1, rect.y1],
            )
        });
        let Some(a) = placed else {
            skip(report, FlattenSkip::Degenerate);
            continue;
        };
        // Do needs a form XObject; some writers leave the type keys off appearance streams.
        if let Some(Object::Stream(s)) = doc.objects.get_mut(&ap) {
            s.dict.set("Type", Object::Name(b"XObject".to_vec()));
            s.dict.set("Subtype", Object::Name(b"Form".to_vec()));
        }

        let opacity = annot
            .get(b"CA")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(number)
            .filter(|v| (0.0..1.0).contains(v));
        let form_name = fresh_name(&xobjects, "PlFlat", &mut counter);
        let (painted, gs) = match opacity {
            Some(alpha) => {
                // A transparency group around the appearance, painted with the opacity.
                let group_bbox = Object::Array(
                    [rect.x0, rect.y0, rect.x1, rect.y1]
                        .iter()
                        .map(|v| crate::dest::real(*v))
                        .collect(),
                );
                let mut res = Dictionary::new();
                let mut xo = Dictionary::new();
                xo.set("Fm", Object::Reference(ap));
                res.set("XObject", Object::Dictionary(xo));
                let mut group = Dictionary::new();
                group.set("S", Object::Name(b"Transparency".to_vec()));
                let mut d = Dictionary::new();
                d.set("Type", Object::Name(b"XObject".to_vec()));
                d.set("Subtype", Object::Name(b"Form".to_vec()));
                d.set("BBox", group_bbox);
                d.set("Group", Object::Dictionary(group));
                d.set("Resources", Object::Dictionary(res));
                let content = format!(
                    "q {} {} {} {} {} {} cm /Fm Do Q\n",
                    fmt(a[0]),
                    fmt(a[1]),
                    fmt(a[2]),
                    fmt(a[3]),
                    fmt(a[4]),
                    fmt(a[5])
                );
                let wrapper = doc.add_object(Stream::new(d, content.into_bytes()));
                let gs_name = fresh_name(&gstates, "PlFlatGS", &mut counter);
                let mut g = Dictionary::new();
                g.set("Type", Object::Name(b"ExtGState".to_vec()));
                g.set("CA", Object::Real(alpha as f32));
                g.set("ca", Object::Real(alpha as f32));
                gstates.set(gs_name.clone(), Object::Dictionary(g));
                (wrapper, Some(gs_name))
            }
            None => (ap, None),
        };
        xobjects.set(form_name.clone(), Object::Reference(painted));

        let oc = annot.get(b"OC").ok().and_then(|o| o.as_reference().ok());
        let oc_name = oc.map(|oc| {
            let n = fresh_name(&properties, "PlFlatOC", &mut counter);
            properties.set(n.clone(), Object::Reference(oc));
            n
        });
        if let Some(n) = &oc_name {
            draw.push_str(&format!("/OC /{n} BDC\n"));
        }
        draw.push_str("q\n");
        if let Some(g) = &gs {
            draw.push_str(&format!("/{g} gs\n"));
        }
        if opacity.is_some() {
            // The wrapper already holds the placement.
            draw.push_str(&format!("/{form_name} Do\n"));
        } else {
            draw.push_str(&format!(
                "{} {} {} {} {} {} cm /{form_name} Do\n",
                fmt(a[0]),
                fmt(a[1]),
                fmt(a[2]),
                fmt(a[3]),
                fmt(a[4]),
                fmt(a[5])
            ));
        }
        draw.push_str("Q\n");
        if oc_name.is_some() {
            draw.push_str("EMC\n");
        }

        remove_slots.insert(slot);
        if let Some(id) = id {
            flattened_ids.push(id);
            dead.insert(id);
        }
        // Its popup goes with it.
        if let Some(popup) = annot.get(b"Popup").ok().and_then(|o| o.as_reference().ok()) {
            if let Some(pslot) = entries
                .iter()
                .position(|e| matches!(e, Object::Reference(p) if *p == popup))
            {
                remove_slots.insert(pslot);
                report.popups_removed += 1;
            }
            dead.insert(popup);
        }
        report.flattened.push(Flattened {
            page_index,
            id,
            subtype,
        });
    }

    // Popups whose parent was flattened but that the parent does not name.
    for (slot, entry) in entries.iter().enumerate() {
        if remove_slots.contains(&slot) {
            continue;
        }
        let Some(d) = deref(doc, entry).and_then(|o| o.as_dict().ok()) else {
            continue;
        };
        let is_popup =
            d.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Popup".as_slice());
        let parent = d.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
        if is_popup && parent.is_some_and(|p| flattened_ids.contains(&p)) {
            remove_slots.insert(slot);
            report.popups_removed += 1;
            if let Ok(id) = entry.as_reference() {
                dead.insert(id);
            }
        }
    }

    if remove_slots.is_empty() {
        return Ok(());
    }
    let kept: Vec<Object> = entries
        .iter()
        .enumerate()
        .filter(|(i, _)| !remove_slots.contains(i))
        .map(|(_, e)| e.clone())
        .collect();

    // Content: q, the existing streams, Q and the appearances.
    let mut contents: Vec<Object> = Vec::new();
    let open = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    contents.push(Object::Reference(open));
    match page_dict.get(b"Contents").ok() {
        Some(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Array(a)) => contents.extend(a.iter().cloned()),
            Ok(_) => contents.push(Object::Reference(*id)),
            Err(_) => {}
        },
        Some(Object::Array(a)) => contents.extend(a.iter().cloned()),
        _ => {}
    }
    let close = doc.add_object(Stream::new(
        Dictionary::new(),
        format!("Q\n{draw}").into_bytes(),
    ));
    contents.push(Object::Reference(close));

    if !xobjects.is_empty() {
        resources.set("XObject", Object::Dictionary(xobjects));
    }
    if !gstates.is_empty() {
        resources.set("ExtGState", Object::Dictionary(gstates));
    }
    if !properties.is_empty() {
        resources.set("Properties", Object::Dictionary(properties));
    }
    let d = doc
        .get_dictionary_mut(page)
        .map_err(|_| crate::PdfError::ObjectNotFound(page))?;
    d.set("Contents", Object::Array(contents));
    d.set("Resources", Object::Dictionary(resources));
    if kept.is_empty() {
        d.remove(b"Annots");
    } else {
        d.set("Annots", Object::Array(kept));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_maps_the_transformed_box_onto_the_rect() {
        // Identity matrix: a 0..100 x 0..50 box onto [100 100 200 150] is a translation.
        let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let a = placement([0.0, 0.0, 100.0, 50.0], &id, [100.0, 100.0, 200.0, 150.0]).unwrap();
        assert_eq!(a, [1.0, 0.0, 0.0, 1.0, 100.0, 100.0]);
        // A box in page coordinates already (as many writers make them) maps to itself.
        let b = placement(
            [300.0, 400.0, 500.0, 470.0],
            &id,
            [300.0, 400.0, 500.0, 470.0],
        )
        .unwrap();
        assert_eq!(b, id);
        // A 90-degree matrix: the box's transformed bounds are 50 x 100.
        let rot = [0.0, 1.0, -1.0, 0.0, 0.0, 0.0];
        let c = placement([0.0, 0.0, 100.0, 50.0], &rot, [10.0, 20.0, 60.0, 120.0]).unwrap();
        // Corner (100, 50) -> (-50, 100); bounds x -50..0, y 0..100.
        assert_eq!(c, [1.0, 0.0, 0.0, 1.0, 60.0, 20.0]);
        let (x, y) = apply(&c, -50.0, 0.0);
        assert_eq!((x, y), (10.0, 20.0));
        // Scaling: a 10 x 10 box onto a 20 x 40 rect.
        let d = placement([0.0, 0.0, 10.0, 10.0], &id, [0.0, 0.0, 20.0, 40.0]).unwrap();
        assert_eq!(d, [2.0, 0.0, 0.0, 4.0, 0.0, 0.0]);
        assert!(placement([0.0, 0.0, 0.0, 10.0], &id, [0.0, 0.0, 1.0, 1.0]).is_none());
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(fmt(1.0), "1");
        assert_eq!(fmt(-0.0), "0");
        assert_eq!(fmt(0.125), "0.125");
        assert_eq!(fmt(1.0 / 3.0), "0.333333");
    }
}
