//! Page operations: insert (blank or from another PDF), delete, reorder, rotate, replace,
//! extract, merge and split, as pure operations on a parsed document.
//!
//! [`PdfDocument::restructure`] starts a [`Restructure`]: a private copy of the object graph
//! that the operations change in turn, each returning a [`PageOpReport`]. [`Restructure::finish`]
//! gives the new [`PdfDocument`], which is saved with
//! [`PdfFile::save_restructured`](crate::PdfFile::save_restructured) (a full rewrite, verified
//! and atomically replaced; unused objects are pruned there).
//!
//! What every operation keeps consistent:
//!
//! - **Page tree.** It is rebuilt flat under the existing root `/Pages` node. Inherited
//!   `/Resources`, `/MediaBox`, `/CropBox` and `/Rotate` are first made each page's own, so no
//!   page changes by losing an ancestor (and no inserted page picks up the target's).
//! - **Annotations** keep their `/Rect` in unrotated user space, so rotating a page changes only
//!   `/Rotate` and they stay where they are on the page. `/P` points at the page that holds them.
//! - **Deleted pages.** An outline item that leads to a deleted page is removed when it has no
//!   children, or keeps its children and loses its destination; a link annotation going there
//!   is removed; a named destination there is removed (with the items and links using it); an
//!   `/OpenAction` there is removed; widgets on deleted pages leave the form's field tree (a
//!   field left with no widget goes too). Any other reference to a deleted page, or to an
//!   annotation that went with it (a `/Pg` in the structure tree, a reply's `/IRT`), becomes
//!   `null`. Each of these is counted or named in the report.
//! - **Replaced pages**: every reference to the old page is repointed at its replacement.
//! - **Pages from another file**: deep-copied with their resources and annotations (popups and
//!   `/IRT` reply chains among them included); their links to pages that come along are
//!   repointed, named destinations they use are written out as explicit ones, and links to
//!   pages left behind are removed. Form fields are merged into the target's `/AcroForm`; a
//!   top-level field name already in use gets `_2`, `_3`... (first free), reported.
//!   Optional-content groups the copied pages use are added to the target's `/OCProperties`
//!   with their default state. Bluebeam `/BSIAnnotColumns` columns the target lacks are appended
//!   (an indirect array, as Revu needs), and copied markups' `/BSIColumnData` is remapped to the
//!   target's column positions by column name. The target's own catalog `/PlumblineData` is
//!   kept; a target without one takes the source's. Page-level data (`/VP` viewports, private
//!   keys) travels with the page.
//! - **Page labels** stay attached to their pages: each keeps the label it showed, and the
//!   ranges are worked out again; a page that brings none is labelled with its new page number.
//! - **Signatures**: every structural edit needs a full rewrite, which invalidates every
//!   signature. Each report lists them in `signatures_invalidated`, and the save refuses unless
//!   told to go ahead.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use lopdf::{decode_text_string, text_string, Dictionary, Document, Object, ObjectId, Stream};
use pl_geom::Rect;

use crate::copy::{inherited_raw, page_annotation_ids, Copier};
use crate::dest::{
    deref, lookup_named, parse_dest_value, raw_named_dests, real, resolve_raw, walk_name_tree,
    DestValue, Destination, RawDest, RawPage,
};
use crate::document::{annots_array, number, rect_from_object, PdfDocument, INHERITABLE_PAGE_KEYS};
use crate::error::{PdfError, Result};
use crate::labels::{self, PageLabel};
use crate::outline::{self, Outline, OutlineItem, OutlineTarget, ReadContext};
use crate::signatures::detect as detect_signatures;

/// A link that an operation removed.
#[derive(Debug, Clone, PartialEq)]
pub struct RemovedLink {
    /// Index of the page it was on, in the document as the operation left it.
    pub page_index: usize,
    pub rect: Rect,
    /// Why: `"target page deleted"`, `"target page not copied"`, `"named destination missing"`.
    pub reason: &'static str,
}

/// What one page operation did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageOpReport {
    /// Pages the operation inserted, moved, rotated or replaced: their indices afterwards.
    pub pages_affected: Vec<usize>,
    /// Indices (before the operation) of the pages it removed.
    pub pages_removed: Vec<usize>,
    /// Titles of outline items removed because they led to a removed page.
    pub outline_items_removed: Vec<String>,
    /// Titles of outline items that kept their children but lost a destination on a removed page.
    pub outline_items_cleared: Vec<String>,
    /// Outline items added (a merge appends the other file's outline).
    pub outline_items_added: usize,
    pub links_removed: Vec<RemovedLink>,
    /// Links (and outline items) of copied pages whose named destination was written out as an
    /// explicit one.
    pub named_dests_inlined: usize,
    /// Named destinations removed because their page was removed.
    pub named_dests_removed: Vec<String>,
    /// The document's `/OpenAction` led to a removed page and was removed.
    pub open_action_removed: bool,
    /// References to replaced pages that now point at their replacements.
    pub refs_repointed: usize,
    /// Other references to removed pages or their annotations that were set to `null`.
    pub refs_dropped: usize,
    /// Top-level form fields renamed on the way in, as `(old, new)`.
    pub fields_renamed: Vec<(String, String)>,
    /// Form fields removed because every widget of theirs was on a removed page.
    pub fields_removed: Vec<String>,
    /// Signatures the rewrite will invalidate (see [`crate::SignatureInfo`]); copied signed
    /// fields are listed as `copied: <name>`.
    pub signatures_invalidated: Vec<String>,
    /// Optional-content groups added to the target's `/OCProperties`.
    pub layers_merged: usize,
    /// Bluebeam custom columns appended to the target's `/BSIAnnotColumns`.
    pub bluebeam_columns_added: Vec<String>,
    /// Markups carried over by a replace that keeps them.
    pub markups_kept: usize,
    /// Markups (with their popups and replies) of replaced pages that went with them: a
    /// replace that does not keep them.
    pub markups_dropped: usize,
    /// True when `/PageLabels` was written again.
    pub labels_rewritten: bool,
    /// Anything else worth telling the user.
    pub notes: Vec<String>,
}

impl PageOpReport {
    /// Add another report's findings to this one (for an operation made of several steps).
    pub fn absorb(&mut self, other: PageOpReport) {
        self.pages_affected.extend(other.pages_affected);
        self.pages_removed.extend(other.pages_removed);
        self.outline_items_removed
            .extend(other.outline_items_removed);
        self.outline_items_cleared
            .extend(other.outline_items_cleared);
        self.outline_items_added += other.outline_items_added;
        self.links_removed.extend(other.links_removed);
        self.named_dests_inlined += other.named_dests_inlined;
        self.named_dests_removed.extend(other.named_dests_removed);
        self.open_action_removed |= other.open_action_removed;
        self.refs_repointed += other.refs_repointed;
        self.refs_dropped += other.refs_dropped;
        self.fields_renamed.extend(other.fields_renamed);
        self.fields_removed.extend(other.fields_removed);
        for s in other.signatures_invalidated {
            if !self.signatures_invalidated.contains(&s) {
                self.signatures_invalidated.push(s);
            }
        }
        self.layers_merged += other.layers_merged;
        self.bluebeam_columns_added
            .extend(other.bluebeam_columns_added);
        self.markups_kept += other.markups_kept;
        self.markups_dropped += other.markups_dropped;
        self.labels_rewritten |= other.labels_rewritten;
        self.notes.extend(other.notes);
    }
}

/// The size of a new blank page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlankPage {
    /// The boxes, rotation and user unit of the page now at the insert position (or the last
    /// page, when inserting at the end).
    LikeNeighbour,
    /// A media box (user space units) and a clockwise rotation (a multiple of 90).
    Size { media_box: Rect, rotate: i32 },
}

/// How [`Restructure::replace`] treats the markups of the pages it replaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplaceOptions {
    /// Move the replaced pages' markups (every annotation but widgets and links) onto their
    /// replacements, at the same coordinates.
    pub keep_markups: bool,
}

/// A document being restructured. See the module documentation.
#[derive(Debug, Clone)]
pub struct Restructure {
    doc: Document,
    pages: Vec<ObjectId>,
    root: ObjectId,
    /// One label per page; `None` for a page that brought none.
    labels: Vec<Option<PageLabel>>,
    /// Some page has a label, so `/PageLabels` is written.
    has_labels: bool,
    bytes: Vec<u8>,
    signatures: Vec<String>,
    /// [`PdfDocument::state`] of the document this started from: a source with the same
    /// state is this document, and its objects are the target's own.
    source_state: u64,
}

fn invalid(msg: impl Into<String>) -> PdfError {
    PdfError::InvalidOperation(msg.into())
}

fn name(n: &str) -> Object {
    Object::Name(n.as_bytes().to_vec())
}

impl PdfDocument {
    /// Start restructuring a copy of this document.
    pub fn restructure(&self) -> Result<Restructure> {
        Restructure::new(self)
    }

    /// A new document with only `pages` (zero-based, in the order given, no repeats).
    pub fn extract_pages(&self, pages: &[usize]) -> Result<(PdfDocument, PageOpReport)> {
        let mut r = self.restructure()?;
        let report = r.extract(pages)?;
        Ok((r.finish()?, report))
    }

