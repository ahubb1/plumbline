//! The edit set: object-level changes collected in memory and applied by a save.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::document::annots_array;
use crate::error::{PdfError, Result};

/// Changes to individual keys of one dictionary (or of a stream's dictionary). Keys not named
/// here are written back exactly as the file had them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DictPatch {
    set: Vec<(Vec<u8>, Object)>,
    remove: Vec<Vec<u8>>,
}

impl DictPatch {
    pub fn new() -> DictPatch {
        DictPatch::default()
    }

    /// Set `key` to `value`, replacing any existing value.
    pub fn set(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Object>) -> &mut DictPatch {
        let key = key.into();
        self.remove.retain(|k| *k != key);
        self.set.retain(|(k, _)| *k != key);
        self.set.push((key, value.into()));
        self
    }

    /// Remove `key` if present.
    pub fn remove(&mut self, key: impl Into<Vec<u8>>) -> &mut DictPatch {
        let key = key.into();
        self.set.retain(|(k, _)| *k != key);
        if !self.remove.contains(&key) {
            self.remove.push(key);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty()
    }

    /// Keys this patch sets or removes.
    pub fn dirty_keys(&self) -> impl Iterator<Item = &[u8]> {
        self.set
            .iter()
            .map(|(k, _)| k.as_slice())
            .chain(self.remove.iter().map(Vec::as_slice))
    }

    fn merge(&mut self, other: DictPatch) {
        for key in other.remove {
            self.remove(key);
        }
        for (key, value) in other.set {
            self.set(key, value);
        }
    }

    fn apply(&self, dict: &mut Dictionary) {
        for key in &self.remove {
            dict.remove(key);
        }
        for (key, value) in &self.set {
            dict.set(key.clone(), value.clone());
        }
    }
}

/// A batch of changes against one document state.
///
/// Create it with `PdfDocument::edit` (or `PdfFile::edit`); the object ids it hands out
/// are real, so new objects can reference each other before anything is written. An edit set
/// is only valid for the document state it was started from: after a save, start a new one.
#[derive(Debug, Clone, PartialEq)]
pub struct EditSet {
    /// The state token of the document this set was started from (see `PdfDocument::state`).
    base_state: u64,
    base_max_id: u32,
    next_id: u32,
    /// New objects and whole replacements. `None` marks a reserved id not yet filled in.
    objects: BTreeMap<ObjectId, Option<Object>>,
    patches: BTreeMap<ObjectId, DictPatch>,
    annots_added: Vec<(usize, ObjectId)>,
    annots_removed: Vec<(usize, ObjectId)>,
}

impl EditSet {
    pub(crate) fn new(base_max_id: u32, base_state: u64) -> EditSet {
        EditSet {
            base_state,
            base_max_id,
            next_id: base_max_id,
            objects: BTreeMap::new(),
            patches: BTreeMap::new(),
            annots_added: Vec::new(),
            annots_removed: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
            && self.patches.values().all(DictPatch::is_empty)
            && self.annots_added.is_empty()
            && self.annots_removed.is_empty()
    }

    /// Reserve an object id to fill in later with [`EditSet::set_object`]. Use it when two new
    /// objects must reference each other (a markup and its popup).
    pub fn reserve_id(&mut self) -> ObjectId {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("object numbers exhausted");
        let id = (self.next_id, 0);
        self.objects.insert(id, None);
        id
    }

    /// Add a new indirect object and return its id.
    pub fn add_object(&mut self, object: impl Into<Object>) -> ObjectId {
        let id = self.reserve_id();
        self.objects.insert(id, Some(object.into()));
        id
    }

    /// Add a new stream object (an appearance stream, say). `/Length` is set from `content`;
    /// the content is written as given, so set `/Filter` in `dict` only if it is already encoded.
    pub fn add_stream(&mut self, dict: Dictionary, content: Vec<u8>) -> ObjectId {
        self.add_object(Object::Stream(Stream::new(dict, content)))
    }

    /// Give a reserved id its object, or replace an existing object of the document wholesale.
    /// Prefer [`EditSet::patch`] for dictionaries so untouched keys are carried over.
    pub fn set_object(&mut self, id: ObjectId, object: impl Into<Object>) {
        self.objects.insert(id, Some(object.into()));
    }

    /// The patch for an existing dictionary or stream object, created empty on first use.
    /// Patching an object added in this same set also works.
    pub fn patch(&mut self, id: ObjectId) -> &mut DictPatch {
        self.patches.entry(id).or_default()
    }

    /// Change only the given keys of an existing annotation (or any dictionary object).
    pub fn modify_annotation(&mut self, id: ObjectId, patch: DictPatch) {
        self.patches.entry(id).or_default().merge(patch);
    }

    /// Add a new annotation object and append it to the `/Annots` of the page at `page_index`.
    /// The dictionary is written as given; nothing is added to it.
    pub fn add_annotation(&mut self, page_index: usize, dict: Dictionary) -> ObjectId {
        let id = self.add_object(Object::Dictionary(dict));
        self.annots_added.push((page_index, id));
        id
    }

    /// Append an object of this set (typically a reserved id) to a page's `/Annots`.
    pub fn attach_annotation(&mut self, page_index: usize, id: ObjectId) {
        self.annots_added.push((page_index, id));
    }

    /// Remove an annotation from the page's `/Annots`. In an incremental save the object itself
    /// stays in the file, unreferenced (earlier revisions are never rewritten); a full save
    /// drops it.
    pub fn delete_annotation(&mut self, page_index: usize, id: ObjectId) {
        if let Some(pos) = self
            .annots_added
            .iter()
            .position(|e| *e == (page_index, id))
        {
            // Added and deleted in the same set: it never reaches the page.
            self.annots_added.remove(pos);
            if id.0 > self.base_max_id {
                self.objects.remove(&id);
                self.patches.remove(&id);
            }
        } else {
            self.annots_removed.push((page_index, id));
        }
    }

    /// Indices of the pages whose `/Annots` this set adds to or removes from, ascending.
    /// These are the pages a save is certain to change the look of; a patch to an existing
    /// object can change others, which only the caller knows.
    pub fn annotation_pages(&self) -> Vec<usize> {
        let pages: BTreeSet<usize> = self
            .annots_added
            .iter()
            .chain(&self.annots_removed)
            .map(|(page, _)| *page)
            .collect();
        pages.into_iter().collect()
    }

    /// Ids of objects this set creates (above the document's previous highest number).
    pub fn new_object_ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        let base = self.base_max_id;
        self.objects.keys().copied().filter(move |id| id.0 > base)
    }

