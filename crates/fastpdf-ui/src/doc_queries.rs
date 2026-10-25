//! One-shot questions about the open document (its page labels and its
//! properties) that are answered off the UI thread and kept for as long as
//! the document is open. They live in `OpenDoc`, so closing or replacing
//! the document drops them with everything else; an answer that arrives
//! for a document that is no longer open is thrown away.

use std::sync::Arc;
use std::time::Instant;

use fastpdf_engine_api::{
    DocumentId, DocumentProperties, EngineError, GuardedDocument, PageLabels,
};
use gpui::{Context, Task};

use crate::reader::{DocState, ReaderView};

/// The state of one lazily loaded answer.
pub(crate) enum Lazy<T> {
    /// Not asked yet, or asked and the engine could not answer at the
    /// time (asked again on demand; the owner may also retry on its own).
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
    /// Times the labels were asked for on their own (after the first page;
    /// `crate::page_labels` bounds them). Demands (go to page, a dev-script
    /// step) do not count.
    pub label_attempts: u8,
    /// Asked for when the properties panel shows (`crate::properties`).
    pub properties: Lazy<Arc<DocumentProperties>>,
    /// Why the last properties question got no answer at the time (the
    /// panel offers to ask again).
    pub properties_problem: Option<String>,
}

impl Default for DocQueries {
    fn default() -> Self {
        Self {
            labels: Lazy::NotLoaded,
            label_attempts: 0,
            properties: Lazy::NotLoaded,
            properties_problem: None,
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

/// Puts the answer to a question about document `asked` into `slot` of the
/// open document's queries (`open`: its id and queries, `None` without
/// one). An answer for a document that is no longer open is dropped
/// (`None`); otherwise the queries come back with the error, if any.
pub(crate) fn deliver<T>(
    open: Option<(DocumentId, &mut DocQueries)>,
    asked: DocumentId,
    slot: fn(&mut DocQueries) -> &mut Lazy<T>,
    answer: Result<Option<T>, EngineError>,
) -> Option<(&mut DocQueries, Option<EngineError>)> {
    let (open_id, queries) = open?;
    if open_id != asked {
        return None;
    }
    let error = answer.as_ref().err().cloned();
    *slot(queries) = settle(answer);
    Some((queries, error))
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
    /// (through [`deliver`]) if the same document is still open, then
    /// `done` sees the queries, the error if there was one and the time
    /// since the session started.
    pub(crate) fn load_doc_query<T: Send + 'static>(
        &mut self,
        slot: fn(&mut DocQueries) -> &mut Lazy<T>,
        query: impl FnOnce(&GuardedDocument) -> Result<Option<T>, EngineError> + Send + 'static,
        done: fn(&mut DocQueries, Option<&EngineError>, std::time::Duration),
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
                let open = match &mut this.doc {
                    DocState::Open(open) => Some((open.session.id(), &mut open.queries)),
                    _ => None,
                };
                if let Some((queries, error)) = deliver(open, document, slot, result) {
                    done(
                        queries,
                        error.as_ref(),
                        Instant::now().duration_since(opened_at),
                    );
                    cx.notify();
                }
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

    #[test]
    fn answers_for_a_closed_document_are_dropped() {
        let asked = DocumentId::from_raw(7);
        let labels: fn(&mut DocQueries) -> &mut Lazy<Arc<PageLabels>> = |q| &mut q.labels;
        let answer = || Ok(Some(Arc::new(PageLabels::default())));
        // No document open any more.
        assert!(deliver(None, asked, labels, answer()).is_none());
        // Another document opened meanwhile: its queries stay as they are.
        let mut other = DocQueries::default();
        let replaced = Some((DocumentId::from_raw(8), &mut other));
        assert!(deliver(replaced, asked, labels, answer()).is_none());
        assert_eq!(name(&other.labels), "not loaded");
        // The same document: the answer lands.
        let mut same = DocQueries::default();
        let (queries, error) = deliver(Some((asked, &mut same)), asked, labels, answer()).unwrap();
        assert_eq!(name(&queries.labels), "ready");
        assert!(error.is_none());
        // A failure comes back with its error.
        let mut failed = DocQueries::default();
        let unavailable = Err(EngineError::Unavailable("restarting".into()));
        let (queries, error) =
            deliver(Some((asked, &mut failed)), asked, labels, unavailable).unwrap();
        assert_eq!(name(&queries.labels), "not loaded");
        assert!(matches!(error, Some(EngineError::Unavailable(_))));
    }
}