    /// One new document per range of pages.
    pub fn split(&self, ranges: &[Range<usize>]) -> Result<Vec<(PdfDocument, PageOpReport)>> {
        ranges
            .iter()
            .map(|range| {
                let pages: Vec<usize> = range.clone().collect();
                self.extract_pages(&pages)
            })
            .collect()
    }

    /// This document followed by every page of each of `others`, with their outlines appended.
    pub fn merge(&self, others: &[&PdfDocument]) -> Result<(PdfDocument, PageOpReport)> {
        let mut r = self.restructure()?;
        let mut report = PageOpReport::default();
        for other in others {
            let at = r.page_count();
            report.absorb(r.insert_document(at, other, true)?);
        }
        Ok((r.finish()?, report))
    }
}

impl Restructure {
    fn new(source: &PdfDocument) -> Result<Restructure> {
        let mut doc = source.doc.clone();
        let pages = source.pages.clone();
        let root = match doc.catalog().map_err(PdfError::Parse)?.get(b"Pages") {
            Ok(Object::Reference(id)) => *id,
            Ok(Object::Dictionary(d)) => {
                let d = d.clone();
                let id = doc.add_object(d);
                doc.catalog_mut()
                    .map_err(PdfError::Parse)?
                    .set("Pages", Object::Reference(id));
                id
            }
            _ => return Err(PdfError::NoPages),
        };
        // Pages that are their own root (a one-page file whose /Pages is the page) need a node.
        let root = if pages.contains(&root) {
            let id = doc.add_object(Dictionary::new());
            doc.catalog_mut()
                .map_err(PdfError::Parse)?
                .set("Pages", Object::Reference(id));
            id
        } else {
            root
        };
        for page in &pages {
            materialise(&mut doc, *page);
        }
        let (labels, has_labels) = match labels::read_ranges(&doc) {
            Some(ranges) => (
                labels::per_page(&ranges, pages.len())
                    .into_iter()
                    .map(Some)
                    .collect(),
                true,
            ),
            None => (vec![None; pages.len()], false),
        };
        let signatures = detect_signatures(&doc).descriptions();
        let mut r = Restructure {
            doc,
            pages,
            root,
            labels,
            has_labels,
            bytes: source.bytes.clone(),
            signatures,
            source_state: source.state,
        };
        r.rebuild_tree();
        Ok(r)
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// The page objects in their current order.
    pub fn page_ids(&self) -> &[ObjectId] {
        &self.pages
    }

    /// The document as it stands, without ending the restructure.
    pub fn snapshot(&self) -> Result<PdfDocument> {
        self.clone().finish()
    }

    /// The new document.
    pub fn finish(self) -> Result<PdfDocument> {
        let document = PdfDocument::derived(self.bytes, self.doc)?;
        if document.pages != self.pages {
            return Err(invalid(
                "the rebuilt page tree does not list the expected pages",
            ));
        }
        Ok(document)
    }

    fn report(&self) -> PageOpReport {
        PageOpReport {
            signatures_invalidated: self.signatures.clone(),
            ..PageOpReport::default()
        }
    }

    fn check_index(&self, index: usize) -> Result<()> {
        if index >= self.pages.len() {
            return Err(PdfError::PageOutOfRange {
                index,
                count: self.pages.len(),
            });
        }
        Ok(())
    }

    fn unique_sorted(&self, pages: &[usize]) -> Result<Vec<usize>> {
        let set: BTreeSet<usize> = pages.iter().copied().collect();
        if set.len() != pages.len() {
            return Err(invalid("a page is listed twice"));
        }
        for p in &set {
            self.check_index(*p)?;
        }
        Ok(set.into_iter().collect())
    }

    /// Rebuild the page tree flat under the root node, and write the labels.
    fn rebuild_tree(&mut self) {
        let kids: Vec<Object> = self.pages.iter().map(|p| Object::Reference(*p)).collect();
        let mut root = match self.doc.objects.get(&self.root) {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        };
        for key in INHERITABLE_PAGE_KEYS {
            root.remove(key);
        }
        root.remove(b"Parent");
        root.set("Type", name("Pages"));
        root.set("Count", Object::Integer(self.pages.len() as i64));
        root.set("Kids", Object::Array(kids));
        self.doc.objects.insert(self.root, Object::Dictionary(root));
        for page in &self.pages {
            if let Ok(d) = self.doc.get_dictionary_mut(*page) {
                d.set("Parent", Object::Reference(self.root));
            }
        }
    }

    fn store_labels(&mut self, report: &mut PageOpReport) -> Result<()> {
        if !self.has_labels {
            return Ok(());
        }
        let labels: Vec<PageLabel> = self
            .labels
            .iter()
            .enumerate()
            .map(|(i, l)| {
                l.clone()
                    .unwrap_or_else(|| PageLabel::decimal(i as i64 + 1))
            })
            .collect();
        labels::store(&mut self.doc, Some(&labels::compact(&labels)))?;
        report.labels_rewritten = true;
        Ok(())
    }

    /// Insert a blank page at `at` (0..=page count).
    pub fn insert_blank(&mut self, at: usize, size: BlankPage) -> Result<PageOpReport> {
        if at > self.pages.len() {
            return Err(PdfError::PageOutOfRange {
                index: at,
                count: self.pages.len(),
            });
        }
        let mut page = Dictionary::new();
        page.set("Type", name("Page"));
        page.set("Parent", Object::Reference(self.root));
        match size {
            BlankPage::LikeNeighbour => {
                let neighbour = self.pages[at.min(self.pages.len() - 1)];
                let nd = self
                    .doc
                    .get_dictionary(neighbour)
                    .cloned()
                    .unwrap_or_default();
                for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"UserUnit"] {
                    if let Some(v) = nd.get(key).ok().and_then(|o| deref(&self.doc, o)) {
                        page.set(key.to_vec(), v.clone());
                    }
                }
                if !page.has(b"MediaBox") {
                    page.set("MediaBox", crate::document::fallback_media_box());
                }
            }
            BlankPage::Size { media_box, rotate } => {
                if rotate.rem_euclid(90) != 0 {
                    return Err(invalid("a page rotation must be a multiple of 90 degrees"));
                }
                if !(media_box.width() > 0.0 && media_box.height() > 0.0) {
                    return Err(invalid("a blank page needs a positive size"));
                }
                page.set(
                    "MediaBox",
                    Object::Array(
                        [media_box.x0, media_box.y0, media_box.x1, media_box.y1]
                            .iter()
                            .map(|v| real(*v))
                            .collect(),
                    ),
                );
                let rotate = rotate.rem_euclid(360);
                if rotate != 0 {
                    page.set("Rotate", Object::Integer(i64::from(rotate)));
                }
            }
        }
        page.set("Resources", Object::Dictionary(Dictionary::new()));
        let contents = self
            .doc
            .add_object(Stream::new(Dictionary::new(), Vec::new()));
        page.set("Contents", Object::Reference(contents));
        let id = self.doc.add_object(page);
        self.pages.insert(at, id);
        self.labels.insert(at, None);
        self.rebuild_tree();
        let mut report = self.report();
        report.pages_affected.push(at);
        self.store_labels(&mut report)?;
        Ok(report)
    }

    /// Delete the pages at `pages` (zero-based). At least one page must remain.
    pub fn delete(&mut self, pages: &[usize]) -> Result<PageOpReport> {
        let sorted = self.unique_sorted(pages)?;
        if sorted.len() == self.pages.len() {
            return Err(invalid("a document must keep at least one page"));
        }
        let removed: BTreeSet<ObjectId> = sorted.iter().map(|i| self.pages[*i]).collect();
        let mut report = self.report();
        // Links, outlines and names are judged against the page list with the pages still in
        // it, so that what refers to them is found; then the pages go.
        self.sweep_before_removal(&removed, &mut report)?;
        for i in sorted.iter().rev() {
            self.pages.remove(*i);
            self.labels.remove(*i);
        }
        self.rebuild_tree();
        self.sweep_after_removal(&removed, &mut report)?;
        report.pages_removed = sorted;
        self.store_labels(&mut report)?;
        Ok(report)
    }

    /// Put the pages in a new order: page `i` of the result is the current page `order[i]`.
    /// `order` must be a permutation of `0..page_count`.
    pub fn reorder(&mut self, order: &[usize]) -> Result<PageOpReport> {
        if order.len() != self.pages.len() {
            return Err(invalid("a new page order must list every page once"));
        }
        let sorted = self.unique_sorted(order)?;
        debug_assert_eq!(sorted.len(), self.pages.len());
        let pages: Vec<ObjectId> = order.iter().map(|i| self.pages[*i]).collect();
        let labels = order.iter().map(|i| self.labels[*i].clone()).collect();
        let mut report = self.report();
        report.pages_affected = (0..order.len()).filter(|i| order[*i] != *i).collect();
        self.pages = pages;
        self.labels = labels;
        self.rebuild_tree();
        self.store_labels(&mut report)?;
        Ok(report)
    }

