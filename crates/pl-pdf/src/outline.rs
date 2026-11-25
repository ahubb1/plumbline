//! The document outline (bookmarks, ISO 32000-1 12.3.3): read the whole tree into an owned
//! model, edit it by path, and write it back with `/First /Last /Prev /Next /Parent /Count`
//! worked out afresh.
//!
//! Writing builds every item object anew (old ones become unreferenced: a full rewrite prunes
//! them, an incremental save leaves them in the earlier revision). Keys the model does not know
//! (`/SE`, private keys) are carried over unchanged.

use std::collections::BTreeSet;

use lopdf::{decode_text_string, text_string, Dictionary, Document, Object, ObjectId};

use crate::dest::{deref, parse_dest_value, resolve_raw, DestValue, Destination, RawDest};
use crate::document::{number, PdfDocument};
use crate::edit::EditSet;
use crate::error::{PdfError, Result};

/// The whole outline: its top-level items in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outline {
    pub items: Vec<OutlineItem>,
}

/// Where an outline item leads.
#[derive(Debug, Clone, PartialEq)]
pub enum OutlineTarget {
    /// Nothing (a heading only).
    None,
    /// A page of this document. `as_action` writes it as `/A << /S /GoTo /D [...] >>` rather
    /// than `/Dest [...]` (both are kept as the file had them).
    Page { dest: Destination, as_action: bool },
    /// A named destination, resolved through the document's `/Dests` or name tree when it is
    /// looked up.
    Named { name: Vec<u8>, as_action: bool },
    /// Some other action (`/URI`, `/GoToR`, `/Named`, a chain...), written back verbatim.
    Action(Object),
    /// A `/Dest` or GoTo whose page is not in the document, kept verbatim as `(key, value)`.
    Unresolved { key: Vec<u8>, value: Object },
}

/// One bookmark.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    /// `/Title`, decoded (PDFDocEncoding, UTF-16BE or UTF-8).
    pub title: String,
    pub target: OutlineTarget,
    /// `/C`, the title colour (DeviceRGB).
    pub color: Option<[f64; 3]>,
    /// `/F` bit 2.
    pub bold: bool,
    /// `/F` bit 1.
    pub italic: bool,
    /// Whether the item shows its children (sign of `/Count`).
    pub open: bool,
    pub children: Vec<OutlineItem>,
    /// Keys the model does not interpret, carried over as written.
    pub(crate) extra: Vec<(Vec<u8>, Object)>,
}

impl OutlineItem {
    /// A closed item with a title and a target and no styling.
    pub fn new(title: impl Into<String>, target: OutlineTarget) -> OutlineItem {
        OutlineItem {
            title: title.into(),
            target,
            color: None,
            bold: false,
            italic: false,
            open: false,
            children: Vec::new(),
            extra: Vec::new(),
        }
    }

    /// An item that goes to `page_index` with `view`, written as `/Dest`.
    pub fn to_page(title: impl Into<String>, dest: Destination) -> OutlineItem {
        OutlineItem::new(
            title,
            OutlineTarget::Page {
                dest,
                as_action: false,
            },
        )
    }

    /// Number of items below this one (all levels).
    pub fn descendant_count(&self) -> usize {
        self.children.iter().map(|c| 1 + c.descendant_count()).sum()
    }

    /// How many descendants are visible when this item is shown: its children if it is open,
    /// and theirs recursively.
    fn visible_below(&self) -> usize {
        if self.open {
            self.children_if_open()
        } else {
            0
        }
    }

    /// The `/Count` magnitude: what would be visible below this item if it were open.
    fn children_if_open(&self) -> usize {
        self.children.iter().map(|c| 1 + c.visible_below()).sum()
    }

    /// The `/Count` this item is written with (`None` for a leaf).
    pub fn count_value(&self) -> Option<i64> {
        if self.children.is_empty() {
            return None;
        }
        let n = self.children_if_open() as i64;
        Some(if self.open { n } else { -n })
    }
}

/// One item listed in pre-order with its depth, as [`Outline::flatten_with`] gives it.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatOutlineItem {
    /// Path of child indices from the top level.
    pub path: Vec<usize>,
    pub level: usize,
    pub title: String,
    /// The page and view the item leads to in this document, when it does.
    pub dest: Option<Destination>,
}

