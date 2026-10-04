//! Page geometry of one document, kept in FastPDF (ADR 0008 §1.1).
//!
//! FastPDF's UI thread calls `page_info` while laying out pages, so it must
//! never wait for render work. Geometry is therefore fetched from the host
//! right after opening (the first pages synchronously, the rest in the
//! background) and kept here, outside any host's lifetime: once a page's
//! geometry is known, host crashes and restarts can no longer make it
//! unavailable. Engine errors for a page's geometry are kept as well; they
//! are answers, not transient failures.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Instant;

use fastpdf_engine_api::{EngineError, PageInfo};

pub(crate) struct Geometry {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    pages: Vec<Option<PageInfo>>,
    errors: HashMap<u32, EngineError>,
    missing: u32,
}

impl State {
    fn get(&self, page: u32) -> Option<Result<PageInfo, EngineError>> {
        let index = usize::try_from(page).ok()?;
        if let Some(Some(info)) = self.pages.get(index) {
            return Some(Ok(*info));
        }
        self.errors.get(&page).cloned().map(Err)
    }

    fn known(&self, page: u32) -> bool {
        usize::try_from(page)
            .ok()
            .and_then(|i| self.pages.get(i))
            .is_some_and(Option::is_some)
            || self.errors.contains_key(&page)
    }

    fn store(&mut self, page: u32, result: &Result<PageInfo, EngineError>) -> bool {
        let Some(slot) = usize::try_from(page).ok().and_then(|i| self.pages.get(i)) else {
            return false;
        };
        if slot.is_some() || self.errors.contains_key(&page) {
            return false;
        }
        match result {
            Ok(info) => self.pages[page as usize] = Some(*info),
            Err(e) => {
                self.errors.insert(page, e.clone());
            }
        }
        self.missing = self.missing.saturating_sub(1);
        true
    }
}

impl Geometry {
    pub(crate) fn new(page_count: u32) -> Self {
        Self {
            state: Mutex::new(State {
                pages: vec![None; page_count as usize],
                errors: HashMap::new(),
                missing: page_count,
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn get(&self, page: u32) -> Option<Result<PageInfo, EngineError>> {
        self.lock().get(page)
    }

    /// Pages whose geometry is not known yet.
    pub(crate) fn missing(&self) -> u32 {
        self.lock().missing
    }

    pub(crate) fn store(&self, page: u32, result: &Result<PageInfo, EngineError>) {
        if self.lock().store(page, result) {
            self.changed.notify_all();
        }
    }

    pub(crate) fn store_batch(&self, first: u32, results: &[Result<PageInfo, EngineError>]) {
        let mut stored = false;
        {
            let mut st = self.lock();
            for (i, result) in results.iter().enumerate() {
                let Some(page) = u32::try_from(i).ok().and_then(|i| first.checked_add(i)) else {
                    break;
                };
                stored |= st.store(page, result);
            }
        }
        if stored {
            self.changed.notify_all();
        }
    }

    /// The first run of unknown pages at or after `from`, at most `max`
    /// long: `(first, count)`.
    pub(crate) fn next_gap(&self, from: u32, max: u32) -> Option<(u32, u32)> {
        let st = self.lock();
        let len = u32::try_from(st.pages.len()).unwrap_or(u32::MAX);
        let first = (from..len).find(|p| !st.known(*p))?;
        let mut count = 0;
        while count < max && first + count < len && !st.known(first + count) {
            count += 1;
        }
        Some((first, count))
    }

    /// Waits until `page` is known or `until` has passed.
    pub(crate) fn wait(&self, page: u32, until: Instant) -> Option<Result<PageInfo, EngineError>> {
        let mut st = self.lock();
        loop {
            if let Some(result) = st.get(page) {
                return Some(result);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            st = self
                .changed
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fastpdf_engine_api::{PageSize, Rotation};

    use super::*;

    fn info(w: f32) -> PageInfo {
        PageInfo {
            size: PageSize::new(w, 10.0),
            rotation: Rotation::R0,
        }
    }

    #[test]
    fn results_are_kept_and_gaps_found() {
        let g = Geometry::new(10);
        assert_eq!(g.missing(), 10);
        g.store_batch(0, &[Ok(info(1.0)), Ok(info(2.0))]);
        g.store(5, &Err(EngineError::Malformed("bad page".into())));
        g.store(1, &Ok(info(99.0))); // first answer wins
        assert_eq!(g.get(1), Some(Ok(info(2.0))));
        assert!(matches!(g.get(5), Some(Err(EngineError::Malformed(_)))));
        assert_eq!(g.get(3), None);
        assert_eq!(g.missing(), 7);
        assert_eq!(g.next_gap(0, 100), Some((2, 3)));
        assert_eq!(g.next_gap(0, 2), Some((2, 2)));
        assert_eq!(g.next_gap(5, 100), Some((6, 4)));
        // Out-of-range pages are ignored.
        g.store(10, &Ok(info(1.0)));
        g.store_batch(9, &[Ok(info(1.0)), Ok(info(1.0))]);
        assert_eq!(g.missing(), 6);
        assert_eq!(g.next_gap(9, 5), None);
    }

    #[test]
    fn waiters_wake_up_when_their_page_arrives() {
        let g = Arc::new(Geometry::new(4));
        let g2 = Arc::clone(&g);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            g2.store(3, &Ok(info(7.0)));
        });
        let started = Instant::now();
        let got = g.wait(3, Instant::now() + Duration::from_secs(5));
        assert_eq!(got, Some(Ok(info(7.0))));
        assert!(started.elapsed() < Duration::from_secs(2));
        writer.join().unwrap();
        // Unknown pages give up at the deadline.
        let started = Instant::now();
        assert_eq!(g.wait(0, Instant::now() + Duration::from_millis(40)), None);
        assert!(started.elapsed() >= Duration::from_millis(35));
    }
}
