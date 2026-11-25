//! Destinations (ISO 32000-1 12.3.2): explicit `[page /View ...]` arrays, named destinations in
//! the catalog's `/Dests` dictionary and the `/Names /Dests` name tree, and their resolution to a
//! page index and a target point.
//!
//! Positions are in the target page's unrotated user space (y-up), as everywhere in pl-pdf.

use std::collections::BTreeSet;

use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};

use crate::document::{number, PdfDocument};

/// How a destination shows its page (the second element of an explicit destination).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DestView {
    /// `/XYZ left top zoom`; `None` keeps the viewer's current value.
    Xyz {
        left: Option<f64>,
        top: Option<f64>,
        zoom: Option<f64>,
    },
    /// `/Fit`: the whole page.
    Fit,
    /// `/FitH top`: the page width, `top` at the top of the window.
    FitH { top: Option<f64> },
    /// `/FitV left`: the page height, `left` at the left of the window.
    FitV { left: Option<f64> },
    /// `/FitR left bottom right top`.
    FitR {
        left: f64,
        bottom: f64,
        right: f64,
        top: f64,
    },
    /// `/FitB`: the page's bounding box.
    FitB,
    /// `/FitBH top`.
    FitBH { top: Option<f64> },
    /// `/FitBV left`.
    FitBV { left: Option<f64> },
}

impl DestView {
    /// `/XYZ` with `left` and `top` and the zoom left unchanged.
    pub fn at(left: f64, top: f64) -> DestView {
        DestView::Xyz {
            left: Some(left),
            top: Some(top),
            zoom: None,
        }
    }

    /// The point the view puts at the top left of the window, where it names one. Either
    /// coordinate is `None` when the view leaves it to the viewer.
    pub fn target_point(&self) -> (Option<f64>, Option<f64>) {
        match *self {
            DestView::Xyz { left, top, .. } => (left, top),
            DestView::FitH { top } | DestView::FitBH { top } => (None, top),
            DestView::FitV { left } | DestView::FitBV { left } => (left, None),
            DestView::FitR { left, top, .. } => (Some(left), Some(top)),
            DestView::Fit | DestView::FitB => (None, None),
        }
    }

    /// The zoom factor an `/XYZ` view sets (`None` for every other view, and for 0).
    pub fn zoom(&self) -> Option<f64> {
        match *self {
            DestView::Xyz { zoom, .. } => zoom.filter(|z| *z > 0.0),
            _ => None,
        }
    }

    pub(crate) fn parse(items: &[Object]) -> Option<DestView> {
        let kind = items.first()?.as_name().ok()?;
        let opt = |i: usize| items.get(i).and_then(number);
        Some(match kind {
            b"XYZ" => DestView::Xyz {
                left: opt(1),
                top: opt(2),
                zoom: opt(3).filter(|z| *z != 0.0),
            },
            b"Fit" => DestView::Fit,
            b"FitH" => DestView::FitH { top: opt(1) },
            b"FitV" => DestView::FitV { left: opt(1) },
            b"FitR" => DestView::FitR {
                left: opt(1)?,
                bottom: opt(2)?,
                right: opt(3)?,
                top: opt(4)?,
            },
            b"FitB" => DestView::FitB,
            b"FitBH" => DestView::FitBH { top: opt(1) },
            b"FitBV" => DestView::FitBV { left: opt(1) },
            _ => return None,
        })
    }

    /// The view as the elements that follow the page in an explicit destination.
    pub(crate) fn to_objects(self) -> Vec<Object> {
        let name = |n: &str| Object::Name(n.as_bytes().to_vec());
        let opt = |v: Option<f64>| v.map_or(Object::Null, real);
        match self {
            DestView::Xyz { left, top, zoom } => {
                vec![name("XYZ"), opt(left), opt(top), opt(zoom)]
            }
            DestView::Fit => vec![name("Fit")],
            DestView::FitH { top } => vec![name("FitH"), opt(top)],
            DestView::FitV { left } => vec![name("FitV"), opt(left)],
            DestView::FitR {
                left,
                bottom,
                right,
                top,
            } => vec![
                name("FitR"),
                real(left),
                real(bottom),
                real(right),
                real(top),
            ],
            DestView::FitB => vec![name("FitB")],
            DestView::FitBH { top } => vec![name("FitBH"), opt(top)],
            DestView::FitBV { left } => vec![name("FitBV"), opt(left)],
        }
    }
}

/// A PDF number: an integer when the value is whole, a real otherwise.
pub(crate) fn real(v: f64) -> Object {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        Object::Integer(v as i64)
    } else {
        Object::Real(v as f32)
    }
}

