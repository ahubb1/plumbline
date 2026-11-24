//! The annotations of a document as one ordered collection.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use pl_geom::Point;
use pl_pdf::PdfDocument;

use crate::model::{AnnotId, AnnotKind, Annotation};
use crate::read::read_pages;
use crate::write::AnnotError;

/// What [`AnnotSet::remove`] took out, so that an undo can put it back exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct Removed {
    /// The removed annotations with the position each had in the set, ascending.
    pub items: Vec<(usize, Annotation)>,
    /// A markup whose `popup` link was cleared because only its popup was removed.
    pub unlinked_parent: Option<AnnotId>,
}

impl Removed {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = &AnnotId> {
        self.items.iter().map(|(_, a)| &a.id)
    }
}

/// The annotations of a document (or of some of its pages).
///
/// Order is stacking order: page by page, and within a page from the bottom of the stack to
/// the top, which is the order of the page's `/Annots` array. A set remembers what was removed
/// from it, so that [`AnnotSet::plan_save`] can take those annotations out of the file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AnnotSet {
    items: Vec<Annotation>,
    removed: Vec<Annotation>,
    /// The pages that were read; `None` for the whole document.
    scope: Option<BTreeSet<usize>>,
    /// Where each id is, so that a lookup does not walk the set.
    index: Index,
}

/// Position by id and the highest page in use.
#[derive(Debug, Default)]
struct Lookup {
    /// The first position of each id.
    by_id: HashMap<AnnotId, usize>,
    max_page: Option<usize>,
}

impl Lookup {
    fn of(items: &[Annotation]) -> Lookup {
        let mut lookup = Lookup {
            by_id: HashMap::with_capacity(items.len()),
            max_page: None,
        };
        for (at, a) in items.iter().enumerate() {
            lookup.by_id.entry(a.id.clone()).or_insert(at);
            lookup.max_page = lookup.max_page.max(Some(a.page));
        }
        lookup
    }
}

/// The lazily built [`Lookup`] of a set. It is derived from the annotations, so it takes no
/// part in comparing two sets and a copy of a set builds its own.
#[derive(Debug, Default)]
struct Index {
    built: OnceLock<Lookup>,
    /// The position and page of the annotation last handed out for editing: its id or page
    /// may have been changed since, which the index would not know.
    handed: Option<(usize, usize)>,
}

impl Clone for Index {
    fn clone(&self) -> Index {
        Index::default()
    }
}

impl PartialEq for Index {
    fn eq(&self, _: &Index) -> bool {
        true
    }
}

impl AnnotSet {
    /// An empty set (for a document with no annotations loaded).
    pub fn new() -> AnnotSet {
        AnnotSet::default()
    }

    /// Reads every annotation of every page. Never fails: malformed dictionaries are read as
    /// far as they can be, and `/Annots` entries that are not dictionaries are skipped.
    pub fn load(doc: &PdfDocument) -> AnnotSet {
        AnnotSet {
            items: read_pages(doc, 0..doc.page_count()),
            removed: Vec::new(),
            scope: None,
            index: Index::default(),
        }
    }

    /// Reads the annotations of some pages only. Links to annotations on pages that were not
    /// read (a reply to a markup on another page, say) are left unresolved and untouched.
    pub fn load_pages(doc: &PdfDocument, pages: &[usize]) -> AnnotSet {
        let scope: BTreeSet<usize> = pages
            .iter()
            .copied()
            .filter(|p| *p < doc.page_count())
            .collect();
        AnnotSet {
            items: read_pages(doc, scope.iter().copied()),
            removed: Vec::new(),
            scope: Some(scope),
            index: Index::default(),
        }
    }

    /// Reads the set again from `doc`, the document as saved. Call it after a successful save
    /// of [`AnnotSet::plan_save`]'s edit set: every annotation keeps its id and order, becomes
    /// clean, and holds the saved dictionary as its baseline.
    pub fn rebase(&mut self, doc: &PdfDocument) {
        *self = match &self.scope {
            None => AnnotSet::load(doc),
            Some(scope) => {
                let pages: Vec<usize> = scope.iter().copied().collect();
                AnnotSet::load_pages(doc, &pages)
            }
        };
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// All annotations, in stacking order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Annotation> + '_ {
        self.items.iter()
    }

