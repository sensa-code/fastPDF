use std::fmt;
use std::sync::Arc;
use std::thread::JoinHandle;

use fastpdf_cache::{BudgetedCache, SharedCache, retention};
use fastpdf_engine_api::{
    CancelToken, DocumentId, EngineDocument, EngineError, PageId, PageIndex, PageRect, TextLayer,
};

use crate::page_text::PageText;
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
    /// Byte range within the page's search text: its characters in reading
    /// order, as [`Matcher`] compares them. Orders the hits of a page.
    pub start: usize,
    pub end: usize,
    /// In reading order: one rectangle per span and line the hit covers
    /// (a hit that wraps onto the next line has one on each).
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

/// Byte-budgeted cache of extracted text layers and, for pages that were
/// searched, their search text (a few percent of the layer for Latin
/// text), so that the next query skips the layout.
pub struct TextCache {
    cache: Arc<SharedCache<PageId, Entry>>,
    /// Documents closed via `remove_document`; extractions still running
    /// for them must not refill the cache. Grows by one id per closed
    /// document, which is negligible.
    closed: std::sync::Mutex<std::collections::HashSet<DocumentId>>,
}

/// A page's text layer and its search text: what searching it needs.
type Searchable = (Arc<TextLayer>, Arc<PageText>);

/// A cached page: its text layer and, once a search without case
/// sensitivity read it, its (case-folded) search text.
#[derive(Debug)]
struct Entry {
    layer: Arc<TextLayer>,
    search: Option<Arc<PageText>>,
}

