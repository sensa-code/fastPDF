//! Process-wide budget for the memory hayro needs while it decodes images
//! (benchmark plan B-5, `docs/benchmarks/b5-memory.md`).
//!
//! hayro decodes every image of a page at full resolution on every render
//! and converts it to premultiplied RGBA; nothing is cached, so this memory
//! is transient. It is also large: a 600-dpi A4 colour scan needs ~240 MB
//! per decode, and the render threads decoding such pages at the same time
//! push the process past the hard memory limit for a moment — too briefly
//! for the memory monitor to notice, and nothing it could evict anyway.
//!
//! Renders of pages whose largest image needs at least `ADMIT_FREELY` (the
//! static scan estimates it, see `scan.rs`) wait here until their bytes fit
//! into the budget. A page needing more than the whole budget runs alone.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use fastpdf_engine_api::{CancelToken, EngineError};

use crate::document::lock;
use crate::stats;

/// Decode working memory all render threads of the process may use at once.
pub(crate) const BUDGET: u64 = 256 * 1024 * 1024;
/// Renders needing less than this never wait (text, vector art, photos up
/// to a few megapixels).
const ADMIT_FREELY: u64 = 16 * 1024 * 1024;
/// How often a waiting render re-checks its cancel token.
const WAIT_POLL: Duration = Duration::from_millis(20);

pub(crate) struct DecodeBudget {
    capacity: u64,
    in_use: Mutex<u64>,
    released: Condvar,
}

static PROCESS: DecodeBudget = DecodeBudget::new(BUDGET);

/// The budget shared by every document of the process.
pub(crate) fn process() -> &'static DecodeBudget {
    &PROCESS
}

impl DecodeBudget {
    pub(crate) const fn new(capacity: u64) -> Self {
        Self {
            capacity,
            in_use: Mutex::new(0),
            released: Condvar::new(),
        }
    }

    /// Waits until `bytes` fit into the budget. The returned guard gives
    /// them back when dropped.
    pub(crate) fn admit(
        &self,
        bytes: u64,
        cancel: Option<&CancelToken>,
    ) -> Result<Admission<'_>, EngineError> {
        if bytes < ADMIT_FREELY {
            return Ok(Admission {
                budget: self,
                bytes: 0,
            });
        }
        let counters = stats::counters();
        let mut in_use = lock(&self.in_use);
        let mut waited = false;
        // Always admit when nothing else is decoding, so a page larger than
        // the whole budget still renders (alone).
        while *in_use > 0 && in_use.saturating_add(bytes) > self.capacity {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(EngineError::Cancelled);
            }
            waited = true;
            in_use = self
                .released
                .wait_timeout(in_use, WAIT_POLL)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        *in_use += bytes;
        stats::add(&counters.decode_bytes, bytes);
        stats::add(&counters.decode_admissions, 1);
        if waited {
            stats::add(&counters.decode_waits, 1);
        }
        Ok(Admission {
            budget: self,
            bytes,
        })
    }

    #[cfg(test)]
    fn in_use(&self) -> u64 {
        *lock(&self.in_use)
    }
}

/// Bytes of the decode budget held by one render.
pub(crate) struct Admission<'a> {
    budget: &'a DecodeBudget,
    bytes: u64,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        let mut in_use = lock(&self.budget.in_use);
        *in_use = in_use.saturating_sub(self.bytes);
        drop(in_use);
        stats::sub(&stats::counters().decode_bytes, self.bytes);
        self.budget.released.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn small_renders_never_wait_or_count() {
        let budget = DecodeBudget::new(100 * MB);
        let a = budget.admit(ADMIT_FREELY - 1, None).unwrap();
        let b = budget.admit(ADMIT_FREELY - 1, None).unwrap();
        assert_eq!(budget.in_use(), 0);
        drop((a, b));
    }

    #[test]
    fn large_renders_wait_for_room() {
        let budget = DecodeBudget::new(100 * MB);
        let first = budget.admit(60 * MB, None).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                let second = budget.admit(60 * MB, None).unwrap();
                tx.send(budget.in_use()).unwrap();
                drop(second);
            });
            // The second render cannot start while the first holds 60 MB.
            assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
            drop(first);
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 60 * MB);
        });
        assert_eq!(budget.in_use(), 0);
    }

    #[test]
    fn oversized_renders_run_alone() {
        let budget = DecodeBudget::new(100 * MB);
        let huge = budget.admit(300 * MB, None).unwrap();
        assert_eq!(budget.in_use(), 300 * MB);
        drop(huge);
        assert_eq!(budget.in_use(), 0);
    }

    #[test]
    fn waiting_renders_can_be_cancelled() {
        let budget = DecodeBudget::new(100 * MB);
        let _held = budget.admit(80 * MB, None).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            budget.admit(40 * MB, Some(&cancel)),
            Err(EngineError::Cancelled)
        ));
        assert_eq!(budget.in_use(), 80 * MB);
    }
}
