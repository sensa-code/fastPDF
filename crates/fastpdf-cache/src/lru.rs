use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;

use crate::CacheStats;

const NIL: usize = usize::MAX;

struct Slot<K, V> {
    entry: Option<(K, V, usize)>,
    prev: usize,
    next: usize,
}

/// A least-recently-used cache bounded by total bytes rather than entry count.
///
/// Callers state each entry's weight on insert. All operations are O(1)
/// except the ones that evict, which are O(evicted). Evicted entries are
/// handed back to the caller so resources tied to them (GPU textures, pooled
/// buffers) can be released deterministically.
pub struct ByteLru<K, V> {
    map: HashMap<K, usize>,
    slots: Vec<Slot<K, V>>,
    free: Vec<usize>,
    head: usize, // most recently used
    tail: usize, // least recently used
    bytes: usize,
    budget: usize,
    stats: CacheStats,
}

impl<K, V> fmt::Debug for ByteLru<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ByteLru")
            .field("entries", &self.map.len())
            .field("bytes", &self.bytes)
            .field("budget", &self.budget)
            .finish()
    }
}

impl<K: Hash + Eq + Clone, V> ByteLru<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            map: HashMap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            head: NIL,
            tail: NIL,
            bytes: 0,
            budget,
            stats: CacheStats::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            bytes: self.bytes,
            budget: self.budget,
            entries: self.map.len(),
            ..self.stats
        }
    }

    pub fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Looks up an entry, marks it most recently used and counts a hit/miss.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let Some(&idx) = self.map.get(key) else {
            self.stats.misses += 1;
            return None;
        };
        self.stats.hits += 1;
        self.move_to_front(idx);
        self.slots[idx].entry.as_ref().map(|(_, v, _)| v)
    }

    /// Looks up an entry without touching recency or statistics.
    pub fn peek(&self, key: &K) -> Option<&V> {
        let &idx = self.map.get(key)?;
        self.slots[idx].entry.as_ref().map(|(_, v, _)| v)
    }

    /// Marks an entry as most recently used without counting a lookup.
    /// Used to keep on-screen tiles from being evicted.
    pub fn touch(&mut self, key: &K) -> bool {
        match self.map.get(key) {
            Some(&idx) => {
                self.move_to_front(idx);
                true
            }
            None => false,
        }
    }

    /// Inserts or replaces an entry of `weight` bytes and evicts least
    /// recently used entries until the cache fits its budget again. An entry
    /// heavier than the whole budget is evicted immediately.
    ///
    /// Returns the replaced value (if any) followed by evicted entries.
    pub fn insert(&mut self, key: K, value: V, weight: usize) -> Vec<(K, V)> {
        self.insert_with_limit(key, value, weight, self.budget)
    }

    /// Like [`ByteLru::insert`], but evicts only down to `limit` bytes
    /// instead of the budget (callers pass a larger limit to keep entries
    /// that are in use right now).
    pub fn insert_with_limit(
        &mut self,
        key: K,
        value: V,
        weight: usize,
        limit: usize,
    ) -> Vec<(K, V)> {
        let mut out = Vec::new();
        if let Some(old) = self.remove(&key) {
            out.push((key.clone(), old));
        }
        let idx = self.alloc(key.clone(), value, weight);
        self.map.insert(key, idx);
        self.push_front(idx);
        self.bytes += weight;
        self.stats.inserts += 1;
        self.evict_until(limit, &mut out);
        out
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let idx = self.map.remove(key)?;
        self.unlink(idx);
        let (_, value, weight) = self.release(idx)?;
        self.bytes -= weight;
        Some(value)
    }

    /// Evicts least recently used entries until at most `target` bytes remain.
    pub fn shrink_to(&mut self, target: usize) -> Vec<(K, V)> {
        let mut out = Vec::new();
        self.evict_until(target, &mut out);
        out
    }

    /// Changes the budget, evicting if the cache no longer fits.
    pub fn set_budget(&mut self, budget: usize) -> Vec<(K, V)> {
        self.budget = budget;
        self.shrink_to(budget)
    }

    /// Removes every entry for which `keep` returns false (e.g. all tiles of
    /// a closed document). Removed entries are returned; they do not count as
    /// evictions.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) -> Vec<(K, V)> {
        let doomed: Vec<K> = self
            .map
            .iter()
            .filter_map(|(k, &idx)| {
                let (_, v, _) = self.slots[idx].entry.as_ref()?;
                (!keep(k, v)).then(|| k.clone())
            })
            .collect();
        doomed
            .into_iter()
            .filter_map(|k| self.remove(&k).map(|v| (k, v)))
            .collect()
    }

    pub fn clear(&mut self) -> Vec<(K, V)> {
        self.retain(|_, _| false)
    }

    /// Iterates entries from most to least recently used.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        let mut idx = self.head;
        std::iter::from_fn(move || {
            while idx != NIL {
                let slot = &self.slots[idx];
                idx = slot.next;
                if let Some((k, v, _)) = &slot.entry {
                    return Some((k, v));
                }
            }
            None
        })
    }

    fn evict_until(&mut self, target: usize, out: &mut Vec<(K, V)>) {
        while self.bytes > target && self.tail != NIL {
            let idx = self.tail;
            self.unlink(idx);
            if let Some((k, v, weight)) = self.release(idx) {
                self.map.remove(&k);
                self.bytes -= weight;
                self.stats.evictions += 1;
                self.stats.evicted_bytes += weight as u64;
                out.push((k, v));
            }
        }
    }

    fn alloc(&mut self, key: K, value: V, weight: usize) -> usize {
        let slot = Slot {
            entry: Some((key, value, weight)),
            prev: NIL,
            next: NIL,
        };
        match self.free.pop() {
            Some(idx) => {
                self.slots[idx] = slot;
                idx
            }
            None => {
                self.slots.push(slot);
                self.slots.len() - 1
            }
        }
    }

    fn release(&mut self, idx: usize) -> Option<(K, V, usize)> {
        let entry = self.slots[idx].entry.take();
        self.free.push(idx);
        entry
    }

    fn unlink(&mut self, idx: usize) {
        let (prev, next) = (self.slots[idx].prev, self.slots[idx].next);
        if prev == NIL {
            self.head = next;
        } else {
            self.slots[prev].next = next;
        }
        if next == NIL {
            self.tail = prev;
        } else {
            self.slots[next].prev = prev;
        }
        self.slots[idx].prev = NIL;
        self.slots[idx].next = NIL;
    }

    fn push_front(&mut self, idx: usize) {
        self.slots[idx].prev = NIL;
        self.slots[idx].next = self.head;
        if self.head != NIL {
            self.slots[self.head].prev = idx;
        }
        self.head = idx;
        if self.tail == NIL {
            self.tail = idx;
        }
    }

    fn move_to_front(&mut self, idx: usize) {
        if self.head != idx {
            self.unlink(idx);
            self.push_front(idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_by_bytes() {
        let mut c = ByteLru::new(100);
        assert!(c.insert("a", 1, 40).is_empty());
        assert!(c.insert("b", 2, 40).is_empty());
        assert_eq!(c.get(&"a"), Some(&1)); // a is now MRU
        let evicted = c.insert("c", 3, 40);
        assert_eq!(evicted, vec![("b", 2)]);
        assert_eq!(c.bytes(), 80);
        assert!(c.contains(&"a") && c.contains(&"c"));
        let s = c.stats();
        assert_eq!((s.hits, s.evictions, s.entries), (1, 1, 2));
    }

    #[test]
    fn oversized_entries_do_not_break_the_budget() {
        let mut c = ByteLru::new(10);
        c.insert(1, (), 5);
        let evicted = c.insert(2, (), 50);
        assert_eq!(evicted.len(), 2);
        assert_eq!(c.bytes(), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn replace_updates_weight() {
        let mut c = ByteLru::new(100);
        c.insert(1, "x", 10);
        let out = c.insert(1, "y", 30);
        assert_eq!(out, vec![(1, "x")]);
        assert_eq!(c.bytes(), 30);
        assert_eq!(c.len(), 1);
        assert_eq!(c.peek(&1), Some(&"y"));
    }

    #[test]
    fn shrink_retain_and_iter_order() {
        let mut c = ByteLru::new(1_000);
        for i in 0..10 {
            c.insert(i, i * 10, 10);
        }
        c.touch(&0);
        let order: Vec<i32> = c.iter().map(|(k, _)| *k).collect();
        assert_eq!(order, vec![0, 9, 8, 7, 6, 5, 4, 3, 2, 1]);
        let evicted: Vec<i32> = c.shrink_to(50).into_iter().map(|(k, _)| k).collect();
        assert_eq!(evicted, vec![1, 2, 3, 4, 5]);
        let removed = c.retain(|k, _| k % 2 == 0);
        assert_eq!(removed.len(), 2); // 7 and 9
        assert_eq!(c.bytes(), 30);
        assert_eq!(c.stats().evictions, 5);
    }

    #[test]
    fn slots_are_reused() {
        let mut c = ByteLru::new(20);
        for i in 0..1_000 {
            c.insert(i, (), 10);
        }
        assert_eq!(c.len(), 2);
        assert!(c.slots.len() <= 3);
    }

    #[test]
    fn misses_are_counted() {
        let mut c: ByteLru<u8, u8> = ByteLru::new(10);
        assert!(c.get(&1).is_none());
        assert_eq!(c.stats().misses, 1);
        assert_eq!(c.stats().miss_rate(), 1.0);
    }
}
