//! Requests to the render backend that are not tiles: page text, page geometry, the outline.
//!
//! The backend is a single thread that may be in the middle of a long tile render. The window
//! never calls it and waits; everything goes to the scheduler as a job and comes back on the
//! result channel a frame or more later. [`Queries`] remembers what has been asked for so each
//! thing is asked for once.

use std::collections::{HashMap, HashSet};

use pl_doc::pl_render::{DocId, JobKind, Scheduler, StreamId};

/// The scheduler stream queries are queued on. It is never advanced, so scrolling a view does
/// not cancel a text request another view is waiting for.
pub const QUERY_STREAM: StreamId = StreamId(u32::MAX);

/// Order within the query class: what the pointer is on comes before a running search.
const ORDER_NOW: u32 = 0;
const ORDER_SEARCH: u32 = 1;

/// What has been asked of the backend and not answered yet, and what it could not answer.
#[derive(Default)]
pub struct Queries {
    text: HashSet<(DocId, u32)>,
    info: HashSet<(DocId, u32)>,
    outline: HashSet<DocId>,
    failed_text: HashSet<(DocId, u32)>,
    failed_info: HashSet<(DocId, u32)>,
    failed_outline: HashMap<DocId, String>,
}

impl Queries {
    /// Asks for a page's text unless that is already under way or has failed. Returns `false`
    /// when the page's text cannot be had (the request failed before).
    pub fn want_text(&mut self, scheduler: &Scheduler, doc: DocId, page: u32) -> bool {
        self.ask_text(scheduler, doc, page, ORDER_NOW)
    }

    /// As [`want_text`](Queries::want_text), for a search walking the document: queued behind
    /// requests for what the user is pointing at.
    pub fn want_search_text(&mut self, scheduler: &Scheduler, doc: DocId, page: u32) -> bool {
        self.ask_text(scheduler, doc, page, ORDER_SEARCH)
    }

    fn ask_text(&mut self, scheduler: &Scheduler, doc: DocId, page: u32, order: u32) -> bool {
        if self.failed_text.contains(&(doc, page)) {
            return false;
        }
        if self.text.insert((doc, page))
            && !scheduler.submit_query(QUERY_STREAM, JobKind::PageText { doc, page }, order)
        {
            self.text.remove(&(doc, page));
            return false;
        }
        true
    }

    /// Asks for a page's real geometry unless that is already under way or has failed.
    pub fn want_info(&mut self, scheduler: &Scheduler, doc: DocId, page: u32) {
        self.ask_info(scheduler, doc, page, ORDER_NOW);
    }

    /// As [`want_info`](Queries::want_info), for a page a search is reading. Queued right
    /// behind that page's text request, so the backend answers both from the one page load.
    /// A hit's quads are in page coordinates, so its page's geometry is wanted with it.
    pub fn want_search_info(&mut self, scheduler: &Scheduler, doc: DocId, page: u32) {
        self.ask_info(scheduler, doc, page, ORDER_SEARCH);
    }

    fn ask_info(&mut self, scheduler: &Scheduler, doc: DocId, page: u32, order: u32) {
        if self.failed_info.contains(&(doc, page)) || !self.info.insert((doc, page)) {
            return;
        }
        if !scheduler.submit_query(QUERY_STREAM, JobKind::PageInfo { doc, page }, order) {
            self.info.remove(&(doc, page));
        }
    }

    /// Asks for the document's outline unless that is already under way or has failed.
    pub fn want_outline(&mut self, scheduler: &Scheduler, doc: DocId) {
        if self.failed_outline.contains_key(&doc) || !self.outline.insert(doc) {
            return;
        }
        if !scheduler.submit_query(QUERY_STREAM, JobKind::Outline { doc }, ORDER_NOW) {
            self.outline.remove(&doc);
        }
    }

    /// Records that a job has come back, with its error if it failed.
    pub fn done(&mut self, kind: &JobKind, error: Option<String>) {
        match *kind {
            JobKind::PageText { doc, page } => {
                self.text.remove(&(doc, page));
                if error.is_some() {
                    self.failed_text.insert((doc, page));
                }
            }
            JobKind::PageInfo { doc, page } => {
                self.info.remove(&(doc, page));
                if error.is_some() {
                    self.failed_info.insert((doc, page));
                }
            }
            JobKind::Outline { doc } => {
                self.outline.remove(&doc);
                if let Some(error) = error {
                    self.failed_outline.insert(doc, error);
                }
            }
            JobKind::Tile { .. } | JobKind::Thumbnail { .. } => {}
        }
    }

    /// Whether any text request for this document is still out.
    pub fn text_pending_for(&self, doc: DocId) -> bool {
        self.text.iter().any(|(d, _)| *d == doc)
    }

    /// Why the outline could not be read, if it could not.
    pub fn outline_error(&self, doc: DocId) -> Option<&str> {
        self.failed_outline.get(&doc).map(String::as_str)
    }

    /// Requests that have not been answered yet.
    pub fn pending(&self) -> usize {
        self.text.len() + self.info.len() + self.outline.len()
    }

    /// Forgets everything about a closed (or reopened) render document.
    pub fn forget_doc(&mut self, doc: DocId) {
        self.text.retain(|(d, _)| *d != doc);
        self.info.retain(|(d, _)| *d != doc);
        self.outline.remove(&doc);
        self.failed_text.retain(|(d, _)| *d != doc);
        self.failed_info.retain(|(d, _)| *d != doc);
        self.failed_outline.remove(&doc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_doc::pl_render::{MockBackend, Request, TileCache};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn each_thing_is_asked_for_once_and_failures_are_remembered() {
        let mock = Arc::new(MockBackend::new());
        let doc = mock.add_document();
        let (scheduler, results) = Scheduler::new(mock.clone(), Arc::new(TileCache::new(1 << 20)));
        let mut q = Queries::default();
        scheduler.pause();
        assert!(q.want_text(&scheduler, doc, 0));
        assert!(q.want_search_text(&scheduler, doc, 0));
        assert!(q.want_text(&scheduler, doc, 5)); // no such page: will fail
        q.want_info(&scheduler, doc, 0);
        q.want_info(&scheduler, doc, 0);
        q.want_outline(&scheduler, doc);
        q.want_outline(&scheduler, doc);
        assert_eq!(q.pending(), 4);
        assert_eq!(scheduler.queued(), 4);
        assert!(q.text_pending_for(doc));
        scheduler.resume();
        assert!(scheduler.wait_idle(Duration::from_secs(20)));
        for result in results.try_iter() {
            q.done(
                &result.job.kind,
                result.outcome.as_ref().err().map(|e| e.to_string()),
            );
        }
        assert_eq!(q.pending(), 0);
        assert!(!q.text_pending_for(doc));
        // The page that failed is not asked for again; the others can be (their answers may
        // have been dropped from a cache since).
        assert!(!q.want_text(&scheduler, doc, 5));
        assert!(q.want_text(&scheduler, doc, 0));
        assert!(scheduler.wait_idle(Duration::from_secs(20)));
        let texts = mock
            .calls()
            .iter()
            .filter(|r| matches!(r, Request::PageText { .. }))
            .count();
        assert_eq!(texts, 3);
        q.forget_doc(doc);
        assert_eq!(q.pending(), 0);
        assert!(
            q.want_text(&scheduler, doc, 5),
            "forgotten with the document"
        );
        assert!(q.outline_error(doc).is_none());
    }
}