    /// Move `pages` (kept in their current relative order) so that the first of them lands at
    /// index `to` of the result (`to` + count <= page count).
    pub fn move_pages(&mut self, pages: &[usize], to: usize) -> Result<PageOpReport> {
        let sorted = self.unique_sorted(pages)?;
        let rest: Vec<usize> = (0..self.pages.len())
            .filter(|i| !sorted.contains(i))
            .collect();
        if to > rest.len() {
            return Err(PdfError::PageOutOfRange {
                index: to,
                count: rest.len(),
            });
        }
        let mut order = rest[..to].to_vec();
        order.extend(&sorted);
        order.extend(&rest[to..]);
        self.reorder(&order)
    }

    /// Rotate pages clockwise by `degrees` (a multiple of 90, negative for anticlockwise). Only
    /// `/Rotate` changes: content and annotations are in unrotated user space and turn with it.
    pub fn rotate(&mut self, pages: &[usize], degrees: i32) -> Result<PageOpReport> {
        if degrees.rem_euclid(90) != 0 {
            return Err(invalid("a page rotation must be a multiple of 90 degrees"));
        }
        let sorted = self.unique_sorted(pages)?;
        for i in &sorted {
            let id = self.pages[*i];
            let current = self
                .doc
                .get_dictionary(id)
                .ok()
                .and_then(|d| d.get(b"Rotate").ok())
                .and_then(|o| deref(&self.doc, o))
                .and_then(number)
                .map_or(0, |v| v.round() as i64);
            let new = (current + i64::from(degrees)).rem_euclid(360);
            let d = self
                .doc
                .get_dictionary_mut(id)
                .map_err(|_| PdfError::ObjectNotFound(id))?;
            if new == 0 {
                d.remove(b"Rotate");
            } else {
                d.set("Rotate", Object::Integer(new));
            }
        }
        let mut report = self.report();
        report.pages_affected = sorted;
        Ok(report)
    }

    /// Keep only `pages`, in the order given.
    pub fn extract(&mut self, pages: &[usize]) -> Result<PageOpReport> {
        if pages.is_empty() {
            return Err(invalid("there are no pages to extract"));
        }
        let keep = self.unique_sorted(pages)?;
        let drop: Vec<usize> = (0..self.pages.len())
            .filter(|i| !keep.contains(i))
            .collect();
        let mut report = if drop.is_empty() {
            self.report()
        } else {
            self.delete(&drop)?
        };
        // Indices of the kept pages after the delete, in the requested order.
        let order: Vec<usize> = pages
            .iter()
            .map(|p| keep.iter().position(|k| k == p).expect("kept"))
            .collect();
        report.absorb(self.reorder(&order)?);
        report.pages_affected = (0..pages.len()).collect();
        Ok(report)
    }

    /// Insert copies of `src_pages` (zero-based pages of `src`, in that order, no repeats) at
    /// `at` (0..=page count). `src` may be the document this restructure started from.
    pub fn insert_from(
        &mut self,
        at: usize,
        src: &PdfDocument,
        src_pages: &[usize],
    ) -> Result<PageOpReport> {
        self.insert_pages(at, src, src_pages, false)
    }

    /// Insert every page of `src` at `at`; with `import_outline`, its outline is appended to
    /// this document's top level, leading to the copied pages.
    pub fn insert_document(
        &mut self,
        at: usize,
        src: &PdfDocument,
        import_outline: bool,
    ) -> Result<PageOpReport> {
        let all: Vec<usize> = (0..src.page_count()).collect();
        self.insert_pages(at, src, &all, import_outline)
    }

    fn insert_pages(
        &mut self,
        at: usize,
        src: &PdfDocument,
        src_pages: &[usize],
        import_outline: bool,
    ) -> Result<PageOpReport> {
        if at > self.pages.len() {
            return Err(PdfError::PageOutOfRange {
                index: at,
                count: self.pages.len(),
            });
        }
        if src_pages.is_empty() {
            return Err(invalid("there are no pages to insert"));
        }
        let set: BTreeSet<usize> = src_pages.iter().copied().collect();
        if set.len() != src_pages.len() {
            return Err(invalid("a source page is listed twice"));
        }
        let src_ids: Vec<ObjectId> = src_pages
            .iter()
            .map(|i| src.page_id(*i))
            .collect::<Result<_>>()?;
        let mut report = self.report();
        let mut copier = Copier::new(&src.doc, &src_ids, &mut self.doc);
        let shared_layers = share_layers(
            &self.doc,
            &src.doc,
            src.state == self.source_state,
            &mut copier,
        );
        let new_ids: Vec<ObjectId> = src_ids
            .iter()
            .map(|p| copier.copy_page(&mut self.doc, *p, self.root))
            .collect();
        copier.run(&mut self.doc);

        fix_copied_annotations(&mut self.doc, &src.doc, &copier, &new_ids, at, &mut report);
        merge_form(&mut self.doc, &src.doc, &mut copier, &new_ids, &mut report)?;
        merge_layers(
            &mut self.doc,
            &src.doc,
            &mut copier,
            &shared_layers,
            &mut report,
        )?;
        merge_bluebeam_columns(&mut self.doc, &src.doc, &mut copier, &new_ids, &mut report)?;
        merge_private(&mut self.doc, &src.doc, &mut copier, &mut report)?;
        copier.run(&mut self.doc);

        for (k, id) in new_ids.iter().enumerate() {
            self.pages.insert(at + k, *id);
        }
        let src_labels = src.page_labels();
        if src_labels.is_some() {
            self.has_labels = true;
        }
        for (k, i) in src_pages.iter().enumerate() {
            let label = src_labels.as_ref().and_then(|l| l.get(*i).cloned());
            self.labels.insert(at + k, label);
        }
        self.rebuild_tree();

        if import_outline {
            let page_map: HashMap<usize, usize> = src_pages
                .iter()
                .enumerate()
                .map(|(k, i)| (*i, at + k))
                .collect();
            let mut theirs = src.outline();
            let mut inlined = 0;
            remap_outline(&mut theirs.items, src, &page_map, &mut inlined);
            report.named_dests_inlined += inlined;
            report.outline_items_added = theirs.len();
            if !theirs.is_empty() {
                let mut ours = self.read_outline(&BTreeSet::new()).0;
                // Indices of our own items' pages moved by the insert: they are read from the
                // page objects, so they are already right.
                ours.items.extend(theirs.items);
                outline::store(&mut self.doc, &self.pages, &ours)?;
            }
        }
        report.pages_affected = (at..at + new_ids.len()).collect();
        self.store_labels(&mut report)?;
        Ok(report)
    }

    /// Replace the pages `at..at + src_pages.len()` with copies of `src_pages` of `src`. The new
    /// pages take the old ones' places and labels; every reference to an old page (outline
    /// items, links, named destinations) now leads to its replacement.
    pub fn replace(
        &mut self,
        at: usize,
        src: &PdfDocument,
        src_pages: &[usize],
        options: ReplaceOptions,
    ) -> Result<PageOpReport> {
        let count = src_pages.len();
        if count == 0 {
            return Err(invalid("there are no pages to replace with"));
        }
        if at + count > self.pages.len() {
            return Err(PdfError::PageOutOfRange {
                index: at + count - 1,
                count: self.pages.len(),
            });
        }
        let old: Vec<ObjectId> = self.pages[at..at + count].to_vec();
        let old_labels: Vec<Option<PageLabel>> = self.labels[at..at + count].to_vec();
        let mut report = self.insert_pages(at, src, src_pages, false)?;
        let new: Vec<ObjectId> = self.pages[at..at + count].to_vec();

        if options.keep_markups {
            for (o, n) in old.iter().zip(&new) {
                report.markups_kept += move_markups(&mut self.doc, *o, *n);
            }
        } else {
            for o in &old {
                report.markups_dropped += markups_of(&self.doc, *o).len();
            }
        }
        // Repoint every reference to an old page at its replacement (the old page objects
        // themselves and their own annotations aside).
        let map: HashMap<ObjectId, ObjectId> =
            old.iter().copied().zip(new.iter().copied()).collect();
        let mut skip: HashSet<ObjectId> = old.iter().copied().collect();
        for o in &old {
            skip.extend(page_annotation_ids(&self.doc, *o));
        }
        report.refs_repointed = remap_references(&mut self.doc, &map, &skip);

        // Now the old pages go; nothing points at them any more except their own annotations.
        let removed: BTreeSet<ObjectId> = old.iter().copied().collect();
        let base = at + count;
        for _ in 0..count {
            self.pages.remove(base);
            self.labels.remove(base);
        }
        self.rebuild_tree();
        self.sweep_after_removal(&removed, &mut report)?;
        for (k, label) in old_labels.into_iter().enumerate() {
            self.labels[at + k] = label;
        }
        report.pages_removed = (at..at + count).collect();
        report.pages_affected = (at..at + count).collect();
        self.store_labels(&mut report)?;
        Ok(report)
    }

