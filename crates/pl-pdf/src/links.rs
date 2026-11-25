//! Link annotations (ISO 32000-1 12.5.6.5) and the actions they carry: GoTo, GoToR, URI,
//! Launch and Named. This module reads, builds and resolves them; it never performs an action
//! (no file is opened, no address visited, no program started).
//!
//! Links are annotations, so creating, editing and deleting one is an annotation-only edit:
//! build the dictionary or patch here and put it in an [`EditSet`], which saves incrementally.

use lopdf::{decode_text_string, text_string, Dictionary, Document, Object, ObjectId};
use pl_geom::Rect;

use crate::dest::{
    deref, parse_dest_value, real, resolve_value, DestValue, DestView, Destination, RawPage,
    ResolvedDest,
};
use crate::document::{number, rect_from_object, PdfDocument};
use crate::edit::{DictPatch, EditSet};
use crate::error::{PdfError, Result};

/// The destination of a local GoTo.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkDest {
    /// A page of this document and a view.
    Page(Destination),
    /// A named destination.
    Named(Vec<u8>),
    /// A destination whose page is not in this document (kept as it was written).
    Broken,
}

/// The destination of a GoToR: a page of another file (by number) or a name in it.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteDest {
    Page { page_index: usize, view: DestView },
    Named(Vec<u8>),
}

/// What clicking a link does.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkAction {
    /// Go to a place in this document (`/Dest`, or `/A << /S /GoTo >>`).
    GoTo(LinkDest),
    /// Go to a place in another PDF file.
    GoToR {
        file: String,
        dest: Option<RemoteDest>,
        new_window: Option<bool>,
    },
    /// Open an address.
    Uri(String),
    /// Open a file (`/F`).
    Launch {
        file: String,
        new_window: Option<bool>,
    },
    /// A named viewer action (`NextPage`, `PrevPage`, `FirstPage`, `LastPage`).
    Named(String),
    /// Some other action type, by its `/S` name; kept as written when a link is edited.
    Other(String),
    /// No action at all.
    None,
}

/// One link annotation as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub page_index: usize,
    /// Position in the page's `/Annots`.
    pub slot: usize,
    /// `None` for a link written directly in the `/Annots` array.
    pub id: Option<ObjectId>,
    /// `/Rect`, unrotated page user space.
    pub rect: Rect,
    /// `/QuadPoints`, eight numbers per quadrilateral.
    pub quad_points: Vec<[f64; 8]>,
    pub action: LinkAction,
    /// `/Border` width or `/BS /W` (0 = no visible border).
    pub border_width: f64,
    /// `/C`.
    pub color: Option<[f64; 3]>,
    /// The goto action was written as `/A` (true) rather than `/Dest` (false).
    pub as_action: bool,
}

/// What a new or edited link should be.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkSpec {
    pub rect: Rect,
    pub quad_points: Vec<[f64; 8]>,
    pub action: LinkAction,
    /// 0 for an invisible link (the usual choice).
    pub border_width: f64,
    pub color: Option<[f64; 3]>,
}

impl LinkSpec {
    pub fn new(rect: Rect, action: LinkAction) -> LinkSpec {
        LinkSpec {
            rect,
            quad_points: Vec::new(),
            action,
            border_width: 0.0,
            color: None,
        }
    }
}

/// A file specification (`/F` or `/UF`, a string or a dictionary) as text.
fn file_spec(doc: &Document, object: &Object) -> Option<String> {
    match deref(doc, object)? {
        s @ Object::String(..) => decode_text_string(s).ok(),
        Object::Dictionary(d) => d
            .get(b"UF")
            .or_else(|_| d.get(b"F"))
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| decode_text_string(o).ok()),
        _ => None,
    }
}

fn name_of(object: &Object) -> Option<String> {
    object
        .as_name()
        .ok()
        .map(|n| String::from_utf8_lossy(n).into_owned())
}

fn local_dest(doc: &Document, pages: &[ObjectId], value: &Object) -> LinkDest {
    match parse_dest_value(doc, value) {
        Some(DestValue::Named(n)) => LinkDest::Named(n),
        Some(v @ DestValue::Explicit(_)) => match resolve_value(doc, pages, &v) {
            Some(d) => LinkDest::Page(d),
            None => LinkDest::Broken,
        },
        None => LinkDest::Broken,
    }
}

