use std::fmt;
use std::sync::Arc;
use std::thread::JoinHandle;

use fastpdf_cache::{BudgetedCache, SharedCache, retention};
use fastpdf_engine_api::{
    CancelToken, DocumentId, EngineDocument, EngineError, PageId, PageIndex, PageRect, TextLayer,
};

use crate::{Matcher, search_order};

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub case_sensitive: bool,
}

/// One hit, with highlight rectangles in page space.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub page: PageIndex,
    /// `char` range within the page's searched text.
    pub start: usize,
    pub end: usize,
    pub rects: Vec<PageRect>,
}

/// Progress stream of a search (delivered on the search thread).
#[derive(Debug, Clone, PartialEq)]
pub enum SearchEvent {
    /// Hits found on one page.
    Hits(Vec<SearchHit>),
    /// After every page: how far the search got.
    Progress {
        pages_done: u32,
        page_count: u32,
        hits: usize,
    },
    /// A page whose text could not be extracted; the search continues.
    PageFailed { page: PageIndex, error: EngineError },
    /// The search ended (all pages done, cancelled, or unsupported engine).
    Finished { hits: usize, cancelled: bool },
}

/// Byte-budgeted cache of extracted text layers.
pub struct TextCache {
    cache: Arc<SharedCache<PageId, Arc<TextLayer>>>,
    /// Documents closed via `remove_document`; extractions still running
    /// for them must not refill the cache. Grows by one id per closed
    /// document, which is negligible.
    closed: std::sync::Mutex<std::collections::HashSet<DocumentId>>,
}

impl fmt::Debug for TextCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextCache")
            .field("stats", &self.cache.stats())
            .finish()
    }
}

impl TextCache {
    /// Spec §15 starting budget: 32 MB.
    pub const DEFAULT_BUDGET: usize = 32 * 1024 * 1024;

    pub fn new(budget: usize) -> Self {
        Self {
            cache: Arc::new(SharedCache::new("text", budget, retention::TEXT)),
            closed: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Handle for [`fastpdf_cache::MemoryBudgetManager::register`].
    pub fn budgeted(&self) -> Arc<dyn BudgetedCache> {
        self.cache.clone()
    }

    /// The text layer of a page, extracted on first use.
    pub fn get_or_extract(
        &self,
        document: DocumentId,
        doc: &dyn EngineDocument,
        page: PageIndex,
        cancel: &CancelToken,
    ) -> Result<Arc<TextLayer>, EngineError> {
        let key = PageId::new(document, page);
        if let Some(layer) = self.cache.with(&key, Arc::clone) {
            return Ok(layer);
        }
        let layer = Arc::new(doc.text_layer(page, cancel)?);
        let bytes = layer.heap_bytes() + std::mem::size_of::<TextLayer>();
        let closed = self
            .closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&document);
        if !closed {
            self.cache.insert(key, Arc::clone(&layer), bytes);
        }
        Ok(layer)
    }
}

impl TextCache {
    /// Drops every cached text layer of `document` (call when it closes;
    /// the cache is shared by all open documents).
    pub fn remove_document(&self, document: DocumentId) {
        // Mark first, so an extraction finishing in between cannot slip a
        // layer back in after the purge.
        self.closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(document);
        self.cache.retain(|key, _| key.document != document);
    }

    pub fn stats(&self) -> fastpdf_cache::CacheStats {
        self.cache.stats()
    }
}

impl Default for TextCache {
    fn default() -> Self {
        Self::new(Self::DEFAULT_BUDGET)
    }
}

/// A running search. Dropping it cancels the search without blocking the
/// caller (the UI thread); the search thread notices within one page and
/// exits. Use [`SearchSession::cancel_and_wait`] where waiting is wanted.
pub struct SearchSession {
    cancel: CancelToken,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for SearchSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchSession")
            .field("cancelled", &self.cancel.is_cancelled())
            .finish()
    }
}

