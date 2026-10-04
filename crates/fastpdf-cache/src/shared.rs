use std::fmt;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use crate::{BudgetedCache, ByteLru, CacheStats};

type EvictionHook<K, V> = Box<dyn Fn(Vec<(K, V)>) + Send + Sync>;

/// A thread-safe [`ByteLru`] that can be registered with the
/// [`crate::MemoryBudgetManager`].
///
/// Entries evicted by budget enforcement — including evictions triggered by
/// the manager under memory pressure — are passed to the eviction hook, so
/// owners can release GPU textures or return buffers to a pool.
pub struct SharedCache<K, V> {
    name: &'static str,
    retention: u8,
    inner: Mutex<ByteLru<K, V>>,
    /// Bytes the memory manager may not reclaim (the visible set).
    protected: AtomicUsize,
    on_evict: Option<EvictionHook<K, V>>,
}

impl<K, V> fmt::Debug for SharedCache<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedCache")
            .field("name", &self.name)
            .field("retention", &self.retention)
            .finish_non_exhaustive()
    }
}

impl<K: Hash + Eq + Clone, V> SharedCache<K, V> {
    /// `retention` orders caches under pressure: lower values are evicted
    /// first (see [`crate::retention`]).
    pub fn new(name: &'static str, budget: usize, retention: u8) -> Self {
        Self {
            name,
            retention,
            inner: Mutex::new(ByteLru::new(budget)),
            protected: AtomicUsize::new(0),
            on_evict: None,
        }
    }

    /// Declares how many of the most recently used bytes are in use right
    /// now (e.g. tiles on screen, which the view touches every frame).
    /// Pressure relief never shrinks the cache below this floor, so the
    /// current view survives a purge (spec §16 "優先保留 visible tiles").
    pub fn set_protected_bytes(&self, bytes: usize) {
        self.protected.store(bytes, Ordering::Relaxed);
    }

    pub fn with_eviction_hook(
        mut self,
        hook: impl Fn(Vec<(K, V)>) + Send + Sync + 'static,
    ) -> Self {
        self.on_evict = Some(Box::new(hook));
        self
    }

    /// Inserts an entry, evicting least recently used ones to stay within
    /// budget — but never below the protected bytes plus the new entry: when
    /// the visible set alone exceeds the budget, evicting visible entries
    /// would only make the view re-render them in a loop.
    pub fn insert(&self, key: K, value: V, weight: usize) {
        let evicted = {
            let mut lru = self.lock();
            let floor = self
                .protected
                .load(Ordering::Relaxed)
                .saturating_add(weight);
            let limit = lru.budget().max(floor);
            lru.insert_with_limit(key, value, weight, limit)
        };
        self.dispose(evicted);
    }

    /// Runs `f` on the value if present (counts a hit or miss).
    pub fn with<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
        self.lock().get(key).map(f)
    }

    pub fn contains(&self, key: &K) -> bool {
        self.lock().contains(key)
    }

    pub fn touch(&self, key: &K) -> bool {
        self.lock().touch(key)
    }

    pub fn remove(&self, key: &K) -> Option<V> {
        self.lock().remove(key)
    }

    pub fn retain(&self, keep: impl FnMut(&K, &V) -> bool) {
        let removed = self.lock().retain(keep);
        self.dispose(removed);
    }

    pub fn set_budget(&self, budget: usize) {
        let evicted = self.lock().set_budget(budget);
        self.dispose(evicted);
    }

    /// Direct access for compound operations; keep the critical section short.
    pub fn lock(&self) -> MutexGuard<'_, ByteLru<K, V>> {
        // A poisoned lock only means another thread panicked mid-operation;
        // the LRU's invariants hold between method calls, so keep going.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn dispose(&self, entries: Vec<(K, V)>) {
        if entries.is_empty() {
            return;
        }
        if let Some(hook) = &self.on_evict {
            hook(entries);
        }
    }
}

impl<K: Hash + Eq + Clone + Send, V: Send> BudgetedCache for SharedCache<K, V> {
    fn name(&self) -> &str {
        self.name
    }

    fn retention(&self) -> u8 {
        self.retention
    }

    fn bytes(&self) -> usize {
        self.lock().bytes()
    }

    fn shrink_to(&self, target: usize) -> usize {
        let (evicted, freed) = {
            let mut lru = self.lock();
            let before = lru.bytes();
            let target = target.max(self.protected.load(Ordering::Relaxed));
            let evicted = lru.shrink_to(target);
            (evicted, before - lru.bytes())
        };
        self.dispose(evicted);
        freed
    }

    fn stats(&self) -> CacheStats {
        self.lock().stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn eviction_hook_sees_budget_and_pressure_evictions() {
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let cache =
            SharedCache::new("tiles", 100, 50).with_eviction_hook(move |e: Vec<(u32, ())>| {
                counter.fetch_add(e.len(), Ordering::Relaxed);
            });
        for i in 0..5 {
            cache.insert(i, (), 30);
        }
        assert_eq!(seen.load(Ordering::Relaxed), 2);
        assert_eq!(cache.shrink_to(30), 60);
        assert_eq!(seen.load(Ordering::Relaxed), 4);
        assert_eq!(BudgetedCache::bytes(&cache), 30);
    }

    #[test]
    fn a_visible_set_larger_than_the_budget_is_not_thrashed() {
        let cache: SharedCache<u32, ()> = SharedCache::new("tiles", 100, 50);
        // 150 bytes are on screen: more than the 100-byte budget.
        cache.set_protected_bytes(150);
        for i in 0..15 {
            cache.insert(i, (), 10);
        }
        // Everything fit under the protected floor; nothing was evicted.
        assert_eq!(BudgetedCache::bytes(&cache), 150);
        assert!(cache.contains(&0));
        // Once the view shrinks, the next insert falls back to the budget.
        cache.set_protected_bytes(0);
        cache.insert(99, (), 10);
        assert!(BudgetedCache::bytes(&cache) <= 100);
    }

    #[test]
    fn protected_bytes_survive_relief() {
        let cache: SharedCache<u32, ()> = SharedCache::new("tiles", 1_000, 50);
        for i in 0..10 {
            cache.insert(i, (), 10);
        }
        cache.set_protected_bytes(40);
        assert_eq!(cache.shrink_to(0), 60);
        assert_eq!(BudgetedCache::bytes(&cache), 40);
        // The survivors are the most recently used entries.
        assert!(cache.contains(&9) && cache.contains(&6) && !cache.contains(&5));
    }
}