/// An explicit destination in this document: a page and a view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Destination {
    pub page_index: usize,
    pub view: DestView,
}

impl Destination {
    pub fn new(page_index: usize, view: DestView) -> Destination {
        Destination { page_index, view }
    }

    /// The explicit destination array, with the page object of `page_index`.
    pub(crate) fn to_object(self, pages: &[ObjectId]) -> Option<Object> {
        let page = *pages.get(self.page_index)?;
        let mut items = vec![Object::Reference(page)];
        items.extend(self.view.to_objects());
        Some(Object::Array(items))
    }
}

/// Where a destination leads, once resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedDest {
    pub page_index: usize,
    pub view: DestView,
    /// The view's target point (see [`DestView::target_point`]).
    pub left: Option<f64>,
    pub top: Option<f64>,
    pub zoom: Option<f64>,
}

impl From<Destination> for ResolvedDest {
    fn from(d: Destination) -> ResolvedDest {
        let (left, top) = d.view.target_point();
        ResolvedDest {
            page_index: d.page_index,
            view: d.view,
            left,
            top,
            zoom: d.view.zoom(),
        }
    }
}

/// The page element of an explicit destination as written, before it is looked up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RawPage {
    /// A page object reference (local destinations).
    Object(ObjectId),
    /// A zero-based page number (remote destinations, and some broken local ones).
    Number(i64),
    /// `null` or anything else.
    Missing,
}

/// An explicit destination as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RawDest {
    pub page: RawPage,
    pub view: DestView,
}

/// Parse an explicit destination array (`[page /View args...]`).
pub(crate) fn parse_explicit(doc: &Document, object: &Object) -> Option<RawDest> {
    let object = deref(doc, object)?;
    let items = object.as_array().ok()?;
    let page = match items.first() {
        Some(Object::Reference(id)) => RawPage::Object(*id),
        Some(Object::Integer(n)) => RawPage::Number(*n),
        _ => RawPage::Missing,
    };
    let rest: Vec<Object> = items
        .iter()
        .skip(1)
        .map(|o| deref(doc, o).cloned().unwrap_or(Object::Null))
        .collect();
    let view = DestView::parse(&rest).unwrap_or(DestView::Fit);
    Some(RawDest { page, view })
}

/// What a `/Dest` value or a GoTo action's `/D` is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DestValue {
    Explicit(RawDest),
    /// A named destination (a name object or a byte string).
    Named(Vec<u8>),
}

pub(crate) fn parse_dest_value(doc: &Document, object: &Object) -> Option<DestValue> {
    match deref(doc, object)? {
        Object::Name(n) => Some(DestValue::Named(n.clone())),
        Object::String(s, _) => Some(DestValue::Named(s.clone())),
        array @ Object::Array(_) => parse_explicit(doc, array).map(DestValue::Explicit),
        _ => None,
    }
}

/// Follow references (at most a few) to a direct object.
pub(crate) fn deref<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    doc.dereference(object).ok().map(|(_, o)| o)
}

/// One named destination of the document.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedDest {
    /// The name as bytes, exactly as stored (usually ASCII).
    pub name: Vec<u8>,
    /// The destination, or `None` when its page is not in the document.
    pub dest: Option<Destination>,
    /// True when the entry is in the catalog's old-style `/Dests` dictionary, false when it is
    /// in the `/Names /Dests` name tree.
    pub in_dests_dict: bool,
}

impl NamedDest {
    /// The name as text (lossy for names that are not UTF-8).
    pub fn name_str(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }
}

/// A name-tree or `/Dests` value: an explicit destination array or a dictionary with `/D`.
fn dest_of_entry<'a>(doc: &'a Document, value: &'a Object) -> Option<&'a Object> {
    match deref(doc, value)? {
        Object::Dictionary(d) => d.get(b"D").ok(),
        other => Some(other),
    }
}

const TREE_DEPTH_LIMIT: usize = 32;

/// Walk a name tree, calling `f(key, value)` for each leaf entry in order.
pub(crate) fn walk_name_tree<'a>(
    doc: &'a Document,
    node: &'a Object,
    f: &mut dyn FnMut(&'a [u8], &'a Object),
) {
    let mut seen = BTreeSet::new();
    walk_name_node(doc, node, f, &mut seen, 0);
}

