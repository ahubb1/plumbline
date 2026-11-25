//! [`MockBackend`]: a deterministic in-memory backend for tests of the scheduler, the cache and
//! anything above this crate that must not need PDFium.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use pl_geom::{PageGeometry, TileId};

use crate::backend::RenderBackend;
use crate::text::search_page_text;
use crate::types::*;

struct MockDoc {
    pages: Vec<PageInfo>,
    hidden: HashMap<(u32, u32), bool>,
    /// Rectangles given by `SetAnnotationRect`.
    rects: HashMap<(u32, u32), RectArray>,
    /// Requests that would make a real engine load a page.
    page_loads: u64,
}

#[derive(Default)]
struct MockState {
    next_id: u64,
    docs: HashMap<DocId, MockDoc>,
    default_pages: Vec<PageInfo>,
    texts: HashMap<u32, PageText>,
    outline: Vec<OutlineItem>,
    log: Vec<Request>,
    paused: bool,
    paused_all: bool,
    fail_renders: bool,
    delay: Duration,
}

/// A fake render backend.
///
/// * Every `Open` succeeds and yields a document with the configured pages (one Letter page by
///   default); the source is ignored.
/// * Pixels are a pure function of the page index and the pixel's position on the whole page
///   ([`MockBackend::pixel`]), so tiles stitch into exactly the full-page render.
/// * Every request is recorded; [`MockBackend::rendered`] lists the render requests in the
///   order they were executed, which is what cancellation tests assert on.
/// * [`MockBackend::pause`] makes render requests block until [`MockBackend::resume`].
pub struct MockBackend {
    state: Mutex<MockState>,
    gate: Condvar,
}

impl Default for MockBackend {
    fn default() -> Self {
        MockBackend::new()
    }
}

impl MockBackend {
    /// A backend whose documents have one US Letter page.
    pub fn new() -> MockBackend {
        MockBackend::with_pages(&[PageGeometry::simple(612.0, 792.0)])
    }

    /// A backend whose documents have the given pages.
    pub fn with_pages(pages: &[PageGeometry]) -> MockBackend {
        MockBackend {
            state: Mutex::new(MockState {
                next_id: 1,
                default_pages: pages.iter().map(PageInfo::from_geometry).collect(),
                ..MockState::default()
            }),
            gate: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens a document without going through a request.
    pub fn add_document(&self) -> DocId {
        let mut s = self.lock();
        let id = DocId(s.next_id);
        s.next_id += 1;
        let pages = s.default_pages.clone();
        s.docs.insert(
            id,
            MockDoc {
                pages,
                hidden: HashMap::new(),
                rects: HashMap::new(),
                page_loads: 0,
            },
        );
        id
    }

    /// Sets the text every document reports for a page.
    pub fn set_page_text(&self, text: PageText) {
        self.lock().texts.insert(text.page, text);
    }

    /// Sets the outline every document reports.
    pub fn set_outline(&self, outline: Vec<OutlineItem>) {
        self.lock().outline = outline;
    }

    /// Makes render requests block until [`resume`](MockBackend::resume) is called.
    pub fn pause(&self) {
        self.lock().paused = true;
    }

    /// Makes *every* request block until [`resume`](MockBackend::resume) is called, as a
    /// single-threaded engine does while it is busy with one long render. A test uses it to
    /// show that some code path does not call the backend directly.
    pub fn pause_all(&self) {
        let mut s = self.lock();
        s.paused = true;
        s.paused_all = true;
    }

    pub fn resume(&self) {
        let mut s = self.lock();
        s.paused = false;
        s.paused_all = false;
        drop(s);
        self.gate.notify_all();
    }

    /// Makes every render request take at least this long.
    pub fn set_render_delay(&self, delay: Duration) {
        self.lock().delay = delay;
    }

    /// Makes every render request fail with a backend error.
    pub fn set_fail_renders(&self, fail: bool) {
        self.lock().fail_renders = fail;
    }

    /// Every request received so far, in order.
    pub fn calls(&self) -> Vec<Request> {
        self.lock().log.clone()
    }

    /// The render requests (tile, page, thumbnail, print window) executed so far, in order. A request that is
    /// blocked by [`pause`](MockBackend::pause) is listed only once it has actually rendered.
    pub fn rendered(&self) -> Vec<Request> {
        self.lock()
            .log
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Request::RenderTile(_)
                        | Request::RenderPage { .. }
                        | Request::Thumbnail { .. }
                        | Request::RenderWindow(_)
                )
            })
            .cloned()
            .collect()
    }

    /// The tiles rendered so far, in order.
    pub fn rendered_tiles(&self) -> Vec<TileRequest> {
        self.lock()
            .log
            .iter()
            .filter_map(|r| match r {
                Request::RenderTile(t) => Some(*t),
                _ => None,
            })
            .collect()
    }

    /// The colour of the whole-page pixel `(gx, gy)` on a page, at any scale.
    pub fn pixel(page: u32, gx: u32, gy: u32) -> [u8; 4] {
        [
            gx as u8,
            gy as u8,
            ((gx >> 8) * 7 + (gy >> 8) * 13 + page) as u8,
            255,
        ]
    }

    fn fill(page: u32, x0: u32, y0: u32, width: u32, height: u32) -> Bitmap {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&Self::pixel(page, x0 + x, y0 + y));
            }
        }
        Bitmap {
            width,
            height,
            pixels,
        }
    }

    fn page_info(s: &MockState, doc: DocId, page: u32) -> Result<PageInfo, RenderError> {
        let d = s.docs.get(&doc).ok_or(RenderError::UnknownDocument(doc))?;
        d.pages
            .get(page as usize)
            .copied()
            .ok_or(RenderError::PageOutOfRange {
                page,
                count: d.pages.len() as u32,
            })
    }

    /// Waits out a pause, applies the delay, then records the request as rendered.
    fn begin_render(&self, request: &Request) -> Result<(), RenderError> {
        let mut s = self.lock();
        while s.paused {
            s = self.gate.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        let delay = s.delay;
        let fail = s.fail_renders;
        s.log.push(request.clone());
        drop(s);
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        if fail {
            return Err(RenderError::Backend("mock render failure".into()));
        }
        Ok(())
    }
}

