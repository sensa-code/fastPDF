//! Search options on the public API: full-width forms, match case, whole
//! words (with CJK neighbours and overlapping candidates), the hit cap and
//! the snippets hits carry.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{
    CancelToken, DocumentId, EngineDocument, EngineError, PageIndex, PageInfo, PageRect, PageSize,
    PixmapMut, RenderOutcome, RenderRequest, Rotation, TextLayer, TextSpan,
};
use fastpdf_search::{
    Matcher, SearchEvent, SearchHit, SearchQuery, SearchSession, TextCache, is_word_char,
};

/// Matches of `query` in `text` as char ranges.
#[allow(clippy::expect_used)] // test queries are never blank
fn find(query: &str, text: &str, case_sensitive: bool, whole_word: bool) -> Vec<(usize, usize)> {
    Matcher::new(query, case_sensitive)
        .expect("a query")
        .whole_word(whole_word)
        .find_all(text)
        .into_iter()
        .map(|m| (m.start, m.end))
        .collect()
}

fn count(query: &str, text: &str) -> usize {
    find(query, text, false, false).len()
}

#[test]
fn full_width_forms_match_their_ascii_counterparts() {
    assert_eq!(count("PDF", "使用ＰＤＦ格式"), 1);
    assert_eq!(count("ＰＤＦ", "使用PDF格式"), 1);
    assert_eq!(count("pdf", "ＰＤＦ與PDF與pdf"), 3);
    assert_eq!(count("１２", "第12條"), 1);
    // Full-width letters and digits keep the CJK spacing rule, so a number
    // or a code wrapped onto the next line still matches...
    assert_eq!(count("ISO9001", "ＩＳＯ\n９００１"), 1);
    assert_eq!(count("115年", "民國１１\n５年"), 1);
    // ...and a full-width phrase matches without its ideographic space,
    // not with an ASCII one (documented trade-off).
    assert_eq!(count("ABCDEF", "ＡＢＣ　ＤＥＦ"), 1);
    assert_eq!(count("ABC DEF", "ＡＢＣ　ＤＥＦ"), 0);
    assert_eq!(count("ABC DEF", "ABC DEF"), 1);
}

#[test]
fn match_case_compares_after_width() {
    assert_eq!(find("Needle", "needle Needle", true, false), vec![(7, 13)]);
    assert_eq!(find("A", "Ａａa", true, false), vec![(0, 1)]);
    assert_eq!(find("A", "Ａａa", false, false).len(), 3);
}

#[test]
fn whole_words_only() {
    let text = "PDF pdfs FastPDF PDF-A pdf_x (pdf)";
    // PDF, the PDF of PDF-A, and (pdf); not pdfs, FastPDF or pdf_x.
    assert_eq!(
        find("pdf", text, false, true),
        vec![(0, 3), (17, 20), (30, 33)]
    );
    assert_eq!(find("pdf", text, false, false).len(), 6);
    // Full-width letters are word characters too.
    assert!(find("PDF", "ＦａｓｔＰＤＦ", false, true).is_empty());
}

#[test]
fn cjk_neighbours_are_word_boundaries() {
    assert_eq!(find("PDF", "使用PDF格式", false, true), vec![(2, 5)]);
    assert_eq!(find("12", "第１２條", false, true), vec![(1, 3)]);
    assert!(find("12", "第112條", false, true).is_empty());
    // A query of CJK characters only is never constrained.
    assert_eq!(find("公文", "並以公文送達", false, true), vec![(2, 4)]);
    assert!(!is_word_char('公') && is_word_char('é') && is_word_char('Ж'));
}

#[test]
fn rejected_candidates_are_searched_inside() {
    // The first candidate (1..6) has a letter before it; the whole word
    // overlaps it.
    assert_eq!(find("ab-ab", "xab-ab-ab", false, true), vec![(4, 9)]);
    // A needle that does not start with a word character retries one
    // character further.
    assert_eq!(find("-ab", "x-abc-ab", false, true), vec![(5, 8)]);
}

#[test]
fn whole_words_stay_linear_on_pathological_pages() {
    let page = "a".repeat(100_000);
    let started = Instant::now();
    assert!(find("aaa", &page, false, true).is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// `pages` pages, each one line of `text`.
struct Lines {
    text: String,
    pages: u32,
}

impl EngineDocument for Lines {
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
        let n = self.text.chars().count() as f32;
        Ok(TextLayer {
            page,
            spans: vec![TextSpan {
                text: self.text.clone(),
                bounds: PageRect::new(0.0, 0.0, n * 10.0, 12.0),
                char_bounds: Vec::new(),
            }],
        })
    }
}

/// Every hit of a search over `doc` and how it finished.
#[allow(clippy::unwrap_used)] // test setup
fn search(doc: Lines, query: &SearchQuery) -> (Vec<SearchHit>, Option<SearchEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let _session = SearchSession::start(
        DocumentId::from_raw(3),
        Arc::new(doc),
        Arc::new(TextCache::default()),
        query,
        PageIndex::FIRST,
        move |e| {
            let _ = tx.lock().unwrap().send(e);
        },
    )
    .unwrap();
    let mut hits = Vec::new();
    while let Ok(event) = rx.recv_timeout(Duration::from_secs(10)) {
        match event {
            SearchEvent::Hits(found) => hits.extend(found),
            finished @ SearchEvent::Finished { .. } => return (hits, Some(finished)),
            _ => {}
        }
    }
    (hits, None)
}

#[test]
fn sessions_apply_the_options_and_the_cap() {
    let doc = |pages| Lines {
        text: "本系統支援ＰＤＦ檔案，也支援 PDF 與 pdf；FastPDF reads PDFs.".into(),
        pages,
    };
    let (hits, _) = search(doc(1), &SearchQuery::new("pdf"));
    assert_eq!(hits.len(), 5);
    let whole = SearchQuery {
        whole_word: true,
        ..SearchQuery::new("pdf")
    };
    assert_eq!(search(doc(1), &whole).0.len(), 3);
    let cased = SearchQuery {
        case_sensitive: true,
        ..SearchQuery::new("PDF")
    };
    assert_eq!(search(doc(1), &cased).0.len(), 4);
    // Snippets show the characters as printed.
    let context = &hits[0].context;
    assert_eq!(context.hit().map(|r| &context.text[r]), Some("ＰＤＦ"));
    // 50 pages of 5 hits, at most 12 kept.
    let capped = SearchQuery {
        max_hits: 12,
        ..SearchQuery::new("pdf")
    };
    let (hits, finished) = search(doc(50), &capped);
    assert_eq!(hits.len(), 12);
    assert_eq!(
        finished,
        Some(SearchEvent::Finished {
            hits: 12,
            cancelled: false,
            limit_reached: true,
        })
    );
    // Exactly the cap on the last page is no limit reached.
    let exact = SearchQuery {
        max_hits: 10,
        ..SearchQuery::new("pdf")
    };
    let (_, finished) = search(doc(2), &exact);
    assert_eq!(
        finished,
        Some(SearchEvent::Finished {
            hits: 10,
            cancelled: false,
            limit_reached: false,
        })
    );
}