impl Outline {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Total number of items at all levels.
    pub fn len(&self) -> usize {
        self.items.iter().map(|i| 1 + i.descendant_count()).sum()
    }

    /// The root `/Count`: the items visible with the tree as it is opened.
    pub fn visible_count(&self) -> usize {
        self.items.iter().map(|i| 1 + i.visible_below()).sum()
    }

    pub fn get(&self, path: &[usize]) -> Option<&OutlineItem> {
        let (first, rest) = path.split_first()?;
        let mut item = self.items.get(*first)?;
        for i in rest {
            item = item.children.get(*i)?;
        }
        Some(item)
    }

    pub fn get_mut(&mut self, path: &[usize]) -> Option<&mut OutlineItem> {
        let (first, rest) = path.split_first()?;
        let mut item = self.items.get_mut(*first)?;
        for i in rest {
            item = item.children.get_mut(*i)?;
        }
        Some(item)
    }

    /// The children list at `parent` (the top level for an empty path).
    fn siblings_mut(&mut self, parent: &[usize]) -> Result<&mut Vec<OutlineItem>> {
        if parent.is_empty() {
            Ok(&mut self.items)
        } else {
            self.get_mut(parent)
                .map(|i| &mut i.children)
                .ok_or_else(|| bad_path(parent))
        }
    }

    /// Insert `item` as child number `index` of `parent` (an empty path is the top level).
    /// `index` may equal the number of children (append).
    pub fn insert(&mut self, parent: &[usize], index: usize, item: OutlineItem) -> Result<()> {
        let siblings = self.siblings_mut(parent)?;
        if index > siblings.len() {
            return Err(PdfError::InvalidOperation(format!(
                "outline position {index} is past the {} items there",
                siblings.len()
            )));
        }
        siblings.insert(index, item);
        Ok(())
    }

    /// Append `item` as the last child of `parent`.
    pub fn push(&mut self, parent: &[usize], item: OutlineItem) -> Result<()> {
        let siblings = self.siblings_mut(parent)?;
        siblings.push(item);
        Ok(())
    }

    /// Remove the item at `path` with its children and return it.
    pub fn remove(&mut self, path: &[usize]) -> Result<OutlineItem> {
        let (last, parent) = path.split_last().ok_or_else(|| bad_path(path))?;
        let siblings = self.siblings_mut(parent)?;
        if *last >= siblings.len() {
            return Err(bad_path(path));
        }
        Ok(siblings.remove(*last))
    }

    pub fn rename(&mut self, path: &[usize], title: impl Into<String>) -> Result<()> {
        self.get_mut(path).ok_or_else(|| bad_path(path))?.title = title.into();
        Ok(())
    }

    /// Point the item at a page and view (written as `/Dest`; a GoTo action it had is
    /// replaced).
    pub fn set_destination(&mut self, path: &[usize], dest: Destination) -> Result<()> {
        self.set_target(
            path,
            OutlineTarget::Page {
                dest,
                as_action: false,
            },
        )
    }

    pub fn set_target(&mut self, path: &[usize], target: OutlineTarget) -> Result<()> {
        self.get_mut(path).ok_or_else(|| bad_path(path))?.target = target;
        Ok(())
    }

    /// Move the item at `from` (with its children) to child number `index` of `new_parent`.
    /// `new_parent` and `index` are read in the tree as it is *after* the item is taken out.
    /// An item cannot become its own descendant: once it is taken out, no path reaches it, so
    /// a parent path that ran through it no longer exists and the move fails.
    pub fn move_item(&mut self, from: &[usize], new_parent: &[usize], index: usize) -> Result<()> {
        let item = self.remove(from)?;
        let backup = item.clone();
        if let Err(e) = self.insert(new_parent, index, item) {
            // Put it back where it was so a failed move changes nothing.
            let (last, parent) = from.split_last().expect("checked by remove");
            self.siblings_mut(parent)?.insert(*last, backup);
            return Err(e);
        }
        Ok(())
    }

    /// Every item in pre-order, with its path, depth and resolved page in `doc`.
    pub fn flatten_with(&self, doc: &PdfDocument) -> Vec<FlatOutlineItem> {
        let mut out = Vec::new();
        fn walk(
            items: &[OutlineItem],
            path: &mut Vec<usize>,
            doc: &PdfDocument,
            out: &mut Vec<FlatOutlineItem>,
        ) {
            for (i, item) in items.iter().enumerate() {
                path.push(i);
                out.push(FlatOutlineItem {
                    path: path.clone(),
                    level: path.len() - 1,
                    title: item.title.clone(),
                    dest: doc.outline_target_dest(&item.target),
                });
                walk(&item.children, path, doc, out);
                path.pop();
            }
        }
        walk(&self.items, &mut Vec::new(), doc, &mut out);
        out
    }
}

