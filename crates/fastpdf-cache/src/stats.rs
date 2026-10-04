/// Counters every cache exposes (spec §46).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub bytes: usize,
    pub budget: usize,
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    pub evicted_bytes: u64,
}

impl CacheStats {
    /// Hit rate in `0.0..=1.0`; `0.0` before the first lookup.
    pub fn hit_rate(&self) -> f64 {
        let lookups = self.hits + self.misses;
        if lookups == 0 {
            0.0
        } else {
            self.hits as f64 / lookups as f64
        }
    }

    pub fn miss_rate(&self) -> f64 {
        let lookups = self.hits + self.misses;
        if lookups == 0 {
            0.0
        } else {
            self.misses as f64 / lookups as f64
        }
    }
}