impl Entry {
    /// Weight in the cache: what the entry keeps on the heap.
    fn bytes(&self) -> usize {
        self.layer.heap_bytes()
            + std::mem::size_of::<TextLayer>()
            + self
                .search
                .as_ref()
                .map_or(0, |t| t.heap_bytes() + std::mem::size_of::<PageText>())
    }
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
        if let Some(layer) = self.cache.with(&key, |e| Arc::clone(&e.layer)) {
            return Ok(layer);
        }
        self.extract(key, doc, cancel)
    }

    /// What searching `page` needs: its text layer and search text, made on
    /// first use (search text without case sensitivity is cached with the
    /// layer). `None` when the page lacks one of the query's characters: it
    /// cannot match, and no search text is made for it.
    fn search_text(
        &self,
        document: DocumentId,
        doc: &dyn EngineDocument,
        page: PageIndex,
        cancel: &CancelToken,
        matcher: &Matcher,
    ) -> Result<Option<Searchable>, EngineError> {
        let key = PageId::new(document, page);
        let folded = !matcher.case_sensitive();
        let cached = self
            .cache
            .with(&key, |e| (Arc::clone(&e.layer), e.search.clone()));
        let layer = match cached {
            Some((layer, Some(text))) if folded => return Ok(Some((layer, text))),
            Some((layer, _)) => layer,
            None => self.extract(key, doc, cancel)?,
        };
        if !matcher.may_match(&layer) {
            return Ok(None);
        }
        let text = Arc::new(PageText::new(&layer, matcher.case_sensitive()));
        if folded {
            self.store(
                key,
                Entry {
                    layer: Arc::clone(&layer),
                    search: Some(Arc::clone(&text)),
                },
            );
        }
        Ok(Some((layer, text)))
    }

    fn extract(
        &self,
        key: PageId,
        doc: &dyn EngineDocument,
        cancel: &CancelToken,
    ) -> Result<Arc<TextLayer>, EngineError> {
        let layer = Arc::new(doc.text_layer(key.page, cancel)?);
        self.store(
            key,
            Entry {
                layer: Arc::clone(&layer),
                search: None,
            },
        );
        Ok(layer)
    }

    /// Caches `entry` (replacing the page's entry) unless its document was
    /// closed. The `closed` lock is held throughout, so `remove_document`
    /// cannot purge in between and see the entry slip back in.
    fn store(&self, key: PageId, entry: Entry) {
        let closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        if !closed.contains(&key.document) {
            let bytes = entry.bytes();
            self.cache.insert(key, entry, bytes);
        }
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
        match texts.search_text(document, doc, page, cancel, matcher) {
            Ok(Some((layer, text))) => {
                let found = search_layer(&layer, &text, matcher);
                if !found.is_empty() {
                    hits += found.len();
                    sink(SearchEvent::Hits(found));
                }
            }
            Ok(None) => {}
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

/// Searches one page, in reading order, across span and line boundaries
/// (see [`Matcher`] for how they compare): matches in the page's search
/// text, with highlight rectangles from its layer.
fn search_layer(layer: &TextLayer, text: &PageText, matcher: &Matcher) -> Vec<SearchHit> {
    matcher
        .find_in(text.text())
        .map(|m| SearchHit {
            page: layer.page,
            start: m.start,
            end: m.end,
            rects: text.rects(layer, m),
        })
        .collect()
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

    /// One page searched the way the session does it.
    fn search(layer: &TextLayer, matcher: &Matcher) -> Vec<SearchHit> {
        if !matcher.may_match(layer) {
            return Vec::new();
        }
        let text = PageText::new(layer, matcher.case_sensitive());
        search_layer(layer, &text, matcher)
    }

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
    fn highlights_split_spans_without_char_geometry() {
        // Engines may omit evenly spaced char boxes; highlights must still
        // cover only the match, not the whole line.
        let layer = TextLayer {
            page: PageIndex::new(2),
            spans: vec![fastpdf_engine_api::TextSpan {
                text: "page 6 needle".into(),
                bounds: PageRect::new(0.0, 0.0, 130.0, 12.0),
                char_bounds: Vec::new(),
            }],
        };
        let hits = search(&layer, &Matcher::new("needle", false).unwrap());
        assert_eq!(hits[0].rects, vec![PageRect::new(70.0, 0.0, 130.0, 12.0)]);
    }

    /// Horizontal text with its top-left corner at (x, y): CJK characters
    /// 10 pt wide, others 5 pt, all 10 pt high.
    fn text(s: &str, x: f32, y: f32) -> TextSpan {
        let mut cx = x;
        let char_bounds: Vec<PageRect> = s
            .chars()
            .map(|c| {
                let w = if fastpdf_engine_api::is_cjk(c) {
                    10.0
                } else {
                    5.0
                };
                let r = PageRect::new(cx, y, cx + w, y + 10.0);
                cx = r.x1;
                r
            })
            .collect();
        let bounds = char_bounds
            .iter()
            .copied()
            .reduce(PageRect::union)
            .unwrap_or_default();
        TextSpan {
            text: s.into(),
            bounds,
            char_bounds,
        }
    }

    /// Vertical CJK text running down from (x, y), 10 pt per character.
    fn column(s: &str, x: f32, y: f32) -> TextSpan {
        let char_bounds: Vec<PageRect> = (0..s.chars().count())
            .map(|i| {
                let y0 = y + 10.0 * i as f32;
                PageRect::new(x, y0, x + 10.0, y0 + 10.0)
            })
            .collect();
        let bounds = char_bounds
            .iter()
            .copied()
            .reduce(PageRect::union)
            .unwrap_or_default();
        TextSpan {
            text: s.into(),
            bounds,
            char_bounds,
        }
    }

    fn find(spans: Vec<TextSpan>, query: &str) -> Vec<SearchHit> {
        let layer = TextLayer {
            page: PageIndex::new(3),
            spans,
        };
        search(&layer, &Matcher::new(query, false).unwrap())
    }

    fn rects(hits: &[SearchHit]) -> Vec<Vec<PageRect>> {
        hits.iter().map(|h| h.rects.clone()).collect()
    }

    #[test]
    fn cjk_words_match_across_spans_and_lines() {
        // One line drawn as two spans (a font switch).
        let spans = vec![text("並以公", 0.0, 0.0), text("文或電子郵件", 30.0, 0.0)];
        assert_eq!(
            rects(&find(spans, "公文")),
            vec![vec![
                PageRect::new(20.0, 0.0, 30.0, 10.0),
                PageRect::new(30.0, 0.0, 40.0, 10.0),
            ]]
        );
        // A word broken over two lines: one rectangle on each.
        let spans = vec![
            text("聯絡窗口，並以公", 0.0, 0.0),
            text("文或電子郵件送達本局。", 0.0, 14.0),
        ];
        let hits = find(spans.clone(), "公文");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, PageIndex::new(3));
        assert_eq!(
            hits[0].rects,
            vec![
                PageRect::new(70.0, 0.0, 80.0, 10.0),
                PageRect::new(0.0, 14.0, 10.0, 24.0),
            ]
        );
        // The query may be typed with a space or a line break in it.
        assert_eq!(rects(&find(spans.clone(), "公 文")), rects(&hits));
        assert_eq!(rects(&find(spans.clone(), "公\n文")), rects(&hits));
        // Longer phrases over the break, and the punctuation around it.
        assert_eq!(find(spans.clone(), "並以公文或電子郵件").len(), 1);
        assert_eq!(find(spans, "窗口，並以公文").len(), 1);
    }

    #[test]
    fn latin_words_match_across_lines_as_one_space() {
        let spans = vec![
            text("the quick brown", 0.0, 0.0),
            text("fox jumps", 0.0, 12.0),
        ];
        let hits = find(spans.clone(), "brown fox");
        assert_eq!(
            rects(&hits),
            vec![vec![
                PageRect::new(50.0, 0.0, 75.0, 10.0),
                PageRect::new(0.0, 12.0, 15.0, 22.0),
            ]]
        );
        // Several spaces (or a tab) in the query are one space.
        assert_eq!(
            rects(&find(spans.clone(), "  BROWN   \tfox ")),
            rects(&hits)
        );
        // The line break is a word boundary.
        assert!(find(spans.clone(), "brownfox").is_empty());
        // A space in one span is covered by its rectangle.
        assert_eq!(
            rects(&find(spans, "quick brown")),
            vec![vec![PageRect::new(20.0, 0.0, 75.0, 10.0)]]
        );
        // Words drawn apart without a space character get one.
        let apart = vec![text("Hello", 0.0, 0.0), text("world", 28.0, 0.0)];
        assert_eq!(find(apart.clone(), "hello world").len(), 1);
        assert!(find(apart, "helloworld").is_empty());
    }

    #[test]
    fn mixed_chinese_and_latin() {
        // Latin text next to Chinese matches with or without the spaces
        // documents put around it, also across a line break.
        let spans = vec![text("本系統使用", 0.0, 0.0), text("PDF格式與", 0.0, 14.0)];
        for q in ["使用PDF格式", "使用 PDF 格式", "使用\nPDF"] {
            let hits = find(spans.clone(), q);
            assert_eq!(hits.len(), 1, "{q:?}");
            assert_eq!(hits[0].rects.len(), 2, "{q:?}");
            assert_eq!(hits[0].rects[0], PageRect::new(30.0, 0.0, 50.0, 10.0));
        }
        let solid = vec![text("請使用FastPDF閱讀", 0.0, 0.0)];
        assert_eq!(find(solid.clone(), "FastPDF閱讀").len(), 1);
        assert_eq!(find(solid.clone(), "使用 FastPDF").len(), 1);
        assert!(find(solid, "Fast PDF").is_empty());
    }

    #[test]
    fn vertical_text_matches_down_its_columns() {
        // Columns right to left, as in vertical writing.
        let spans = vec![column("元。數位", 300.0, 0.0), column("轉型讓", 286.0, 0.0)];
        let hits = find(spans, "數位轉型");
        assert_eq!(
            rects(&hits),
            vec![vec![
                PageRect::new(300.0, 20.0, 310.0, 40.0),
                PageRect::new(286.0, 0.0, 296.0, 20.0),
            ]]
        );
    }

    #[test]
    fn hits_come_in_reading_order() {
        // The document draws the lower line first.
        let spans = vec![
            text("second needle", 0.0, 12.0),
            text("first needle", 0.0, 0.0),
        ];
        let hits = find(spans, "needle");
        assert_eq!(hits.len(), 2);
        assert!(hits[0].start < hits[1].start);
        assert_eq!(hits[0].rects, vec![PageRect::new(30.0, 0.0, 60.0, 10.0)]);
        assert_eq!(hits[1].rects, vec![PageRect::new(35.0, 12.0, 65.0, 22.0)]);
    }

    #[test]
    fn pages_without_a_query_character_are_skipped() {
        let spans = vec![text("公告", 0.0, 0.0), text("文件", 0.0, 14.0)];
        let layer = TextLayer {
            page: PageIndex::FIRST,
            spans,
        };
        assert!(!Matcher::new("公文書", false).unwrap().may_match(&layer));
        assert!(Matcher::new("公文", false).unwrap().may_match(&layer));
        // Every character occurs, but not in this order.
        assert!(search(&layer, &Matcher::new("文公", false).unwrap()).is_empty());
        // Case folding applies to the check as to the match.
        let latin = TextLayer {
            page: PageIndex::FIRST,
            spans: vec![text("FastPDF", 0.0, 0.0)],
        };
        assert!(Matcher::new("pdf", false).unwrap().may_match(&latin));
        assert!(!Matcher::new("pdf", true).unwrap().may_match(&latin));
    }

    #[test]
    fn hostile_layers_are_searched_without_panicking() {
        let bad = PageRect {
            x0: f32::NAN,
            y0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            y1: f32::NAN,
        };
        let spans = vec![
            TextSpan {
                text: "公文".into(),
                bounds: bad,
                char_bounds: vec![bad],
            },
            TextSpan {
                text: "文".into(),
                bounds: PageRect::default(),
                char_bounds: Vec::new(),
            },
            TextSpan::default(),
        ];
        let hits = find(spans, "公文");
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].rects.is_empty());
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
    fn search_text_is_cached_with_the_layer() {
        let d = doc(6, 0);
        let texts = Arc::new(TextCache::default());
        let (_a, rx) = start(d.clone(), texts.clone(), "page", 0);
        collect(&rx);
        // The same pages without their search text weigh less.
        let layers_only = TextCache::default();
        for p in 0..6 {
            let _ = layers_only.get_or_extract(
                DocumentId::from_raw(1),
                d.as_ref(),
                PageIndex::new(p),
                &CancelToken::new(),
            );
        }
        assert!(texts.stats().bytes > layers_only.stats().bytes);
        // The next query reads the cached text: same hits, and only the
        // failing page is extracted again.
        let extracted = d.extracted.load(Ordering::Relaxed);
        let (_b, rx) = start(d.clone(), texts.clone(), "NEEDLE", 0);
        let found: Vec<u32> = collect(&rx)
            .into_iter()
            .filter_map(|e| match e {
                SearchEvent::Hits(h) => Some(h[0].page.get()),
                _ => None,
            })
            .collect();
        assert_eq!(found, vec![0, 3]);
        assert_eq!(d.extracted.load(Ordering::Relaxed), extracted + 1);
        // Case-sensitive searches make their own (unfolded) text.
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let _c = SearchSession::start(
            DocumentId::from_raw(1),
            d,
            texts,
            &SearchQuery {
                text: "Needle".into(),
                case_sensitive: true,
            },
            PageIndex::FIRST,
            move |e| {
                let _ = tx.lock().unwrap().send(e);
            },
        )
        .unwrap();
        assert!(matches!(
            collect(&rx).last(),
            Some(SearchEvent::Finished {
                hits: 0,
                cancelled: false
            })
        ));
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