    fn read_outline(&self, removed: &BTreeSet<ObjectId>) -> (Outline, Vec<Vec<usize>>) {
        let mut ctx = ReadContext {
            doc: &self.doc,
            pages: &self.pages,
            removed,
            hits: Vec::new(),
        };
        let outline = outline::read(&mut ctx);
        (outline, ctx.hits)
    }

    /// Fix what refers to pages about to be removed, while they are still in the page list.
    fn sweep_before_removal(
        &mut self,
        removed: &BTreeSet<ObjectId>,
        report: &mut PageOpReport,
    ) -> Result<()> {
        // Outline items.
        let (mut tree, mut hits) = self.read_outline(removed);
        if !hits.is_empty() {
            hits.sort();
            hits.dedup();
            for path in hits.iter().rev() {
                let Some(item) = tree.get_mut(path) else {
                    continue;
                };
                if item.children.is_empty() {
                    let gone = tree.remove(path)?;
                    report.outline_items_removed.push(gone.title);
                } else {
                    item.target = OutlineTarget::None;
                    report.outline_items_cleared.push(item.title.clone());
                }
            }
            outline::store(&mut self.doc, &self.pages, &tree)?;
        }

        // Links on the pages that stay.
        let removed_names: BTreeSet<Vec<u8>> = raw_named_dests(&self.doc)
            .into_iter()
            .filter(|(_, raw, _)| raw.is_some_and(|r| points_into(&r, removed)))
            .map(|(n, _, _)| n)
            .collect();
        let mut kept_index = 0;
        for page in self.pages.clone() {
            if removed.contains(&page) {
                continue;
            }
            let doomed: Vec<(usize, Rect, &'static str)> = annots_array(&self.doc, page)
                .unwrap_or(&[])
                .iter()
                .enumerate()
                .filter_map(|(slot, entry)| {
                    let d = deref(&self.doc, entry)?.as_dict().ok()?;
                    let reason = goto_problem(
                        &self.doc,
                        d,
                        &|raw| points_into(raw, removed),
                        &removed_names,
                    )?;
                    let rect = d
                        .get(b"Rect")
                        .ok()
                        .and_then(|o| deref(&self.doc, o))
                        .and_then(rect_from_object)
                        .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
                    Some((slot, rect, reason))
                })
                .collect();
            if !doomed.is_empty() {
                let slots: Vec<usize> = doomed.iter().map(|d| d.0).collect();
                remove_annot_slots(&mut self.doc, page, &slots);
                for (_, rect, reason) in doomed {
                    report.links_removed.push(RemovedLink {
                        // Index after the removal.
                        page_index: kept_index,
                        rect,
                        reason,
                    });
                }
            }
            kept_index += 1;
        }

        // Named destinations.
        if !removed_names.is_empty() {
            remove_named_dests(&mut self.doc, &removed_names)?;
            report.named_dests_removed = removed_names
                .iter()
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .collect();
        }

        // The open action.
        let open = self
            .doc
            .catalog()
            .ok()
            .and_then(|c| c.get(b"OpenAction").ok())
            .and_then(|o| deref(&self.doc, o))
            .cloned();
        if let Some(open) = open {
            let raw = match &open {
                Object::Array(_) => crate::dest::parse_explicit(&self.doc, &open),
                Object::Dictionary(a) => {
                    a.get(b"D")
                        .ok()
                        .and_then(|d| match parse_dest_value(&self.doc, d)? {
                            DestValue::Explicit(raw) => Some(raw),
                            DestValue::Named(n) => lookup_named(&self.doc, &n),
                        })
                }
                _ => None,
            };
            if raw.is_some_and(|r| points_into(&r, removed)) {
                self.doc
                    .catalog_mut()
                    .map_err(PdfError::Parse)?
                    .remove(b"OpenAction");
                report.open_action_removed = true;
            }
        }
        Ok(())
    }

    /// After pages left the tree: the form's field tree, then any other reference.
    fn sweep_after_removal(
        &mut self,
        removed: &BTreeSet<ObjectId>,
        report: &mut PageOpReport,
    ) -> Result<()> {
        let live: HashSet<ObjectId> = self
            .pages
            .iter()
            .flat_map(|p| page_annotation_ids(&self.doc, *p))
            .collect();
        let mut dead: HashSet<ObjectId> = removed.iter().copied().collect();
        for page in removed {
            for a in page_annotation_ids(&self.doc, *page) {
                if !live.contains(&a) {
                    dead.insert(a);
                }
            }
        }
        report
            .fields_removed
            .extend(prune_fields(&mut self.doc, &live, &dead));
        report.refs_dropped += null_references(&mut self.doc, &dead);
        Ok(())
    }
}

/// Make each inherited attribute the page's own.
fn materialise(doc: &mut Document, page: ObjectId) {
    let mut add = Vec::new();
    {
        let Ok(d) = doc.get_dictionary(page) else {
            return;
        };
        for key in INHERITABLE_PAGE_KEYS {
            if !d.has(key) {
                if let Some(v) = inherited_raw(doc, page, key) {
                    add.push((key.to_vec(), v));
                }
            }
        }
        if !d.has(b"MediaBox") && !add.iter().any(|(k, _)| k == b"MediaBox") {
            add.push((b"MediaBox".to_vec(), crate::document::fallback_media_box()));
        }
        if !d.has(b"Resources") && !add.iter().any(|(k, _)| k == b"Resources") {
            add.push((b"Resources".to_vec(), Object::Dictionary(Dictionary::new())));
        }
    }
    if let Ok(d) = doc.get_dictionary_mut(page) {
        for (k, v) in add {
            d.set(k, v);
        }
    }
}

fn points_into(raw: &RawDest, pages: &BTreeSet<ObjectId>) -> bool {
    matches!(raw.page, RawPage::Object(id) if pages.contains(&id))
}

/// Why the GoTo of a link annotation no longer works, if it does not: its explicit page fails
/// `bad`, its named destination is in `bad_names` (or missing), or its page is `null`.
fn goto_problem(
    doc: &Document,
    d: &Dictionary,
    bad: &dyn Fn(&RawDest) -> bool,
    bad_names: &BTreeSet<Vec<u8>>,
) -> Option<&'static str> {
    if d.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Link".as_slice()) {
        return None;
    }
    let value = if let Ok(v) = d.get(b"Dest") {
        v
    } else {
        let a = deref(doc, d.get(b"A").ok()?)?.as_dict().ok()?;
        if a.get(b"S").and_then(Object::as_name).ok() != Some(b"GoTo".as_slice()) {
            return None;
        }
        a.get(b"D").ok()?
    };
    match parse_dest_value(doc, value)? {
        DestValue::Explicit(raw) => {
            if bad(&raw) {
                Some("target page deleted")
            } else if raw.page == RawPage::Missing {
                Some("target page not copied")
            } else {
                None
            }
        }
        DestValue::Named(n) => bad_names.contains(&n).then_some("target page deleted"),
    }
}

/// Remove the entries at `slots` from a page's `/Annots`.
fn remove_annot_slots(doc: &mut Document, page: ObjectId, slots: &[usize]) {
    let holder = match doc
        .get_dictionary(page)
        .ok()
        .and_then(|d| d.get(b"Annots").ok())
    {
        Some(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    let array = match holder {
        Some(id) => doc
            .get_object_mut(id)
            .ok()
            .and_then(|o| o.as_array_mut().ok()),
        None => doc
            .get_dictionary_mut(page)
            .ok()
            .and_then(|d| d.get_mut(b"Annots").ok())
            .and_then(|o| o.as_array_mut().ok()),
    };
    if let Some(array) = array {
        let mut i = 0;
        array.retain(|_| {
            let keep = !slots.contains(&i);
            i += 1;
            keep
        });
    }
}

/// Remove named destinations: entries of `/Dests`, and of the `/Names /Dests` tree (which is
/// written again flat when anything in it goes).
fn remove_named_dests(doc: &mut Document, names: &BTreeSet<Vec<u8>>) -> Result<()> {
    // Old-style dictionary.
    let dests_holder = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Dests").ok())
        .and_then(|o| o.as_reference().ok());
    let dests = match dests_holder {
        Some(id) => doc
            .get_object_mut(id)
            .ok()
            .and_then(|o| o.as_dict_mut().ok()),
        None => doc
            .catalog_mut()
            .ok()
            .and_then(|c| c.get_mut(b"Dests").ok())
            .and_then(|o| o.as_dict_mut().ok()),
    };
    if let Some(dests) = dests {
        for n in names {
            dests.remove(n);
        }
    }
    // Name tree.
    let Some(tree) = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Names").ok())
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|n| n.get(b"Dests").ok())
        .cloned()
    else {
        return Ok(());
    };
    let mut entries = Vec::new();
    let mut changed = false;
    walk_name_tree(doc, &tree, &mut |key, value| {
        if names.contains(key) {
            changed = true;
        } else {
            entries.push((key.to_vec(), value.clone()));
        }
    });
    if !changed {
        return Ok(());
    }
    let mut flat = Vec::with_capacity(entries.len() * 2);
    for (k, v) in entries {
        flat.push(crate::dest::byte_string(&k));
        flat.push(v);
    }
    let mut node = Dictionary::new();
    node.set("Names", Object::Array(flat));
    let node_id = doc.add_object(node);
    let names_holder = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Names").ok())
        .and_then(|o| o.as_reference().ok());
    let names_dict = match names_holder {
        Some(id) => doc
            .get_object_mut(id)
            .ok()
            .and_then(|o| o.as_dict_mut().ok()),
        None => doc
            .catalog_mut()
            .ok()
            .and_then(|c| c.get_mut(b"Names").ok())
            .and_then(|o| o.as_dict_mut().ok()),
    };
    if let Some(n) = names_dict {
        n.set("Dests", Object::Reference(node_id));
    }
    Ok(())
}

/// Text of a field's `/T`.
fn field_title(doc: &Document, d: &Dictionary) -> Option<String> {
    d.get(b"T")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| decode_text_string(o).ok())
}