fn walk_name_node<'a>(
    doc: &'a Document,
    node: &'a Object,
    f: &mut dyn FnMut(&'a [u8], &'a Object),
    seen: &mut BTreeSet<ObjectId>,
    depth: usize,
) {
    if depth > TREE_DEPTH_LIMIT {
        return;
    }
    if let Object::Reference(id) = node {
        if !seen.insert(*id) {
            return;
        }
    }
    let Some(Object::Dictionary(dict)) = deref(doc, node) else {
        return;
    };
    if let Some(Object::Array(names)) = dict.get(b"Names").ok().and_then(|o| deref(doc, o)) {
        for pair in names.chunks(2) {
            if let [key, value] = pair {
                if let Some(Object::String(k, _)) = deref(doc, key) {
                    f(k, value);
                }
            }
        }
    }
    if let Some(Object::Array(kids)) = dict.get(b"Kids").ok().and_then(|o| deref(doc, o)) {
        for kid in kids {
            walk_name_node(doc, kid, f, seen, depth + 1);
        }
    }
}

/// Walk a number tree, calling `f(key, value)` for each leaf entry in order.
pub(crate) fn walk_number_tree<'a>(
    doc: &'a Document,
    node: &'a Object,
    f: &mut dyn FnMut(i64, &'a Object),
) {
    fn walk<'a>(
        doc: &'a Document,
        node: &'a Object,
        f: &mut dyn FnMut(i64, &'a Object),
        seen: &mut BTreeSet<ObjectId>,
        depth: usize,
    ) {
        if depth > TREE_DEPTH_LIMIT {
            return;
        }
        if let Object::Reference(id) = node {
            if !seen.insert(*id) {
                return;
            }
        }
        let Some(Object::Dictionary(dict)) = deref(doc, node) else {
            return;
        };
        if let Some(Object::Array(nums)) = dict.get(b"Nums").ok().and_then(|o| deref(doc, o)) {
            for pair in nums.chunks(2) {
                if let [key, value] = pair {
                    if let Some(Object::Integer(k)) = deref(doc, key) {
                        f(*k, value);
                    }
                }
            }
        }
        if let Some(Object::Array(kids)) = dict.get(b"Kids").ok().and_then(|o| deref(doc, o)) {
            for kid in kids {
                walk(doc, kid, f, seen, depth + 1);
            }
        }
    }
    walk(doc, node, f, &mut BTreeSet::new(), 0);
}

fn catalog(doc: &Document) -> Option<&Dictionary> {
    doc.catalog().ok()
}

/// Every named destination entry as `(name, raw destination, in /Dests dict)`.
pub(crate) fn raw_named_dests(doc: &Document) -> Vec<(Vec<u8>, Option<RawDest>, bool)> {
    let mut out = Vec::new();
    let Some(cat) = catalog(doc) else {
        return out;
    };
    if let Some(Object::Dictionary(dests)) = cat.get(b"Dests").ok().and_then(|o| deref(doc, o)) {
        for (key, value) in dests.iter() {
            let raw = dest_of_entry(doc, value).and_then(|d| parse_explicit(doc, d));
            out.push((key.clone(), raw, true));
        }
    }
    if let Some(tree) = cat
        .get(b"Names")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|names| names.get(b"Dests").ok())
    {
        walk_name_tree(doc, tree, &mut |key, value| {
            let raw = dest_of_entry(doc, value).and_then(|d| parse_explicit(doc, d));
            out.push((key.to_vec(), raw, false));
        });
    }
    out
}

/// Look a named destination up: the name tree first (PDF 1.2 and later), then `/Dests`.
pub(crate) fn lookup_named(doc: &Document, name: &[u8]) -> Option<RawDest> {
    let all = raw_named_dests(doc);
    all.iter()
        .find(|(k, _, dict)| !dict && k == name)
        .or_else(|| all.iter().find(|(k, _, dict)| *dict && k == name))
        .and_then(|(_, raw, _)| *raw)
}

/// Resolve a destination value to a page of `pages`.
pub(crate) fn resolve_value(
    doc: &Document,
    pages: &[ObjectId],
    value: &DestValue,
) -> Option<Destination> {
    let raw = match value {
        DestValue::Explicit(raw) => *raw,
        DestValue::Named(name) => lookup_named(doc, name)?,
    };
    resolve_raw(pages, raw)
}

pub(crate) fn resolve_raw(pages: &[ObjectId], raw: RawDest) -> Option<Destination> {
    let page_index = match raw.page {
        RawPage::Object(id) => pages.iter().position(|p| *p == id)?,
        // Some writers put a page number in a local destination; accept it when it fits.
        RawPage::Number(n) => usize::try_from(n).ok().filter(|i| *i < pages.len())?,
        RawPage::Missing => return None,
    };
    Some(Destination {
        page_index,
        view: raw.view,
    })
}

