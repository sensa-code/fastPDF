//! Tile storage with progressive-rendering fallback (spec §13, §17).
//!
//! The cache is generic over the stored value so the UI can keep whatever it
//! draws (a GPU image handle) while this crate stays toolkit-independent.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use fastpdf_cache::{BudgetedCache, CacheStats, SharedCache, retention};
use fastpdf_engine_api::{ColorMode, PageId, PixelRect, Rotation};

use crate::{ScaleBucket, TileGrid, TileKey};

type PageKey = (PageId, Rotation, ColorMode);
type Index = Arc<Mutex<HashMap<PageKey, HashSet<TileKey>>>>;

/// A cached tile that can stand in for a missing one: draw `key`'s image
/// covering `dest`, a rectangle in the *wanted* bucket's page pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fallback {
    pub key: TileKey,
    pub dest: [f32; 4],
}

/// Byte-budgeted tile store, registered with the memory budget manager.
pub struct TileCache<V> {
    cache: Arc<SharedCache<TileKey, V>>,
    index: Index,
}

impl<V> fmt::Debug for TileCache<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TileCache")
            .field("cache", &self.cache)
            .finish()
    }
}

impl<V: Send + 'static> TileCache<V> {
    /// `on_evict` receives tiles dropped by budget enforcement or memory
    /// pressure so the UI can free their GPU resources.
    pub fn new(
        budget: usize,
        on_evict: impl Fn(Vec<(TileKey, V)>) + Send + Sync + 'static,
    ) -> Self {
        let index: Index = Arc::default();
        let hook_index = Arc::clone(&index);
        let cache = SharedCache::new("tiles", budget, retention::TILES).with_eviction_hook(
            move |evicted: Vec<(TileKey, V)>| {
                {
                    let mut idx = lock(&hook_index);
                    for (key, _) in &evicted {
                        unindex(&mut idx, key);
                    }
                }
                on_evict(evicted);
            },
        );
        Self {
            cache: Arc::new(cache),
            index,
        }
    }

    /// Handle for [`fastpdf_cache::MemoryBudgetManager::register`].
    pub fn budgeted(&self) -> Arc<dyn BudgetedCache> {
        self.cache.clone()
    }

    pub fn insert(&self, key: TileKey, value: V, bytes: usize) {
        lock(&self.index)
            .entry(page_key(&key))
            .or_default()
            .insert(key);
        self.cache.insert(key, value, bytes);
    }

    pub fn contains(&self, key: &TileKey) -> bool {
        self.cache.contains(key)
    }

    /// Runs `f` on a cached tile, marking it recently used.
    pub fn with<R>(&self, key: &TileKey, f: impl FnOnce(&V) -> R) -> Option<R> {
        self.cache.with(key, f)
    }

    /// Marks the tiles on screen as most recently used and protects their
    /// bytes from memory-pressure relief (spec §16).
    pub fn mark_visible<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a TileKey>,
        visible_bytes: usize,
    ) {
        let mut lru = self.cache.lock();
        for key in keys {
            lru.touch(key);
        }
        drop(lru);
        self.cache.set_protected_bytes(visible_bytes);
    }

    /// Drops every tile of the given document (on close).
    pub fn remove_document(&self, document: fastpdf_engine_api::DocumentId) {
        lock(&self.index).retain(|(page, _, _), _| page.document != document);
        self.cache.retain(|key, _| key.page.document != document);
    }

    pub fn stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// Cached tiles of other scale buckets that cover `wanted`, best first
    /// (closest bucket, preferring higher resolution). `grid_for` returns the
    /// tile grid of the page at a bucket.
    pub fn fallbacks(
        &self,
        wanted: &TileKey,
        grid_for: impl Fn(ScaleBucket) -> Option<TileGrid>,
    ) -> Vec<Fallback> {
        let Some(wanted_grid) = grid_for(wanted.bucket) else {
            return Vec::new();
        };
        let Some(wanted_rect) = wanted_grid.tile_rect(wanted.coord) else {
            return Vec::new();
        };
        let candidates: Vec<TileKey> = lock(&self.index)
            .get(&page_key(wanted))
            .map(|set| {
                set.iter()
                    .filter(|k| k.bucket != wanted.bucket)
                    .copied()
                    .collect()
            })
            .unwrap_or_default();

        let target = wanted.bucket.display_scale();
        let mut found: Vec<(f32, Fallback)> = candidates
            .into_iter()
            .filter(|k| self.cache.contains(k))
            .filter_map(|key| {
                let grid = grid_for(key.bucket)?;
                let rect = grid.tile_rect(key.coord)?;
                // Map the candidate's rectangle into the wanted bucket's pixels.
                let f = target / key.bucket.display_scale();
                let dest = [
                    rect.x as f32 * f,
                    rect.y as f32 * f,
                    rect.width as f32 * f,
                    rect.height as f32 * f,
                ];
                overlaps(&dest, wanted_rect).then(|| {
                    let ratio = key.bucket.display_scale() / target;
                    // Prefer close buckets; break ties toward higher resolution.
                    let rank = ratio.ln().abs() - if ratio > 1.0 { 0.01 } else { 0.0 };
                    (rank, Fallback { key, dest })
                })
            })
            .collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.into_iter().map(|(_, f)| f).collect()
    }
}