fn is_widget(d: &Dictionary) -> bool {
    d.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Widget".as_slice())
}

/// Remove widgets that are on no live page from the field tree, and fields left with nothing.
/// Returns the fully qualified names of the fields removed.
pub(crate) fn prune_fields(
    doc: &mut Document,
    live: &HashSet<ObjectId>,
    dead: &HashSet<ObjectId>,
) -> Vec<String> {
    let Some(form_id) = acro_form_holder(doc) else {
        return Vec::new();
    };
    let fields = match form_dict(doc, form_id).and_then(|f| f.get(b"Fields").ok()) {
        Some(Object::Array(a)) => a.clone(),
        Some(Object::Reference(id)) => doc
            .get_object(*id)
            .ok()
            .and_then(|o| o.as_array().ok())
            .cloned()
            .unwrap_or_default(),
        _ => return Vec::new(),
    };
    let mut removed_names = Vec::new();
    let mut kept = Vec::new();
    for f in fields {
        if keep_field(doc, &f, None, live, dead, &mut removed_names, 0) {
            kept.push(f);
        }
    }
    if let Some(form) = form_dict_mut(doc, form_id) {
        form.set("Fields", Object::Array(kept));
    }
    removed_names
}

/// Whether the field (or widget) at `node` survives; prunes its kids in place.
fn keep_field(
    doc: &mut Document,
    node: &Object,
    parent: Option<&str>,
    live: &HashSet<ObjectId>,
    dead: &HashSet<ObjectId>,
    removed: &mut Vec<String>,
    depth: usize,
) -> bool {
    let Object::Reference(id) = node else {
        return true;
    };
    if depth > 32 {
        return true;
    }
    let Ok(d) = doc.get_dictionary(*id) else {
        return false;
    };
    let own = field_title(doc, d);
    let full = match (parent, &own) {
        (Some(p), Some(o)) => Some(format!("{p}.{o}")),
        (None, Some(o)) => Some(o.clone()),
        (Some(p), None) => Some(p.to_string()),
        (None, None) => None,
    };
    let kids = match d.get(b"Kids") {
        Ok(Object::Array(k)) => Some(k.clone()),
        _ => None,
    };
    match kids {
        None => {
            // A terminal field merged with its widget, or a bare widget.
            if is_widget(d) && dead.contains(id) && !live.contains(id) {
                if let Some(n) = full {
                    if own.is_some() {
                        removed.push(n);
                    }
                }
                false
            } else {
                true
            }
        }
        Some(kids) => {
            let before = kids.len();
            let mut kept = Vec::new();
            for k in kids {
                if matches!(k, Object::Null) {
                    continue;
                }
                if keep_field(doc, &k, full.as_deref(), live, dead, removed, depth + 1) {
                    kept.push(k);
                }
            }
            let empty = kept.is_empty() && before > 0;
            if let Ok(d) = doc.get_dictionary_mut(*id) {
                d.set("Kids", Object::Array(kept));
            }
            if empty {
                if let Some(n) = full {
                    if !removed.iter().any(|r| r.starts_with(&n)) {
                        removed.push(n);
                    }
                }
                false
            } else {
                true
            }
        }
    }
}

/// Where the `/AcroForm` dictionary lives: `Some(Some(id))` in its own object, `Some(None)`
/// directly in the catalog, `None` when there is none.
fn acro_form_holder(doc: &Document) -> Option<Option<ObjectId>> {
    match doc.catalog().ok()?.get(b"AcroForm").ok()? {
        Object::Reference(id) => Some(Some(*id)),
        Object::Dictionary(_) => Some(None),
        _ => None,
    }
}

fn form_dict(doc: &Document, holder: Option<ObjectId>) -> Option<&Dictionary> {
    match holder {
        Some(id) => doc.get_dictionary(id).ok(),
        None => doc.catalog().ok()?.get(b"AcroForm").ok()?.as_dict().ok(),
    }
}

fn form_dict_mut(doc: &mut Document, holder: Option<ObjectId>) -> Option<&mut Dictionary> {
    match holder {
        Some(id) => doc.get_dictionary_mut(id).ok(),
        None => doc
            .catalog_mut()
            .ok()?
            .get_mut(b"AcroForm")
            .ok()?
            .as_dict_mut()
            .ok(),
    }
}

/// Visit every object reachable from the trailer, never entering `stop`, with `f` on each
/// reachable object's value (the objects themselves, not the trailer).
fn reachable(doc: &Document, stop: &HashSet<ObjectId>) -> Vec<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut order = Vec::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    fn refs(o: &Object, out: &mut Vec<ObjectId>) {
        match o {
            Object::Reference(id) => out.push(*id),
            Object::Array(a) => a.iter().for_each(|x| refs(x, out)),
            Object::Dictionary(d) => d.iter().for_each(|(_, v)| refs(v, out)),
            Object::Stream(s) => s.dict.iter().for_each(|(_, v)| refs(v, out)),
            _ => {}
        }
    }
    let mut first = Vec::new();
    for (_, v) in doc.trailer.iter() {
        refs(v, &mut first);
    }
    stack.extend(first);
    while let Some(id) = stack.pop() {
        if stop.contains(&id) || !seen.insert(id) {
            continue;
        }
        let Some(o) = doc.objects.get(&id) else {
            continue;
        };
        order.push(id);
        let mut next = Vec::new();
        refs(o, &mut next);
        stack.extend(next);
    }
    order
}

/// Replace references to `dead` objects with `null` (dropping dictionary entries; removing
/// elements of arrays that hold only references, such as `/Kids`, `/Annots`, `/OCGs`) in every
/// object still reachable. Returns how many were changed.
pub(crate) fn null_references(doc: &mut Document, dead: &HashSet<ObjectId>) -> usize {
    if dead.is_empty() {
        return 0;
    }
    let ids = reachable(doc, dead);
    let mut count = 0;
    for id in ids {
        if let Some(o) = doc.objects.get_mut(&id) {
            count += null_in(o, dead);
        }
    }
    count
}

fn null_in(o: &mut Object, dead: &HashSet<ObjectId>) -> usize {
    match o {
        Object::Array(a) => {
            let all_refs = !a.is_empty() && a.iter().all(|x| matches!(x, Object::Reference(_)));
            let mut n = 0;
            if all_refs {
                let before = a.len();
                a.retain(|x| !matches!(x, Object::Reference(id) if dead.contains(id)));
                n += before - a.len();
            } else {
                for x in a.iter_mut() {
                    if matches!(x, Object::Reference(id) if dead.contains(id)) {
                        *x = Object::Null;
                        n += 1;
                    } else {
                        n += null_in(x, dead);
                    }
                }
            }
            n
        }
        Object::Dictionary(d) => null_in_dict(d, dead),
        Object::Stream(s) => null_in_dict(&mut s.dict, dead),
        _ => 0,
    }
}

fn null_in_dict(d: &mut Dictionary, dead: &HashSet<ObjectId>) -> usize {
    let gone: Vec<Vec<u8>> = d
        .iter()
        .filter(|(_, v)| matches!(v, Object::Reference(id) if dead.contains(id)))
        .map(|(k, _)| k.clone())
        .collect();
    let mut n = gone.len();
    for k in gone {
        d.remove(&k);
    }
    for (_, v) in d.iter_mut() {
        n += null_in(v, dead);
    }
    n
}

/// Repoint references by `map` everywhere but in the objects of `skip`. Returns the count.
fn remap_references(
    doc: &mut Document,
    map: &HashMap<ObjectId, ObjectId>,
    skip: &HashSet<ObjectId>,
) -> usize {
    fn remap(o: &mut Object, map: &HashMap<ObjectId, ObjectId>) -> usize {
        match o {
            Object::Reference(id) => match map.get(id) {
                Some(new) => {
                    *id = *new;
                    1
                }
                None => 0,
            },
            Object::Array(a) => a.iter_mut().map(|x| remap(x, map)).sum(),
            Object::Dictionary(d) => d.iter_mut().map(|(_, v)| remap(v, map)).sum(),
            Object::Stream(s) => s.dict.iter_mut().map(|(_, v)| remap(v, map)).sum(),
            _ => 0,
        }
    }
    let mut n = 0;
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        if skip.contains(&id) {
            continue;
        }
        if let Some(o) = doc.objects.get_mut(&id) {
            n += remap(o, map);
        }
    }
    n
}