impl SearchSession {
    /// Starts searching on a background thread, beginning at `start_page`.
    /// Returns `None` for an empty query.
    pub fn start(
        document: DocumentId,
        doc: Arc<dyn EngineDocument>,
        texts: Arc<TextCache>,
        query: &SearchQuery,
        start_page: PageIndex,
        sink: impl Fn(SearchEvent) + Send + 'static,
    ) -> Option<Self> {
        let matcher = Matcher::new(&query.text, query.case_sensitive)?;
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("fastpdf-search".into())
            .spawn(move || {
                run(
                    document,
                    doc.as_ref(),
                    &texts,
                    &matcher,
                    start_page,
                    &token,
                    &sink,
                )
            })
            .ok()?;
        Some(Self {
            cancel,
            thread: Some(thread),
        })
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Cancels and blocks until the search thread has exited.
    pub fn cancel_and_wait(mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SearchSession {
    fn drop(&mut self) {
        // Detach: joining here could stall the UI thread for the duration
        // of one page's text extraction.
        self.cancel.cancel();
    }
}

fn run(
    document: DocumentId,
    doc: &dyn EngineDocument,
    texts: &TextCache,
    matcher: &Matcher,
    start_page: PageIndex,
    cancel: &CancelToken,
    sink: &dyn Fn(SearchEvent),
) {
    let page_count = doc.page_count();
    let mut hits = 0;
    let mut pages_done = 0;
    for page in search_order(start_page, page_count) {
        if cancel.is_cancelled() {
            sink(SearchEvent::Finished {
                hits,
                cancelled: true,
            });
            return;
        }
        match texts.get_or_extract(document, doc, page, cancel) {
            Ok(layer) => {
                let found = search_layer(&layer, matcher);
                if !found.is_empty() {
                    hits += found.len();
                    sink(SearchEvent::Hits(found));
                }
            }
            Err(EngineError::Cancelled) => continue,
            Err(EngineError::Unsupported(what)) => {
                sink(SearchEvent::PageFailed {
                    page,
                    error: EngineError::Unsupported(what),
                });
                sink(SearchEvent::Finished {
                    hits,
                    cancelled: false,
                });
                return;
            }
            Err(error) => sink(SearchEvent::PageFailed { page, error }),
        }
        pages_done += 1;
        sink(SearchEvent::Progress {
            pages_done,
            page_count,
            hits,
        });
    }
    sink(SearchEvent::Finished {
        hits,
        cancelled: cancel.is_cancelled(),
    });
}

/// Searches one page. Spans are joined with line breaks, which the matcher
/// treats as whitespace, so phrases that wrap across spans are found.
fn search_layer(layer: &TextLayer, matcher: &Matcher) -> Vec<SearchHit> {
    // Map from char index in the joined text to (span, char within span).
    let mut text = String::new();
    let mut origin: Vec<Option<(usize, usize)>> = Vec::new();
    for (si, span) in layer.spans.iter().enumerate() {
        for (ci, c) in span.text.chars().enumerate() {
            text.push(c);
            origin.push(Some((si, ci)));
        }
        text.push('\n');
        origin.push(None);
    }
    matcher
        .find_all(&text)
        .into_iter()
        .map(|m| SearchHit {
            page: layer.page,
            start: m.start,
            end: m.end,
            rects: highlight_rects(layer, &origin[m.start..m.end]),
        })
        .collect()
}

/// One rectangle per span touched by the match: the union of the matched
/// characters' boxes, or the whole span box when the engine only provides
/// span-level geometry.
fn highlight_rects(layer: &TextLayer, chars: &[Option<(usize, usize)>]) -> Vec<PageRect> {
    let mut rects: Vec<(usize, PageRect)> = Vec::new();
    for &(si, ci) in chars.iter().flatten() {
        let span = &layer.spans[si];
        let rect = span.char_bounds.get(ci).copied().unwrap_or(span.bounds);
        match rects.last_mut() {
            Some((last, r)) if *last == si => *r = r.union(rect),
            _ => rects.push((si, rect)),
        }
    }
    rects.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{
        PageInfo, PageSize, PixmapMut, RenderOutcome, RenderRequest, Rotation, TextSpan,
    };
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    /// Page `n` contains "page n" plus "needle" on pages divisible by 3.
    struct TextDoc {
        pages: u32,
        extracted: AtomicU32,
        delay: Duration,
    }

    impl EngineDocument for TextDoc {
        fn page_count(&self) -> u32 {
            self.pages
        }
        fn page_info(&self, _: PageIndex) -> Result<PageInfo, EngineError> {
            Ok(PageInfo {
                size: PageSize::LETTER,
                rotation: Rotation::R0,
            })
        }
        fn render(
            &self,
            _: &RenderRequest,
            _: &mut PixmapMut<'_>,
            _: &CancelToken,
        ) -> Result<RenderOutcome, EngineError> {
            Ok(RenderOutcome::default())
        }
        fn text_layer(&self, page: PageIndex, _: &CancelToken) -> Result<TextLayer, EngineError> {
            self.extracted.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(self.delay);
            if page.get() == 4 {
                return Err(EngineError::Malformed("broken font".into()));
            }
            let mut text = format!("page {}", page.get());
            if page.get().is_multiple_of(3) {
                text.push_str(" needle");
            }
            let n = text.chars().count();
            Ok(TextLayer {
                page,
                spans: vec![TextSpan {
                    text,
                    bounds: PageRect::new(0.0, 0.0, n as f32 * 10.0, 12.0),
                    char_bounds: (0..n)
                        .map(|i| PageRect::new(i as f32 * 10.0, 0.0, (i + 1) as f32 * 10.0, 12.0))
                        .collect(),
                }],
            })
        }
    }

    fn doc(pages: u32, delay_ms: u64) -> Arc<TextDoc> {
        Arc::new(TextDoc {
            pages,
            extracted: AtomicU32::new(0),
            delay: Duration::from_millis(delay_ms),
        })
    }

    fn start(
        d: Arc<TextDoc>,
        texts: Arc<TextCache>,
        q: &str,
        from: u32,
    ) -> (SearchSession, mpsc::Receiver<SearchEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let query = SearchQuery {
            text: q.into(),
            case_sensitive: false,
        };
        let session = SearchSession::start(
            DocumentId::from_raw(1),
            d,
            texts,
            &query,
            PageIndex::new(from),
            move |e| {
                let _ = tx.lock().unwrap().send(e);
            },
        )
        .unwrap();
        (session, rx)
    }

    fn collect(rx: &mpsc::Receiver<SearchEvent>) -> Vec<SearchEvent> {
        let mut events = Vec::new();
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(10)) {
            let done = matches!(e, SearchEvent::Finished { .. });
            events.push(e);
            if done {
                break;
            }
        }
        events
    }

    #[test]
    fn finds_hits_nearest_first_and_streams_progress() {
        let d = doc(10, 0);
        let (_s, rx) = start(d, Arc::new(TextCache::default()), "NEEDLE", 5);
        let events = collect(&rx);
        let pages: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                SearchEvent::Hits(h) => Some(h[0].page.get()),
                _ => None,
            })
            .collect();
        // Order from page 5: 5, 6, 4, 7, 3, 8, 2, 9, 1, 0.
        assert_eq!(pages, vec![6, 3, 9, 0]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SearchEvent::PageFailed { page, .. } if page.get() == 4))
        );
        assert_eq!(
            events.last(),
            Some(&SearchEvent::Finished {
                hits: 4,
                cancelled: false
            })
        );
        let first_hit = events.iter().find_map(|e| match e {
            SearchEvent::Hits(h) => Some(h[0].clone()),
            _ => None,
        });
        // "page 6 needle": the match covers chars 7..13 -> x 70..130.
        assert_eq!(
            first_hit.unwrap().rects,
            vec![PageRect::new(70.0, 0.0, 130.0, 12.0)]
        );
    }

    #[test]
    fn text_layers_are_cached_between_searches() {
        let d = doc(6, 0);
        let texts = Arc::new(TextCache::default());
        let (_a, rx) = start(d.clone(), texts.clone(), "needle", 0);
        collect(&rx);
        let after_first = d.extracted.load(Ordering::Relaxed);
        let (_b, rx) = start(d.clone(), texts, "page", 0);
        collect(&rx);
        // Only the failing page is extracted again.
        assert_eq!(d.extracted.load(Ordering::Relaxed), after_first + 1);
    }

    #[test]
    fn dropping_the_session_cancels_promptly() {
        let d = doc(1000, 20);
        let (session, rx) = start(d.clone(), Arc::new(TextCache::default()), "needle", 0);
        std::thread::sleep(Duration::from_millis(50));
        session.cancel_and_wait();
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(
            events.last(),
            Some(SearchEvent::Finished {
                cancelled: true,
                ..
            })
        ));
        assert!(d.extracted.load(Ordering::Relaxed) < 20);
    }

    #[test]
    fn drop_does_not_block_and_documents_can_be_forgotten() {
        let d = doc(1000, 20);
        let texts = Arc::new(TextCache::default());
        let (session, rx) = start(d.clone(), texts.clone(), "needle", 0);
        std::thread::sleep(Duration::from_millis(50));
        let started = std::time::Instant::now();
        drop(session);
        // Dropping only signals; it must not wait for the running extraction.
        assert!(started.elapsed() < Duration::from_millis(15));
        // The thread still finishes and reports cancellation.
        let finished = rx
            .iter()
            .find(|e| matches!(e, SearchEvent::Finished { .. }));
        assert!(matches!(
            finished,
            Some(SearchEvent::Finished {
                cancelled: true,
                ..
            })
        ));
        assert!(texts.stats().entries > 0);
        texts.remove_document(DocumentId::from_raw(1));
        assert_eq!(texts.stats().entries, 0);
        // A late extraction for the closed document is not cached again.
        let layer = texts
            .get_or_extract(
                DocumentId::from_raw(1),
                d.as_ref(),
                PageIndex::new(1),
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(layer.page, PageIndex::new(1));
        assert_eq!(texts.stats().entries, 0);
    }

    #[test]
    fn empty_queries_do_not_start() {
        let query = SearchQuery {
            text: "  ".into(),
            case_sensitive: false,
        };
        let s = SearchSession::start(
            DocumentId::from_raw(1),
            doc(1, 0),
            Arc::new(TextCache::default()),
            &query,
            PageIndex::FIRST,
            |_| {},
        );
        assert!(s.is_none());
    }
}
