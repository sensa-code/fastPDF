use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::EngineError;

/// Cooperative cancellation flag shared between the scheduler and a render job.
///
/// The scheduler cancels jobs the user scrolled away from (spec §14). Engines
/// that support cooperative cancellation poll [`CancelToken::is_cancelled`];
/// for the others the guard layer checks the token before and after the call
/// so stale results are dropped instead of displayed.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// `Err(EngineError::Cancelled)` once cancelled; convenient with `?`.
    pub fn check(&self) -> Result<(), EngineError> {
        if self.is_cancelled() {
            Err(EngineError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_state() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(b.check().is_ok());
        a.cancel();
        assert!(b.is_cancelled());
        assert!(matches!(b.check(), Err(EngineError::Cancelled)));
    }
}
