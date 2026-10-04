//! Process-wide memory counters of the adapter, read by benchmarks through
//! `diagnostics::memory` (benchmark plan B-5). They cover the memory the
//! adapter holds outside FastPDF's budgeted caches: rendered blocks and the
//! content streams hayro keeps decoded inside each `Pdf` generation.
//!
//! Relaxed atomics: the values are exact once rendering is idle and
//! approximate while renders are in flight.

use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct Counters {
    /// Bytes of finished blocks in every document's block cache.
    pub(crate) block_bytes: AtomicU64,
    /// Decoded content-stream bytes accounted in live generations.
    pub(crate) content_bytes: AtomicU64,
    /// Live documents (`DocInner`), counted until every field is dropped.
    pub(crate) documents: AtomicU64,
    /// Live `Pdf` generations (one per open document, plus replaced ones
    /// that a render thread still uses), counted until the `Pdf` is dropped.
    pub(crate) generations: AtomicU64,
    /// Generations replaced by a reopen since process start.
    pub(crate) reopens: AtomicU64,
    /// Live render-pool threads, and those executing a job.
    pub(crate) threads: AtomicU64,
    pub(crate) busy_threads: AtomicU64,
    /// `trim_memory` calls since process start.
    pub(crate) soft_trims: AtomicU64,
    pub(crate) hard_trims: AtomicU64,
    /// Estimated buffers of the pool threads' render contexts.
    pub(crate) context_bytes: AtomicU64,
    /// Decode budget held by running renders (`decode.rs`).
    pub(crate) decode_bytes: AtomicU64,
    /// Renders admitted by the decode budget, and those that had to wait.
    pub(crate) decode_admissions: AtomicU64,
    pub(crate) decode_waits: AtomicU64,
}

static COUNTERS: Counters = Counters {
    block_bytes: AtomicU64::new(0),
    content_bytes: AtomicU64::new(0),
    documents: AtomicU64::new(0),
    generations: AtomicU64::new(0),
    reopens: AtomicU64::new(0),
    threads: AtomicU64::new(0),
    busy_threads: AtomicU64::new(0),
    soft_trims: AtomicU64::new(0),
    hard_trims: AtomicU64::new(0),
    context_bytes: AtomicU64::new(0),
    decode_bytes: AtomicU64::new(0),
    decode_admissions: AtomicU64::new(0),
    decode_waits: AtomicU64::new(0),
};

pub(crate) fn counters() -> &'static Counters {
    &COUNTERS
}

pub(crate) fn add(gauge: &AtomicU64, n: u64) {
    gauge.fetch_add(n, Ordering::Relaxed);
}

/// Saturating, so a bookkeeping slip can never wrap a gauge around.
pub(crate) fn sub(gauge: &AtomicU64, n: u64) {
    // A CAS loop: `AtomicU64::update` needs a newer Rust than the MSRV.
    let mut current = gauge.load(Ordering::Relaxed);
    while let Err(actual) = gauge.compare_exchange_weak(
        current,
        current.saturating_sub(n),
        Ordering::Relaxed,
        Ordering::Relaxed,
    ) {
        current = actual;
    }
}

/// Adds bytes to a gauge until dropped.
pub(crate) struct Held<'a> {
    gauge: &'a AtomicU64,
    bytes: u64,
}

impl<'a> Held<'a> {
    pub(crate) fn new(gauge: &'a AtomicU64, bytes: u64) -> Self {
        add(gauge, bytes);
        Self { gauge, bytes }
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        sub(self.gauge, self.bytes);
    }
}

/// Counts one live object in a gauge until dropped, on every exit path
/// (panics included). As the last field of a struct it is dropped after
/// all other fields, so the gauge only falls once their memory is freed.
pub(crate) struct Live(&'static AtomicU64);

impl Live {
    pub(crate) fn thread() -> Self {
        Self::new(&COUNTERS.threads)
    }

    pub(crate) fn document() -> Self {
        Self::new(&COUNTERS.documents)
    }

    pub(crate) fn generation() -> Self {
        Self::new(&COUNTERS.generations)
    }

    fn new(gauge: &'static AtomicU64) -> Self {
        add(gauge, 1);
        Self(gauge)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        sub(self.0, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_guards_count_until_dropped() {
        static GAUGE: AtomicU64 = AtomicU64::new(0);
        let a = Live::new(&GAUGE);
        let b = Live::new(&GAUGE);
        assert_eq!(GAUGE.load(Ordering::Relaxed), 2);
        drop(a);
        assert_eq!(GAUGE.load(Ordering::Relaxed), 1);
        drop(b);
        assert_eq!(GAUGE.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn gauges_saturate_at_zero() {
        let gauge = AtomicU64::new(3);
        sub(&gauge, 5);
        assert_eq!(gauge.load(Ordering::Relaxed), 0);
        add(&gauge, 2);
        assert_eq!(gauge.load(Ordering::Relaxed), 2);
    }
}
