//! Page-handle bookkeeping through real PDFium. This file holds one test so that it has a
//! process, and therefore a PDFium thread and a render LRU, to itself: the counts asserted
//! here would be disturbed by other tests rendering at the same time.

mod common;

use common::*;
use pl_geom::TileId;
use pl_render::RenderBackend;

#[test]
fn text_and_search_do_not_make_the_rendered_page_load_again() {
    // The page-handle LRU exists so a dense sheet is parsed once. A search across the
    // document, or text for many other pages, must not evict the sheet being viewed.
    let fixture = pl_testkit::multi_page();
    let backend = backend();
    let doc = open(&backend, &fixture);
    assert!(fixture.manifest.page_count() > pl_render::PAGE_HANDLE_LRU + 4);
    let tile = TileId {
        level: 0,
        x: 0,
        y: 0,
    };
    backend.render_tile(doc, 0, tile).unwrap();
    assert_eq!(backend.page_loads(doc).unwrap(), 1);

    let others = (pl_render::PAGE_HANDLE_LRU + 4) as u32;
    for page in 1..=others {
        backend.page_text(doc, page).unwrap();
        backend
            .search(doc, page, "plumb", Default::default())
            .unwrap();
        backend.annotations(doc, page).unwrap();
    }
    // One load per other page, although each was touched three times.
    assert_eq!(backend.page_loads(doc).unwrap(), 1 + others as u64);

    // The viewed page is still open: its next tile, its text and its geometry load nothing.
    backend.render_tile(doc, 0, tile).unwrap();
    backend.page_text(doc, 0).unwrap();
    backend.page_info(doc, 0).unwrap();
    assert_eq!(backend.page_loads(doc).unwrap(), 1 + others as u64);

    // Rendering does evict rendering's own oldest page, as before.
    for page in 1..=(pl_render::PAGE_HANDLE_LRU as u32) {
        backend.render_tile(doc, page, tile).unwrap();
    }
    let before = backend.page_loads(doc).unwrap();
    backend.render_tile(doc, 0, tile).unwrap();
    assert_eq!(backend.page_loads(doc).unwrap(), before + 1);
    backend.close(doc).unwrap();
}