/// Read an action dictionary.
pub(crate) fn parse_action(doc: &Document, pages: &[ObjectId], action: &Dictionary) -> LinkAction {
    let kind = action
        .get(b"S")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(name_of);
    let new_window = action
        .get(b"NewWindow")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_bool().ok());
    match kind.as_deref() {
        Some("GoTo") => match action.get(b"D") {
            Ok(d) => LinkAction::GoTo(local_dest(doc, pages, d)),
            Err(_) => LinkAction::GoTo(LinkDest::Broken),
        },
        Some("GoToR") => {
            let file = action
                .get(b"F")
                .ok()
                .and_then(|f| file_spec(doc, f))
                .unwrap_or_default();
            let dest = action
                .get(b"D")
                .ok()
                .and_then(|d| match parse_dest_value(doc, d)? {
                    DestValue::Named(n) => Some(RemoteDest::Named(n)),
                    DestValue::Explicit(raw) => match raw.page {
                        RawPage::Number(n) => Some(RemoteDest::Page {
                            page_index: usize::try_from(n).ok()?,
                            view: raw.view,
                        }),
                        _ => None,
                    },
                });
            LinkAction::GoToR {
                file,
                dest,
                new_window,
            }
        }
        Some("URI") => LinkAction::Uri(
            action
                .get(b"URI")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_str().ok())
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default(),
        ),
        Some("Launch") => {
            let file = action
                .get(b"F")
                .ok()
                .and_then(|f| file_spec(doc, f))
                .or_else(|| {
                    // The Windows-specific launch parameters.
                    action
                        .get(b"Win")
                        .ok()
                        .and_then(|o| deref(doc, o))
                        .and_then(|o| o.as_dict().ok())
                        .and_then(|w| w.get(b"F").ok())
                        .and_then(|f| file_spec(doc, f))
                })
                .unwrap_or_default();
            LinkAction::Launch { file, new_window }
        }
        Some("Named") => LinkAction::Named(
            action
                .get(b"N")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(name_of)
                .unwrap_or_default(),
        ),
        Some(other) => LinkAction::Other(other.to_string()),
        None => LinkAction::None,
    }
}

/// The action of a link annotation dictionary, and whether it was written as `/A`.
pub(crate) fn link_action(
    doc: &Document,
    pages: &[ObjectId],
    dict: &Dictionary,
) -> (LinkAction, bool) {
    if let Ok(dest) = dict.get(b"Dest") {
        return (LinkAction::GoTo(local_dest(doc, pages, dest)), false);
    }
    match dict.get(b"A").ok().and_then(|a| deref(doc, a)) {
        Some(Object::Dictionary(a)) => (parse_action(doc, pages, a), true),
        _ => (LinkAction::None, false),
    }
}

fn quad_points(doc: &Document, dict: &Dictionary) -> Vec<[f64; 8]> {
    let Some(Object::Array(items)) = dict.get(b"QuadPoints").ok().and_then(|o| deref(doc, o))
    else {
        return Vec::new();
    };
    let values: Vec<f64> = items
        .iter()
        .filter_map(|o| deref(doc, o).and_then(number))
        .collect();
    values.as_chunks::<8>().0.to_vec()
}

fn border_width(doc: &Document, dict: &Dictionary) -> f64 {
    if let Some(w) = dict
        .get(b"BS")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|bs| bs.get(b"W").ok())
        .and_then(|o| deref(doc, o))
        .and_then(number)
    {
        return w;
    }
    match dict.get(b"Border").ok().and_then(|o| deref(doc, o)) {
        Some(Object::Array(b)) if b.len() >= 3 => deref(doc, &b[2]).and_then(number).unwrap_or(1.0),
        // The default border is a 1-point solid line.
        _ => 1.0,
    }
}

fn color(doc: &Document, dict: &Dictionary) -> Option<[f64; 3]> {
    let Some(Object::Array(c)) = dict.get(b"C").ok().and_then(|o| deref(doc, o)) else {
        return None;
    };
    if c.len() != 3 {
        return None;
    }
    let mut out = [0.0; 3];
    for (slot, v) in out.iter_mut().zip(c) {
        *slot = deref(doc, v).and_then(number)?;
    }
    Some(out)
}

fn file_object(file: &str) -> Object {
    let mut spec = Dictionary::new();
    spec.set("Type", Object::Name(b"Filespec".to_vec()));
    spec.set("F", text_string(file));
    spec.set("UF", text_string(file));
    Object::Dictionary(spec)
}

fn action_dict(kind: &str) -> Dictionary {
    let mut a = Dictionary::new();
    a.set("Type", Object::Name(b"Action".to_vec()));
    a.set("S", Object::Name(kind.as_bytes().to_vec()));
    a
}

