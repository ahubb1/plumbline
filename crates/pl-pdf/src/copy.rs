//! Deep copy of pages (with everything they reference) from one parsed document into another.
//!
//! The copy follows references with a work list, so deep graphs cannot overflow the stack.
//! What it does not follow:
//!
//! - page-tree nodes, the catalog and the document information dictionary: a reference to a
//!   page that is copied becomes a reference to its copy; one to any other page, a `/Pages`
//!   node, the catalog or `/Info` becomes `null` (dropped from a dictionary);
//! - annotations of the source that are not on a copied page (a reply's `/IRT` to a markup left
//!   behind, a widget of the same field on another page): dropped the same way;
//! - `/Parent`, `/B` (article beads) and `/StructParents` of the page, `/StructParent` of its
//!   annotations: the structure tree and threads of the source are not copied.

use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::document::INHERITABLE_PAGE_KEYS;

pub(crate) struct Copier<'s> {
    src: &'s Document,
    /// Source object -> copy.
    pub memo: HashMap<ObjectId, ObjectId>,
    queue: Vec<ObjectId>,
    /// Source page -> its copy.
    pub page_map: HashMap<ObjectId, ObjectId>,
    /// Ids the copy never follows (pages not copied, page-tree nodes, catalog, info).
    blocked: HashSet<ObjectId>,
    /// Annotations of copied pages (source ids).
    pub copied_annots: HashSet<ObjectId>,
}

/// Every annotation id referenced from a page's `/Annots`.
pub(crate) fn page_annotation_ids(doc: &Document, page: ObjectId) -> Vec<ObjectId> {
    crate::document::annots_array(doc, page)
        .map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect())
        .unwrap_or_default()
}