/// Move every markup (not a widget or a link) of page `from` to page `to`. Returns the count.
/// The `/Annots` entries of a page that are markups (their popups included): everything but
/// form widgets and links.
fn markups_of(doc: &Document, page: ObjectId) -> Vec<Object> {
    let entries: Vec<Object> = annots_array(doc, page)
        .map(<[Object]>::to_vec)
        .unwrap_or_default();
    entries
        .into_iter()
        .filter(|entry| {
            let Some(d) = deref(doc, entry).and_then(|o| o.as_dict().ok()) else {
                return false;
            };
            let sub = d.get(b"Subtype").and_then(Object::as_name).ok();
            !matches!(sub, Some(b"Widget") | Some(b"Link"))
        })
        .collect()
}

fn move_markups(doc: &mut Document, from: ObjectId, to: ObjectId) -> usize {
    let moving = markups_of(doc, from);
    for entry in &moving {
        match entry {
            Object::Reference(id) => {
                if let Ok(d) = doc.get_dictionary_mut(*id) {
                    d.set("P", Object::Reference(to));
                }
            }
            Object::Dictionary(_) => {}
            _ => {}
        }
    }
    let count = moving.len();
    let moved: Vec<Object> = moving
        .into_iter()
        .map(|mut o| {
            if let Object::Dictionary(d) = &mut o {
                d.set("P", Object::Reference(to));
            }
            o
        })
        .collect();
    let target = match doc
        .get_dictionary(to)
        .ok()
        .and_then(|d| d.get(b"Annots").ok())
    {
        Some(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    match target {
        Some(id) => {
            if let Ok(a) = doc.get_object_mut(id).and_then(Object::as_array_mut) {
                a.extend(moved);
            }
        }
        None => {
            if let Ok(d) = doc.get_dictionary_mut(to) {
                match d.get_mut(b"Annots") {
                    Ok(Object::Array(a)) => a.extend(moved),
                    _ => d.set("Annots", Object::Array(moved)),
                }
            }
        }
    }
    count
}

/// After a copy: `/P` of every copied annotation is its new page; GoTo links whose page was
/// not copied are removed; named destinations they use become explicit.
fn fix_copied_annotations(
    dst: &mut Document,
    src: &Document,
    copier: &Copier<'_>,
    new_pages: &[ObjectId],
    at: usize,
    report: &mut PageOpReport,
) {
    let src_pages: Vec<ObjectId> = src.page_iter().collect();
    for (page_pos, page) in new_pages.iter().enumerate() {
        let ids: Vec<ObjectId> = page_annotation_ids(dst, *page);
        for id in &ids {
            if let Ok(d) = dst.get_dictionary_mut(*id) {
                d.set("P", Object::Reference(*page));
            }
        }
        let mut doomed = Vec::new();
        let entries: Vec<Object> = annots_array(dst, *page)
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        for (slot, entry) in entries.iter().enumerate() {
            let Some(id) = entry.as_reference().ok() else {
                continue;
            };
            let Ok(d) = dst.get_dictionary(id) else {
                continue;
            };
            if d.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Link".as_slice()) {
                continue;
            }
            // Named destination: look it up in the source and write the page out.
            let (key, value, in_action) = if let Ok(v) = d.get(b"Dest") {
                ("Dest", v.clone(), false)
            } else if let Some(a) = d
                .get(b"A")
                .ok()
                .and_then(|a| deref(dst, a))
                .and_then(|a| a.as_dict().ok())
            {
                if a.get(b"S").and_then(Object::as_name).ok() != Some(b"GoTo".as_slice()) {
                    continue;
                }
                match a.get(b"D") {
                    Ok(v) => ("D", v.clone(), true),
                    Err(_) => continue,
                }
            } else {
                continue;
            };
            let explicit = match parse_dest_value(dst, &value) {
                Some(DestValue::Named(n)) => {
                    let Some(raw) = lookup_named(src, &n) else {
                        doomed.push((slot, "named destination missing"));
                        continue;
                    };
                    let target = match raw.page {
                        RawPage::Object(p) => copier.page_map.get(&p).copied(),
                        RawPage::Number(n) => usize::try_from(n)
                            .ok()
                            .and_then(|i| src_pages.get(i))
                            .and_then(|p| copier.page_map.get(p).copied()),
                        RawPage::Missing => None,
                    };
                    let Some(target) = target else {
                        doomed.push((slot, "target page not copied"));
                        continue;
                    };
                    let mut items = vec![Object::Reference(target)];
                    items.extend(raw.view.to_objects());
                    report.named_dests_inlined += 1;
                    Object::Array(items)
                }
                Some(DestValue::Explicit(raw)) => {
                    if raw.page == RawPage::Missing {
                        doomed.push((slot, "target page not copied"));
                    }
                    continue;
                }
                None => continue,
            };
            // An action held in its own object is replaced by a direct copy of it.
            let action = dst
                .get_dictionary(id)
                .ok()
                .and_then(|d| d.get(b"A").ok())
                .and_then(|a| deref(dst, a))
                .and_then(|a| a.as_dict().ok())
                .cloned();
            let Ok(d) = dst.get_dictionary_mut(id) else {
                continue;
            };
            if in_action {
                let mut a = action.unwrap_or_default();
                a.set(key, explicit);
                d.set("A", Object::Dictionary(a));
            } else {
                d.set(key, explicit);
            }
        }
        if !doomed.is_empty() {
            let rects: Vec<(Rect, &'static str)> = doomed
                .iter()
                .map(|(slot, reason)| {
                    let rect = entries[*slot]
                        .as_reference()
                        .ok()
                        .and_then(|id| dst.get_dictionary(id).ok())
                        .and_then(|d| d.get(b"Rect").ok())
                        .and_then(rect_from_object)
                        .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
                    (rect, *reason)
                })
                .collect();
            let slots: Vec<usize> = doomed.iter().map(|d| d.0).collect();
            remove_annot_slots(dst, *page, &slots);
            for (rect, reason) in rects {
                report.links_removed.push(RemovedLink {
                    page_index: at + page_pos,
                    rect,
                    reason,
                });
            }
        }
    }
}

/// The catalog dictionary entry `key`, as a mutable dictionary (following one reference).
fn catalog_entry_dict_mut<'a>(doc: &'a mut Document, key: &[u8]) -> Option<&'a mut Dictionary> {
    let holder = match doc.catalog().ok()?.get(key).ok()? {
        Object::Reference(id) => Some(*id),
        Object::Dictionary(_) => None,
        _ => return None,
    };
    match holder {
        Some(id) => doc.get_dictionary_mut(id).ok(),
        None => doc
            .catalog_mut()
            .ok()?
            .get_mut(key)
            .ok()?
            .as_dict_mut()
            .ok(),
    }
}

fn catalog_entry<'a>(doc: &'a Document, key: &[u8]) -> Option<&'a Object> {
    let o = doc.catalog().ok()?.get(key).ok()?;
    deref(doc, o)
}

