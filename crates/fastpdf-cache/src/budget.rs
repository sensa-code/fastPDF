use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use crate::{CacheStats, MemoryPressure};

/// Conventional retention priorities: under pressure, caches with lower
/// values are shrunk first (spec §16 "優先丟" order).
pub mod retention {
    /// Thumbnails are cheap to regenerate and rarely critical.
    pub const THUMBNAILS: u8 = 10;
    /// Text layers kept for search / selection of off-screen pages.
    pub const TEXT: u8 = 20;
    /// Tiles prefetched for pages that are not visible.
    pub const PREFETCH: u8 = 30;
    /// Tiles of the current view (visible tiles are kept most-recently-used).
    pub const TILES: u8 = 60;
    /// Caches at or above this level survive hard-pressure purges and are
    /// only shrunk proportionally.
    pub const CRITICAL: u8 = 50;
}

/// A cache the manager can observe and shrink.
pub trait BudgetedCache: Send + Sync {
    fn name(&self) -> &str;
    /// Lower values are evicted first under pressure.
    fn retention(&self) -> u8;
    fn bytes(&self) -> usize;
    /// Evicts until at most `target` bytes remain; returns bytes freed.
    fn shrink_to(&self, target: usize) -> usize;
    fn stats(&self) -> CacheStats;
}

/// Process-level memory thresholds (spec §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetConfig {
    /// Above this, evict aggressively.
    pub soft_limit: usize,
    /// Above this, drop everything not needed for the current view.
    pub hard_limit: usize,
    /// Relief aims for `soft_limit * relief_target_percent / 100`.
    pub relief_target_percent: u8,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        // Sum of the per-cache starting budgets from spec §15 is 288 MB.
        Self {
            soft_limit: 320 * 1024 * 1024,
            hard_limit: 512 * 1024 * 1024,
            relief_target_percent: 85,
        }
    }
}

/// Snapshot of one registered cache, for the development overlay and the
/// benchmark harness.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheSnapshot {
    pub name: String,
    pub retention: u8,
    pub stats: CacheStats,
}

/// Result of [`MemoryBudgetManager::relieve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Relief {
    pub pressure: MemoryPressure,
    pub freed: usize,
}

/// Central authority over cache memory (spec §16).
///
/// The manager does not measure process memory itself; callers pass the
/// bytes they account for outside the registered caches (engine-internal
/// caches, GPU staging, ...) so the policy stays platform-neutral and testable.
pub struct MemoryBudgetManager {
    config: BudgetConfig,
    caches: Mutex<Vec<Weak<dyn BudgetedCache>>>,
}

impl fmt::Debug for MemoryBudgetManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryBudgetManager")
            .field("config", &self.config)
            .field("caches", &self.live_caches().len())
            .finish()
    }
}

