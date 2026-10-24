//! One-shot questions about the open document (its page labels, later its
//! properties) that are answered off the UI thread and kept for as long as
//! the document is open. They live in `OpenDoc`, so closing or replacing
//! the document drops them with everything else; an answer that arrives
//! for a document that is no longer open is thrown away.

use std::sync::Arc;
use std::time::Instant;

use fastpdf_engine_api::{EngineError, GuardedDocument, PageLabels};
use gpui::{Context, Task};

use crate::reader::{DocState, ReaderView};

/// The state of one lazily loaded answer.
pub(crate) enum Lazy<T> {
    /// Not asked yet, or asked and the engine could not answer at the
    /// time (asked again on the next demand).
    NotLoaded,
    /// The question runs in the background.
    Loading(#[allow(dead_code)] Task<()>),
    Ready(T),
    /// The document has nothing to show, or the engine cannot tell.
    Absent,
    Failed(String),
}

impl<T> Lazy<T> {
    /// Whether a question is still worth asking or waiting for.
    pub(crate) fn is_settled(&self) -> bool {
        matches!(self, Self::Ready(_) | Self::Absent | Self::Failed(_))
    }
}

/// The open document's lazily loaded answers.
pub(crate) struct DocQueries {
    pub labels: Lazy<Arc<PageLabels>>,
    /// The labels were asked for once on their own (after the first page);
    /// after that only a demand (go to page, a dev-script step) asks again.
    pub labels_requested: bool,
}

impl Default for DocQueries {
    fn default() -> Self {
        Self {
            labels: Lazy::NotLoaded,
            labels_requested: false,
        }
    }
}

/// What an engine answer means for the slot: something to keep, nothing
/// to show, a failure of the moment (ask again later) or a final failure.
pub(crate) fn settle<T>(result: Result<Option<T>, EngineError>) -> Lazy<T> {
    match result {
        Ok(Some(value)) => Lazy::Ready(value),
        Ok(None) | Err(EngineError::Unsupported(_)) => Lazy::Absent,
        Err(e) if e.is_transient() => Lazy::NotLoaded,
        Err(e) => Lazy::Failed(e.to_string()),
    }
}

impl ReaderView {
    pub(crate) fn doc_queries(&self) -> Option<&DocQueries> {
        match &self.doc {
            DocState::Open(open) => Some(&open.queries),
            _ => None,
        }
    }

    /// Asks `query` about the open document on a background thread, unless
    /// `slot` is already loading or settled; the answer goes into `slot`
    /// (through [`settle`]) if the same document is still open, then
    /// `done` sees it with the time since the session started.
    pub(crate) fn load_doc_query<T: Send + 'static>(
        &mut self,
        slot: fn(&mut DocQueries) -> &mut Lazy<T>,
        query: impl FnOnce(&GuardedDocument) -> Result<Option<T>, EngineError> + Send + 'static,
        done: fn(&Lazy<T>, std::time::Duration),
        cx: &mut Context<'_, Self>,
    ) {
        let DocState::Open(open) = &mut self.doc else {
            return;
        };
        if !matches!(slot(&mut open.queries), Lazy::NotLoaded) {
            return;
        }
        let doc = Arc::clone(open.session.document());
        let document = open.session.id();
        let opened_at = open.opened_at;
        let work = cx
            .background_executor()
            .spawn(async move { query(doc.as_ref()) });
        let task = cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                let DocState::Open(open) = &mut this.doc else {
                    return;
                };
                if open.session.id() != document {
                    return;
                }
                let state = slot(&mut open.queries);
                *state = settle(result);
                done(state, Instant::now().duration_since(opened_at));
                cx.notify();
            });
        });
        if let DocState::Open(open) = &mut self.doc {
            *slot(&mut open.queries) = Lazy::Loading(task);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{HostExit, HostExitReason};

    fn name<T>(lazy: &Lazy<T>) -> &'static str {
        match lazy {
            Lazy::NotLoaded => "not loaded",
            Lazy::Loading(_) => "loading",
            Lazy::Ready(_) => "ready",
            Lazy::Absent => "absent",
            Lazy::Failed(_) => "failed",
        }
    }

    #[test]
    fn answers_settle_by_kind() {
        assert_eq!(name(&settle(Ok(Some(1)))), "ready");
        assert_eq!(name(&settle::<u8>(Ok(None))), "absent");
        assert_eq!(
            name(&settle::<u8>(Err(EngineError::Unsupported("x".into())))),
            "absent"
        );
        // Failures of the moment are asked again later.
        assert_eq!(
            name(&settle::<u8>(Err(EngineError::Unavailable(
                "restarting".into()
            )))),
            "not loaded"
        );
        let exit = HostExit {
            reason: HostExitReason::Crashed { exit_code: None },
            permanent: false,
        };
        assert_eq!(
            name(&settle::<u8>(Err(EngineError::HostExited(exit)))),
            "not loaded"
        );
        let permanent = HostExit {
            permanent: true,
            ..exit
        };
        let failed = settle::<u8>(Err(EngineError::HostExited(permanent)));
        assert_eq!(name(&failed), "failed");
        assert!(failed.is_settled());
        assert_eq!(
            name(&settle::<u8>(Err(EngineError::Panicked("boom".into())))),
            "failed"
        );
        assert!(!Lazy::<u8>::NotLoaded.is_settled());
        assert!(Lazy::<u8>::Absent.is_settled());
    }
}