/// The `/Dest` or `/A` entry for an action: `(key, value)`. `None` for [`LinkAction::None`].
pub(crate) fn action_entry(
    pages: &[ObjectId],
    names: &crate::dest::NameForms,
    action: &LinkAction,
) -> Result<Option<(&'static str, Object)>> {
    let entry = match action {
        LinkAction::None => return Ok(None),
        LinkAction::GoTo(LinkDest::Page(dest)) => {
            let d = dest.to_object(pages).ok_or(PdfError::PageOutOfRange {
                index: dest.page_index,
                count: pages.len(),
            })?;
            let mut a = action_dict("GoTo");
            a.set("D", d);
            ("A", Object::Dictionary(a))
        }
        LinkAction::GoTo(LinkDest::Named(name)) => {
            let mut a = action_dict("GoTo");
            a.set("D", names.object(name));
            ("A", Object::Dictionary(a))
        }
        LinkAction::GoTo(LinkDest::Broken) => {
            return Err(PdfError::InvalidOperation(
                "a link cannot be given a broken destination".into(),
            ))
        }
        LinkAction::GoToR {
            file,
            dest,
            new_window,
        } => {
            let mut a = action_dict("GoToR");
            a.set("F", file_object(file));
            match dest {
                Some(RemoteDest::Page { page_index, view }) => {
                    let mut items = vec![Object::Integer(*page_index as i64)];
                    items.extend(view.to_objects());
                    a.set("D", Object::Array(items));
                }
                Some(RemoteDest::Named(n)) => a.set("D", crate::dest::byte_string(n)),
                // /D is required: the first page, whole.
                None => a.set(
                    "D",
                    Object::Array(vec![Object::Integer(0), Object::Name(b"Fit".to_vec())]),
                ),
            }
            if let Some(w) = new_window {
                a.set("NewWindow", Object::Boolean(*w));
            }
            ("A", Object::Dictionary(a))
        }
        LinkAction::Uri(uri) => {
            let mut a = action_dict("URI");
            // URIs are 7-bit ASCII strings, not text strings (ISO 32000-1 12.6.4.7).
            a.set(
                "URI",
                Object::String(uri_ascii(uri), lopdf::StringFormat::Literal),
            );
            ("A", Object::Dictionary(a))
        }
        LinkAction::Launch { file, new_window } => {
            let mut a = action_dict("Launch");
            a.set("F", file_object(file));
            if let Some(w) = new_window {
                a.set("NewWindow", Object::Boolean(*w));
            }
            ("A", Object::Dictionary(a))
        }
        LinkAction::Named(n) => {
            let mut a = action_dict("Named");
            a.set("N", Object::Name(n.as_bytes().to_vec()));
            ("A", Object::Dictionary(a))
        }
        LinkAction::Other(kind) => {
            return Err(PdfError::InvalidOperation(format!(
                "Plumbline does not write {kind} actions"
            )))
        }
    };
    Ok(Some(entry))
}

/// `uri` as the 7-bit ASCII a `/URI` must be: every byte of a non-ASCII character, every
/// control character and every space percent-encoded (RFC 3987 3.1), the rest unchanged. A
/// host name must already be ASCII (its `xn--` form) to stay one host after this.
pub(crate) fn uri_ascii(uri: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(uri.len());
    for b in uri.bytes() {
        if b >= 0x80 || b <= 0x20 || b == 0x7f {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        } else {
            out.push(b);
        }
    }
    out
}

fn rect_object(r: Rect) -> Object {
    Object::Array(vec![real(r.x0), real(r.y0), real(r.x1), real(r.y1)])
}

fn spec_keys(
    pages: &[ObjectId],
    names: &crate::dest::NameForms,
    spec: &LinkSpec,
) -> Result<Vec<(&'static str, Option<Object>)>> {
    let mut keys = vec![("Rect", Some(rect_object(spec.rect)))];
    keys.push((
        "QuadPoints",
        (!spec.quad_points.is_empty()).then(|| {
            Object::Array(
                spec.quad_points
                    .iter()
                    .flatten()
                    .map(|v| real(*v))
                    .collect(),
            )
        }),
    ));
    let mut bs = Dictionary::new();
    bs.set("W", real(spec.border_width));
    bs.set("S", Object::Name(b"S".to_vec()));
    keys.push(("BS", Some(Object::Dictionary(bs))));
    keys.push((
        "Border",
        Some(Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            real(spec.border_width),
        ])),
    ));
    keys.push((
        "C",
        spec.color
            .map(|c| Object::Array(c.iter().map(|v| real(*v)).collect())),
    ));
    let (dest, action) = match action_entry(pages, names, &spec.action)? {
        Some(("A", a)) => (None, Some(a)),
        Some((_, d)) => (Some(d), None),
        None => (None, None),
    };
    keys.push(("Dest", dest));
    keys.push(("A", action));
    Ok(keys)
}

