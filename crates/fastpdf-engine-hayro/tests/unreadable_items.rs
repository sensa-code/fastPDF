//! Arrays and dictionaries with an item hayro skips but cannot parse (a
//! dictionary followed by `stream` inside them, which hayro's `raw_iter`
//! and `Dict::entries` unwrap): page labels read up to it, the pre-render
//! scan refuses such an array and reads such a dictionary, and nothing
//! panics. Links: `tests/links.rs`.

mod common;

use common::{PageSpec, build_full, open};
use fastpdf_engine_api::{
    EngineDocument, EngineError, LimitKind, PageIndex, PixelFormat, Rotation,
};

/// Skipped by hayro as stray tokens, rejected by its parser (`/F`).
const BAD: &str = "<< /F 1 >> stream\nabc\nendstream";

fn document(page: PageSpec, catalog_extra: &str) -> Vec<u8> {
    build_full(
        &[page, PageSpec::new([0.0, 0.0, 200.0, 200.0], "")],
        catalog_extra,
        |_| {},
    )
}

fn not_panicked<T: std::fmt::Debug>(what: &str, r: &Result<T, EngineError>) {
    assert!(!matches!(r, Err(EngineError::Panicked(_))), "{what}: {r:?}");
}

#[test]
fn page_labels_read_up_to_an_unreadable_item() {
    let nums = format!("/PageLabels << /Nums [0 << /S /r >> {BAD} 1 << /S /D >>] >>");
    let doc = open(document(PageSpec::new([0.0, 0.0, 200.0, 200.0], ""), &nums));
    let labels = doc.page_labels();
    not_panicked("nums", &labels);
    let labels = labels.unwrap_or_default();
    assert_eq!(labels.label(PageIndex::FIRST).as_deref(), Some("i"));

    let kids = format!("/PageLabels << /Kids [<< /Nums [0 << /S /A >>] >> {BAD}] >>");
    let doc = open(document(PageSpec::new([0.0, 0.0, 200.0, 200.0], ""), &kids));
    let labels = doc.page_labels();
    not_panicked("kids", &labels);
    assert_eq!(
        labels
            .unwrap_or_default()
            .label(PageIndex::FIRST)
            .as_deref(),
        Some("A")
    );
}

#[test]
fn the_scan_refuses_arrays_it_cannot_read_and_reads_dictionaries() {
    // A resource array with an item that cannot be read: hayro may read
    // past it while the scan cannot, so the page is refused, not rendered
    // unchecked (and nothing panics).
    let mut page = PageSpec::new([0.0, 0.0, 200.0, 200.0], "0 0 1 rg 10 10 50 50 re f");
    page.resources = format!("/ColorSpace << /CS0 [/Indexed /DeviceRGB 1 {BAD}] >>");
    let doc = open(document(page, ""));
    let request = common::full_request(&doc, 0, 1.0, Rotation::R0);
    let rendered = common::render(&doc, &request, PixelFormat::default());
    assert!(
        matches!(
            rendered,
            Err(EngineError::LimitExceeded(LimitKind::ObjectSize))
        ),
        "{rendered:?}"
    );
    // The same item as the value of a resource dictionary is read as the
    // dictionary in front of `stream`, as hayro reads it: the page renders.
    let mut page = PageSpec::new([0.0, 0.0, 200.0, 200.0], "0 0 1 rg 10 10 50 50 re f");
    page.resources = format!("/Pattern << /P0 {BAD} >>");
    let doc = open(document(page, ""));
    let request = common::full_request(&doc, 0, 1.0, Rotation::R0);
    let rendered = common::render(&doc, &request, PixelFormat::default());
    not_panicked("render", &rendered);
    assert!(rendered.is_ok(), "{rendered:?}");
    assert_eq!(doc.panic_count(), 0);
}
