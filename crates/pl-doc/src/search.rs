//! Search across a whole document, with next / previous navigation.
//!
//! The backend searches one page per call, so [`DocSearch`] walks the pages. It can be stepped
//! a few pages at a time ([`DocSearch::step`]) so a UI never blocks on a long document, or run
//! to the end in one go ([`DocSearch::run`]).

use std::collections::BTreeMap;

use pl_render::{
    search_page_text, DocId, PageText, RenderBackend, RenderError, SearchHit, SearchOptions,
};

/// A search over one document and its results so far.
#[derive(Clone, Debug)]
pub struct DocSearch {
    query: String,
    options: SearchOptions,
    page_count: u32,
    next_page: u32,
    /// Hits in document order (page, then position on the page).
    hits: Vec<SearchHit>,
    current: Option<usize>,
    /// Hits of pages fed ahead of `next_page`, waiting for the pages before them.
    ahead: BTreeMap<u32, Vec<SearchHit>>,
}

impl DocSearch {
    /// A search that has not looked at any page yet.
    pub fn new(query: impl Into<String>, options: SearchOptions, page_count: u32) -> DocSearch {
        DocSearch {
            query: query.into(),
            options,
            page_count,
            next_page: 0,
            hits: Vec::new(),
            current: None,
            ahead: BTreeMap::new(),
        }
    }

    /// The first page that has not been searched yet (the page count once finished).
    pub fn next_page(&self) -> u32 {
        self.next_page.min(self.page_count)
    }

    /// Whether [`feed`](DocSearch::feed) still wants this page: it has not been searched and
    /// its text has not been fed yet.
    pub fn wants(&self, page: u32) -> bool {
        !self.is_blank()
            && page >= self.next_page
            && page < self.page_count
            && !self.ahead.contains_key(&page)
    }

    fn is_blank(&self) -> bool {
        self.query.trim().is_empty()
    }

    /// Searches one page from its text, without the backend: the text comes from a scheduler
    /// job (or a cache), so nothing here waits. Pages may be fed in any order; hits are still
    /// kept in document order, and the search advances as far as the fed pages reach without
    /// a gap. `None` stands for a page whose text could not be read: it has no hits and does
    /// not hold the search up. A page that is already searched is ignored.
    ///
    /// Returns `true` once every page has been searched.
    pub fn feed(&mut self, page: u32, text: Option<&PageText>) -> bool {
        if self.is_blank() {
            self.next_page = self.page_count;
            return true;
        }
        if self.wants(page) {
            let mut hits = text
                .map(|t| search_page_text(t, &self.query, self.options))
                .unwrap_or_default();
            for hit in &mut hits {
                hit.page = page;
            }
            self.ahead.insert(page, hits);
        }
        while let Some(hits) = self.ahead.remove(&self.next_page) {
            self.hits.extend(hits);
            self.next_page += 1;
        }
        self.is_finished()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn options(&self) -> SearchOptions {
        self.options
    }

    /// Searches up to `max_pages` more pages. Returns `true` once every page has been searched.
    /// A blank query finishes at once with no hits.
    pub fn step(
        &mut self,
        backend: &dyn RenderBackend,
        doc: DocId,
        max_pages: u32,
    ) -> Result<bool, RenderError> {
        if self.query.trim().is_empty() {
            self.next_page = self.page_count;
            return Ok(true);
        }
        let stop = self
            .next_page
            .saturating_add(max_pages)
            .min(self.page_count);
        while self.next_page < stop {
            let found = backend.search(doc, self.next_page, &self.query, self.options)?;
            self.hits.extend(found);
            self.next_page += 1;
        }
        Ok(self.is_finished())
    }

    /// Searches every remaining page.
    pub fn run(&mut self, backend: &dyn RenderBackend, doc: DocId) -> Result<(), RenderError> {
        self.step(backend, doc, u32::MAX).map(|_| ())
    }

    pub fn is_finished(&self) -> bool {
        self.next_page >= self.page_count
    }

    /// Pages searched so far and the total, for a progress display.
    pub fn progress(&self) -> (u32, u32) {
        (self.next_page.min(self.page_count), self.page_count)
    }

    /// Every hit found so far, in document order.
    pub fn hits(&self) -> &[SearchHit] {
        &self.hits
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    /// The hits on one page.
    pub fn hits_on_page(&self, page: u32) -> &[SearchHit] {
        let start = self.hits.partition_point(|h| h.page < page);
        let end = self.hits.partition_point(|h| h.page <= page);
        &self.hits[start..end]
    }

    /// Index of the current hit in [`hits`](DocSearch::hits).
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<&SearchHit> {
        self.current.and_then(|i| self.hits.get(i))
    }

    /// Makes hit `index` current.
    pub fn set_current(&mut self, index: usize) -> Option<&SearchHit> {
        if index < self.hits.len() {
            self.current = Some(index);
        }
        self.current()
    }

    /// Moves to the next hit, wrapping from the last to the first. With no current hit this
    /// selects the first one.
    pub fn next_hit(&mut self) -> Option<&SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        self.current = Some(match self.current {
            Some(i) => (i + 1) % self.hits.len(),
            None => 0,
        });
        self.current()
    }

    /// Moves to the previous hit, wrapping from the first to the last. With no current hit this
    /// selects the last one.
    pub fn previous_hit(&mut self) -> Option<&SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        let n = self.hits.len();
        self.current = Some(match self.current {
            Some(i) => (i + n - 1) % n,
            None => n - 1,
        });
        self.current()
    }

