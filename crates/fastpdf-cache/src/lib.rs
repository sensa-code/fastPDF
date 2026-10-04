//! Byte-budgeted caches and the global memory budget manager.
//!
//! Spec §15–§16 and §46: no cache may grow without bound, every cache is
//! observable (bytes, entries, hit/miss rate, evictions), and a single
//! [`MemoryBudgetManager`] decides what to drop under memory pressure.

mod budget;
mod lru;
mod shared;
mod stats;

pub use budget::{
    BudgetConfig, BudgetedCache, CacheSnapshot, MemoryBudgetManager, Relief, retention,
};
pub use lru::ByteLru;
pub use shared::SharedCache;
pub use stats::CacheStats;

pub use fastpdf_engine_api::MemoryPressure;