impl MemoryBudgetManager {
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            caches: Mutex::new(Vec::new()),
        }
    }

    pub fn config(&self) -> BudgetConfig {
        self.config
    }

    /// Registers a cache. The manager holds it weakly; dropping the cache
    /// unregisters it.
    pub fn register(&self, cache: Arc<dyn BudgetedCache>) {
        let mut caches = self.caches.lock().unwrap_or_else(|e| e.into_inner());
        caches.retain(|c| c.strong_count() > 0);
        caches.push(Arc::downgrade(&cache));
    }

    /// Bytes held by all registered caches.
    pub fn cache_bytes(&self) -> usize {
        self.live_caches().iter().map(|c| c.bytes()).sum()
    }

    /// Pressure level for the cache bytes plus `external_bytes`.
    pub fn pressure(&self, external_bytes: usize) -> MemoryPressure {
        let total = self.cache_bytes().saturating_add(external_bytes);
        if total >= self.config.hard_limit {
            MemoryPressure::Hard
        } else if total >= self.config.soft_limit {
            MemoryPressure::Soft
        } else {
            MemoryPressure::Normal
        }
    }

    /// Shrinks caches according to the current pressure.
    ///
    /// * Soft: shrink caches in ascending retention order until the total
    ///   is back under the relief target.
    /// * Hard: first empty every cache below [`retention::CRITICAL`], then
    ///   continue like Soft.
    pub fn relieve(&self, external_bytes: usize) -> Relief {
        let pressure = self.pressure(external_bytes);
        if pressure == MemoryPressure::Normal {
            return Relief::default();
        }
        let mut caches = self.live_caches();
        caches.sort_by_key(|c| c.retention());

        let target = self.config.soft_limit / 100 * usize::from(self.config.relief_target_percent);
        let mut total = self.cache_bytes().saturating_add(external_bytes);
        let mut freed = 0;

        if pressure == MemoryPressure::Hard {
            for cache in caches
                .iter()
                .filter(|c| c.retention() < retention::CRITICAL)
            {
                let f = cache.shrink_to(0);
                freed += f;
                total = total.saturating_sub(f);
            }
        }
        for cache in &caches {
            if total <= target {
                break;
            }
            let excess = total - target;
            let current = cache.bytes();
            let f = cache.shrink_to(current.saturating_sub(excess));
            freed += f;
            total = total.saturating_sub(f);
        }
        Relief { pressure, freed }
    }

    pub fn snapshot(&self) -> Vec<CacheSnapshot> {
        self.live_caches()
            .iter()
            .map(|c| CacheSnapshot {
                name: c.name().to_owned(),
                retention: c.retention(),
                stats: c.stats(),
            })
            .collect()
    }

    fn live_caches(&self) -> Vec<Arc<dyn BudgetedCache>> {
        let caches = self.caches.lock().unwrap_or_else(|e| e.into_inner());
        caches.iter().filter_map(Weak::upgrade).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SharedCache;

    const MB: usize = 1024 * 1024;

    fn filled(name: &'static str, retention: u8, mb: usize) -> Arc<SharedCache<usize, ()>> {
        let cache = Arc::new(SharedCache::new(name, 1024 * MB, retention));
        for i in 0..mb {
            cache.insert(i, (), MB);
        }
        cache
    }

    fn manager() -> MemoryBudgetManager {
        MemoryBudgetManager::new(BudgetConfig {
            soft_limit: 100 * MB,
            hard_limit: 200 * MB,
            relief_target_percent: 80,
        })
    }

    #[test]
    fn soft_pressure_shrinks_low_retention_first() {
        let m = manager();
        let thumbs = filled("thumbs", retention::THUMBNAILS, 30);
        let tiles = filled("tiles", retention::TILES, 90);
        m.register(thumbs.clone());
        m.register(tiles.clone());
        assert_eq!(m.pressure(0), MemoryPressure::Soft);

        let relief = m.relieve(0);
        assert_eq!(relief.pressure, MemoryPressure::Soft);
        // Target is 80 MB: 40 MB must go, thumbnails (30) first, then tiles (10).
        assert_eq!(relief.freed, 40 * MB);
        assert_eq!(thumbs.bytes(), 0);
        assert_eq!(tiles.bytes(), 80 * MB);
        assert_eq!(m.pressure(0), MemoryPressure::Normal);
    }

    #[test]
    fn hard_pressure_purges_non_critical_caches() {
        let m = manager();
        let text = filled("text", retention::TEXT, 10);
        let tiles = filled("tiles", retention::TILES, 50);
        m.register(text.clone());
        m.register(tiles.clone());
        // External memory (engine caches) pushes the process over the hard limit.
        let relief = m.relieve(150 * MB);
        assert_eq!(relief.pressure, MemoryPressure::Hard);
        assert_eq!(text.bytes(), 0);
        // 150 external + tiles must reach 80: tiles shrink to zero as well.
        assert_eq!(tiles.bytes(), 0);
    }

    #[test]
    fn dropped_caches_unregister() {
        let m = manager();
        {
            let c = filled("tmp", retention::TEXT, 5);
            m.register(c);
        }
        assert_eq!(m.cache_bytes(), 0);
        assert!(m.snapshot().is_empty());
    }
}
