//! Search on real text layers: page 1 of three generated fixtures as
//! `fastpdf-engine-hayro` extracts it, recorded in `tests/layers/`. This
//! crate depends on no engine (and the generated fixtures are not
//! committed), so the layers are recorded rather than extracted here;
//! record them again when text extraction changes. Words the fixtures break
//! over two lines are found, with a highlight rectangle on each line.
//!
//! Format, one span after another in content order: `span x0 y0 x1 y1`,
//! `text …`, and `boxes …` (four numbers per `char`) when the engine gives
//! per-char boxes. Lines starting with `#` are comments.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use fastpdf_engine_api::{
    CancelToken, DocumentId, EngineDocument, EngineError, PageIndex, PageInfo, PageRect, PageSize,
    PixmapMut, RenderOutcome, RenderRequest, Rotation, TextLayer, TextSpan,
};
use fastpdf_search::{SearchEvent, SearchHit, SearchQuery, SearchSession, TextCache};

/// `traditional-chinese/gov-letter-cid-msung-nonembedded.pdf`: an official
/// letter (公文) whose paragraphs wrap mid-word.
const GOV_LETTER: &str = include_str!("layers/gov-letter-p1.txt");
/// `cjk/cid-nonembedded-vertical.pdf`: vertical writing, columns right to
/// left.
const VERTICAL: &str = include_str!("layers/vertical-p1.txt");
/// `small-text/one-page-helvetica.pdf`, its first five spans: a title and a
/// paragraph of four lines.
const HELVETICA: &str = include_str!("layers/helvetica-p1.txt");

fn numbers(s: &str) -> Option<Vec<f32>> {
    s.split_whitespace().map(|n| n.parse().ok()).collect()
}

fn parse(src: &str) -> Option<TextLayer> {
    let mut spans: Vec<TextSpan> = Vec::new();
    for line in src.lines().filter(|l| !l.starts_with('#')) {
        let (kind, rest) = line.split_once(' ')?;
        match kind {
            "span" => {
                let [x0, y0, x1, y1] = numbers(rest)?[..] else {
                    return None;
                };
                spans.push(TextSpan {
                    bounds: PageRect::new(x0, y0, x1, y1),
                    ..TextSpan::default()
                });
            }
            "text" => spans.last_mut()?.text = rest.to_string(),
            "boxes" => {
                let span = spans.last_mut()?;
                let numbers = numbers(rest)?;
                let (boxes, rest) = numbers.as_chunks::<4>();
                if !rest.is_empty() || boxes.len() != span.char_count() {
                    return None;
                }
                span.char_bounds = boxes
                    .iter()
                    .map(|&[x0, y0, x1, y1]| PageRect::new(x0, y0, x1, y1))
                    .collect();
            }
            _ => return None,
        }
    }
    Some(TextLayer {
        page: PageIndex::FIRST,
        spans,
    })
}

#[allow(clippy::expect_used)] // test data that does not parse fails the test
fn layer(src: &str) -> TextLayer {
    parse(src).expect("recorded layer parses")
}

/// A one-page document whose page is a recorded layer.
struct Recorded(TextLayer);

impl EngineDocument for Recorded {
    fn page_count(&self) -> u32 {
        1
    }
    fn page_info(&self, _: PageIndex) -> Result<PageInfo, EngineError> {
        Ok(PageInfo {
            size: PageSize::new(595.0, 842.0),
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
    fn text_layer(&self, _: PageIndex, _: &CancelToken) -> Result<TextLayer, EngineError> {
        Ok(self.0.clone())
    }
}

/// Every hit of `query` on the page, in reading order, through a whole
/// search session.
#[allow(clippy::unwrap_used)] // test setup
fn search(page: &TextLayer, query: &str) -> Vec<SearchHit> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let _session = SearchSession::start(
        DocumentId::from_raw(7),
        Arc::new(Recorded(page.clone())),
        Arc::new(TextCache::default()),
        &SearchQuery {
            text: query.into(),
            case_sensitive: false,
        },
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
            SearchEvent::Finished { .. } => break,
            _ => {}
        }
    }
    hits.sort_by_key(|h| h.start);
    hits
}

/// The span whose text contains `text`.
#[allow(clippy::expect_used)] // the recorded layers are known
fn span<'a>(page: &'a TextLayer, text: &str) -> &'a TextSpan {
    page.spans
        .iter()
        .find(|s| s.text.contains(text))
        .expect("span in the recorded layer")
}