    /// Makes the first hit on or after `page` current (wrapping to the first hit of the
    /// document), which is where "find" should start when the user is in the middle of a file.
    pub fn select_first_from(&mut self, page: u32) -> Option<&SearchHit> {
        if self.hits.is_empty() {
            return None;
        }
        let i = self.hits.partition_point(|h| h.page < page);
        self.current = Some(if i < self.hits.len() { i } else { 0 });
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::synthetic_text;
    use pl_geom::PageGeometry;
    use pl_render::{MockBackend, Request};

    fn mock() -> (MockBackend, DocId) {
        let mock = MockBackend::with_pages(&[PageGeometry::simple(612.0, 792.0); 5]);
        mock.set_page_text(synthetic_text(0, &[("wall type A", [10.0, 100.0], 10.0)]));
        mock.set_page_text(synthetic_text(2, &[("Wall and wall", [10.0, 100.0], 10.0)]));
        mock.set_page_text(synthetic_text(4, &[("no match here", [10.0, 100.0], 10.0)]));
        let doc = mock.add_document();
        (mock, doc)
    }

    #[test]
    fn finds_hits_on_every_page_in_document_order() {
        let (mock, doc) = mock();
        let mut s = DocSearch::new("wall", SearchOptions::default(), 5);
        s.run(&mock, doc).unwrap();
        assert!(s.is_finished());
        let at: Vec<(u32, u32)> = s.hits().iter().map(|h| (h.page, h.char_start)).collect();
        assert_eq!(at, [(0, 0), (2, 0), (2, 9)]);
        assert_eq!(s.hits_on_page(2).len(), 2);
        assert_eq!(s.hits_on_page(1).len(), 0);
        assert_eq!(s.progress(), (5, 5));
    }

    #[test]
    fn next_and_previous_wrap() {
        let (mock, doc) = mock();
        let mut s = DocSearch::new("wall", SearchOptions::default(), 5);
        s.run(&mock, doc).unwrap();
        assert!(s.current().is_none());
        assert_eq!(s.next_hit().unwrap().page, 0);
        assert_eq!(s.next_hit().unwrap().char_start, 0);
        assert_eq!(s.next_hit().unwrap().char_start, 9);
        assert_eq!(s.current_index(), Some(2));
        assert_eq!(s.next_hit().unwrap().page, 0, "wraps to the first hit");
        assert_eq!(
            s.previous_hit().unwrap().char_start,
            9,
            "wraps to the last hit"
        );
        assert_eq!(s.previous_hit().unwrap().char_start, 0);
        assert_eq!(s.current().unwrap().page, 2);

        let mut fresh = DocSearch::new("wall", SearchOptions::default(), 5);
        fresh.run(&mock, doc).unwrap();
        assert_eq!(fresh.previous_hit().unwrap().char_start, 9);
        assert_eq!(fresh.select_first_from(1).unwrap().page, 2);
        assert_eq!(fresh.select_first_from(3).unwrap().page, 0);
        assert_eq!(fresh.set_current(1).unwrap().page, 2);
        assert_eq!(
            fresh.set_current(99).unwrap().page,
            2,
            "out of range is ignored"
        );
    }

    #[test]
    fn stepping_searches_a_few_pages_at_a_time() {
        let (mock, doc) = mock();
        let mut s = DocSearch::new("wall", SearchOptions::default(), 5);
        assert!(!s.step(&mock, doc, 2).unwrap());
        assert_eq!(s.progress(), (2, 5));
        assert_eq!(s.len(), 1);
        assert!(!s.step(&mock, doc, 2).unwrap());
        assert_eq!(s.len(), 3);
        assert!(s.step(&mock, doc, 2).unwrap());
        assert!(s.step(&mock, doc, 2).unwrap());
        let searched = mock
            .calls()
            .iter()
            .filter(|r| matches!(r, Request::Search { .. }))
            .count();
        assert_eq!(searched, 5, "each page is searched exactly once");
    }

    #[test]
    fn options_and_blank_queries() {
        let (mock, doc) = mock();
        let mut s = DocSearch::new(
            "Wall",
            SearchOptions {
                match_case: true,
                whole_word: false,
            },
            5,
        );
        s.run(&mock, doc).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.hits()[0].page, 2);

        let mut blank = DocSearch::new("  ", SearchOptions::default(), 5);
        assert!(blank.step(&mock, doc, 1).unwrap());
        assert!(blank.is_empty());
        assert!(blank.next_hit().is_none() && blank.previous_hit().is_none());
        assert!(blank.select_first_from(0).is_none());

        let mut none = DocSearch::new("zebra", SearchOptions::default(), 5);
        none.run(&mock, doc).unwrap();
        assert!(none.is_empty() && none.is_finished());
    }