impl RenderBackend for MockBackend {
    fn call(&self, request: Request) -> Result<Reply, RenderError> {
        match &request {
            Request::RenderTile(t) => {
                let info = Self::page_info(&self.lock(), t.doc, t.page)?;
                let (_, rect) = tile_grid_for(&info.geometry(), t.tile())?;
                self.begin_render(&request)?;
                return Ok(Reply::Bitmap(Self::fill(
                    t.page,
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                )));
            }
            Request::RenderPage {
                doc, page, scale, ..
            } => {
                let info = Self::page_info(&self.lock(), *doc, *page)?;
                if !scale.is_finite() || *scale <= 0.0 {
                    return Err(RenderError::InvalidRequest("scale must be positive".into()));
                }
                let (w, h) = page_pixel_size(&info.geometry(), *scale);
                check_bitmap_size(w, h)?;
                self.begin_render(&request)?;
                return Ok(Reply::Bitmap(Self::fill(*page, 0, 0, w, h)));
            }
            Request::Thumbnail { doc, page, max_px } => {
                let info = Self::page_info(&self.lock(), *doc, *page)?;
                let geometry = info.geometry();
                let (w, h) = page_pixel_size(&geometry, thumbnail_scale(&geometry, *max_px));
                check_bitmap_size(w, h)?;
                self.begin_render(&request)?;
                return Ok(Reply::Bitmap(Self::fill(*page, 0, 0, w, h)));
            }
            Request::RenderWindow(w) => {
                Self::page_info(&self.lock(), w.doc, w.page)?;
                w.validate()?;
                self.begin_render(&request)?;
                // The same pure function of the position in the whole page image as a tile, so
                // windows stitch exactly; the extra rotation and the options are not drawn.
                let [x, y, width, height] = w.window;
                let bitmap = Self::fill(w.page, x, y, width, height);
                let scratch_bytes = bitmap.byte_len() as u64;
                return Ok(Reply::Window(WindowReply {
                    bitmap,
                    scratch_bytes,
                }));
            }
            _ => {}
        }

        let mut s = self.lock();
        while s.paused_all {
            s = self.gate.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        s.log.push(request.clone());
        // The mock keeps no page handles: every request a real engine needs a page for counts
        // as a load.
        if let Request::PageInfo { doc, .. }
        | Request::PageText { doc, .. }
        | Request::Search { doc, .. }
        | Request::Annotations { doc, .. } = &request
        {
            if let Some(d) = s.docs.get_mut(doc) {
                d.page_loads += 1;
            }
        }
        match request {
            Request::Open(_) => {
                let id = DocId(s.next_id);
                s.next_id += 1;
                let pages = s.default_pages.clone();
                let page_count = pages.len() as u32;
                s.docs.insert(
                    id,
                    MockDoc {
                        pages,
                        hidden: HashMap::new(),
                        rects: HashMap::new(),
                        page_loads: 0,
                    },
                );
                Ok(Reply::Opened(DocInfo {
                    doc: id,
                    page_count,
                }))
            }
            Request::Close { doc } => s
                .docs
                .remove(&doc)
                .map(|_| Reply::Done)
                .ok_or(RenderError::UnknownDocument(doc)),
            Request::PageCount { doc } => s
                .docs
                .get(&doc)
                .map(|d| Reply::PageCount(d.pages.len() as u32))
                .ok_or(RenderError::UnknownDocument(doc)),
            Request::PageInfo { doc, page } => Self::page_info(&s, doc, page).map(Reply::PageInfo),
            Request::PageSizes { doc } => s
                .docs
                .get(&doc)
                .map(|d| {
                    Reply::PageSizes(
                        d.pages
                            .iter()
                            .map(|p| {
                                let size = p.geometry().display_size();
                                Some([size.width, size.height])
                            })
                            .collect(),
                    )
                })
                .ok_or(RenderError::UnknownDocument(doc)),
            Request::PageLoads { doc } => s
                .docs
                .get(&doc)
                .map(|d| Reply::PageLoads(d.page_loads))
                .ok_or(RenderError::UnknownDocument(doc)),
            Request::SetPageUserUnit {
                doc,
                page,
                user_unit,
            } => {
                Self::page_info(&s, doc, page)?;
                if !user_unit.is_finite() || user_unit <= 0.0 {
                    return Err(RenderError::InvalidRequest("bad user unit".into()));
                }
                let d = s
                    .docs
                    .get_mut(&doc)
                    .ok_or(RenderError::UnknownDocument(doc))?;
                d.pages[page as usize].user_unit = user_unit;
                Ok(Reply::Done)
            }
            Request::PageText { doc, page } => {
                Self::page_info(&s, doc, page)?;
                let text = s.texts.get(&page).cloned().unwrap_or(PageText {
                    page,
                    chars: Vec::new(),
                });
                Ok(Reply::Text(text))
            }
            Request::Search {
                doc,
                page,
                query,
                options,
            } => {
                Self::page_info(&s, doc, page)?;
                let hits = s
                    .texts
                    .get(&page)
                    .map(|t| search_page_text(t, &query, options))
                    .unwrap_or_default();
                Ok(Reply::Hits(hits))
            }
            Request::Outline { doc } => {
                if !s.docs.contains_key(&doc) {
                    return Err(RenderError::UnknownDocument(doc));
                }
                Ok(Reply::Outline(s.outline.clone()))
            }
            Request::Annotations { doc, page } => {
                Self::page_info(&s, doc, page)?;
                let d = &s.docs[&doc];
                // Every annotation a request has named, hidden or moved.
                let mut indices: Vec<u32> = d
                    .hidden
                    .keys()
                    .chain(d.rects.keys())
                    .filter(|(p, _)| *p == page)
                    .map(|(_, index)| *index)
                    .collect();
                indices.sort_unstable();
                indices.dedup();
                let mut list: Vec<AnnotInfo> = indices
                    .into_iter()
                    .map(|index| AnnotInfo {
                        index,
                        subtype: "Mock".into(),
                        rect: d.rects.get(&(page, index)).copied().unwrap_or([0.0; 4]),
                        hidden: d.hidden.get(&(page, index)).copied().unwrap_or(false),
                        name: None,
                    })
                    .collect();
                list.sort_by_key(|a| a.index);
                Ok(Reply::Annotations(list))
            }
            Request::SetAnnotationHidden {
                doc,
                page,
                index,
                hidden,
            } => {
                Self::page_info(&s, doc, page)?;
                let d = s
                    .docs
                    .get_mut(&doc)
                    .ok_or(RenderError::UnknownDocument(doc))?;
                d.hidden.insert((page, index), hidden);
                Ok(Reply::Done)
            }
            Request::SetAnnotationRect {
                doc,
                page,
                index,
                rect,
            } => {
                Self::page_info(&s, doc, page)?;
                if rect.iter().any(|v| !v.is_finite()) {
                    return Err(RenderError::InvalidRequest(
                        "the annotation rectangle is not finite".into(),
                    ));
                }
                let d = s
                    .docs
                    .get_mut(&doc)
                    .ok_or(RenderError::UnknownDocument(doc))?;
                d.rects.insert((page, index), rect);
                Ok(Reply::Done)
            }
            Request::RenderTile(_)
            | Request::RenderPage { .. }
            | Request::Thumbnail { .. }
            | Request::RenderWindow(_) => {
                unreachable!("handled above")
            }
        }
    }
}

/// The tile ids of a request list, for assertions.
pub fn tiles_of(requests: &[TileRequest]) -> Vec<TileId> {
    requests.iter().map(TileRequest::tile).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::synthetic_text;
    use pl_geom::TileGrid;

    #[test]
    fn mock_tiles_stitch_into_the_full_page() {
        let mock = MockBackend::new();
        let doc = mock.open(DocSource::Bytes(Vec::new()), None).unwrap().doc;
        let geometry = mock.page_info(doc, 0).unwrap().geometry();
        let grid = TileGrid::new(&geometry, 1);
        let full = mock.render_page(doc, 0, 2.0).unwrap();
        assert_eq!(
            (full.width, full.height),
            (grid.width_px(), grid.height_px())
        );
        let mut stitched = Bitmap::filled(full.width, full.height, [0; 4]);
        for id in grid.all().iter() {
            let r = grid.tile_pixel_rect(id).unwrap();
            let tile = mock.render_tile(doc, 0, id).unwrap();
            assert_eq!((tile.width, tile.height), (r.width, r.height));
            stitched.blit(&tile, r.x, r.y);
        }
        assert_eq!(stitched, full);
        assert_eq!(mock.rendered().len(), 1 + grid.tile_count() as usize);
    }

    #[test]
    fn mock_reports_errors_like_a_real_backend() {
        let mock = MockBackend::new();
        let doc = mock.add_document();
        assert_eq!(mock.page_count(doc).unwrap(), 1);
        assert_eq!(
            mock.page_info(doc, 5),
            Err(RenderError::PageOutOfRange { page: 5, count: 1 })
        );
        assert_eq!(
            mock.page_count(DocId(99)),
            Err(RenderError::UnknownDocument(DocId(99)))
        );
        let far = TileId {
            level: 0,
            x: 50,
            y: 0,
        };
        assert_eq!(
            mock.render_tile(doc, 0, far),
            Err(RenderError::TileOutOfRange)
        );
        mock.close(doc).unwrap();
        assert!(mock.close(doc).is_err());
    }

    #[test]
    fn a_moved_annotation_reports_its_new_rectangle() {
        let mock = MockBackend::new();
        let doc = mock.add_document();
        mock.set_annotation_rect(doc, 0, 2, [10.0, 20.0, 110.0, 80.0])
            .unwrap();
        let list = mock.annotations(doc, 0).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].index, list[0].hidden), (2, false));
        assert_eq!(list[0].rect, [10.0, 20.0, 110.0, 80.0]);
        // Hiding the same annotation keeps its rectangle, and the other way round.
        mock.set_annotation_hidden(doc, 0, 2, true).unwrap();
        mock.set_annotation_rect(doc, 0, 2, [0.0, 0.0, 5.0, 5.0])
            .unwrap();
        let list = mock.annotations(doc, 0).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].hidden);
        assert_eq!(list[0].rect, [0.0, 0.0, 5.0, 5.0]);
        assert!(matches!(
            mock.set_annotation_rect(doc, 0, 2, [0.0, f64::NAN, 5.0, 5.0]),
            Err(RenderError::InvalidRequest(_))
        ));
        assert!(matches!(
            mock.set_annotation_rect(doc, 7, 0, [0.0; 4]),
            Err(RenderError::PageOutOfRange { .. })
        ));
        assert!(mock
            .calls()
            .iter()
            .any(|r| matches!(r, Request::SetAnnotationRect { index: 2, .. })));
    }

    #[test]
    fn mock_text_search_and_user_unit() {
        let mock = MockBackend::new();
        let doc = mock.add_document();
        mock.set_page_text(synthetic_text(0, &[("Door D-12", [50.0, 500.0], 10.0)]));
        let hits = mock
            .search(doc, 0, "d-12", SearchOptions::default())
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].char_start, 5);
        assert_eq!(mock.page_text(doc, 0).unwrap().text(), "Door D-12");
        mock.set_page_user_unit(doc, 0, 2.0).unwrap();
        assert_eq!(mock.page_info(doc, 0).unwrap().user_unit, 2.0);
        mock.set_annotation_hidden(doc, 0, 3, true).unwrap();
        assert!(mock.annotations(doc, 0).unwrap()[0].hidden);
    }
}
