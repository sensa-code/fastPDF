//! Process memory monitoring that feeds the memory budget manager (spec §16).
//!
//! There is no timer: the monitor samples only when the app reports work
//! (a tile arrived, a document opened), at most once per interval, so an
//! idle reader never wakes up for memory bookkeeping (spec §1: idle CPU ≈ 0).

use std::fmt;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use fastpdf_cache::{MemoryBudgetManager, MemoryPressure, Relief};
use fastpdf_engine_api::{EngineDocument, GuardedDocument};

pub struct MemoryMonitor {
    manager: Arc<MemoryBudgetManager>,
    documents: Vec<Weak<GuardedDocument>>,
    min_interval: Duration,
    last_sample: Option<Instant>,
    last_pressure: MemoryPressure,
    probe: Box<dyn Fn() -> Option<u64> + Send>,
}

impl fmt::Debug for MemoryMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryMonitor")
            .field("manager", &self.manager)
            .field("last_pressure", &self.last_pressure)
            .finish_non_exhaustive()
    }
}

impl MemoryMonitor {
    pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

    pub fn new(manager: Arc<MemoryBudgetManager>) -> Self {
        Self::with_probe(manager, Self::DEFAULT_INTERVAL, process_private_bytes)
    }

    /// `probe` returns the process's private (committed) bytes.
    pub fn with_probe(
        manager: Arc<MemoryBudgetManager>,
        min_interval: Duration,
        probe: impl Fn() -> Option<u64> + Send + 'static,
    ) -> Self {
        Self {
            manager,
            documents: Vec::new(),
            min_interval,
            last_sample: None,
            last_pressure: MemoryPressure::Normal,
            probe: Box::new(probe),
        }
    }

    pub fn manager(&self) -> &Arc<MemoryBudgetManager> {
        &self.manager
    }

    /// Documents receive `trim_memory` when pressure rises.
    pub fn watch(&mut self, doc: &Arc<GuardedDocument>) {
        self.documents.retain(|d| d.strong_count() > 0);
        self.documents.push(Arc::downgrade(doc));
    }

    pub fn last_pressure(&self) -> MemoryPressure {
        self.last_pressure
    }

    /// Call after work happened. Samples at most once per interval and
    /// relieves pressure when needed.
    pub fn poll(&mut self) -> Option<Relief> {
        let now = Instant::now();
        if self
            .last_sample
            .is_some_and(|t| now.duration_since(t) < self.min_interval)
        {
            return None;
        }
        self.last_sample = Some(now);
        let private = (self.probe)()?;
        // Memory we do not track in registered caches: engine-internal
        // caches, allocator slack, GPU staging, code.
        let external = usize::try_from(private)
            .unwrap_or(usize::MAX)
            .saturating_sub(self.manager.cache_bytes());
        let relief = self.manager.relieve(external);
        if relief.pressure > MemoryPressure::Normal {
            for doc in self.documents.iter().filter_map(Weak::upgrade) {
                doc.trim_memory(relief.pressure);
            }
        }
        self.last_pressure = relief.pressure;
        Some(relief)
    }
}

/// Committed private bytes of this process.
pub fn process_private_bytes() -> Option<u64> {
    imp::private_bytes()
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub(super) fn private_bytes() -> Option<u64> {
        let mut c = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: pseudo-handle of the current process; `c` is a writable,
        // correctly sized PROCESS_MEMORY_COUNTERS_EX with `cb` set.
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
                c.cb,
            )
        };
        (ok != 0).then_some(c.PrivateUsage as u64)
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn private_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmData:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_cache::{BudgetConfig, SharedCache, retention};
    use std::sync::atomic::{AtomicU64, Ordering};

    const MB: u64 = 1024 * 1024;

    #[test]
    fn samples_are_rate_limited_and_relieve_pressure() {
        let manager = Arc::new(MemoryBudgetManager::new(BudgetConfig {
            soft_limit: 100 * MB as usize,
            hard_limit: 200 * MB as usize,
            relief_target_percent: 80,
        }));
        let thumbs = Arc::new(SharedCache::new(
            "thumbs",
            usize::MAX,
            retention::THUMBNAILS,
        ));
        for i in 0..50 {
            thumbs.insert(i, (), MB as usize);
        }
        manager.register(thumbs.clone());

        let private = Arc::new(AtomicU64::new(60 * MB));
        let probe_value = Arc::clone(&private);
        let mut monitor =
            MemoryMonitor::with_probe(manager, Duration::from_secs(3600), move || {
                Some(probe_value.load(Ordering::Relaxed))
            });

        // 60 MB private includes the 50 MB of thumbnails: no pressure.
        assert_eq!(
            monitor.poll().map(|r| r.pressure),
            Some(MemoryPressure::Normal)
        );
        // Rate limited: the next call does not sample at all.
        private.store(500 * MB, Ordering::Relaxed);
        assert_eq!(monitor.poll(), None);

        let mut monitor =
            MemoryMonitor::with_probe(monitor.manager().clone(), Duration::ZERO, move || {
                Some(private.load(Ordering::Relaxed))
            });
        let relief = monitor.poll().unwrap();
        assert_eq!(relief.pressure, MemoryPressure::Hard);
        assert_eq!(fastpdf_cache::BudgetedCache::bytes(thumbs.as_ref()), 0);
    }

    #[test]
    fn real_probe_reports_something() {
        assert!(process_private_bytes().is_some_and(|b| b > 0));
    }
}