impl PdfDocument {
    /// The link annotations of a page, in `/Annots` order.
    pub fn links(&self, page_index: usize) -> Result<Vec<Link>> {
        let mut out = Vec::new();
        for entry in self.annotations(page_index)? {
            if entry.subtype() != Some(b"Link") {
                continue;
            }
            let dict = &entry.dict;
            let (action, as_action) = link_action(&self.doc, &self.pages, dict);
            out.push(Link {
                page_index,
                slot: entry.slot,
                id: entry.id,
                rect: dict
                    .get(b"Rect")
                    .ok()
                    .and_then(|o| deref(&self.doc, o))
                    .and_then(rect_from_object)
                    .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0)),
                quad_points: quad_points(&self.doc, dict),
                action,
                border_width: border_width(&self.doc, dict),
                color: color(&self.doc, dict),
                as_action,
            });
        }
        Ok(out)
    }

    /// Where a local GoTo leads in this document. Named destinations are looked up.
    pub fn resolve_link_dest(&self, dest: &LinkDest) -> Option<ResolvedDest> {
        match dest {
            LinkDest::Page(d) => (d.page_index < self.pages.len()).then(|| ResolvedDest::from(*d)),
            LinkDest::Named(n) => self.resolve_named_destination(n),
            LinkDest::Broken => None,
        }
    }

    /// Where an action leads inside this document, when it does: GoTo destinations, and the
    /// page-navigation named actions relative to `from_page`. Everything else (another file, an
    /// address, a program) gives `None`; nothing is ever opened or run.
    pub fn resolve_link_action(
        &self,
        action: &LinkAction,
        from_page: usize,
    ) -> Option<ResolvedDest> {
        let page = |page_index: usize| {
            (page_index < self.pages.len())
                .then(|| ResolvedDest::from(Destination::new(page_index, DestView::Fit)))
        };
        match action {
            LinkAction::GoTo(d) => self.resolve_link_dest(d),
            LinkAction::Named(n) => match n.as_str() {
                "NextPage" => page(from_page + 1),
                "PrevPage" => from_page.checked_sub(1).and_then(page),
                "FirstPage" => page(0),
                "LastPage" => self.pages.len().checked_sub(1).and_then(page),
                _ => None,
            },
            _ => None,
        }
    }

    /// The dictionary of a new link annotation on `page_index` (`/P` set), ready for
    /// [`EditSet::add_annotation`].
    pub fn link_dictionary(&self, page_index: usize, spec: &LinkSpec) -> Result<Dictionary> {
        let page = self.page_id(page_index)?;
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Annot".to_vec()));
        d.set("Subtype", Object::Name(b"Link".to_vec()));
        // Print flag, as for every markup this crate's callers write.
        d.set("F", Object::Integer(4));
        d.set("P", Object::Reference(page));
        for (key, value) in spec_keys(&self.pages, &crate::dest::NameForms::of(&self.doc), spec)? {
            if let Some(v) = value {
                d.set(key, v);
            }
        }
        Ok(d)
    }

    /// Add a new link to `page_index` through `edits`; returns the annotation's id.
    pub fn add_link(
        &self,
        edits: &mut EditSet,
        page_index: usize,
        spec: &LinkSpec,
    ) -> Result<ObjectId> {
        let dict = self.link_dictionary(page_index, spec)?;
        Ok(edits.add_annotation(page_index, dict))
    }

    /// The dirty-key patch that turns an existing link into `spec`: rectangle, quadrilaterals,
    /// border, colour and action. Other keys (`/NM`, `/BSIColumnData`...) are left as they are.
    pub fn link_patch(&self, spec: &LinkSpec) -> Result<DictPatch> {
        let mut patch = DictPatch::new();
        for (key, value) in spec_keys(&self.pages, &crate::dest::NameForms::of(&self.doc), spec)? {
            match value {
                Some(v) => patch.set(key, v),
                None => patch.remove(key),
            };
        }
        Ok(patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_are_written_as_seven_bit_ascii() {
        assert_eq!(
            uri_ascii("https://example.org/a?b=c#d"),
            b"https://example.org/a?b=c#d".to_vec()
        );
        assert_eq!(
            uri_ascii("https://example.org/plan\u{e9} 2"),
            b"https://example.org/plan%C3%A9%202".to_vec()
        );
        let action = LinkAction::Uri("https://example.org/\u{fc}".into());
        let (_, a) = action_entry(&[], &crate::dest::NameForms::default(), &action)
            .unwrap()
            .unwrap();
        let uri = a.as_dict().unwrap().get(b"URI").unwrap().as_str().unwrap();
        assert_eq!(uri, b"https://example.org/%C3%BC");
        assert!(uri.is_ascii());
    }
}
