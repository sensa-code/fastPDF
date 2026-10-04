//! Text-layer size and copy behaviour on the generated corpus (B-5: the
//! text cache budget is spent on these layers).

mod common;

use common::*;
use fastpdf_engine_api::{CancelToken, EngineDocument, PageIndex, ResourceLimits};

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn text_layer_sizes() {
    for file in [
        "large-text/dense-300p-times.pdf",
        "large-text/dense-300p-2col-helvetica.pdf",
        "traditional-chinese/gov-letter-embedded-ttfsubset.pdf",
        "traditional-chinese/gov-letter-embedded-type0.pdf",
        "traditional-chinese/gov-letter-cid-msung-nonembedded.pdf",
        "traditional-chinese/big5-level1-chars-ttfsubset.pdf",
        "traditional-chinese/big5-level1-chars-type0.pdf",
    ] {
        let Some(bytes) = fixture(file) else { continue };
        let doc = open_with(bytes, None, ResourceLimits::default()).unwrap();
        let pages = doc.page_count().min(40);
        let (mut heap, mut spans, mut chars, mut max_heap) = (0usize, 0usize, 0usize, 0usize);
        for p in 0..pages {
            let layer = doc
                .text_layer(PageIndex::new(p), &CancelToken::new())
                .unwrap();
            let h = layer.heap_bytes();
            heap += h;
            max_heap = max_heap.max(h);
            spans += layer.spans.len();
            chars += layer
                .spans
                .iter()
                .map(|s| s.text.chars().count())
                .sum::<usize>();
        }
        let n = pages.max(1) as usize;
        println!(
            "{file}: {pages} pages, heap/page avg {} B max {max_heap} B, spans/page {}, chars/span {:.1}",
            heap / n,
            spans / n,
            chars as f64 / spans.max(1) as f64
        );
    }
}

/// Writes the `select_all` copy text of every page to the file named by
/// `$FASTPDF_TEXT_SNAPSHOT` (one line per page, line breaks escaped), to
/// compare text extraction before and after a change.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn copy_text_snapshot() {
    use fastpdf_core::selection::{select_all, selected_text};
    let Some(out) = std::env::var_os("FASTPDF_TEXT_SNAPSHOT") else {
        return;
    };
    let mut lines = Vec::new();
    for file in SNAPSHOT_FILES {
        let Some(bytes) = fixture(file) else { continue };
        let doc = open_with(bytes, None, ResourceLimits::default()).unwrap();
        for p in 0..doc.page_count().min(40) {
            let layer = doc
                .text_layer(PageIndex::new(p), &CancelToken::new())
                .unwrap();
            let text = selected_text(&layer, &select_all(&layer));
            lines.push(format!("{file}#{p}\t{}", text.replace('\n', "\\n")));
        }
    }
    std::fs::write(out, lines.join("\n")).unwrap();
}

const SNAPSHOT_FILES: [&str; 7] = [
    "large-text/dense-300p-times.pdf",
    "large-text/dense-300p-2col-helvetica.pdf",
    "traditional-chinese/gov-letter-embedded-ttfsubset.pdf",
    "traditional-chinese/gov-letter-embedded-type0.pdf",
    "traditional-chinese/gov-letter-cid-msung-nonembedded.pdf",
    "traditional-chinese/big5-level1-chars-ttfsubset.pdf",
    "traditional-chinese/big5-level1-chars-type0.pdf",
];

/// Text layers fill the byte-budgeted text cache, so their size per char is
/// a regression target; copy text must keep its spaces and line breaks.
#[test]
fn text_layers_stay_compact_and_copy_cleanly() {
    use fastpdf_core::selection::{select_all, selected_text};

    let cases: [(&str, f64, &[&str]); 3] = [
        (
            "large-text/dense-300p-times.pdf",
            20.0,
            &[
                // Headings and titles stand apart: an empty line follows.
                "Chapter 1 - dense text page 1\n\nMeasure clip (height) stream catalog",
                "\nline small stroke kerning layout document width;",
            ],
        ),
        (
            "traditional-chinese/gov-letter-embedded-ttfsubset.pdf",
            18.0,
            &[
                "虛構市政府環境保護局　函\n\n地址：",
                "電話：(00)0000-0000 分機123\n",
            ],
        ),
        (
            "traditional-chinese/big5-level1-chars-ttfsubset.pdf",
            12.0,
            &["\nA440 一乙丁七乃九了二人儿入八几刀刁力匕十卜又三下丈上丫丸凡久么也\nA45E 乞"],
        ),
    ];
    for (file, max_bytes_per_char, snippets) in cases {
        let Some(bytes) = fixture(file) else { continue };
        let doc = open_with(bytes, None, ResourceLimits::default()).unwrap();
        let layer = doc
            .text_layer(PageIndex::FIRST, &CancelToken::new())
            .unwrap();
        let chars: usize = layer.spans.iter().map(|s| s.text.chars().count()).sum();
        let per_char = layer.heap_bytes() as f64 / chars.max(1) as f64;
        assert!(
            per_char <= max_bytes_per_char,
            "{file}: {per_char:.1} bytes per char"
        );
        let text = selected_text(&layer, &select_all(&layer));
        for snippet in snippets {
            assert!(
                text.contains(snippet),
                "{file}: {snippet:?} not in {text:?}"
            );
        }
    }
}
