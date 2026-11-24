//! A byte-budgeted LRU cache of rendered tiles.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use pl_geom::{PageGeometry, Rect, TileGrid, TileId};

use crate::types::{Bitmap, DocId, RenderOptions};

/// The default budget: 512 MB.
pub const DEFAULT_CACHE_BUDGET: usize = 512 * 1024 * 1024;

/// Bookkeeping bytes charged per entry on top of its pixels.
pub const ENTRY_OVERHEAD: usize = 128;

/// Identifies one rendered tile.
///
/// `revision` is the page's content revision: the owner bumps it whenever the render mirror
/// changes what the page looks like (an annotation hidden or moved), which makes every older
/// tile unreachable without a scan. `layer_hash` identifies the set of visible layers.
/// `options` is what was drawn besides the page content: a tile rendered without annotations
/// and one rendered with them are different tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub doc: DocId,
    pub page: u32,
    pub revision: u64,
    pub level: i8,
    pub x: u32,
    pub y: u32,
    pub layer_hash: u64,
    pub options: RenderOptions,
}

impl TileKey {
    /// The key of a tile rendered with the default options (annotations drawn).
    pub fn new(doc: DocId, page: u32, revision: u64, tile: TileId, layer_hash: u64) -> TileKey {
        TileKey {
            doc,
            page,
            revision,
            level: tile.level,
            x: tile.x,
            y: tile.y,
            layer_hash,
            options: RenderOptions::default(),
        }
    }

    /// The same tile rendered with other options.
    pub fn with_options(self, options: RenderOptions) -> TileKey {
        TileKey { options, ..self }
    }

    pub fn tile(&self) -> TileId {
        TileId {
            level: self.level,
            x: self.x,
            y: self.y,
        }
    }

    /// The tile one level coarser that contains this one, or `None` at the coarsest level.
    pub fn parent(&self) -> Option<TileKey> {
        if self.level <= pl_geom::MIN_LEVEL {
            return None;
        }
        Some(TileKey {
            level: self.level - 1,
            x: self.x / 2,
            y: self.y / 2,
            ..*self
        })
    }
}

/// Counters, for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    /// Inserts refused because the single entry was larger than the whole budget.
    pub rejected: u64,
    /// The largest value `bytes()` has ever had.
    pub peak_bytes: usize,
}

struct Entry {
    bitmap: Arc<Bitmap>,
    cost: usize,
    tick: u64,
}

struct Inner {
    budget: usize,
    bytes: usize,
    tick: u64,
    map: HashMap<TileKey, Entry>,
    /// Recency order: lowest tick is least recently used.
    order: BTreeMap<u64, TileKey>,
    stats: CacheStats,
}

impl Inner {
    fn remove(&mut self, key: &TileKey) -> Option<Entry> {
        let entry = self.map.remove(key)?;
        self.order.remove(&entry.tick);
        self.bytes -= entry.cost;
        Some(entry)
    }

    fn evict_until(&mut self, target: usize) {
        while self.bytes > target {
            let Some((_, key)) = self.order.pop_first() else {
                break;
            };
            if let Some(entry) = self.map.remove(&key) {
                self.bytes -= entry.cost;
                self.stats.evictions += 1;
            }
        }
    }
}

/// A thread-safe LRU of tiles whose total cost never exceeds its byte budget.
///
/// The cost of an entry is its pixel bytes plus [`ENTRY_OVERHEAD`]. Room is made *before* an
/// entry is stored, so the budget holds at every instant, not just after the call. Bitmaps are
/// handed out as `Arc`s; one that is evicted while a caller still holds it stays alive until
/// that caller drops it, and no longer counts against the budget.
pub struct TileCache {
    inner: Mutex<Inner>,
}

impl Default for TileCache {
    fn default() -> Self {
        TileCache::new(DEFAULT_CACHE_BUDGET)
    }
}