fn bad_path(path: &[usize]) -> PdfError {
    PdfError::InvalidOperation(format!("there is no outline item at {path:?}"))
}

const OUTLINE_DEPTH_LIMIT: usize = 64;
const OUTLINE_ITEM_LIMIT: usize = 1_000_000;

/// Keys the model reads; everything else is kept in `extra`.
const MODEL_KEYS: [&[u8]; 11] = [
    b"Title", b"Dest", b"A", b"C", b"F", b"Count", b"First", b"Last", b"Prev", b"Next", b"Parent",
];

/// How the reader treats an explicit destination.
pub(crate) struct ReadContext<'a> {
    pub doc: &'a Document,
    pub pages: &'a [ObjectId],
    /// Page objects that have just been removed: items pointing at them are listed.
    pub removed: &'a BTreeSet<ObjectId>,
    /// Paths of items whose destination is one of `removed` (pre-order).
    pub hits: Vec<Vec<usize>>,
}

impl ReadContext<'_> {
    fn points_at_removed(&self, raw: &RawDest) -> bool {
        matches!(raw.page, crate::dest::RawPage::Object(id) if self.removed.contains(&id))
    }

    fn target_of(&mut self, dict: &Dictionary, path: &[usize]) -> OutlineTarget {
        let doc = self.doc;
        let (value, key, as_action) = if let Ok(d) = dict.get(b"Dest") {
            (d, b"Dest".as_slice(), false)
        } else if let Ok(a) = dict.get(b"A") {
            let action = deref(doc, a).and_then(|o| o.as_dict().ok());
            let is_goto = action
                .and_then(|a| a.get(b"S").ok())
                .and_then(|s| s.as_name().ok())
                == Some(b"GoTo".as_slice());
            let chained = action.is_some_and(|a| a.has(b"Next"));
            match action.and_then(|a| a.get(b"D").ok()) {
                Some(d) if is_goto && !chained => (d, b"A".as_slice(), true),
                _ => return OutlineTarget::Action(a.clone()),
            }
        } else {
            return OutlineTarget::None;
        };
        let raw_value = || {
            if as_action {
                dict.get(b"A").cloned().unwrap_or(Object::Null)
            } else {
                value.clone()
            }
        };
        match parse_dest_value(doc, value) {
            Some(DestValue::Explicit(raw)) => {
                if self.points_at_removed(&raw) {
                    self.hits.push(path.to_vec());
                }
                match resolve_raw(self.pages, raw) {
                    Some(dest) => OutlineTarget::Page { dest, as_action },
                    None => OutlineTarget::Unresolved {
                        key: key.to_vec(),
                        value: raw_value(),
                    },
                }
            }
            Some(DestValue::Named(name)) => {
                if let Some(raw) = crate::dest::lookup_named(doc, &name) {
                    if self.points_at_removed(&raw) {
                        self.hits.push(path.to_vec());
                    }
                }
                OutlineTarget::Named { name, as_action }
            }
            None => OutlineTarget::Unresolved {
                key: key.to_vec(),
                value: raw_value(),
            },
        }
    }
}

/// Read the outline of `doc` (an empty outline when there is none).
pub(crate) fn read(ctx: &mut ReadContext<'_>) -> Outline {
    let doc = ctx.doc;
    let Some(root) = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Outlines").ok())
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
    else {
        return Outline::default();
    };
    let mut seen = BTreeSet::new();
    let mut budget = OUTLINE_ITEM_LIMIT;
    let items = read_siblings(
        ctx,
        root.get(b"First").ok(),
        &mut Vec::new(),
        &mut seen,
        &mut budget,
    );
    Outline { items }
}

