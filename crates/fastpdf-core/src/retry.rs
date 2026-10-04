//! Retrying transient engine failures (ADR 0008).
//!
//! With a render host, some failures are of the moment rather than answers
//! about the document: the host is restarting, did not answer in time, or
//! died while handling a request (`EngineError::is_transient`). The session
//! keeps the page's estimated size, shows no error, and asks again after a
//! growing delay; after [`MAX_ATTEMPTS`] the last error is final.
//!
//! A [`RetryClock`] wakes the UI when the earliest retry is due, so pages
//! recover without user input. It never polls: before the first retry there
//! is no thread at all, and with nothing due its thread waits on a condvar
//! without a timeout, using no CPU.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Delay before the first retry; doubled after every failed retry.
pub(crate) const FIRST_DELAY: Duration = Duration::from_millis(250);
/// Longest delay between two attempts.
pub(crate) const MAX_DELAY: Duration = Duration::from_secs(8);
/// Failed attempts (the first one included) before an error is final:
/// about 48 s of trying in total.
pub(crate) const MAX_ATTEMPTS: u32 = 10;

/// Retry state of one item (a page's geometry, a tile, a thumbnail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Backoff {
    /// Failed attempts so far.
    pub attempts: u32,
    /// When the next attempt may start.
    pub next: Instant,
}

impl Backoff {
    /// After a failure: the next attempt's time, or `None` when the item
    /// has failed `MAX_ATTEMPTS` times and the error is final.
    pub(crate) fn after_failure(previous: Option<Self>, now: Instant) -> Option<Self> {
        let attempts = previous.map_or(0, |b| b.attempts) + 1;
        if attempts >= MAX_ATTEMPTS {
            return None;
        }
        let delay = FIRST_DELAY
            .saturating_mul(1 << (attempts - 1).min(16))
            .min(MAX_DELAY);
        Some(Self {
            attempts,
            next: now + delay,
        })
    }

    pub(crate) fn waiting(&self, now: Instant) -> bool {
        now < self.next
    }
}

struct ClockState {
    due: Option<Instant>,
    stop: bool,
}

struct ClockShared {
    state: Mutex<ClockState>,
    cv: Condvar,
}

fn lock(m: &Mutex<ClockState>) -> MutexGuard<'_, ClockState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Calls `wake` once the earliest scheduled time has passed.
pub(crate) struct RetryClock {
    shared: Arc<ClockShared>,
    wake: Arc<dyn Fn() + Send + Sync>,
    started: AtomicBool,
}

impl RetryClock {
    pub(crate) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            shared: Arc::new(ClockShared {
                state: Mutex::new(ClockState {
                    due: None,
                    stop: false,
                }),
                cv: Condvar::new(),
            }),
            wake,
            started: AtomicBool::new(false),
        }
    }

    /// Wakes the UI at `at` (or earlier, when something else is due first).
    pub(crate) fn schedule(&self, at: Instant) {
        {
            let mut st = lock(&self.shared.state);
            st.due = Some(st.due.map_or(at, |due| due.min(at)));
        }
        self.shared.cv.notify_one();
        if !self.started.swap(true, Ordering::AcqRel) {
            let shared = Arc::clone(&self.shared);
            let wake = Arc::clone(&self.wake);
            let spawned = std::thread::Builder::new()
                .name("fastpdf-retry".into())
                .spawn(move || run(&shared, &*wake));
            if spawned.is_err() {
                // No timer thread: retries then wait for the next repaint.
                self.started.store(false, Ordering::Release);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
}

fn run(shared: &ClockShared, wake: &(dyn Fn() + Send + Sync)) {
    loop {
        {
            let mut st = lock(&shared.state);
            loop {
                if st.stop {
                    return;
                }
                match st.due {
                    // Nothing pending: park without a timeout (no CPU).
                    None => st = shared.cv.wait(st).unwrap_or_else(|e| e.into_inner()),
                    Some(at) => {
                        let now = Instant::now();
                        if now >= at {
                            st.due = None;
                            break;
                        }
                        st = shared
                            .cv
                            .wait_timeout(st, at - now)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                }
            }
        }
        wake();
    }
}

impl Drop for RetryClock {
    fn drop(&mut self) {
        lock(&self.shared.state).stop = true;
        self.shared.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;

    #[test]
    fn delays_double_up_to_a_cap_and_attempts_are_bounded() {
        let t = Instant::now();
        let mut b = Backoff::after_failure(None, t).unwrap();
        assert_eq!((b.attempts, b.next - t), (1, FIRST_DELAY));
        let mut delays = vec![b.next - t];
        while let Some(next) = Backoff::after_failure(Some(b), t) {
            delays.push(next.next - t);
            b = next;
        }
        assert_eq!(b.attempts, MAX_ATTEMPTS - 1);
        assert_eq!(delays[1], FIRST_DELAY * 2);
        assert_eq!(*delays.last().unwrap(), MAX_DELAY);
        assert!(delays.windows(2).all(|w| w[0] <= w[1]));
        assert!(b.waiting(t) && !b.waiting(b.next));
    }

    #[test]
    fn the_clock_wakes_once_per_due_time_and_idles_otherwise() {
        let wakes = Arc::new(AtomicU32::new(0));
        let w = Arc::clone(&wakes);
        let clock = RetryClock::new(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(!clock.is_running(), "no thread before the first retry");
        let start = Instant::now();
        clock.schedule(start + Duration::from_millis(80));
        clock.schedule(start + Duration::from_millis(30)); // earlier wins
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        // The later time was replaced, not kept: nothing else is due.
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        clock.schedule(Instant::now());
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        drop(clock);
    }
}