    /// Work out exactly which objects have to be written, against `doc`.
    ///
    /// `state` is the document's state token. It changes on every load and every save, so a set
    /// started before a save is refused even when that save added no object numbers (a patch
    /// or a delete on a file with a classic cross-reference table).
    pub(crate) fn resolve(
        &self,
        doc: &Document,
        pages: &[ObjectId],
        state: u64,
    ) -> Result<ResolvedEdits> {
        if state != self.base_state || doc.max_id != self.base_max_id {
            return Err(PdfError::StaleEditSet);
        }

        let mut objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
        for (id, object) in &self.objects {
            match object {
                Some(object) => {
                    objects.insert(*id, object.clone());
                }
                None => return Err(PdfError::ReservedIdUnset(*id)),
            }
        }

        // Dirty-key patches start from the object as the file has it.
        for (id, patch) in &self.patches {
            if patch.is_empty() {
                continue;
            }
            let object = current(&mut objects, doc, *id)?;
            match object {
                Object::Dictionary(dict) => patch.apply(dict),
                Object::Stream(stream) => patch.apply(&mut stream.dict),
                _ => return Err(PdfError::NotADictionary(*id)),
            }
        }

        // /Annots arrays. Each page's array is rewritten at most once, in whichever object
        // holds it: the array object when /Annots is a reference, otherwise the page.
        let mut annotation_counts: Vec<usize> = pages
            .iter()
            .map(|id| annots_array(doc, *id).map_or(0, <[Object]>::len))
            .collect();
        let mut touched: BTreeSet<usize> = BTreeSet::new();
        for (page_index, _) in self.annots_removed.iter().chain(&self.annots_added) {
            if *page_index >= pages.len() {
                return Err(PdfError::PageOutOfRange {
                    index: *page_index,
                    count: pages.len(),
                });
            }
            touched.insert(*page_index);
        }
        for page_index in touched {
            let page_id = pages[page_index];
            let page = current(&mut objects, doc, page_id)?;
            let page_dict = match page {
                Object::Dictionary(d) => d,
                _ => return Err(PdfError::NotADictionary(page_id)),
            };
            let holder = match page_dict.get(b"Annots") {
                Ok(Object::Reference(array_id)) => Some(*array_id),
                Ok(Object::Array(_)) => None,
                Ok(Object::Null) | Err(_) => {
                    page_dict.set("Annots", Object::Array(Vec::new()));
                    None
                }
                Ok(_) => return Err(PdfError::AnnotsNotArray { page_index }),
            };
            let array = match holder {
                Some(array_id) => {
                    // The page itself is unchanged unless a patch touched it.
                    if !self.objects.contains_key(&page_id) && !self.patches.contains_key(&page_id)
                    {
                        objects.remove(&page_id);
                    }
                    match current(&mut objects, doc, array_id)? {
                        Object::Array(a) => a,
                        _ => return Err(PdfError::AnnotsNotArray { page_index }),
                    }
                }
                None => match current(&mut objects, doc, page_id)? {
                    Object::Dictionary(d) => match d.get_mut(b"Annots") {
                        Ok(Object::Array(a)) => a,
                        _ => return Err(PdfError::AnnotsNotArray { page_index }),
                    },
                    _ => return Err(PdfError::NotADictionary(page_id)),
                },
            };
            for (_, id) in self.annots_removed.iter().filter(|(p, _)| *p == page_index) {
                let pos = array
                    .iter()
                    .position(|o| matches!(o, Object::Reference(r) if r == id))
                    .ok_or(PdfError::AnnotationNotOnPage {
                        id: *id,
                        page_index,
                    })?;
                array.remove(pos);
            }
            for (_, id) in self.annots_added.iter().filter(|(p, _)| *p == page_index) {
                array.push(Object::Reference(*id));
            }
            annotation_counts[page_index] = array.len();
        }

        // Every annotation appended to a page must exist somewhere.
        for (_, id) in &self.annots_added {
            if !objects.contains_key(id) && !doc.objects.contains_key(id) {
                return Err(PdfError::ObjectNotFound(*id));
            }
        }

        let max_id = objects
            .keys()
            .map(|id| id.0)
            .max()
            .unwrap_or(0)
            .max(self.next_id)
            .max(doc.max_id);
        Ok(ResolvedEdits {
            objects,
            annotation_counts,
            max_id,
        })
    }
}

/// The object as this save will write it: the pending version if there is one, otherwise a
/// copy of the document's, which then becomes pending.
fn current<'a>(
    objects: &'a mut BTreeMap<ObjectId, Object>,
    doc: &Document,
    id: ObjectId,
) -> Result<&'a mut Object> {
    match objects.entry(id) {
        Entry::Occupied(entry) => Ok(entry.into_mut()),
        Entry::Vacant(entry) => {
            let original = doc.objects.get(&id).ok_or(PdfError::ObjectNotFound(id))?;
            Ok(entry.insert(original.clone()))
        }
    }
}

/// What a save has to write, and what the result must look like.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedEdits {
    /// Complete new versions of every object that changes or is new.
    pub objects: BTreeMap<ObjectId, Object>,
    /// `/Annots` length per page after the edits.
    pub annotation_counts: Vec<usize>,
    /// Highest object number after the edits.
    pub max_id: u32,
}