/// Merge the copied pages' form fields into the target's `/AcroForm`.
fn merge_form(
    dst: &mut Document,
    src: &Document,
    copier: &mut Copier<'_>,
    new_pages: &[ObjectId],
    report: &mut PageOpReport,
) -> Result<()> {
    // Copied widgets, and the top-level field each belongs to.
    let mut roots: Vec<ObjectId> = Vec::new();
    for page in new_pages {
        for a in page_annotation_ids(dst, *page) {
            let Ok(d) = dst.get_dictionary(a) else {
                continue;
            };
            if !is_widget(d) {
                continue;
            }
            let mut node = a;
            for _ in 0..32 {
                match dst
                    .get_dictionary(node)
                    .ok()
                    .and_then(|d| d.get(b"Parent").ok())
                    .and_then(|o| o.as_reference().ok())
                {
                    Some(p) if dst.objects.contains_key(&p) => node = p,
                    _ => break,
                }
            }
            if !roots.contains(&node) {
                roots.push(node);
            }
        }
    }
    if roots.is_empty() {
        return Ok(());
    }
    // Drop the null kids left by widgets that were not copied.
    for r in &roots {
        clean_kids(dst, *r, 0);
    }
    // Names in use at the top level of the target.
    let existing: Vec<Object> = match catalog_entry(dst, b"AcroForm")
        .and_then(|f| f.as_dict().ok())
        .and_then(|f| f.get(b"Fields").ok())
        .and_then(|o| deref(dst, o))
    {
        Some(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let mut taken: BTreeSet<String> = existing
        .iter()
        .filter_map(|o| deref(dst, o).and_then(|o| o.as_dict().ok()))
        .filter_map(|d| field_title(dst, d))
        .collect();
    for r in &roots {
        let Ok(d) = dst.get_dictionary(*r) else {
            continue;
        };
        let Some(title) = field_title(dst, d) else {
            continue;
        };
        let signed = d.get(b"FT").and_then(Object::as_name).ok() == Some(b"Sig".as_slice())
            && d.get(b"V")
                .ok()
                .and_then(|v| deref(dst, v))
                .is_some_and(|v| v.as_dict().is_ok());
        let mut new_title = title.clone();
        if taken.contains(&title) {
            let mut n = 2;
            while taken.contains(&format!("{title}_{n}")) {
                n += 1;
            }
            new_title = format!("{title}_{n}");
            if let Ok(d) = dst.get_dictionary_mut(*r) {
                d.set("T", text_string(&new_title));
            }
            report
                .fields_renamed
                .push((title.clone(), new_title.clone()));
        }
        if signed {
            report
                .signatures_invalidated
                .push(format!("copied: {new_title}"));
        }
        taken.insert(new_title);
    }
    // The target's /AcroForm, created when missing.
    if acro_form_holder(dst).is_none() {
        let mut form = Dictionary::new();
        form.set("Fields", Object::Array(Vec::new()));
        let id = dst.add_object(form);
        dst.catalog_mut()
            .map_err(PdfError::Parse)?
            .set("AcroForm", Object::Reference(id));
    }
    let src_form = catalog_entry(src, b"AcroForm")
        .and_then(|f| f.as_dict().ok())
        .cloned();
    // The source's default resources, one level made direct in the source before the copy so
    // the merge below sees dictionaries, not references to objects still being copied.
    let src_dr = src_form
        .as_ref()
        .and_then(|f| f.get(b"DR").ok())
        .and_then(|dr| deref(src, dr))
        .and_then(|dr| dr.as_dict().ok())
        .map(|dr| {
            let mut direct = Dictionary::new();
            for (k, v) in dr.iter() {
                direct.set(k.clone(), deref(src, v).cloned().unwrap_or(Object::Null));
            }
            copier.map_obj(dst, &Object::Dictionary(direct))
        });
    let holder = acro_form_holder(dst).expect("created above");
    // /Fields may itself be an indirect array.
    let fields_holder = form_dict(dst, holder)
        .and_then(|f| f.get(b"Fields").ok())
        .and_then(|o| o.as_reference().ok());
    let root_refs: Vec<Object> = roots.iter().map(|r| Object::Reference(*r)).collect();
    match fields_holder {
        Some(id) => {
            if let Ok(a) = dst.get_object_mut(id).and_then(Object::as_array_mut) {
                a.extend(root_refs);
            }
        }
        None => {
            let form = form_dict_mut(dst, holder).expect("form exists");
            match form.get_mut(b"Fields") {
                Ok(Object::Array(a)) => a.extend(root_refs),
                _ => form.set("Fields", Object::Array(root_refs)),
            }
        }
    }
    // Default resources: the source's fonts the target does not name.
    let dr_holder = form_dict(dst, holder)
        .and_then(|f| f.get(b"DR").ok())
        .and_then(|o| o.as_reference().ok());
    if let Some(Object::Dictionary(src_dr)) = src_dr {
        let target_dr = match dr_holder {
            Some(id) => dst.get_dictionary_mut(id).ok(),
            None => {
                let form = form_dict_mut(dst, holder).expect("form exists");
                if !form.has(b"DR") {
                    form.set("DR", Object::Dictionary(Dictionary::new()));
                }
                form.get_mut(b"DR").ok().and_then(|o| o.as_dict_mut().ok())
            }
        };
        if let Some(target_dr) = target_dr {
            merge_resource_dict(target_dr, &src_dr);
        }
    }
    let form = form_dict_mut(dst, holder).expect("form exists");
    if let Some(src_form) = &src_form {
        if !form.has(b"DA") {
            if let Ok(da) = src_form.get(b"DA") {
                form.set("DA", da.clone());
            }
        }
        if src_form
            .get(b"NeedAppearances")
            .and_then(Object::as_bool)
            .unwrap_or(false)
        {
            form.set("NeedAppearances", Object::Boolean(true));
        }
        let flags = |f: &Dictionary| {
            f.get(b"SigFlags")
                .ok()
                .and_then(number)
                .map_or(0, |v| v as i64)
        };
        let merged = flags(form) | flags(src_form);
        if merged != 0 {
            form.set("SigFlags", Object::Integer(merged));
        }
    }
    Ok(())
}

/// Add the entries of `src` (a resource dictionary, one level of sub-dictionaries) that
/// `target` lacks.
fn merge_resource_dict(target: &mut Dictionary, src: &Dictionary) {
    for (k, v) in src.iter() {
        match (target.get_mut(k), v) {
            (Ok(Object::Dictionary(t)), Object::Dictionary(s)) => {
                for (k2, v2) in s.iter() {
                    if !t.has(k2) {
                        t.set(k2.clone(), v2.clone());
                    }
                }
            }
            (Ok(_), _) => {}
            (Err(_), _) => target.set(k.clone(), v.clone()),
        }
    }
}

fn clean_kids(doc: &mut Document, node: ObjectId, depth: usize) {
    if depth > 32 {
        return;
    }
    let kids = match doc
        .get_dictionary_mut(node)
        .ok()
        .and_then(|d| d.get_mut(b"Kids").ok())
    {
        Some(Object::Array(k)) => {
            k.retain(|o| !matches!(o, Object::Null));
            k.clone()
        }
        _ => return,
    };
    for k in kids {
        if let Object::Reference(id) = k {
            clean_kids(doc, id, depth + 1);
        }
    }
}

/// Add the optional-content groups the copy brought to the target's `/OCProperties`.
/// The source's optional-content groups the target already has, made to map onto the
/// target's own group instead of being copied: every group when the source is the target
/// itself (`same_document`), else a group whose `/Name` names exactly one group on each side.
/// Returns the source ids so shared.
fn share_layers(
    dst: &Document,
    src: &Document,
    same_document: bool,
    copier: &mut Copier<'_>,
) -> HashSet<ObjectId> {
    fn groups(doc: &Document) -> Vec<(ObjectId, Option<String>)> {
        let Some(oc) = catalog_entry(doc, b"OCProperties").and_then(|o| o.as_dict().ok()) else {
            return Vec::new();
        };
        match oc.get(b"OCGs").ok().and_then(|o| deref(doc, o)) {
            Some(Object::Array(a)) => a
                .iter()
                .filter_map(|x| x.as_reference().ok())
                .map(|id| {
                    let name = doc
                        .get_dictionary(id)
                        .ok()
                        .and_then(|d| d.get(b"Name").ok())
                        .and_then(|o| deref(doc, o))
                        .and_then(|o| decode_text_string(o).ok());
                    (id, name)
                })
                .collect(),
            _ => Vec::new(),
        }
    }
    let theirs = groups(src);
    let ours = groups(dst);
    let mut shared = HashSet::new();
    for (id, name) in &theirs {
        let target = if same_document {
            ours.iter().any(|(o, _)| o == id).then_some(*id)
        } else {
            let unique = |list: &[(ObjectId, Option<String>)]| {
                let mut same = list.iter().filter(|(_, n)| n.is_some() && n == name);
                match (same.next(), same.next()) {
                    (Some((o, _)), None) => Some(*o),
                    _ => None,
                }
            };
            unique(&theirs).and(unique(&ours))
        };
        if let Some(target) = target {
            copier.memo.insert(*id, target);
            shared.insert(*id);
        }
    }
    shared
}

fn merge_layers(
    dst: &mut Document,
    src: &Document,
    copier: &mut Copier<'_>,
    shared: &HashSet<ObjectId>,
    report: &mut PageOpReport,
) -> Result<()> {
    let Some(src_oc) = catalog_entry(src, b"OCProperties").and_then(|o| o.as_dict().ok()) else {
        return Ok(());
    };
    let refs = |o: Option<&Object>| -> Vec<ObjectId> {
        match o.and_then(|o| deref(src, o)) {
            Some(Object::Array(a)) => a.iter().filter_map(|x| x.as_reference().ok()).collect(),
            _ => Vec::new(),
        }
    };
    let src_ocgs = refs(src_oc.get(b"OCGs").ok());
    let src_d = src_oc
        .get(b"D")
        .ok()
        .and_then(|o| deref(src, o))
        .and_then(|o| o.as_dict().ok());
    let src_off: HashSet<ObjectId> = refs(src_d.and_then(|d| d.get(b"OFF").ok()))
        .into_iter()
        .collect();
    let base_off = src_d
        .and_then(|d| d.get(b"BaseState").ok())
        .and_then(|o| o.as_name().ok())
        == Some(b"OFF".as_slice());
    let src_on: HashSet<ObjectId> = refs(src_d.and_then(|d| d.get(b"ON").ok()))
        .into_iter()
        .collect();
    // Only the groups the copied pages use came along.
    let copied: Vec<(ObjectId, bool)> = src_ocgs
        .iter()
        .filter(|g| !shared.contains(g))
        .filter_map(|g| {
            let new = copier.memo.get(g).copied()?;
            let off = src_off.contains(g) || (base_off && !src_on.contains(g));
            Some((new, off))
        })
        .collect();
    if copied.is_empty() {
        return Ok(());
    }
    if catalog_entry(dst, b"OCProperties").is_none() {
        let mut d = Dictionary::new();
        d.set("Name", text_string("Default"));
        let mut oc = Dictionary::new();
        oc.set("OCGs", Object::Array(Vec::new()));
        oc.set("D", Object::Dictionary(d));
        dst.catalog_mut()
            .map_err(PdfError::Parse)?
            .set("OCProperties", Object::Dictionary(oc));
    }
    // Work on a direct copy of the target's /OCProperties and /D, then store it in the catalog.
    let mut oc = catalog_entry(dst, b"OCProperties")
        .and_then(|o| o.as_dict().ok())
        .cloned()
        .unwrap_or_default();
    let mut d = oc
        .get(b"D")
        .ok()
        .and_then(|o| deref(dst, o))
        .and_then(|o| o.as_dict().ok())
        .cloned()
        .unwrap_or_default();
    let list = |dict: &mut Dictionary, key: &str, add: &[ObjectId]| {
        if add.is_empty() {
            return;
        }
        let mut items = match dict.get(key.as_bytes()).ok().and_then(|o| deref(dst, o)) {
            Some(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        for id in add {
            if !items.contains(&Object::Reference(*id)) {
                items.push(Object::Reference(*id));
            }
        }
        dict.set(key, Object::Array(items));
    };
    let all: Vec<ObjectId> = copied.iter().map(|(id, _)| *id).collect();
    let off: Vec<ObjectId> = copied
        .iter()
        .filter(|(_, off)| *off)
        .map(|(id, _)| *id)
        .collect();
    let on: Vec<ObjectId> = copied
        .iter()
        .filter(|(_, off)| !*off)
        .map(|(id, _)| *id)
        .collect();
    // A target that had no /Order showed all its groups; an /Order of only the new ones would
    // hide them from the viewers that list layers by /Order, so it starts with the target's.
    if !d.has(b"Order") {
        let own = match oc.get(b"OCGs").ok().and_then(|o| deref(dst, o)) {
            Some(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        d.set("Order", Object::Array(own));
    }
    list(&mut oc, "OCGs", &all);
    list(&mut d, "Order", &all);
    let target_base_off =
        d.get(b"BaseState").and_then(Object::as_name).ok() == Some(b"OFF".as_slice());
    list(&mut d, "OFF", &off);
    if target_base_off {
        list(&mut d, "ON", &on);
    }
    oc.set("D", Object::Dictionary(d));
    match catalog_entry_dict_mut(dst, b"OCProperties") {
        Some(target) => *target = oc,
        None => dst
            .catalog_mut()
            .map_err(PdfError::Parse)?
            .set("OCProperties", Object::Dictionary(oc)),
    }
    report.layers_merged += all.len();
    Ok(())
}

/// Column names of a `/BSIAnnotColumns` array, by position.
fn bluebeam_names(doc: &Document, array: &[Object]) -> Vec<String> {
    array
        .iter()
        .map(|entry| {
            deref(doc, entry)
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| d.get(b"Name").ok())
                .and_then(|o| deref(doc, o))
                .and_then(|o| decode_text_string(o).ok())
                .unwrap_or_default()
        })
        .collect()
}

fn bluebeam_array(doc: &Document) -> Option<(Option<ObjectId>, Vec<Object>)> {
    let o = doc.catalog().ok()?.get(b"BSIAnnotColumns").ok()?;
    match o {
        Object::Reference(id) => Some((
            Some(*id),
            doc.get_object(*id).ok()?.as_array().ok()?.clone(),
        )),
        Object::Array(a) => Some((None, a.clone())),
        _ => None,
    }
}

/// Keep Bluebeam's custom columns working for copied markups (see the module documentation).
fn merge_bluebeam_columns(
    dst: &mut Document,
    src: &Document,
    copier: &mut Copier<'_>,
    new_pages: &[ObjectId],
    report: &mut PageOpReport,
) -> Result<()> {
    let Some((_, src_cols)) = bluebeam_array(src) else {
        return Ok(());
    };
    let src_names = bluebeam_names(src, &src_cols);
    let (holder, mut dst_cols) = bluebeam_array(dst).unwrap_or_default();
    let mut dst_names = bluebeam_names(dst, &dst_cols);
    for (i, n) in src_names.iter().enumerate() {
        if !dst_names.contains(n) {
            let copied = copier.map_obj(dst, &src_cols[i]);
            dst_cols.push(copied);
            dst_names.push(n.clone());
            report.bluebeam_columns_added.push(n.clone());
        }
    }
    if !report.bluebeam_columns_added.is_empty() || holder.is_none() {
        let array = Object::Array(dst_cols);
        match holder {
            Some(id) => {
                dst.objects.insert(id, array);
            }
            None => {
                // Revu only reads an indirect array.
                let id = dst.add_object(array);
                dst.catalog_mut()
                    .map_err(PdfError::Parse)?
                    .set("BSIAnnotColumns", Object::Reference(id));
            }
        }
    }
    if dst_names.starts_with(&src_names) {
        // Same positions: values need no remapping.
        return Ok(());
    }
    // Position in the source -> position in the target, by name (first occurrence).
    let position: Vec<Option<usize>> = src_names
        .iter()
        .map(|n| dst_names.iter().position(|d| d == n))
        .collect();
    let mut annots = Vec::new();
    for page in new_pages {
        annots.extend(page_annotation_ids(dst, *page));
    }
    for id in annots {
        let Ok(d) = dst.get_dictionary_mut(id) else {
            continue;
        };
        let Ok(Object::Array(values)) = d.get(b"BSIColumnData") else {
            continue;
        };
        let mut out =
            vec![Object::String(Vec::new(), lopdf::StringFormat::Literal); dst_names.len()];
        for (i, v) in values.iter().enumerate() {
            if let Some(Some(j)) = position.get(i) {
                out[*j] = v.clone();
            }
        }
        d.set("BSIColumnData", Object::Array(out));
    }
    Ok(())
}

/// The target keeps its own `/PlumblineData`; a target without one takes the source's.
fn merge_private(
    dst: &mut Document,
    src: &Document,
    copier: &mut Copier<'_>,
    report: &mut PageOpReport,
) -> Result<()> {
    const KEY: &[u8] = b"PlumblineData";
    let Some(theirs) = src.catalog().ok().and_then(|c| c.get(KEY).ok()).cloned() else {
        return Ok(());
    };
    let has = dst.catalog().map_err(PdfError::Parse)?.has(KEY);
    if !has {
        let copied = copier.map_obj(dst, &theirs);
        dst.catalog_mut().map_err(PdfError::Parse)?.set(KEY, copied);
        report
            .notes
            .push("the inserted file's Plumbline document settings were taken over".into());
    }
    Ok(())
}

/// Turn the source outline's targets into targets in the merged document.
fn remap_outline(
    items: &mut [OutlineItem],
    src: &PdfDocument,
    page_map: &HashMap<usize, usize>,
    inlined: &mut usize,
) {
    for item in items.iter_mut() {
        let dest = match &item.target {
            OutlineTarget::Page { dest, .. } => Some(*dest),
            OutlineTarget::Named { name, .. } => {
                let d = lookup_named(&src.doc, name).and_then(|raw| resolve_raw(&src.pages, raw));
                if d.is_some() {
                    *inlined += 1;
                }
                d
            }
            _ => None,
        };
        item.target = match (&item.target, dest) {
            (OutlineTarget::Action(a), _) if !has_references(a) => OutlineTarget::Action(a.clone()),
            (_, Some(d)) => match page_map.get(&d.page_index) {
                Some(new) => OutlineTarget::Page {
                    dest: Destination::new(*new, d.view),
                    as_action: false,
                },
                None => OutlineTarget::None,
            },
            _ => OutlineTarget::None,
        };
        // Keys that may point into the source document do not come along.
        item.extra.retain(|(_, v)| !has_references(v));
        remap_outline(&mut item.children, src, page_map, inlined);
    }
}

fn has_references(o: &Object) -> bool {
    match o {
        Object::Reference(_) => true,
        Object::Array(a) => a.iter().any(has_references),
        Object::Dictionary(d) => d.iter().any(|(_, v)| has_references(v)),
        Object::Stream(s) => s.dict.iter().any(|(_, v)| has_references(v)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_references_removes_from_reference_lists_and_nulls_in_mixed_arrays() {
        let dead: HashSet<ObjectId> = [(9, 0)].into_iter().collect();
        let mut kids = Object::Array(vec![Object::Reference((1, 0)), Object::Reference((9, 0))]);
        assert_eq!(null_in(&mut kids, &dead), 1);
        assert_eq!(kids, Object::Array(vec![Object::Reference((1, 0))]));
        let mut dest = Object::Array(vec![Object::Reference((9, 0)), name("Fit")]);
        assert_eq!(null_in(&mut dest, &dead), 1);
        assert_eq!(dest, Object::Array(vec![Object::Null, name("Fit")]));
        let mut d = Dictionary::new();
        d.set("Pg", Object::Reference((9, 0)));
        d.set("K", Object::Integer(3));
        let mut o = Object::Dictionary(d);
        assert_eq!(null_in(&mut o, &dead), 1);
        assert!(!o.as_dict().unwrap().has(b"Pg"));
    }
}
