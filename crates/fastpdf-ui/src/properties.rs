//! The document properties panel (Ctrl+D, the ⓘ toolbar button): what the
//! file and the engine say about the open document, every value copyable.
//!
//! The engine's part (`EngineDocument::properties`) is asked for when the
//! panel opens, once per document, off the UI thread, and kept with the
//! document (`crate::doc_queries`); nothing runs while a document opens or
//! while the panel is closed. File name, size, page count and the current
//! page's size are the reader's own and show at once; the page size
//! follows scrolling because the rows are built when the panel renders.
//! The panel takes the settings panel's place at the top right: opening
//! one closes the other. Permissions are shown, not applied.

mod panel;
mod rows;
mod text;

use std::sync::Arc;
use std::time::{Duration, Instant};

use fastpdf_engine_api::display::clean_display;
use fastpdf_engine_api::{DocumentProperties, EngineDocument, EngineError};
use gpui::{ClipboardItem, Context, Window};

use self::rows::{Answer, Facts, Section};
use crate::doc_queries::{DocQueries, Lazy};
use crate::i18n::PropertyLabel;
use crate::reader::{DocState, ReaderView};

/// Bytes of an engine error shown in the panel at most.
const MAX_PROBLEM_BYTES: usize = 300;

/// The panel's state; the answer itself lives with the document.
#[derive(Debug, Default)]
pub(crate) struct PropertiesPanel {
    pub open: bool,
    /// What the last Copy click copied, until the next one or until the
    /// panel closes (no timer).
    copied: Option<Copied>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Copied {
    Row(PropertyLabel),
    All,
}

impl ReaderView {
    pub(crate) fn toggle_properties_panel(&mut self, cx: &mut Context<'_, Self>) {
        if self.properties.open {
            self.close_properties_panel();
            return;
        }
        // Nothing opens behind a prompt card.
        if self.active_prompt().is_some() {
            return;
        }
        self.properties = PropertiesPanel {
            open: true,
            copied: None,
        };
        self.settings_open = false;
        self.menus.open = None;
        self.ensure_properties(cx);
    }

    pub(crate) fn close_properties_panel(&mut self) {
        self.properties = PropertiesPanel::default();
    }

    /// A document opened while the panel is open: its properties are asked
    /// for at once, like the sidebar's outline.
    pub(crate) fn properties_follow_document(&mut self, cx: &mut Context<'_, Self>) {
        self.properties.copied = None;
        if self.properties.open {
            self.ensure_properties(cx);
        }
    }

    /// Asks the engine for the open document's properties unless they are
    /// known or on their way.
    pub(crate) fn ensure_properties(&mut self, cx: &mut Context<'_, Self>) {
        self.load_doc_query(
            |queries| &mut queries.properties,
            |doc| {
                let started = Instant::now();
                let answer = doc.properties();
                log_answer(&answer, started.elapsed());
                answer.map(|p| Some(Arc::new(p)))
            },
            properties_answered,
            cx,
        );
    }

    /// Asks again after a failure (the Retry button).
    pub(crate) fn retry_properties(&mut self, cx: &mut Context<'_, Self>) {
        if let DocState::Open(open) = &mut self.doc
            && matches!(open.queries.properties, Lazy::Failed(_) | Lazy::NotLoaded)
        {
            open.queries.properties = Lazy::NotLoaded;
            open.queries.properties_problem = None;
        }
        self.ensure_properties(cx);
        cx.notify();
    }

    /// Whether the properties question is answered (or there is no
    /// document to ask about).
    pub(crate) fn properties_settled(&self) -> bool {
        match self.doc_queries() {
            Some(q) => {
                q.properties.is_settled()
                    || (matches!(q.properties, Lazy::NotLoaded) && q.properties_problem.is_some())
            }
            None => true,
        }
    }

    /// The panel's rows; `None` without a document.
    fn property_sections(&self) -> Option<Vec<Section>> {
        let DocState::Open(open) = &self.doc else {
            return None;
        };
        let session = &open.session;
        let page = session.current_page();
        let answer = match &open.queries.properties {
            Lazy::Ready(p) => Answer::Ready(p.as_ref()),
            Lazy::Loading(_) => Answer::Pending,
            Lazy::NotLoaded if open.queries.properties_problem.is_none() => Answer::Pending,
            _ => Answer::None,
        };
        let facts = Facts {
            path: &open.path,
            file_bytes: open.file_bytes,
            page_count: session.page_count(),
            page,
            page_size: session.page_size_known(page),
            password: open.protected,
            answer,
        };
        Some(rows::sections(&facts, self.strings()))
    }

    /// Why the engine gave no answer, made safe to show.
    fn properties_problem(&self) -> Option<String> {
        let queries = self.doc_queries()?;
        let why = match &queries.properties {
            Lazy::Failed(e) => e.as_str(),
            Lazy::NotLoaded => queries.properties_problem.as_deref()?,
            _ => return None,
        };
        Some(clean_display(why, MAX_PROBLEM_BYTES))
    }

    /// Everything the panel shows as text, one "label: value" line per
    /// row (Copy all; the dev script saves it to a file).
    pub(crate) fn properties_text(&self) -> String {
        let strings = self.strings();
        match self.property_sections() {
            Some(sections) => rows::as_text(&sections, strings),
            None => format!("{}\n", strings.no_document_open()),
        }
    }

    fn copy_property(&mut self, label: PropertyLabel, cx: &mut Context<'_, Self>) {
        let value = self
            .property_sections()
            .into_iter()
            .flatten()
            .flat_map(|s| s.rows)
            .find(|r| r.label == label)
            .map(|r| text::copy_value(&r.value));
        if let Some(value) = value {
            cx.write_to_clipboard(ClipboardItem::new_string(value));
            self.properties.copied = Some(Copied::Row(label));
            cx.notify();
        }
    }

    fn copy_all_properties(&mut self, cx: &mut Context<'_, Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.properties_text()));
        self.properties.copied = Some(Copied::All);
        cx.notify();
    }

    /// The Close button.
    fn close_properties_from_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.close_properties_panel();
        window.focus(&self.focus, cx);
        cx.notify();
    }
}

/// An answer came in: a failure of the moment is kept to show with Retry
/// (the slot itself goes back to "not loaded").
fn properties_answered(queries: &mut DocQueries, error: Option<&EngineError>, _since: Duration) {
    queries.properties_problem = error.filter(|e| e.is_transient()).map(ToString::to_string);
}

/// Structural facts only: never the document's text.
fn log_answer(answer: &Result<DocumentProperties, EngineError>, took: Duration) {
    let ms = took.as_secs_f64() * 1000.0;
    match answer {
        Ok(p) => log::info!(
            "document properties: read in {ms:.1} ms (form fields {}; encrypted {}, tagged {:?}, linearized {:?})",
            p.forms.map_or_else(
                || "unknown".to_owned(),
                |f| format!(
                    "{}{}, {} signed",
                    if f.truncated { "at least " } else { "" },
                    f.fields,
                    f.signatures
                )
            ),
            p.metadata.encrypted,
            p.tagged,
            p.linearized
        ),
        Err(e) => log::info!("document properties: unavailable after {ms:.1} ms: {e}"),
    }
}
