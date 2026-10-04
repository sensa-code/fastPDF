//! Small byte-weighted LRU of interpreted pages.
//!
//! Interpreting a page (content stream → `DisplayList`, decoding its images)
//! is the expensive, serialized part of a zpdf render; rasterizing a tile from
//! the result is cheap and parallel. Keeping the last few interpreted pages
//! lets every tile of a page reuse one interpretation (spec §12, ADR 0003).

use std::sync::Arc;

/// Identifies one interpretation of a page. The intrinsic `/Rotate` is fixed
/// per page, so the user rotation is enough to identify the baked rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PreparedKey {
    pub(crate) page: u32,
    /// User rotation in quarter turns, applied on top of `/Rotate`.
    pub(crate) user_quarter_turns: u8,
    pub(crate) annotations: bool,
}

/// Anything the cache can weigh.
pub(crate) trait Weighted {
    /// Approximate retained heap bytes.
    fn weight(&self) -> u64;
}

/// Least-recently-used cache bounded by entry count and total weight.
///
/// The most recently inserted entry is always kept, even when it alone
/// exceeds the byte budget: it is the page being rendered right now.
/// Evicting an entry only drops the cache's reference; renders that still
/// hold the `Arc` finish normally and free it afterwards.
#[derive(Debug)]
pub(crate) struct PreparedCache<T> {
    /// Ordered from least to most recently used.
    entries: Vec<(PreparedKey, Arc<T>)>,
    max_entries: usize,
    max_bytes: u64,
}

impl<T: Weighted> PreparedCache<T> {
    pub(crate) fn new(max_entries: usize, max_bytes: u64) -> Self {
        Self {
            entries: Vec::new(),
            max_entries: max_entries.max(1),
            max_bytes,
        }
    }

    pub(crate) fn get(&mut self, key: PreparedKey) -> Option<Arc<T>> {
        let index = self.entries.iter().position(|(k, _)| *k == key)?;
        let entry = self.entries.remove(index);
        let value = Arc::clone(&entry.1);
        self.entries.push(entry);
        Some(value)
    }

    pub(crate) fn insert(&mut self, key: PreparedKey, value: Arc<T>) {
        if let Some(index) = self.entries.iter().position(|(k, _)| *k == key) {
            self.entries.remove(index);
        }
        self.entries.push((key, value));
        // Weights can grow after insertion (per-scale data attached to an
        // entry), so the total is recomputed rather than tracked.
        while self.entries.len() > self.max_entries
            || (self.bytes() > self.max_bytes && self.entries.len() > 1)
        {
            self.entries.remove(0);
        }
    }

    /// Current total weight of the cached entries.
    pub(crate) fn bytes(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |sum, (_, v)| sum.saturating_add(v.weight()))
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// The cached values, least recently used first.
    pub(crate) fn values(&self) -> impl Iterator<Item = &Arc<T>> {
        self.entries.iter().map(|(_, v)| v)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Blob(u64);

    impl Weighted for Blob {
        fn weight(&self) -> u64 {
            self.0
        }
    }

    fn key(page: u32) -> PreparedKey {
        PreparedKey {
            page,
            user_quarter_turns: 0,
            annotations: true,
        }
    }

    #[test]
    fn evicts_least_recently_used_by_count() {
        let mut cache = PreparedCache::new(2, u64::MAX);
        cache.insert(key(0), Arc::new(Blob(1)));
        cache.insert(key(1), Arc::new(Blob(1)));
        assert!(cache.get(key(0)).is_some()); // 0 becomes most recent
        cache.insert(key(2), Arc::new(Blob(1)));
        assert!(cache.get(key(1)).is_none());
        assert!(cache.get(key(0)).is_some());
        assert!(cache.get(key(2)).is_some());
    }

    #[test]
    fn evicts_by_bytes_but_keeps_the_newest_entry() {
        let mut cache = PreparedCache::new(8, 100);
        cache.insert(key(0), Arc::new(Blob(60)));
        cache.insert(key(1), Arc::new(Blob(60)));
        assert_eq!(cache.len(), 1);
        assert!(cache.get(key(1)).is_some());
        cache.insert(key(2), Arc::new(Blob(500)));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), 500);
    }

    #[test]
    fn reinserting_a_key_replaces_its_weight() {
        let mut cache = PreparedCache::new(8, u64::MAX);
        cache.insert(key(0), Arc::new(Blob(10)));
        cache.insert(key(0), Arc::new(Blob(30)));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), 30);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.bytes(), 0);
    }
}