    #[test]
    fn feeding_page_text_gives_the_same_hits_as_the_backend_in_any_order() {
        let (mock, doc) = mock();
        let mut by_backend = DocSearch::new("wall", SearchOptions::default(), 5);
        by_backend.run(&mock, doc).unwrap();

        let text = |page: u32| mock.page_text(doc, page).unwrap();
        let before = mock.calls().len();
        let mut fed = DocSearch::new("wall", SearchOptions::default(), 5);
        assert_eq!(fed.next_page(), 0);
        // Out of order: pages 2 and 4 arrive first and wait for the ones before them.
        assert!(!fed.feed(2, Some(&text(2))));
        assert!(!fed.feed(4, Some(&text(4))));
        assert!(
            fed.is_empty(),
            "no hit is shown before the pages ahead of it"
        );
        assert_eq!(fed.progress(), (0, 5));
        assert!(fed.wants(0) && fed.wants(1) && fed.wants(3));
        assert!(!fed.wants(2) && !fed.wants(4) && !fed.wants(5));
        assert!(!fed.feed(0, Some(&text(0))));
        assert_eq!(fed.progress(), (1, 5));
        assert_eq!(fed.len(), 1);
        // Page 1 failed to load: no hits, and the search moves on over the waiting page 2.
        assert!(!fed.feed(1, None));
        assert_eq!(fed.progress(), (3, 5));
        assert_eq!(fed.len(), 3);
        // A page that is already done is ignored, even with different text.
        assert!(!fed.feed(0, Some(&text(2))));
        assert_eq!(fed.len(), 3);
        assert!(fed.feed(3, Some(&text(3))));
        assert!(fed.is_finished());
        assert_eq!(fed.next_page(), 5);
        assert_eq!(fed.hits(), by_backend.hits());
        assert!(fed.hits().windows(2).all(|w| w[0].page <= w[1].page));
        // Feeding searched nothing through the backend (only the text was fetched above).
        assert!(mock.calls()[before..]
            .iter()
            .all(|r| matches!(r, Request::PageText { .. })));

        // A blank query is finished the moment anything is fed, and wants nothing.
        let mut blank = DocSearch::new("  ", SearchOptions::default(), 5);
        assert!(!blank.wants(0));
        assert!(blank.feed(0, None));
        assert!(blank.is_empty());
        // Feeding and stepping can be mixed: the backend picks up where the feed stopped.
        let mut mixed = DocSearch::new("wall", SearchOptions::default(), 5);
        mixed.feed(0, Some(&text(0)));
        mixed.run(&mock, doc).unwrap();
        assert_eq!(mixed.hits(), by_backend.hits());
    }

    #[test]
    fn a_backend_error_is_reported_and_the_search_can_be_retried() {
        let (mock, _) = mock();
        let mut s = DocSearch::new("wall", SearchOptions::default(), 5);
        assert!(s.run(&mock, DocId(999)).is_err());
        assert_eq!(s.progress(), (0, 5));
    }
}
