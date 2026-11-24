//! GPU textures for tiles and thumbnails.

use std::collections::{HashMap, HashSet};

use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};
use pl_doc::pl_render::{Bitmap, DocId, Scheduler, StreamId, TileCache, TileKey};

/// Tile textures kept beyond those drawn in the current frame.
pub const MAX_TILE_TEXTURES: usize = 192;
/// The longest side of a thumbnail, in pixels.
pub const THUMBNAIL_PX: u32 = 192;
/// The scheduler stream thumbnails are queued on. It is never advanced, so thumbnails are not
/// cancelled by scrolling a view.
pub const THUMBNAIL_STREAM: StreamId = StreamId(0);

/// Textures uploaded from rendered bitmaps. Tiles are looked up by the tile cache's own key, so
/// a new page revision or layer state simply misses; the stale textures of a changed page are
/// dropped at once ([`TextureStore::forget_stale`]).
#[derive(Default)]
pub struct TextureStore {
    tiles: HashMap<TileKey, (TextureHandle, u64)>,
    thumbs: HashMap<(DocId, u32), TextureHandle>,
    thumbs_requested: HashSet<(DocId, u32)>,
}

fn upload(ctx: &egui::Context, name: String, bitmap: &Bitmap) -> Option<TextureHandle> {
    let (w, h) = (bitmap.width as usize, bitmap.height as usize);
    if w == 0 || h == 0 || bitmap.pixels.len() != w * h * 4 {
        return None;
    }
    // Rendered bitmaps are opaque, so their bytes are already premultiplied.
    let image = ColorImage::from_rgba_premultiplied([w, h], &bitmap.pixels);
    Some(ctx.load_texture(name, image, TextureOptions::LINEAR))
}

impl TextureStore {
    /// The texture for a tile, uploading it if the tile cache has the bitmap. `None` while the
    /// tile has not been rendered.
    pub fn tile(
        &mut self,
        ctx: &egui::Context,
        cache: &TileCache,
        key: &TileKey,
        frame: u64,
    ) -> Option<TextureId> {
        if let Some((texture, used)) = self.tiles.get_mut(key) {
            *used = frame;
            return Some(texture.id());
        }
        let bitmap = cache.get(key)?;
        self.tile_from(ctx, key, &bitmap, frame)
    }

    /// The texture for a tile whose bitmap is already in hand.
    pub fn tile_from(
        &mut self,
        ctx: &egui::Context,
        key: &TileKey,
        bitmap: &Bitmap,
        frame: u64,
    ) -> Option<TextureId> {
        if let Some((texture, used)) = self.tiles.get_mut(key) {
            *used = frame;
            return Some(texture.id());
        }
        let name = format!(
            "tile-{}-{}-{}-{}-{}",
            key.doc.0, key.page, key.level, key.x, key.y
        );
        let texture = upload(ctx, name, bitmap)?;
        let id = texture.id();
        self.tiles.insert(*key, (texture, frame));
        Some(id)
    }

    /// Frees the least recently drawn tile textures beyond the budget. Textures drawn in
    /// `frame` are always kept.
    pub fn end_frame(&mut self, frame: u64) {
        if self.tiles.len() <= MAX_TILE_TEXTURES {
            return;
        }
        let mut idle: Vec<(u64, TileKey)> = self
            .tiles
            .iter()
            .filter(|(_, (_, used))| *used < frame)
            .map(|(key, (_, used))| (*used, *key))
            .collect();
        idle.sort_unstable();
        let excess = self.tiles.len() - MAX_TILE_TEXTURES;
        for (_, key) in idle.into_iter().take(excess) {
            self.tiles.remove(&key);
        }
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// A page's thumbnail, if it has arrived.
    pub fn thumb(&self, doc: DocId, page: u32) -> Option<TextureId> {
        self.thumbs.get(&(doc, page)).map(TextureHandle::id)
    }

    /// Asks for a page's thumbnail once.
    pub fn request_thumb(&mut self, scheduler: &Scheduler, doc: DocId, page: u32) {
        if self.thumbs_requested.insert((doc, page)) {
            scheduler.submit_thumbnail(THUMBNAIL_STREAM, doc, page, THUMBNAIL_PX, page);
        }
    }

    pub fn set_thumb(&mut self, ctx: &egui::Context, doc: DocId, page: u32, bitmap: &Bitmap) {
        if let Some(texture) = upload(ctx, format!("thumb-{}-{page}", doc.0), bitmap) {
            self.thumbs.insert((doc, page), texture);
        }
    }

    pub fn thumb_count(&self) -> usize {
        self.thumbs.len()
    }

    /// Drops the tile textures of one page that are not of its current content revision.
    pub fn forget_stale(&mut self, doc: DocId, page: u32, current: u64) {
        self.tiles
            .retain(|key, _| key.doc != doc || key.page != page || key.revision == current);
    }

    /// Drops every tile texture of one page (tiles under an edited annotation were removed
    /// from the cache; the others are uploaded again from it).
    pub fn forget_page(&mut self, doc: DocId, page: u32) {
        self.tiles
            .retain(|key, _| key.doc != doc || key.page != page);
    }

    /// Drops everything that belongs to a closed render document.
    pub fn forget_doc(&mut self, doc: DocId) {
        self.tiles.retain(|key, _| key.doc != doc);
        self.thumbs.retain(|(d, _), _| *d != doc);
        self.thumbs_requested.retain(|(d, _)| *d != doc);
    }
}