impl TileCache {
    pub fn new(budget_bytes: usize) -> TileCache {
        TileCache {
            inner: Mutex::new(Inner {
                budget: budget_bytes,
                bytes: 0,
                tick: 0,
                map: HashMap::new(),
                order: BTreeMap::new(),
                stats: CacheStats::default(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock cannot leave the maps inconsistent in a way that
        // matters more than losing the cache, so poisoning is ignored.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cost charged for a bitmap.
    pub fn cost_of(bitmap: &Bitmap) -> usize {
        bitmap.byte_len() + ENTRY_OVERHEAD
    }

    /// Looks a tile up and marks it most recently used.
    pub fn get(&self, key: &TileKey) -> Option<Arc<Bitmap>> {
        let mut g = self.lock();
        g.tick += 1;
        let tick = g.tick;
        let found = g.map.get_mut(key).map(|entry| {
            let old = std::mem::replace(&mut entry.tick, tick);
            (old, entry.bitmap.clone())
        });
        match found {
            Some((old, bitmap)) => {
                g.order.remove(&old);
                g.order.insert(tick, *key);
                g.stats.hits += 1;
                Some(bitmap)
            }
            None => {
                g.stats.misses += 1;
                None
            }
        }
    }

    /// Whether a tile is cached. Does not change recency or the hit counters.
    pub fn contains(&self, key: &TileKey) -> bool {
        self.lock().map.contains_key(key)
    }

    /// Stores a tile, evicting least recently used tiles first to make room. Returns `false`
    /// (and stores nothing) if the tile alone is larger than the budget.
    pub fn insert(&self, key: TileKey, bitmap: Arc<Bitmap>) -> bool {
        let cost = Self::cost_of(&bitmap);
        let mut g = self.lock();
        g.remove(&key);
        if cost > g.budget {
            g.stats.rejected += 1;
            return false;
        }
        let target = g.budget - cost;
        g.evict_until(target);
        g.tick += 1;
        let tick = g.tick;
        g.map.insert(key, Entry { bitmap, cost, tick });
        g.order.insert(tick, key);
        g.bytes += cost;
        g.stats.inserts += 1;
        g.stats.peak_bytes = g.stats.peak_bytes.max(g.bytes);
        debug_assert!(g.bytes <= g.budget);
        true
    }

    /// The nearest cached coarser tile that covers `key`, for an upscaled placeholder. Looks
    /// up to `max_levels` levels up. The returned key tells the caller which part to magnify.
    pub fn find_ancestor(&self, key: &TileKey, max_levels: u8) -> Option<(TileKey, Arc<Bitmap>)> {
        let mut cur = *key;
        for _ in 0..max_levels {
            cur = cur.parent()?;
            if let Some(b) = self.get(&cur) {
                return Some((cur, b));
            }
        }
        None
    }

    /// Removes one tile.
    pub fn remove(&self, key: &TileKey) -> bool {
        self.lock().remove(key).is_some()
    }

    /// Removes every tile for which `pred` is true; returns how many were removed.
    pub fn remove_where(&self, mut pred: impl FnMut(&TileKey) -> bool) -> usize {
        let mut g = self.lock();
        let doomed: Vec<TileKey> = g.map.keys().filter(|k| pred(k)).copied().collect();
        for key in &doomed {
            g.remove(key);
        }
        doomed.len()
    }

    /// Removes every tile of one page.
    pub fn remove_page(&self, doc: DocId, page: u32) -> usize {
        self.remove_where(|k| k.doc == doc && k.page == page)
    }

    /// Removes the tiles of one page whose content revision is not `current`: call it after
    /// bumping a page's revision, so the tiles nothing can reach any more give their memory
    /// back at once instead of waiting for LRU pressure. Other pages are not touched.
    pub fn remove_stale(&self, doc: DocId, page: u32, current: u64) -> usize {
        self.remove_where(|k| k.doc == doc && k.page == page && k.revision != current)
    }

    /// Removes the tiles of one page, at every level and for every option set, that show any
    /// part of `rect` (page space). This is the invalidation for a change confined to a
    /// rectangle, such as one annotation hidden or moved: the page keeps its revision and its
    /// other tiles stay cached. `geometry` must be the page's real geometry.
    pub fn remove_covering(
        &self,
        doc: DocId,
        page: u32,
        geometry: &PageGeometry,
        rect: Rect,
    ) -> usize {
        self.remove_where(|k| {
            k.doc == doc
                && k.page == page
                && TileGrid::new(geometry, k.level)
                    .tiles_covering_page_rect(geometry, rect)
                    .contains(k.tile())
        })
    }

    /// Removes every tile of one document (call on close).
    pub fn remove_doc(&self, doc: DocId) -> usize {
        self.remove_where(|k| k.doc == doc)
    }

    /// Empties the cache.
    pub fn clear(&self) {
        let mut g = self.lock();
        g.map.clear();
        g.order.clear();
        g.bytes = 0;
    }

    /// Changes the budget, evicting at once if the cache is now over it.
    pub fn set_budget(&self, budget_bytes: usize) {
        let mut g = self.lock();
        g.budget = budget_bytes;
        g.evict_until(budget_bytes);
    }

    pub fn budget(&self) -> usize {
        self.lock().budget
    }

    /// Total cost of the cached tiles.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> CacheStats {
        self.lock().stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> TileKey {
        TileKey {
            doc: DocId(1),
            page: 0,
            revision: 0,
            level: 0,
            x: n,
            y: 0,
            layer_hash: 0,
            options: RenderOptions::default(),
        }
    }

    fn bmp(w: u32, h: u32) -> Arc<Bitmap> {
        Arc::new(Bitmap::filled(w, h, [1, 2, 3, 255]))
    }

    #[test]
    fn default_budget_is_512_mb() {
        assert_eq!(TileCache::default().budget(), 512 * 1024 * 1024);
    }

    #[test]
    fn never_exceeds_budget_under_random_load() {
        // Tiles of 16 different sizes, a budget that holds only a handful of them.
        let budget = 40_000;
        let cache = TileCache::new(budget);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for i in 0..5_000u32 {
            let r = next();
            let w = 4 + (r % 16) as u32 * 4;
            let h = 4 + ((r >> 8) % 16) as u32 * 4;
            match r >> 60 {
                0 => {
                    cache.get(&key((r >> 20) as u32 % 64));
                }
                1 => {
                    cache.remove(&key((r >> 20) as u32 % 64));
                }
                _ => {
                    cache.insert(key((r >> 20) as u32 % 64), bmp(w, h));
                }
            }
            assert!(cache.bytes() <= budget, "over budget after step {i}");
        }
        let stats = cache.stats();
        assert!(stats.peak_bytes <= budget);
        assert!(stats.evictions > 0, "the test must actually evict");
        // The byte count is exactly the sum of the entries.
        let g = cache.lock();
        let sum: usize = g.map.values().map(|e| e.cost).sum();
        assert_eq!(sum, g.bytes);
        assert_eq!(g.map.len(), g.order.len());
    }

    #[test]
    fn evicts_least_recently_used_first() {
        let one = TileCache::cost_of(&bmp(10, 10));
        let cache = TileCache::new(one * 3);
        for n in 0..3 {
            assert!(cache.insert(key(n), bmp(10, 10)));
        }
        // Touch 0 so that 1 is now the oldest.
        assert!(cache.get(&key(0)).is_some());
        assert!(cache.insert(key(3), bmp(10, 10)));
        assert!(cache.contains(&key(0)));
        assert!(!cache.contains(&key(1)));
        assert!(cache.contains(&key(2)));
        assert!(cache.contains(&key(3)));
        assert_eq!(cache.bytes(), one * 3);
    }

    #[test]
    fn an_entry_larger_than_the_budget_is_refused() {
        let cache = TileCache::new(1_000);
        assert!(cache.insert(key(0), bmp(4, 4)));
        assert!(!cache.insert(key(1), bmp(100, 100)));
        assert!(cache.contains(&key(0)), "a refused insert must not evict");
        assert_eq!(cache.stats().rejected, 1);
        assert!(cache.bytes() <= 1_000);
    }

    #[test]
    fn replacing_a_key_does_not_leak_bytes() {
        let cache = TileCache::new(100_000);
        cache.insert(key(0), bmp(10, 10));
        cache.insert(key(0), bmp(20, 20));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), TileCache::cost_of(&bmp(20, 20)));
    }

    #[test]
    fn shrinking_the_budget_evicts_immediately() {
        let cache = TileCache::new(1_000_000);
        for n in 0..10 {
            cache.insert(key(n), bmp(32, 32));
        }
        let one = TileCache::cost_of(&bmp(32, 32));
        cache.set_budget(one * 2);
        assert_eq!(cache.len(), 2);
        assert!(cache.bytes() <= one * 2);
        assert!(cache.contains(&key(9)) && cache.contains(&key(8)));
    }

    #[test]
    fn revision_and_layer_hash_separate_entries_and_page_removal_works() {
        let cache = TileCache::new(1_000_000);
        let a = key(0);
        let b = TileKey { revision: 1, ..a };
        let c = TileKey { layer_hash: 7, ..a };
        let other_page = TileKey { page: 1, ..a };
        for k in [a, b, c, other_page] {
            cache.insert(k, bmp(4, 4));
        }
        assert_eq!(cache.len(), 4);
        assert_eq!(cache.remove_page(DocId(1), 0), 3);
        assert!(cache.contains(&other_page));
        assert_eq!(cache.remove_doc(DocId(1)), 1);
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn find_ancestor_walks_to_coarser_levels() {
        let cache = TileCache::new(1_000_000);
        let fine = TileKey {
            level: 3,
            x: 13,
            y: 6,
            ..key(0)
        };
        let coarse = TileKey {
            level: 1,
            x: 3,
            y: 1,
            ..key(0)
        };
        assert!(cache.find_ancestor(&fine, 4).is_none());
        cache.insert(coarse, bmp(4, 4));
        assert_eq!(cache.find_ancestor(&fine, 4).map(|(k, _)| k), Some(coarse));
        assert!(cache.find_ancestor(&fine, 1).is_none());
    }
}

#[cfg(test)]
mod invalidation_tests {
    use super::*;

    fn tile(level: i8, x: u32, y: u32) -> TileId {
        TileId { level, x, y }
    }

    fn fill(cache: &TileCache, key: TileKey) {
        assert!(cache.insert(key, Arc::new(Bitmap::filled(4, 4, [1, 2, 3, 255]))));
    }

    #[test]
    fn stale_revisions_of_one_page_are_dropped_and_nothing_else() {
        let cache = TileCache::new(1 << 20);
        let doc = DocId(1);
        for page in 0..3 {
            for revision in 0..2 {
                fill(&cache, TileKey::new(doc, page, revision, tile(0, 0, 0), 0));
                fill(&cache, TileKey::new(doc, page, revision, tile(1, 1, 0), 0));
            }
        }
        fill(&cache, TileKey::new(DocId(2), 1, 0, tile(0, 0, 0), 0));
        assert_eq!(cache.len(), 13);
        // Page 1 moved to revision 1: only its revision-0 tiles go.
        assert_eq!(cache.remove_stale(doc, 1, 1), 2);
        assert_eq!(cache.len(), 11);
        assert!(cache.contains(&TileKey::new(doc, 1, 1, tile(0, 0, 0), 0)));
        assert!(!cache.contains(&TileKey::new(doc, 1, 0, tile(0, 0, 0), 0)));
        assert!(cache.contains(&TileKey::new(doc, 0, 0, tile(0, 0, 0), 0)));
        assert!(cache.contains(&TileKey::new(doc, 2, 0, tile(1, 1, 0), 0)));
        assert!(cache.contains(&TileKey::new(DocId(2), 1, 0, tile(0, 0, 0), 0)));
        assert_eq!(cache.remove_stale(doc, 1, 1), 0);
    }

    #[test]
    fn only_the_tiles_under_a_rectangle_are_dropped() {
        let cache = TileCache::new(8 << 20);
        let doc = DocId(1);
        // Letter at level 1: 1224 x 1584 px, 3 x 4 tiles of 512 px = 256 units.
        let geometry = PageGeometry::simple(612.0, 792.0);
        let grid = TileGrid::new(&geometry, 1);
        assert_eq!((grid.cols(), grid.rows()), (3, 4));
        let without = RenderOptions { annotations: false };
        for id in grid.all().iter() {
            fill(&cache, TileKey::new(doc, 0, 0, id, 0));
            fill(&cache, TileKey::new(doc, 0, 0, id, 0).with_options(without));
            fill(&cache, TileKey::new(doc, 1, 0, id, 0));
        }
        fill(&cache, TileKey::new(doc, 0, 0, tile(0, 0, 0), 0));
        fill(&cache, TileKey::new(doc, 0, 0, tile(0, 1, 1), 0));
        let before = cache.len();

        // A small annotation near the top-left corner of the page (y-up: the top is y = 792):
        // display position about (40, 30), inside tile (0, 0) at level 1 and level 0.
        let rect = Rect::new(30.0, 752.0, 50.0, 772.0);
        let removed = cache.remove_covering(doc, 0, &geometry, rect);
        assert_eq!(
            removed, 3,
            "both option variants at level 1, and the level 0 tile"
        );
        assert_eq!(cache.len(), before - 3);
        let top_left = TileKey::new(doc, 0, 0, tile(1, 0, 0), 0);
        assert!(!cache.contains(&top_left));
        assert!(!cache.contains(&top_left.with_options(without)));
        assert!(!cache.contains(&TileKey::new(doc, 0, 0, tile(0, 0, 0), 0)));
        // Its neighbours, the other level-0 tile and the other page are untouched.
        assert!(cache.contains(&TileKey::new(doc, 0, 0, tile(1, 1, 0), 0)));
        assert!(cache.contains(&TileKey::new(doc, 0, 0, tile(1, 0, 1), 0)));
        assert!(cache.contains(&TileKey::new(doc, 0, 0, tile(0, 1, 1), 0)));
        assert!(cache.contains(&TileKey::new(doc, 1, 0, tile(1, 0, 0), 0)));

        // A rectangle across a tile seam takes the tiles on both sides.
        let seam = Rect::new(250.0, 700.0, 262.0, 710.0);
        assert_eq!(cache.remove_covering(doc, 0, &geometry, seam), 2);
        assert!(!cache.contains(&TileKey::new(doc, 0, 0, tile(1, 1, 0), 0)));
    }
}
