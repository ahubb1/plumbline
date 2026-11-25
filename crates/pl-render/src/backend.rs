//! The [`RenderBackend`] trait: one `call` that maps a plain [`Request`] to a plain [`Reply`],
//! plus typed convenience methods built on it.

use pl_geom::TileId;

use crate::types::*;

/// A PDF render engine behind a message boundary.
///
/// Implementations only have to provide [`call`](RenderBackend::call). Requests and replies are
/// plain serialisable data, so an implementation may forward them to another thread (as
/// [`PdfiumBackend`](crate::PdfiumBackend) does) or, later, to a worker process.
///
/// All coordinates in requests and replies are in unrotated page user space, y-up (the pl-geom
/// convention). Bitmaps are in display orientation: `/Rotate` applied, top-left of the crop box
/// at pixel (0, 0).
pub trait RenderBackend: Send + Sync {
    /// Runs one request to completion and returns its reply.
    fn call(&self, request: Request) -> Result<Reply, RenderError>;

    /// Opens a document.
    fn open(&self, source: DocSource, password: Option<&str>) -> Result<DocInfo, RenderError> {
        match self.call(Request::Open(OpenRequest {
            source,
            password: password.map(str::to_owned),
        }))? {
            Reply::Opened(info) => Ok(info),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Closes a document and releases its page handles and file mapping.
    fn close(&self, doc: DocId) -> Result<(), RenderError> {
        match self.call(Request::Close { doc })? {
            Reply::Done => Ok(()),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    fn page_count(&self, doc: DocId) -> Result<u32, RenderError> {
        match self.call(Request::PageCount { doc })? {
            Reply::PageCount(n) => Ok(n),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// The displayed size of every page, without loading any page (see
    /// [`Request::PageSizes`]).
    fn page_sizes(&self, doc: DocId) -> Result<Vec<Option<[f64; 2]>>, RenderError> {
        match self.call(Request::PageSizes { doc })? {
            Reply::PageSizes(sizes) => Ok(sizes),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// How many page loads the document has cost so far (see [`Request::PageLoads`]).
    fn page_loads(&self, doc: DocId) -> Result<u64, RenderError> {
        match self.call(Request::PageLoads { doc })? {
            Reply::PageLoads(n) => Ok(n),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Media box, crop box, rotation and user unit of a page. This loads the page.
    fn page_info(&self, doc: DocId, page: u32) -> Result<PageInfo, RenderError> {
        match self.call(Request::PageInfo { doc, page })? {
            Reply::PageInfo(info) => Ok(info),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Records a page's `/UserUnit` (see [`Request::SetPageUserUnit`]).
    fn set_page_user_unit(&self, doc: DocId, page: u32, user_unit: f64) -> Result<(), RenderError> {
        match self.call(Request::SetPageUserUnit {
            doc,
            page,
            user_unit,
        })? {
            Reply::Done => Ok(()),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Renders one tile with the default options (annotations and form fields drawn).
    fn render_tile(&self, doc: DocId, page: u32, tile: TileId) -> Result<Bitmap, RenderError> {
        self.render_tile_request(TileRequest::new(doc, page, tile))
    }

    fn render_tile_request(&self, request: TileRequest) -> Result<Bitmap, RenderError> {
        match self.call(Request::RenderTile(request))? {
            Reply::Bitmap(b) => Ok(b),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Renders the whole page at `scale` pixels per user-space unit.
    fn render_page(&self, doc: DocId, page: u32, scale: f64) -> Result<Bitmap, RenderError> {
        self.render_page_with(doc, page, scale, RenderOptions::default())
    }

    fn render_page_with(
        &self,
        doc: DocId,
        page: u32,
        scale: f64,
        options: RenderOptions,
    ) -> Result<Bitmap, RenderError> {
        match self.call(Request::RenderPage {
            doc,
            page,
            scale,
            options,
        })? {
            Reply::Bitmap(b) => Ok(b),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Renders the whole page so that its longer side is `max_px` pixels.
    fn thumbnail(&self, doc: DocId, page: u32, max_px: u32) -> Result<Bitmap, RenderError> {
        match self.call(Request::Thumbnail { doc, page, max_px })? {
            Reply::Bitmap(b) => Ok(b),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// The page's characters with their boxes in page space.
    fn page_text(&self, doc: DocId, page: u32) -> Result<PageText, RenderError> {
        match self.call(Request::PageText { doc, page })? {
            Reply::Text(t) => Ok(t),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Searches one page. Callers walk the pages themselves so that a long search never blocks
    /// tile rendering for more than one page's worth of work.
    fn search(
        &self,
        doc: DocId,
        page: u32,
        query: &str,
        options: SearchOptions,
    ) -> Result<Vec<SearchHit>, RenderError> {
        match self.call(Request::Search {
            doc,
            page,
            query: query.to_owned(),
            options,
        })? {
            Reply::Hits(h) => Ok(h),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// The bookmark tree, flattened in document order with a level per item.
    fn outline(&self, doc: DocId) -> Result<Vec<OutlineItem>, RenderError> {
        match self.call(Request::Outline { doc })? {
            Reply::Outline(o) => Ok(o),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// The annotations of a page as the render mirror currently holds them.
    fn annotations(&self, doc: DocId, page: u32) -> Result<Vec<AnnotInfo>, RenderError> {
        match self.call(Request::Annotations { doc, page })? {
            Reply::Annotations(a) => Ok(a),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Hides or shows one annotation in the in-memory mirror; the file is not touched.
    fn set_annotation_hidden(
        &self,
        doc: DocId,
        page: u32,
        index: u32,
        hidden: bool,
    ) -> Result<(), RenderError> {
        match self.call(Request::SetAnnotationHidden {
            doc,
            page,
            index,
            hidden,
        })? {
            Reply::Done => Ok(()),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Gives one annotation a new rectangle in the in-memory mirror (see
    /// [`Request::SetAnnotationRect`]); the file is not touched.
    fn set_annotation_rect(
        &self,
        doc: DocId,
        page: u32,
        index: u32,
        rect: RectArray,
    ) -> Result<(), RenderError> {
        match self.call(Request::SetAnnotationRect {
            doc,
            page,
            index,
            rect,
        })? {
            Reply::Done => Ok(()),
            _ => Err(RenderError::UnexpectedReply),
        }
    }

    /// Renders a window of a page drawn at any size and extra rotation (see
    /// [`WindowRequest`]); the print path's band render.
    fn render_window(&self, request: WindowRequest) -> Result<WindowReply, RenderError> {
        match self.call(Request::RenderWindow(request))? {
            Reply::Window(w) => Ok(w),
            _ => Err(RenderError::UnexpectedReply),
        }
    }
}

impl<T: RenderBackend + ?Sized> RenderBackend for std::sync::Arc<T> {
    fn call(&self, request: Request) -> Result<Reply, RenderError> {
        (**self).call(request)
    }
}