    /// The annotations of one page, bottom of the stack first.
    pub fn on_page(&self, page: usize) -> impl DoubleEndedIterator<Item = &Annotation> + '_ {
        self.items.iter().filter(move |a| a.page == page)
    }

    /// The position index, when it can be trusted: built on first use, and set aside while an
    /// annotation handed out by [`AnnotSet::get_mut`] no longer has the id and page it had.
    fn lookup(&self) -> Option<&Lookup> {
        let lookup = self.index.built.get_or_init(|| Lookup::of(&self.items));
        match self.index.handed {
            None => Some(lookup),
            Some((at, page)) => {
                let a = self.items.get(at)?;
                (a.page == page && lookup.by_id.get(&a.id) == Some(&at)).then_some(lookup)
            }
        }
    }

    /// Called by everything that changes the set: an index that an edit through
    /// [`AnnotSet::get_mut`] made wrong is thrown away (and built again when next needed).
    fn settle(&mut self) {
        if self.lookup().is_none() {
            self.index = Index::default();
        }
        self.index.handed = None;
    }

    fn forget_index(&mut self) {
        self.index = Index::default();
    }

    pub fn get(&self, id: &AnnotId) -> Option<&Annotation> {
        self.position(id).map(|at| &self.items[at])
    }

    /// The annotation to edit. Changing its `id` to one already in the set makes
    /// [`AnnotSet::plan_save`] fail with a duplicate-id error.
    pub fn get_mut(&mut self, id: &AnnotId) -> Option<&mut Annotation> {
        self.settle();
        let at = self.position(id)?;
        self.index.handed = Some((at, self.items[at].page));
        Some(&mut self.items[at])
    }

    pub fn contains(&self, id: &AnnotId) -> bool {
        self.position(id).is_some()
    }

    /// Position of an annotation in the set. A lookup by id does not grow with the size of the
    /// set.
    pub fn position(&self, id: &AnnotId) -> Option<usize> {
        match self.lookup() {
            Some(lookup) => lookup.by_id.get(id).copied(),
            None => self.items.iter().position(|a| a.id == *id),
        }
    }

    /// Where a new annotation of `page` goes so that the set stays in page order: the position
    /// of the first annotation of a later page, the length of the set when there is none.
    pub fn top_of_page(&self, page: usize) -> usize {
        if let Some(lookup) = self.lookup() {
            if lookup.max_page.is_none_or(|max| max <= page) {
                return self.items.len();
            }
        }
        self.items
            .iter()
            .position(|a| a.page > page)
            .unwrap_or(self.items.len())
    }

    /// Adds an annotation on top of its page's stack.
    pub fn add(&mut self, annotation: Annotation) -> Result<AnnotId, AnnotError> {
        let at = self.items.len();
        self.insert(at, annotation)
    }

    /// Adds an annotation at a position in the set (clamped to its length).
    pub fn insert(&mut self, index: usize, annotation: Annotation) -> Result<AnnotId, AnnotError> {
        self.settle();
        if self.contains(&annotation.id) {
            return Err(AnnotError::DuplicateId(annotation.id));
        }
        let id = annotation.id.clone();
        // Putting back something that was removed: it is no longer pending deletion.
        if !self.removed.is_empty() {
            self.removed.retain(|r| r.id != id);
        }
        if index >= self.items.len() {
            // On top of everything: the index is kept up, so a run of additions stays cheap.
            if let Some(lookup) = self.index.built.get_mut() {
                lookup.by_id.insert(id.clone(), self.items.len());
                lookup.max_page = lookup.max_page.max(Some(annotation.page));
            }
            self.items.push(annotation);
        } else {
            self.items.insert(index, annotation);
            self.forget_index();
        }
        Ok(id)
    }

    /// Removes an annotation together with what cannot outlive it: its popup window, and
    /// every reply and review-state annotation that hangs off it (`/IRT`), recursively.
    /// Removing a popup alone clears its parent's `popup` link.
    ///
    /// Returns what was removed, empty if there is no such annotation.
    pub fn remove(&mut self, id: &AnnotId) -> Removed {
        self.remove_many(std::slice::from_ref(id))
    }

    /// As [`AnnotSet::remove`] for several annotations at once, in one pass over the set
    /// however many there are. Ids that are not in the set are ignored.
    pub fn remove_many(&mut self, ids: &[AnnotId]) -> Removed {
        self.settle();
        // What hangs off what: replies and status annotations off what they answer, a popup
        // off its markup.
        let mut dependants: HashMap<&AnnotId, Vec<&AnnotId>> = HashMap::new();
        for other in &self.items {
            if let Some(answered) = &other.meta.reply_to {
                dependants.entry(answered).or_default().push(&other.id);
            }
            if other.kind == AnnotKind::Popup {
                if let Some(parent) = &other.parent {
                    if other.meta.reply_to.as_ref() != Some(parent) {
                        dependants.entry(parent).or_default().push(&other.id);
                    }
                }
            }
        }
        let mut doomed: HashSet<AnnotId> = HashSet::new();
        let mut queue: Vec<&AnnotId> = ids.iter().collect();
        while let Some(next) = queue.pop() {
            let Some(a) = self.get(next) else { continue };
            if !doomed.insert(next.clone()) {
                continue;
            }
            if let Some(popup) = &a.popup {
                queue.push(popup);
            }
            if let Some(more) = dependants.get(next) {
                queue.extend(more.iter().copied());
            }
        }
        if doomed.is_empty() {
            return Removed {
                items: Vec::new(),
                unlinked_parent: None,
            };
        }
        let mut unlinked_parent = None;
        if doomed.len() == 1 {
            let only = doomed.iter().next().expect("one");
            if let Some(parent) = self
                .items
                .iter_mut()
                .find(|a| a.popup.as_ref() == Some(only) && !doomed.contains(&a.id))
            {
                parent.popup = None;
                unlinked_parent = Some(parent.id.clone());
            }
        } else {
            // A popup removed together with other things may still leave a parent behind
            // (a reply's popup): clear those links too, without recording them for undo, as
            // the parent is itself removed in that case.
            for a in &mut self.items {
                if a.popup.as_ref().is_some_and(|p| doomed.contains(p)) && !doomed.contains(&a.id) {
                    a.popup = None;
                    unlinked_parent = Some(a.id.clone());
                }
            }
        }
        let mut items = Vec::new();
        let mut kept = Vec::with_capacity(self.items.len());
        for (index, a) in self.items.drain(..).enumerate() {
            if doomed.contains(&a.id) {
                items.push((index, a));
            } else {
                kept.push(a);
            }
        }
        self.items = kept;
        self.forget_index();
        for (_, a) in &items {
            if a.baseline.is_some() {
                self.removed.push(a.clone());
            }
        }
        Removed {
            items,
            unlinked_parent,
        }
    }

    /// Puts back what [`AnnotSet::remove`] took out, at the same positions (the undo of a
    /// delete). Annotations whose id is in the set again are skipped.
    pub fn restore(&mut self, removed: Removed) {
        self.settle();
        let Removed {
            items,
            unlinked_parent,
        } = removed;
        let popup_of_parent = unlinked_parent.as_ref().and_then(|parent| {
            items
                .iter()
                .find(|(_, a)| a.kind == AnnotKind::Popup && a.parent.as_ref() == Some(parent))
                .map(|(_, a)| a.id.clone())
        });
        let ascending = items.as_slice().is_sorted_by_key(|(index, _)| *index);
        if items.len() < 2 || !ascending {
            for (index, annotation) in items {
                let _ = self.insert(index, annotation);
            }
        } else {
            // Many at once: one merge instead of an insertion each. It gives what inserting
            // them one after the other in ascending order gives.
            let mut coming: HashSet<AnnotId> = HashSet::with_capacity(items.len());
            let items: Vec<(usize, Annotation)> = items
                .into_iter()
                .filter(|(_, a)| !self.contains(&a.id) && coming.insert(a.id.clone()))
                .collect();
            if !self.removed.is_empty() {
                self.removed.retain(|r| !coming.contains(&r.id));
            }
            let old = std::mem::take(&mut self.items);
            let mut merged = Vec::with_capacity(old.len() + items.len());
            let mut old = old.into_iter();
            for (index, annotation) in items {
                while merged.len() < index {
                    match old.next() {
                        Some(a) => merged.push(a),
                        None => break,
                    }
                }
                merged.push(annotation);
            }
            merged.extend(old);
            self.items = merged;
            self.forget_index();
        }
        if let (Some(parent), Some(popup)) = (unlinked_parent, popup_of_parent) {
            if let Some(a) = self.get_mut(&parent) {
                a.popup = Some(popup);
            }
        }
    }

    /// Annotations read from the file that have been removed and not put back; a save deletes
    /// them.
    pub fn removed(&self) -> &[Annotation] {
        &self.removed
    }

    /// True when a save would write something.
    pub fn is_dirty(&self) -> bool {
        if !self.removed.is_empty() || self.items.iter().any(Annotation::is_dirty) {
            return true;
        }
        // Stacking order: within each page, file positions must still ascend.
        let mut last: Option<(usize, usize)> = None;
        for a in &self.items {
            let Some(base) = &a.baseline else { continue };
            let here = (a.page, base.slot);
            if last.is_some_and(|l| l.0 == here.0 && l.1 > here.1) {
                return true;
            }
            last = Some(here);
        }
        false
    }

    /// Restacks one page: `order` lists that page's annotations from bottom to top. Ids that
    /// are not on the page are ignored, and annotations of the page that `order` leaves out
    /// keep their relative order beneath the listed ones.
    pub fn set_order(&mut self, page: usize, order: &[AnnotId]) {
        self.settle();
        self.forget_index();
        let rank = |a: &Annotation| order.iter().position(|id| *id == a.id);
        let mut on_page: Vec<Annotation> = Vec::new();
        let mut slots: Vec<usize> = Vec::new();
        for (i, a) in self.items.iter().enumerate() {
            if a.page == page {
                slots.push(i);
                on_page.push(a.clone());
            }
        }
        // Stable: unlisted (None) first in their old order, then the listed ones by rank.
        on_page.sort_by_key(|a| rank(a).map_or(0, |r| r + 1));
        for (slot, a) in slots.into_iter().zip(on_page) {
            self.items[slot] = a;
        }
    }

    /// Moves an annotation to the top of its page's stack.
    pub fn bring_to_front(&mut self, id: &AnnotId) {
        if let Some(page) = self.get(id).map(|a| a.page) {
            self.set_order(page, std::slice::from_ref(id));
        }
    }

    /// Moves an annotation to the bottom of its page's stack.
    pub fn send_to_back(&mut self, id: &AnnotId) {
        let Some(page) = self.get(id).map(|a| a.page) else {
            return;
        };
        let mut order = vec![id.clone()];
        order.extend(
            self.on_page(page)
                .filter(|a| a.id != *id)
                .map(|a| a.id.clone()),
        );
        self.set_order(page, &order);
    }

    /// The annotations of `page` under `point`, topmost first. Popups and annotations hidden
    /// by their flags are never hit. `tolerance` is in page units.
    pub fn hits(&self, page: usize, point: Point, tolerance: f64) -> Vec<&Annotation> {
        self.on_page(page)
            .rev()
            .filter(|a| a.kind != AnnotKind::Popup && !a.meta.flags.is_hidden())
            .filter(|a| a.hit_test(point, tolerance))
            .collect()
    }

    /// The topmost annotation of `page` under `point`.
    pub fn hit_test(&self, page: usize, point: Point, tolerance: f64) -> Option<&Annotation> {
        self.on_page(page)
            .rev()
            .filter(|a| a.kind != AnnotKind::Popup && !a.meta.flags.is_hidden())
            .find(|a| a.hit_test(point, tolerance))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Origin;
    use crate::objects::PdfDate;
    use pl_draw::Rgb;
    use pl_geom::Rect;

    fn origin() -> Origin {
        Origin::new("Tester", PdfDate::from_unix(1_700_000_000))
    }

    fn square(page: usize, x: f64) -> Annotation {
        let mut a = Annotation::square(page, Rect::new(x, 0.0, x + 50.0, 50.0), &origin());
        a.style.fill = Some(Rgb::BLUE);
        a
    }

    #[test]
    fn add_get_and_duplicate_ids() {
        let mut set = AnnotSet::new();
        let a = square(0, 0.0);
        let id = set.add(a.clone()).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set.get(&id).unwrap().geometry, a.geometry);
        assert!(matches!(set.add(a), Err(AnnotError::DuplicateId(d)) if d == id));
        assert!(set.is_dirty());
        assert!(!AnnotSet::new().is_dirty());
    }

    #[test]
    fn the_topmost_annotation_is_hit_first() {
        let mut set = AnnotSet::new();
        let bottom = set.add(square(0, 0.0)).unwrap();
        let top = set.add(square(0, 25.0)).unwrap();
        let other_page = set.add(square(1, 25.0)).unwrap();
        let p = Point::new(40.0, 25.0);
        assert_eq!(set.hit_test(0, p, 1.0).unwrap().id, top);
        let ids: Vec<&AnnotId> = set.hits(0, p, 1.0).iter().map(|a| &a.id).collect();
        assert_eq!(ids, [&top, &bottom]);
        assert_eq!(set.hit_test(1, p, 1.0).unwrap().id, other_page);
        assert!(set.hit_test(0, Point::new(200.0, 200.0), 1.0).is_none());

        set.send_to_back(&top);
        assert_eq!(set.hit_test(0, p, 1.0).unwrap().id, bottom);
        set.bring_to_front(&top);
        assert_eq!(set.hit_test(0, p, 1.0).unwrap().id, top);

        // Hidden annotations and popups are passed over.
        let hidden = crate::model::Flags(crate::model::Flags::PRINT | crate::model::Flags::HIDDEN);
        set.get_mut(&top).unwrap().meta.flags = hidden;
        assert_eq!(set.hit_test(0, p, 1.0).unwrap().id, bottom);
    }

    #[test]
    fn set_order_restacks_one_page_only() {
        let mut set = AnnotSet::new();
        let a = set.add(square(0, 0.0)).unwrap();
        let x = set.add(square(1, 0.0)).unwrap();
        let b = set.add(square(0, 10.0)).unwrap();
        let c = set.add(square(0, 20.0)).unwrap();
        set.set_order(0, &[c.clone(), a.clone()]);
        let order: Vec<AnnotId> = set.on_page(0).map(|a| a.id.clone()).collect();
        assert_eq!(order, [b, c, a]);
        assert_eq!(
            set.position(&x),
            Some(1),
            "the other page's annotation did not move"
        );
    }

    #[test]
    fn removing_a_markup_takes_its_popup_and_replies_and_restore_undoes_it() {
        let mut set = AnnotSet::new();
        let mut note = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "n", &origin());
        let popup = Annotation::popup_for(&mut note, Rect::new(30.0, 0.0, 130.0, 60.0), &origin());
        let mut reply = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "r", &origin());
        reply.meta.reply_to = Some(note.id.clone());
        let mut reply2 = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "rr", &origin());
        reply2.meta.reply_to = Some(reply.id.clone());
        let bystander = set.add(square(0, 100.0)).unwrap();
        let note_id = set.add(note).unwrap();
        let popup_id = set.add(popup).unwrap();
        set.add(reply).unwrap();
        set.add(reply2).unwrap();
        let before = set.clone();

        let removed = set.remove(&note_id);
        assert_eq!(removed.items.len(), 4);
        assert_eq!(set.len(), 1);
        assert!(set.contains(&bystander));
        assert!(set.removed().is_empty(), "nothing removed was in a file");
        set.restore(removed);
        assert_eq!(set, before);

        // Removing only the popup unlinks it from its parent; restore links it again.
        let removed = set.remove(&popup_id);
        assert_eq!(removed.items.len(), 1);
        assert_eq!(removed.unlinked_parent.as_ref(), Some(&note_id));
        assert_eq!(set.get(&note_id).unwrap().popup, None);
        set.restore(removed);
        assert_eq!(set, before);

        assert!(set.remove(&AnnotId::from("nope")).is_empty());
    }

    /// A plain walk of the set: what every lookup has to agree with.
    fn check_lookups(set: &AnnotSet) {
        let ids: Vec<AnnotId> = set.iter().map(|a| a.id.clone()).collect();
        for (at, id) in ids.iter().enumerate() {
            let first = ids.iter().position(|other| other == id).unwrap();
            assert_eq!(set.position(id), Some(first), "{id} at {at}");
            assert_eq!(set.get(id).map(|a| &a.id), Some(id));
            assert!(set.contains(id));
        }
        for page in 0..6 {
            let want = set.iter().position(|a| a.page > page).unwrap_or(set.len());
            assert_eq!(set.top_of_page(page), want, "page {page}");
        }
        assert_eq!(set.position(&AnnotId::from("absent")), None);
    }

    #[test]
    fn lookups_by_id_follow_every_kind_of_change() {
        let mut set = AnnotSet::new();
        let mut ids = Vec::new();
        for i in 0..40 {
            ids.push(set.add(square(i % 4, i as f64)).unwrap());
            check_lookups(&set);
        }
        // In the middle, restacked, removed and put back.
        set.insert(3, square(0, 500.0)).unwrap();
        check_lookups(&set);
        set.set_order(1, &[ids[5].clone(), ids[1].clone()]);
        check_lookups(&set);
        let removed = set.remove_many(&[ids[2].clone(), ids[30].clone(), ids[7].clone()]);
        assert_eq!(removed.items.len(), 3);
        assert!(!set.contains(&ids[30]));
        check_lookups(&set);
        let before = set.clone();
        set.restore(removed.clone());
        check_lookups(&set);
        // The merge gives what one insertion after the other gives.
        let mut one_by_one = before.clone();
        for (index, a) in removed.items {
            one_by_one.insert(index, a).unwrap();
        }
        assert_eq!(set, one_by_one);

        // An id or a page changed through `get_mut` is found under its new value at once.
        let renamed = AnnotId::from("renamed");
        set.get_mut(&ids[10]).unwrap().id = renamed.clone();
        assert!(set.contains(&renamed) && !set.contains(&ids[10]));
        check_lookups(&set);
        set.get_mut(&renamed).unwrap().page = 5;
        assert_eq!(set.top_of_page(4), set.position(&renamed).unwrap());
        check_lookups(&set);
        assert!(set.add(square(0, 0.0)).is_ok());
        check_lookups(&set);
        // A copy answers for itself.
        let mut copy = set.clone();
        copy.remove(&renamed);
        assert!(set.contains(&renamed) && !copy.contains(&renamed));
        check_lookups(&copy);
    }

    #[test]
    fn restoring_many_skips_what_is_back_and_clamps_past_the_end() {
        let mut set = AnnotSet::new();
        let a = set.add(square(0, 0.0)).unwrap();
        let extra = [square(0, 10.0), square(0, 20.0), square(0, 30.0)];
        let again = set.get(&a).unwrap().clone();
        let removed = Removed {
            items: vec![
                (0, extra[0].clone()),
                (0, again),
                (1, extra[1].clone()),
                (99, extra[2].clone()),
            ],
            unlinked_parent: None,
        };
        set.restore(removed);
        let order: Vec<AnnotId> = set.iter().map(|x| x.id.clone()).collect();
        assert_eq!(
            order,
            [
                extra[0].id.clone(),
                extra[1].id.clone(),
                a,
                extra[2].id.clone()
            ]
        );
        check_lookups(&set);
    }

    #[test]
    fn a_lookup_in_a_large_set_does_not_walk_it() {
        let mut set = AnnotSet::new();
        let mut ids = Vec::new();
        for i in 0..20_000 {
            ids.push(set.add(square(0, i as f64)).unwrap());
        }
        // 20,000 additions and 2,000,000 lookups: minutes if either walked the set.
        let start = std::time::Instant::now();
        let mut found = 0usize;
        for _ in 0..100 {
            for id in &ids {
                found += usize::from(set.contains(id));
            }
        }
        assert_eq!(found, 2_000_000);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            start.elapsed()
        );
    }
}