impl<'s> Copier<'s> {
    /// A copier for the pages `pages` of `src`. Their copies' ids are allocated in `dst` now,
    /// so references between copied pages resolve whatever the order.
    pub fn new(src: &'s Document, src_pages: &[ObjectId], dst: &mut Document) -> Copier<'s> {
        let mut blocked: HashSet<ObjectId> = HashSet::new();
        let all_pages: Vec<ObjectId> = src.page_iter().collect();
        for p in &all_pages {
            blocked.insert(*p);
        }
        for (id, object) in &src.objects {
            if let Object::Dictionary(d) = object {
                if d.get(b"Type").and_then(Object::as_name).ok() == Some(b"Pages".as_slice()) {
                    blocked.insert(*id);
                }
            }
        }
        for key in [b"Root".as_slice(), b"Info"] {
            if let Ok(Object::Reference(id)) = src.trailer.get(key) {
                blocked.insert(*id);
            }
        }
        let mut copied_annots = HashSet::new();
        let mut all_annots = HashSet::new();
        for p in &all_pages {
            for a in page_annotation_ids(src, *p) {
                all_annots.insert(a);
                if src_pages.contains(p) {
                    copied_annots.insert(a);
                }
            }
        }
        for a in &all_annots {
            if !copied_annots.contains(a) {
                blocked.insert(*a);
            }
        }
        let mut page_map = HashMap::new();
        for p in src_pages {
            page_map.insert(*p, dst.new_object_id());
        }
        Copier {
            src,
            memo: HashMap::new(),
            queue: Vec::new(),
            page_map,
            blocked,
            copied_annots,
        }
    }

    /// The copy of the source object `id`, allocated (and queued) on first sight. `None` for an
    /// object the copy does not follow.
    pub fn map_ref(&mut self, dst: &mut Document, id: ObjectId) -> Option<ObjectId> {
        if let Some(p) = self.page_map.get(&id) {
            return Some(*p);
        }
        if self.blocked.contains(&id) || !self.src.objects.contains_key(&id) {
            return None;
        }
        if let Some(c) = self.memo.get(&id) {
            return Some(*c);
        }
        let new = dst.new_object_id();
        self.memo.insert(id, new);
        self.queue.push(id);
        Some(new)
    }

    /// The object with every reference mapped. A dictionary entry whose reference is not
    /// followed is dropped; an array element becomes `null`.
    pub fn map_obj(&mut self, dst: &mut Document, object: &Object) -> Object {
        match object {
            Object::Reference(id) => match self.map_ref(dst, *id) {
                Some(new) => Object::Reference(new),
                None => Object::Null,
            },
            Object::Array(items) => {
                Object::Array(items.iter().map(|o| self.map_obj(dst, o)).collect())
            }
            Object::Dictionary(d) => Object::Dictionary(self.map_dict(dst, d)),
            Object::Stream(s) => {
                let mut s = s.clone();
                s.dict = self.map_dict(dst, &s.dict);
                Object::Stream(s)
            }
            other => other.clone(),
        }
    }

    fn map_dict(&mut self, dst: &mut Document, d: &Dictionary) -> Dictionary {
        let mut out = Dictionary::new();
        for (k, v) in d.iter() {
            let mapped = self.map_obj(dst, v);
            if matches!(v, Object::Reference(_)) && mapped == Object::Null {
                continue;
            }
            out.set(k.clone(), mapped);
        }
        out
    }

    /// Copy everything queued so far (and whatever that pulls in).
    pub fn run(&mut self, dst: &mut Document) {
        while let Some(src_id) = self.queue.pop() {
            let Some(object) = self.src.objects.get(&src_id) else {
                continue;
            };
            let mut mapped = self.map_obj(dst, object);
            if self.copied_annots.contains(&src_id) {
                if let Object::Dictionary(d) = &mut mapped {
                    d.remove(b"StructParent");
                }
            }
            let new = self.memo[&src_id];
            dst.objects.insert(new, mapped);
        }
    }

    /// Copy one page: its dictionary with inherited attributes made its own, its annotations
    /// and resources. Returns the copy's id (already allocated in [`Copier::new`]).
    pub fn copy_page(
        &mut self,
        dst: &mut Document,
        src_page: ObjectId,
        parent: ObjectId,
    ) -> ObjectId {
        let new_id = self.page_map[&src_page];
        let mut d = self
            .src
            .get_dictionary(src_page)
            .cloned()
            .unwrap_or_default();
        for key in INHERITABLE_PAGE_KEYS {
            if !d.has(key) {
                if let Some(v) = inherited_raw(self.src, src_page, key) {
                    d.set(key.to_vec(), v);
                }
            }
        }
        if !d.has(b"MediaBox") {
            d.set("MediaBox", crate::document::fallback_media_box());
        }
        if !d.has(b"Resources") {
            d.set("Resources", Object::Dictionary(Dictionary::new()));
        }
        // An /Annots held in its own array object becomes a direct array of the page.
        if let Ok(Object::Reference(id)) = d.get(b"Annots") {
            let inline = self
                .src
                .get_object(*id)
                .ok()
                .cloned()
                .unwrap_or(Object::Null);
            d.set("Annots", inline);
        }
        for key in [b"Parent".as_slice(), b"B", b"StructParents"] {
            d.remove(key);
        }
        let mut mapped = self.map_dict(dst, &d);
        if let Ok(Object::Array(annots)) = mapped.get_mut(b"Annots") {
            annots.retain(|o| !matches!(o, Object::Null));
            for a in annots.iter_mut() {
                // Direct annotation dictionaries: same treatment as indirect ones.
                if let Object::Dictionary(ad) = a {
                    ad.remove(b"StructParent");
                    ad.set("P", Object::Reference(new_id));
                }
            }
        }
        mapped.set("Parent", Object::Reference(parent));
        dst.objects.insert(new_id, Object::Dictionary(mapped));
        new_id
    }
}

/// An inherited page attribute as stored on the nearest ancestor (not dereferenced).
pub(crate) fn inherited_raw(doc: &Document, page: ObjectId, key: &[u8]) -> Option<Object> {
    let mut node = doc.get_dictionary(page).ok()?;
    for _ in 0..64 {
        if let Ok(v) = node.get(key) {
            return Some(v.clone());
        }
        let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
        node = doc.get_dictionary(parent).ok()?;
    }
    None
}
