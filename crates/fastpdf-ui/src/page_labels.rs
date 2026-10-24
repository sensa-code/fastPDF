//! Page labels in the reader (the document's own page numbering, such as
//! `iii` or `A-1`): the toolbar shows `iii (3 / 120)`, thumbnails `iii (3)`,
//! the scroll-bar drag label the same as the toolbar (`display.rs`).
//!
//! Labels are asked for after the document's first page is fully rendered
//! (never while it opens), or when something needs them (go to page, a
//! dev-script step). When the render host could not answer at the time,
//! they are asked for once more on a later wake-up, unless the request
//! itself brought the host down. Documents whose labels are just their
//! page numbers count as having none, so nothing extra is shown; a label
//! equal to the page number, or empty, is not shown either.

mod display;

use std::sync::Arc;
use std::time::Duration;

use fastpdf_engine_api::{EngineDocument, EngineError, PageIndex, PageLabels};
use gpui::Context;

pub(crate) use self::display::indicator;
use crate::doc_queries::{DocQueries, Lazy};
use crate::reader::{DocState, ReaderView};

/// Requests for the labels made without a demand: the first after the
/// first page, and one more after the host could not answer at the time.
const AUTO_ATTEMPTS: u8 = 2;

impl ReaderView {
    /// Asks for the open document's page labels unless they are known or
    /// on their way.
    pub(crate) fn ensure_page_labels(&mut self, cx: &mut Context<'_, Self>) {
        self.load_doc_query(
            |queries| &mut queries.labels,
            |doc| {
                let labels = doc.page_labels()?;
                Ok((!labels.is_plain_numbers()).then(|| Arc::new(labels)))
            },
            labels_answered,
            cx,
        );
    }

    /// On every wake-up (cheap): once the first page is fully rendered, the
    /// labels are asked for, and asked again once after a failure of the
    /// moment.
    pub(crate) fn poll_page_labels(&mut self, cx: &mut Context<'_, Self>) {
        let due = match &mut self.doc {
            DocState::Open(open)
                if auto_request_due(
                    open.exact_reported,
                    &open.queries.labels,
                    open.queries.label_attempts,
                ) =>
            {
                open.queries.label_attempts += 1;
                true
            }
            _ => false,
        };
        if due {
            self.ensure_page_labels(cx);
        }
    }

    /// The labels, once loaded (documents with plain numbering have none).
    pub(crate) fn page_labels(&self) -> Option<&PageLabels> {
        match &self.doc_queries()?.labels {
            Lazy::Ready(labels) => Some(labels),
            _ => None,
        }
    }

    /// Whether the labels question is answered (or can no longer be).
    pub(crate) fn page_labels_settled(&self) -> bool {
        self.doc_queries().is_some_and(|q| q.labels.is_settled())
    }

    /// The label of `page` when it says something its number does not.
    pub(crate) fn page_label(&self, page: PageIndex) -> Option<String> {
        shown_label(self.page_labels()?.label(page)?, page.display_number())
    }
}

/// A label worth showing next to page `number`: trimmed, not empty (a range
/// with neither style nor prefix), not just the number again.
fn shown_label(label: String, number: u32) -> Option<String> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed == number.to_string() {
        None
    } else if trimmed.len() == label.len() {
        Some(label)
    } else {
        Some(trimmed.to_owned())
    }
}

/// Whether the labels should be asked for without a demand now.
fn auto_request_due(exact_reported: bool, labels: &Lazy<Arc<PageLabels>>, attempts: u8) -> bool {
    exact_reported && attempts < AUTO_ATTEMPTS && matches!(labels, Lazy::NotLoaded)
}

/// An answer came in: logged, and when the request itself brought the
/// render host down, nothing asks again without a demand.
fn labels_answered(queries: &mut DocQueries, error: Option<&EngineError>, since_open: Duration) {
    log_labels(&queries.labels, since_open);
    queries.label_attempts = attempts_after(queries.label_attempts, error);
}

fn attempts_after(attempts: u8, error: Option<&EngineError>) -> u8 {
    if matches!(error, Some(EngineError::HostExited(_))) {
        AUTO_ATTEMPTS
    } else {
        attempts
    }
}

fn log_labels(state: &Lazy<Arc<PageLabels>>, since_open: Duration) {
    let ms = since_open.as_secs_f64() * 1000.0;
    match state {
        Lazy::Ready(labels) => log::info!(
            "page labels: {} ranges, ready {ms:.1} ms after the session started",
            labels.ranges().len()
        ),
        Lazy::Absent => log::info!("page labels: none, ready {ms:.1} ms after the session started"),
        Lazy::Failed(e) => log::warn!("page labels failed: {e}"),
        Lazy::NotLoaded => log::debug!("page labels are not available yet"),
        Lazy::Loading(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use fastpdf_engine_api::{HostExit, HostExitReason};

    use super::*;

    #[test]
    fn empty_labels_and_plain_numbers_are_not_shown() {
        assert_eq!(shown_label("iii".into(), 3), Some("iii".into()));
        assert_eq!(shown_label(String::new(), 1), None);
        assert_eq!(shown_label("  ".into(), 1), None);
        assert_eq!(shown_label("3".into(), 3), None);
        assert_eq!(shown_label(" 3 ".into(), 3), None);
        assert_eq!(shown_label(" A-1".into(), 9), Some("A-1".into()));
    }

    #[test]
    fn labels_are_asked_again_once_after_a_failure_of_the_moment() {
        let not_loaded = Lazy::NotLoaded;
        // Not before the first page is on screen.
        assert!(!auto_request_due(false, &not_loaded, 0));
        assert!(auto_request_due(true, &not_loaded, 0));
        // The host was restarting: one more try on a later wake-up.
        let unavailable = EngineError::Unavailable("restarting".into());
        let attempts = attempts_after(1, Some(&unavailable));
        assert!(auto_request_due(true, &not_loaded, attempts));
        assert!(!auto_request_due(true, &not_loaded, attempts + 1));
        // The request brought the host down: only a demand asks again.
        let crashed = EngineError::HostExited(HostExit {
            reason: HostExitReason::Crashed { exit_code: None },
            permanent: false,
        });
        assert!(!auto_request_due(
            true,
            &not_loaded,
            attempts_after(1, Some(&crashed))
        ));
        // Answered or on its way: nothing to ask.
        assert!(!auto_request_due(true, &Lazy::Absent, 0));
        assert!(!auto_request_due(
            true,
            &Lazy::Ready(Arc::new(PageLabels::default())),
            0
        ));
        assert_eq!(attempts_after(1, None), 1);
    }
}
