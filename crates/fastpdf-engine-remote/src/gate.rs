//! Shared/exclusive admission of requests to one host (ADR 0008 §2).
//!
//! Requests for suspect pages run *exclusively* — with nothing else in
//! flight — so that if the host crashes again the culprit is known. The gate
//! prefers exclusive waiters, so a steady stream of tile requests cannot
//! starve a retry, and every wait polls the request's [`CancelToken`].
//!
//! A pass is owned: it is stored with its request and released only when
//! the host has sent that request's terminal reply (or died), not when the
//! caller stops waiting. A cancelled request that the host is still working
//! on therefore keeps its admission, and an exclusive one keeps everything
//! else out until it is really over.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use fastpdf_engine_api::{CancelToken, EngineError};

const POLL: Duration = Duration::from_millis(5);

#[derive(Debug, Default)]
struct State {
    shared: u32,
    exclusive: bool,
    waiting_exclusive: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Gate {
    state: Mutex<State>,
    cv: Condvar,
}

/// Admission to the host; released on drop.
#[derive(Debug)]
pub(crate) struct OwnedPass {
    gate: Arc<Gate>,
    exclusive: bool,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Gate {
    pub(crate) fn enter(
        self: &Arc<Self>,
        exclusive: bool,
        cancel: Option<&CancelToken>,
    ) -> Result<OwnedPass, EngineError> {
        let mut st = lock(&self.state);
        if exclusive {
            st.waiting_exclusive += 1;
        }
        loop {
            let free = if exclusive {
                !st.exclusive && st.shared == 0
            } else {
                !st.exclusive && st.waiting_exclusive == 0
            };
            if free {
                break;
            }
            if cancel.is_some_and(CancelToken::is_cancelled) {
                if exclusive {
                    st.waiting_exclusive -= 1;
                    self.cv.notify_all();
                }
                return Err(EngineError::Cancelled);
            }
            st = self
                .cv
                .wait_timeout(st, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if exclusive {
            st.waiting_exclusive -= 1;
            st.exclusive = true;
        } else {
            st.shared += 1;
        }
        Ok(OwnedPass {
            gate: Arc::clone(self),
            exclusive,
        })
    }
}

impl Drop for OwnedPass {
    fn drop(&mut self) {
        let mut st = lock(&self.gate.state);
        if self.exclusive {
            st.exclusive = false;
        } else {
            st.shared = st.shared.saturating_sub(1);
        }
        drop(st);
        self.gate.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use super::*;

    #[test]
    fn exclusive_passes_wait_for_shared_ones_and_block_new_ones() {
        let gate = Arc::new(Gate::default());
        let shared = gate.enter(false, None).unwrap();
        let inside = Arc::new(AtomicU32::new(0));
        let g = Arc::clone(&gate);
        let i = Arc::clone(&inside);
        let exclusive = std::thread::spawn(move || {
            let _pass = g.enter(true, None).unwrap();
            i.store(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            i.store(2, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(inside.load(Ordering::SeqCst), 0, "exclusive entered early");
        // A new shared request now queues behind the exclusive waiter.
        let g = Arc::clone(&gate);
        let i = Arc::clone(&inside);
        let late = std::thread::spawn(move || {
            let _pass = g.enter(false, None).unwrap();
            i.load(Ordering::SeqCst)
        });
        std::thread::sleep(Duration::from_millis(10));
        drop(shared);
        exclusive.join().unwrap();
        assert_eq!(
            late.join().unwrap(),
            2,
            "shared request overtook the exclusive one"
        );
    }

    #[test]
    fn a_pass_handed_to_another_thread_keeps_admission() {
        let gate = Arc::new(Gate::default());
        let pass = gate.enter(true, None).unwrap();
        // The pass moves to whoever waits for the host's terminal reply.
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(pass);
        });
        let started = Instant::now();
        let _next = gate.enter(false, None).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(50));
        holder.join().unwrap();
    }

    #[test]
    fn waits_honor_cancellation() {
        let gate = Arc::new(Gate::default());
        let held = gate.enter(true, None).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let started = Instant::now();
        assert_eq!(
            gate.enter(false, Some(&cancel)).map(drop),
            Err(EngineError::Cancelled)
        );
        assert_eq!(
            gate.enter(true, Some(&cancel)).map(drop),
            Err(EngineError::Cancelled)
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        // The cancelled exclusive waiter did not leave a stale reservation.
        drop(held);
        assert!(gate.enter(false, None).is_ok());
    }
}