/// A byte string object (literal notation).
pub(crate) fn byte_string(bytes: &[u8]) -> Object {
    Object::String(bytes.to_vec(), StringFormat::Literal)
}

/// How a reference to a named destination is written in a document. ISO 32000-1 12.3.2.3
/// looks a name object up in the catalog's old-style `/Dests` dictionary and a string in the
/// `/Names /Dests` name tree, so a name only the dictionary has is written as a name object;
/// every other one as a byte string.
#[derive(Debug, Clone, Default)]
pub(crate) struct NameForms {
    dict_only: std::collections::HashSet<Vec<u8>>,
}

impl NameForms {
    pub(crate) fn of(doc: &Document) -> NameForms {
        let all = raw_named_dests(doc);
        let in_tree: std::collections::HashSet<&[u8]> = all
            .iter()
            .filter(|(_, _, dict)| !dict)
            .map(|(k, _, _)| k.as_slice())
            .collect();
        NameForms {
            dict_only: all
                .iter()
                .filter(|(k, _, dict)| *dict && !in_tree.contains(k.as_slice()))
                .map(|(k, _, _)| k.clone())
                .collect(),
        }
    }

    pub(crate) fn object(&self, name: &[u8]) -> Object {
        if self.dict_only.contains(name) {
            Object::Name(name.to_vec())
        } else {
            byte_string(name)
        }
    }
}

impl PdfDocument {
    /// Every named destination: the `/Names /Dests` name tree entries, then the old-style
    /// catalog `/Dests` dictionary entries.
    pub fn named_destinations(&self) -> Vec<NamedDest> {
        let mut out: Vec<NamedDest> = raw_named_dests(&self.doc)
            .into_iter()
            .map(|(name, raw, in_dests_dict)| NamedDest {
                name,
                dest: raw.and_then(|r| resolve_raw(&self.pages, r)),
                in_dests_dict,
            })
            .collect();
        out.sort_by_key(|n| n.in_dests_dict);
        out
    }

    /// Resolve a named destination to a page and a view.
    pub fn resolve_named_destination(&self, name: &[u8]) -> Option<ResolvedDest> {
        lookup_named(&self.doc, name)
            .and_then(|raw| resolve_raw(&self.pages, raw))
            .map(ResolvedDest::from)
    }

    /// Resolve an explicit destination array or a name, as it would appear under `/Dest`.
    pub fn resolve_destination_object(&self, object: &Object) -> Option<ResolvedDest> {
        let value = parse_dest_value(&self.doc, object)?;
        resolve_value(&self.doc, &self.pages, &value).map(ResolvedDest::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> Object {
        Object::Name(n.as_bytes().to_vec())
    }

    #[test]
    fn views_parse_and_write_back() {
        let cases = [
            DestView::Xyz {
                left: Some(72.0),
                top: Some(700.5),
                zoom: None,
            },
            DestView::Fit,
            DestView::FitH { top: Some(10.0) },
            DestView::FitV { left: None },
            DestView::FitR {
                left: 1.0,
                bottom: 2.0,
                right: 3.0,
                top: 4.0,
            },
            DestView::FitB,
            DestView::FitBH { top: Some(5.0) },
            DestView::FitBV { left: Some(6.0) },
        ];
        for view in cases {
            let objects = view.to_objects();
            assert_eq!(DestView::parse(&objects), Some(view), "{view:?}");
        }
        assert_eq!(DestView::parse(&[name("Bogus")]), None);
        // A zero zoom means "unchanged".
        let xyz = DestView::parse(&[
            name("XYZ"),
            Object::Integer(1),
            Object::Null,
            Object::Integer(0),
        ]);
        assert_eq!(
            xyz,
            Some(DestView::Xyz {
                left: Some(1.0),
                top: None,
                zoom: None
            })
        );
    }

    #[test]
    fn target_points() {
        assert_eq!(
            DestView::at(3.0, 4.0).target_point(),
            (Some(3.0), Some(4.0))
        );
        assert_eq!(
            DestView::FitH { top: Some(9.0) }.target_point(),
            (None, Some(9.0))
        );
        assert_eq!(DestView::Fit.target_point(), (None, None));
        let r = DestView::FitR {
            left: 1.0,
            bottom: 2.0,
            right: 3.0,
            top: 4.0,
        };
        assert_eq!(r.target_point(), (Some(1.0), Some(4.0)));
    }
}