fn overlaps(dest: &[f32; 4], rect: PixelRect) -> bool {
    let (x0, y0) = (rect.x as f32, rect.y as f32);
    let (x1, y1) = (x0 + rect.width as f32, y0 + rect.height as f32);
    dest[0] < x1 && dest[0] + dest[2] > x0 && dest[1] < y1 && dest[1] + dest[3] > y0
}

fn page_key(key: &TileKey) -> PageKey {
    (key.page, key.rotation, key.color)
}

fn unindex(index: &mut HashMap<PageKey, HashSet<TileKey>>, key: &TileKey) {
    let pk = page_key(key);
    if let Some(set) = index.get_mut(&pk) {
        set.remove(key);
        if set.is_empty() {
            index.remove(&pk);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TileCoord;
    use fastpdf_engine_api::{DocumentId, PageIndex, PageSize};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key(bucket: f32, col: u32, row: u32) -> TileKey {
        TileKey {
            page: PageId::new(DocumentId::from_raw(1), PageIndex::FIRST),
            bucket: ScaleBucket::for_display_scale(bucket),
            rotation: Rotation::R0,
            color: ColorMode::Normal,
            tile_size: 256,
            coord: TileCoord { col, row },
        }
    }

    fn grid(bucket: ScaleBucket) -> Option<TileGrid> {
        let px = bucket
            .render_scale()
            .page_pixels(PageSize::LETTER, Rotation::R0);
        Some(TileGrid::new(px, 256))
    }

    #[test]
    fn zoom_in_falls_back_to_lower_resolution_tiles() {
        let cache: TileCache<u32> = TileCache::new(1 << 30, |_| {});
        // At 1.0 the page is 816 px wide: tile (1,0) covers x 256..512.
        cache.insert(key(1.0, 1, 0), 1, 100);
        cache.insert(key(0.5, 0, 0), 2, 100);
        // At 2.0 the wanted tile (2,0) covers x 512..768 = x 256..384 at 1.0.
        let fallbacks = cache.fallbacks(&key(2.0, 2, 0), grid);
        assert_eq!(fallbacks.len(), 2);
        assert_eq!(fallbacks[0].key, key(1.0, 1, 0)); // closest bucket first
        assert_eq!(fallbacks[0].dest, [512.0, 0.0, 512.0, 512.0]);
    }

    #[test]
    fn non_overlapping_and_other_pages_are_ignored() {
        let cache: TileCache<u32> = TileCache::new(1 << 30, |_| {});
        cache.insert(key(1.0, 0, 2), 1, 100); // far below the wanted tile
        let mut other_page = key(1.0, 1, 0);
        other_page.page.page = PageIndex::new(5);
        cache.insert(other_page, 2, 100);
        assert!(cache.fallbacks(&key(2.0, 2, 0), grid).is_empty());
    }

    #[test]
    fn evictions_update_the_index_and_notify() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&evicted);
        let cache: TileCache<u32> = TileCache::new(150, move |e| {
            counter.fetch_add(e.len(), Ordering::Relaxed);
        });
        cache.insert(key(1.0, 1, 0), 1, 100);
        cache.insert(key(0.5, 0, 0), 2, 100); // evicts the 1.0 tile
        assert_eq!(evicted.load(Ordering::Relaxed), 1);
        let fallbacks = cache.fallbacks(&key(2.0, 2, 0), grid);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0].key, key(0.5, 0, 0));
    }

    #[test]
    fn visible_tiles_survive_pressure_and_documents_can_be_dropped() {
        let cache: TileCache<u32> = TileCache::new(1 << 30, |_| {});
        for col in 0..4 {
            cache.insert(key(1.0, col, 0), col, 100);
        }
        let visible = [key(1.0, 0, 0)];
        cache.mark_visible(visible.iter(), 100);
        let budgeted = cache.budgeted();
        budgeted.shrink_to(0);
        assert!(cache.contains(&key(1.0, 0, 0)));
        assert_eq!(budgeted.bytes(), 100);
        cache.remove_document(DocumentId::from_raw(1));
        assert!(!cache.contains(&key(1.0, 0, 0)));
    }
}