/// `rect` lies on the line of `span`, at its start or its end.
fn at_start(rect: PageRect, span: &TextSpan) -> bool {
    (rect.x0 - span.bounds.x0).abs() < 0.01 && within(rect, span)
}

fn at_end(rect: PageRect, span: &TextSpan) -> bool {
    (rect.x1 - span.bounds.x1).abs() < 0.01 && within(rect, span)
}

fn within(rect: PageRect, span: &TextSpan) -> bool {
    let b = span.bounds;
    rect.x0 >= b.x0 - 0.01
        && rect.x1 <= b.x1 + 0.01
        && rect.y0 >= b.y0 - 0.01
        && rect.y1 <= b.y1 + 0.01
}

#[test]
fn chinese_words_broken_over_two_lines_are_found() {
    let page = layer(GOV_LETTER);
    // (query, end of the first line, start of the next)
    let wrapped = [
        ("公文", "並以公", "文或電子郵件"),
        ("第12條", "第5條及第12", "條規定辦理"),
        ("詳如附表", "結果詳", "如附表"),
        ("相關規定", "依相關", "規定續處"),
        ("電話及字號", "電話及", "字號均非真實"),
    ];
    for (query, first, next) in wrapped {
        let hits = search(&page, query);
        assert_eq!(hits.len(), 1, "{query}");
        let rects = &hits[0].rects;
        assert_eq!(rects.len(), 2, "{query}: one rectangle per line");
        assert!(at_end(rects[0], span(&page, first)), "{query}: {rects:?}");
        assert!(at_start(rects[1], span(&page, next)), "{query}: {rects:?}");
    }
    // Typed with a space or a line break inside, the same hit.
    assert_eq!(search(&page, "公 文"), search(&page, "公文"));
    assert_eq!(search(&page, "公\n文"), search(&page, "公文"));
}

#[test]
fn hits_on_a_page_come_in_reading_order() {
    let page = layer(GOV_LETTER);
    // 主旨 wraps 「…檢測結果未 / 符合規定一案」; 說明四 has 「仍未符合規定者」 on one line.
    let hits = search(&page, "未符合規定");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].rects.len(), 2);
    assert!(at_end(hits[0].rects[0], span(&page, "主旨：")));
    assert_eq!(hits[1].rects.len(), 1);
    assert!(within(hits[1].rects[0], span(&page, "四、逾期")));
    // The running header comes first, the page number last.
    let header = search(&page, "測試樣本");
    let number = search(&page, "第 1 頁");
    let body = search(&page, "受文者");
    assert!(header[0].start < body[0].start && body[0].start < number[0].start);
}

#[test]
fn spaced_out_headings_match_without_their_blanks() {
    let page = layer(GOV_LETTER);
    // 「檔　　號：」 is set with ideographic spaces.
    let hits = search(&page, "檔號");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].rects.len(), 1);
    assert!(within(hits[0].rects[0], span(&page, "檔")));
    // A table row: cells a character width or more apart.
    assert_eq!(search(&page, "項次檢測項目").len(), 1);
    assert_eq!(search(&page, "項次 檢測項目").len(), 1);
}

#[test]
fn vertical_columns_continue_into_the_next_column() {
    let page = layer(VERTICAL);
    let hits = search(&page, "數位轉型");
    assert_eq!(hits.len(), 1);
    let rects = &hits[0].rects;
    assert_eq!(rects.len(), 2);
    // Down the end of the first column, then the top of the next one to
    // its left.
    assert!(within(rects[0], span(&page, "數字與單位")));
    assert!(within(rects[1], span(&page, "轉型讓政府")));
    assert!(rects[1].x1 < rects[0].x0);
    // 「回報執行 / 情形」 and 「並回 / 報執行情形」: both wrap.
    let hits = search(&page, "回報執行情形");
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.rects.len() == 2));
}

#[test]
fn latin_words_across_lines_are_one_space_apart() {
    let page = layer(HELVETICA);
    for (query, first, next) in [
        ("request frame", "Blend as request", "frame thumbnail"),
        ("glyph image", "stream glyph", "image next"),
        ("RENDER   search", "request render", "search shading"),
    ] {
        let hits = search(&page, query);
        assert_eq!(hits.len(), 1, "{query}");
        let rects = &hits[0].rects;
        assert_eq!(rects.len(), 2, "{query}");
        assert!(at_end(rects[0], span(&page, first)), "{query}: {rects:?}");
        assert!(at_start(rects[1], span(&page, next)), "{query}: {rects:?}");
    }
    // The line break is a word boundary.
    assert!(search(&page, "requestframe").is_empty());
}