fn read_siblings(
    ctx: &mut ReadContext<'_>,
    first: Option<&Object>,
    path: &mut Vec<usize>,
    seen: &mut BTreeSet<ObjectId>,
    budget: &mut usize,
) -> Vec<OutlineItem> {
    let doc = ctx.doc;
    let mut out = Vec::new();
    if path.len() > OUTLINE_DEPTH_LIMIT {
        return out;
    }
    let mut next = first.and_then(|o| o.as_reference().ok());
    while let Some(id) = next {
        if *budget == 0 || !seen.insert(id) {
            break;
        }
        *budget -= 1;
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        path.push(out.len());
        let item = read_item(ctx, dict, path, seen, budget);
        path.pop();
        out.push(item);
        next = dict.get(b"Next").ok().and_then(|o| o.as_reference().ok());
    }
    out
}

fn read_item(
    ctx: &mut ReadContext<'_>,
    dict: &Dictionary,
    path: &mut Vec<usize>,
    seen: &mut BTreeSet<ObjectId>,
    budget: &mut usize,
) -> OutlineItem {
    let doc = ctx.doc;
    let title = dict
        .get(b"Title")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| decode_text_string(o).ok())
        .unwrap_or_default();
    let color = dict
        .get(b"C")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_array().ok())
        .filter(|a| a.len() == 3)
        .and_then(|a| {
            let mut c = [0.0; 3];
            for (slot, v) in c.iter_mut().zip(a) {
                *slot = deref(doc, v).and_then(number)?;
            }
            Some(c)
        });
    let flags = dict
        .get(b"F")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(number)
        .map_or(0, |f| f as i64);
    let count = dict
        .get(b"Count")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(number)
        .unwrap_or(0.0);
    let target = ctx.target_of(dict, path);
    let children = read_siblings(ctx, dict.get(b"First").ok(), path, seen, budget);
    let extra = dict
        .iter()
        .filter(|(k, _)| !MODEL_KEYS.contains(&k.as_slice()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    OutlineItem {
        title,
        target,
        color,
        bold: flags & 2 != 0,
        italic: flags & 1 != 0,
        open: count > 0.0,
        children,
        extra,
    }
}

/// The objects of a written outline: the root's id and every object to store.
pub(crate) struct WrittenOutline {
    pub root: ObjectId,
    pub objects: Vec<(ObjectId, Object)>,
}

/// Build the outline objects. `alloc` hands out fresh object ids. `None` for an empty outline.
pub(crate) fn build(
    outline: &Outline,
    pages: &[ObjectId],
    names: &crate::dest::NameForms,
    alloc: &mut dyn FnMut() -> ObjectId,
) -> Result<Option<WrittenOutline>> {
    if outline.items.is_empty() {
        return Ok(None);
    }
    let root = alloc();
    let mut objects = Vec::new();
    let (first, last) = build_siblings(&outline.items, root, pages, names, alloc, &mut objects)?;
    let mut root_dict = Dictionary::new();
    root_dict.set("Type", Object::Name(b"Outlines".to_vec()));
    root_dict.set("First", Object::Reference(first));
    root_dict.set("Last", Object::Reference(last));
    root_dict.set("Count", Object::Integer(outline.visible_count() as i64));
    objects.push((root, Object::Dictionary(root_dict)));
    Ok(Some(WrittenOutline { root, objects }))
}

fn build_siblings(
    items: &[OutlineItem],
    parent: ObjectId,
    pages: &[ObjectId],
    names: &crate::dest::NameForms,
    alloc: &mut dyn FnMut() -> ObjectId,
    objects: &mut Vec<(ObjectId, Object)>,
) -> Result<(ObjectId, ObjectId)> {
    let ids: Vec<ObjectId> = items.iter().map(|_| alloc()).collect();
    for (i, item) in items.iter().enumerate() {
        let mut d = Dictionary::new();
        for (k, v) in &item.extra {
            d.set(k.clone(), v.clone());
        }
        d.set("Title", text_string(&item.title));
        d.set("Parent", Object::Reference(parent));
        if i > 0 {
            d.set("Prev", Object::Reference(ids[i - 1]));
        }
        if i + 1 < ids.len() {
            d.set("Next", Object::Reference(ids[i + 1]));
        }
        if !item.children.is_empty() {
            let (first, last) =
                build_siblings(&item.children, ids[i], pages, names, alloc, objects)?;
            d.set("First", Object::Reference(first));
            d.set("Last", Object::Reference(last));
            d.set(
                "Count",
                Object::Integer(item.count_value().expect("has children")),
            );
        }
        write_target(&mut d, &item.target, pages, names)?;
        if let Some(c) = item.color {
            d.set(
                "C",
                Object::Array(c.iter().map(|v| crate::dest::real(*v)).collect()),
            );
        }
        let flags = i64::from(item.italic) | (i64::from(item.bold) << 1);
        if flags != 0 {
            d.set("F", Object::Integer(flags));
        }
        objects.push((ids[i], Object::Dictionary(d)));
    }
    Ok((ids[0], *ids.last().expect("items is not empty")))
}

fn goto(d: Object) -> Object {
    let mut a = Dictionary::new();
    a.set("S", Object::Name(b"GoTo".to_vec()));
    a.set("D", d);
    Object::Dictionary(a)
}

fn write_target(
    d: &mut Dictionary,
    target: &OutlineTarget,
    pages: &[ObjectId],
    names: &crate::dest::NameForms,
) -> Result<()> {
    match target {
        OutlineTarget::None => {}
        OutlineTarget::Page { dest, as_action } => {
            let array = dest.to_object(pages).ok_or(PdfError::PageOutOfRange {
                index: dest.page_index,
                count: pages.len(),
            })?;
            if *as_action {
                d.set("A", goto(array));
            } else {
                d.set("Dest", array);
            }
        }
        OutlineTarget::Named { name, as_action } => {
            let value = names.object(name);
            if *as_action {
                d.set("A", goto(value));
            } else {
                d.set("Dest", value);
            }
        }
        OutlineTarget::Action(a) => d.set("A", a.clone()),
        OutlineTarget::Unresolved { key, value } => d.set(key.clone(), value.clone()),
    }
    Ok(())
}

/// Write `outline` into `doc` in place: new item objects, the catalog's `/Outlines` set (or
/// removed for an empty outline). Old item objects are left for the full rewrite to prune.
pub(crate) fn store(doc: &mut Document, pages: &[ObjectId], outline: &Outline) -> Result<()> {
    let mut next = doc.max_id;
    let names = crate::dest::NameForms::of(doc);
    let built = build(outline, pages, &names, &mut || {
        next += 1;
        (next, 0)
    })?;
    doc.max_id = next;
    let catalog = doc.catalog_mut().map_err(PdfError::Parse)?;
    match built {
        Some(w) => {
            catalog.set("Outlines", Object::Reference(w.root));
            for (id, object) in w.objects {
                doc.objects.insert(id, object);
            }
        }
        None => {
            catalog.remove(b"Outlines");
        }
    }
    Ok(())
}

impl PdfDocument {
    /// The outline (bookmarks) as an owned tree. A document without one gives an empty tree.
    pub fn outline(&self) -> Outline {
        let removed = BTreeSet::new();
        let mut ctx = ReadContext {
            doc: &self.doc,
            pages: &self.pages,
            removed: &removed,
            hits: Vec::new(),
        };
        read(&mut ctx)
    }

    /// The page and view an outline target leads to in this document.
    pub fn outline_target_dest(&self, target: &OutlineTarget) -> Option<Destination> {
        match target {
            OutlineTarget::Page { dest, .. } => {
                (dest.page_index < self.pages.len()).then_some(*dest)
            }
            OutlineTarget::Named { name, .. } => crate::dest::lookup_named(&self.doc, name)
                .and_then(|raw| resolve_raw(&self.pages, raw)),
            _ => None,
        }
    }

    /// A copy of this document with `outline` in place of its outline, for the full-rewrite
    /// save ([`crate::PdfFile::save_restructured`]).
    pub fn with_outline(&self, outline: &Outline) -> Result<PdfDocument> {
        let mut doc = self.doc.clone();
        store(&mut doc, &self.pages, outline)?;
        PdfDocument::derived(self.bytes.clone(), doc)
    }

    /// Add the changes that replace the outline with `outline` to an edit set, so a bookmark
    /// edit can be saved incrementally: new item objects and a patch of the catalog's
    /// `/Outlines`.
    pub fn outline_edits(&self, edits: &mut EditSet, outline: &Outline) -> Result<()> {
        let catalog_id = self
            .doc
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .map_err(PdfError::Parse)?;
        let mut reserved = Vec::new();
        let names = crate::dest::NameForms::of(&self.doc);
        let built = build(outline, &self.pages, &names, &mut || {
            let id = edits.reserve_id();
            reserved.push(id);
            id
        })?;
        match built {
            Some(w) => {
                for (id, object) in w.objects {
                    edits.set_object(id, object);
                }
                edits
                    .patch(catalog_id)
                    .set("Outlines", Object::Reference(w.root));
            }
            None => {
                edits.patch(catalog_id).remove("Outlines");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dest::DestView;

    fn leaf(t: &str, page: usize) -> OutlineItem {
        OutlineItem::to_page(t, Destination::new(page, DestView::Fit))
    }

    fn sample() -> Outline {
        let mut a = leaf("A", 0);
        a.open = true;
        a.children = vec![leaf("A1", 1), leaf("A2", 2)];
        let mut b = leaf("B", 3);
        b.children = vec![leaf("B1", 4)];
        Outline {
            items: vec![a, b, leaf("C", 5)],
        }
    }

    #[test]
    fn counts_follow_open_state() {
        let o = sample();
        assert_eq!(o.len(), 6);
        // A (open, 2 kids) shows A1 and A2; B is closed.
        assert_eq!(o.visible_count(), 3 + 2);
        assert_eq!(o.items[0].count_value(), Some(2));
        assert_eq!(o.items[1].count_value(), Some(-1));
        assert_eq!(o.items[2].count_value(), None);
    }

    #[test]
    fn edits_by_path() {
        let mut o = sample();
        o.rename(&[0, 1], "A-two").unwrap();
        assert_eq!(o.get(&[0, 1]).unwrap().title, "A-two");
        o.move_item(&[2], &[1], 0).unwrap();
        assert_eq!(o.items.len(), 2);
        assert_eq!(o.get(&[1, 0]).unwrap().title, "C");
        assert!(o.move_item(&[1], &[1, 0], 0).is_err(), "under itself");
        assert!(o.move_item(&[0], &[9], 0).is_err());
        assert_eq!(o.items[0].title, "A", "a failed move changes nothing");
        // Into the following sibling: [0] goes under B, which is [0] once A is taken out.
        let mut nest = sample();
        nest.move_item(&[0], &[0], 0).unwrap();
        assert_eq!(nest.items[0].title, "B");
        assert_eq!(nest.get(&[0, 0]).unwrap().title, "A");
        assert_eq!(nest.len(), 6);
        let removed = o.remove(&[0]).unwrap();
        assert_eq!(removed.children.len(), 2);
        assert_eq!(o.len(), 3);
        o.insert(&[], 0, leaf("Z", 0)).unwrap();
        assert!(o.insert(&[], 9, leaf("bad", 0)).is_err());
        o.set_destination(&[0], Destination::new(2, DestView::at(1.0, 2.0)))
            .unwrap();
        assert!(matches!(
            o.items[0].target,
            OutlineTarget::Page { dest, .. } if dest.page_index == 2
        ));
    }

    #[test]
    fn build_links_siblings_and_parents() {
        let o = sample();
        let pages: Vec<ObjectId> = (1..=6).map(|i| (100 + i, 0)).collect();
        let mut n = 0;
        let w = build(&o, &pages, &Default::default(), &mut || {
            n += 1;
            (n, 0)
        })
        .unwrap()
        .unwrap();
        let get = |id: ObjectId| {
            w.objects
                .iter()
                .find(|(i, _)| *i == id)
                .and_then(|(_, o)| o.as_dict().ok())
                .unwrap()
        };
        let root = get(w.root);
        assert_eq!(root.get(b"Count").unwrap(), &Object::Integer(5));
        let first = root.get(b"First").unwrap().as_reference().unwrap();
        let a = get(first);
        assert_eq!(a.get(b"Parent").unwrap(), &Object::Reference(w.root));
        assert!(!a.has(b"Prev"));
        let a1 = get(a.get(b"First").unwrap().as_reference().unwrap());
        assert_eq!(a1.get(b"Parent").unwrap(), &Object::Reference(first));
        let b = get(a.get(b"Next").unwrap().as_reference().unwrap());
        assert_eq!(b.get(b"Count").unwrap(), &Object::Integer(-1));
        assert_eq!(b.get(b"Prev").unwrap(), &Object::Reference(first));
        let dest = b.get(b"Dest").unwrap().as_array().unwrap();
        assert_eq!(dest[0], Object::Reference(pages[3]));
        let out_of_range = Outline {
            items: vec![leaf("bad", 99)],
        };
        assert!(build(&out_of_range, &pages, &Default::default(), &mut || (1, 0)).is_err());
    }
}
