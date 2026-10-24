//! What the go-to-page field's text names: a page label, a page number, or
//! `#N` for the N-th page whatever its label.
//!
//! A label wins over a number (as in other readers): in a book whose front
//! matter is numbered i–x, `5` is the page labeled 5, and `#5` the fifth
//! page. Full-width forms typed with a CJK input method (`１２`, `＃３`)
//! count as their ASCII counterparts, after a label written with them
//! (`Ａ-1`) had its chance as typed.

use fastpdf_engine_api::display::fold_full_width;
use fastpdf_engine_api::{PageIndex, PageLabels};

/// Why the field's text names no page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageInputError {
    /// Nothing but spaces.
    Empty,
    /// No page has that label or number.
    NoSuchPage,
}

/// The page `input` names in a document of `page_count` pages whose labels
/// are `labels` (`None`: none, or not loaded yet; numbers still work).
pub fn resolve_page_input(
    input: &str,
    labels: Option<&PageLabels>,
    page_count: u32,
) -> Result<PageIndex, PageInputError> {
    let typed = input.trim();
    let folded: String = typed.chars().map(fold_full_width).collect();
    let text = folded.trim();
    if text.is_empty() {
        return Err(PageInputError::Empty);
    }
    if let Some(number) = text.strip_prefix('#') {
        return page_number(number.trim(), page_count).ok_or(PageInputError::NoSuchPage);
    }
    if let Some(labels) = labels.filter(|labels| !labels.is_plain_numbers())
        && let Some(page) = labels.find(typed).or_else(|| labels.find(text))
    {
        return Ok(page);
    }
    page_number(text, page_count).ok_or(PageInputError::NoSuchPage)
}

/// What is known about a document's page labels when the field opens.
#[derive(Debug, Clone, Copy)]
pub enum KnownLabels<'a> {
    /// Not loaded yet: text that names a page now may name another one
    /// once they arrive.
    Loading,
    /// Loaded; `None` when the document has none.
    Loaded(Option<&'a PageLabels>),
}

/// The text the field starts with on `page`: `shown` (the label the reader
/// shows for the page, else its number) when Enter on that text comes back
/// to `page`, otherwise `#N`. That is the case when another page has the
/// same label (chapters that restart at 1), when the page's number is
/// another page's label, and always while the labels are still loading.
pub fn prefill_text(
    page: PageIndex,
    shown: Option<&str>,
    labels: KnownLabels<'_>,
    page_count: u32,
) -> String {
    let number = page.display_number();
    let physical = format!("#{number}");
    let KnownLabels::Loaded(labels) = labels else {
        return physical;
    };
    let text = shown.map_or_else(|| number.to_string(), str::to_owned);
    if resolve_page_input(&text, labels, page_count) == Ok(page) {
        text
    } else {
        physical
    }
}

/// A one-based page number in `1..=page_count`, digits only.
fn page_number(text: &str, page_count: u32) -> Option<PageIndex> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = text.parse().ok()?;
    (1..=page_count).contains(&n).then(|| PageIndex::new(n - 1))
}

#[cfg(test)]
mod tests {
    use fastpdf_engine_api::{PageLabelRange, PageLabelStyle};

    use super::*;

    /// i, ii, iii, 1, 2, 3, ..., 117 over 120 pages.
    fn book() -> PageLabels {
        let range = |first, style| PageLabelRange {
            first_page: PageIndex::new(first),
            style,
            prefix: String::new(),
            start: 1,
        };
        PageLabels::new(
            vec![
                range(0, PageLabelStyle::LowerRoman),
                range(3, PageLabelStyle::Decimal),
            ],
            120,
        )
    }

    fn page(n: u32) -> Result<PageIndex, PageInputError> {
        Ok(PageIndex::new(n - 1))
    }

    #[test]
    fn labels_win_over_numbers_and_hash_forces_physical() {
        let labels = book();
        let resolve = |text| resolve_page_input(text, Some(&labels), 120);
        assert_eq!(resolve("iii"), page(3));
        assert_eq!(resolve("III"), page(3));
        // The page labeled 5 is the eighth page.
        assert_eq!(resolve("5"), page(8));
        assert_eq!(resolve("#5"), page(5));
        assert_eq!(resolve("# 5"), page(5));
        // Past the last label, numbers still name pages.
        assert_eq!(resolve("119"), page(119));
        assert_eq!(resolve("117"), page(120));
        assert_eq!(resolve("#121"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve("#"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve("#iii"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve("nope"), Err(PageInputError::NoSuchPage));
    }

    #[test]
    fn numbers_resolve_without_labels() {
        let resolve = |text| resolve_page_input(text, None, 120);
        assert_eq!(resolve("1"), page(1));
        assert_eq!(resolve("120"), page(120));
        assert_eq!(resolve("  7 "), page(7));
        assert_eq!(resolve("007"), page(7));
        assert_eq!(resolve("0"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve("121"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve("-3"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve("+3"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve("99999999999"), Err(PageInputError::NoSuchPage));
        assert_eq!(resolve(""), Err(PageInputError::Empty));
        assert_eq!(resolve(" \t "), Err(PageInputError::Empty));
        // Plain labels change nothing.
        let plain = PageLabels::new(
            vec![PageLabelRange {
                first_page: PageIndex::FIRST,
                style: PageLabelStyle::Decimal,
                prefix: String::new(),
                start: 1,
            }],
            120,
        );
        assert_eq!(resolve_page_input("12", Some(&plain), 120), page(12));
    }

    #[test]
    fn prefill_comes_back_to_the_same_page() {
        let range = |first, start| PageLabelRange {
            first_page: PageIndex::new(first),
            style: PageLabelStyle::Decimal,
            prefix: String::new(),
            start,
        };
        let at = |n: u32| PageIndex::new(n - 1);
        // Two parts that both start at 1: pages 1-3 and 4-6.
        let parts = PageLabels::new(vec![range(0, 1), range(3, 1)], 6);
        let known = KnownLabels::Loaded(Some(&parts));
        assert_eq!(prefill_text(at(2), Some("2"), known, 6), "2");
        assert_eq!(prefill_text(at(5), Some("2"), known, 6), "#5");
        // Pages 1-2 have no label, and their numbers are the labels of
        // pages 3-4.
        let late = PageLabels::new(vec![range(2, 1)], 6);
        let known = KnownLabels::Loaded(Some(&late));
        assert_eq!(prefill_text(at(1), None, known, 6), "#1");
        assert_eq!(prefill_text(at(2), None, known, 6), "#2");
        assert_eq!(prefill_text(at(4), Some("2"), known, 6), "2");
        // Numbers nobody else uses stay numbers.
        let later = PageLabels::new(vec![range(2, 5)], 6);
        let known = KnownLabels::Loaded(Some(&later));
        assert_eq!(prefill_text(at(1), None, known, 6), "1");
        assert_eq!(prefill_text(at(3), Some("5"), known, 6), "5");
        // A book with roman front matter.
        let labels = book();
        let known = KnownLabels::Loaded(Some(&labels));
        assert_eq!(prefill_text(at(3), Some("iii"), known, 120), "iii");
        assert_eq!(prefill_text(at(11), Some("8"), known, 120), "8");
        // No labels: the number.
        assert_eq!(
            prefill_text(at(3), None, KnownLabels::Loaded(None), 120),
            "3"
        );
        // Labels still loading: a number typed now could mean a label later.
        assert_eq!(
            prefill_text(at(11), Some("8"), KnownLabels::Loading, 120),
            "#11"
        );
        assert_eq!(prefill_text(at(3), None, KnownLabels::Loading, 120), "#3");
    }

    #[test]
    fn full_width_labels_match_as_written() {
        let labels = PageLabels::new(
            vec![
                PageLabelRange {
                    first_page: PageIndex::FIRST,
                    style: PageLabelStyle::Decimal,
                    prefix: "Ａ-".into(),
                    start: 1,
                },
                PageLabelRange {
                    first_page: PageIndex::new(2),
                    style: PageLabelStyle::Decimal,
                    prefix: "A-".into(),
                    start: 1,
                },
            ],
            4,
        );
        let resolve = |text| resolve_page_input(text, Some(&labels), 4);
        assert_eq!(resolve("Ａ-2"), page(2));
        assert_eq!(resolve("A-2"), page(4));
        // Typed full width, found folded.
        assert_eq!(resolve("Ａ－２"), page(4));
    }

    #[test]
    fn full_width_input_from_the_ime() {
        let labels = book();
        assert_eq!(resolve_page_input("１２", None, 120), page(12));
        assert_eq!(resolve_page_input("＃３", Some(&labels), 120), page(3));
        assert_eq!(
            resolve_page_input("\u{3000}２\u{3000}", Some(&labels), 120),
            page(5)
        );
        assert_eq!(resolve_page_input("ｉｉ", Some(&labels), 120), page(2));
        assert_eq!(
            resolve_page_input("\u{3000}", None, 120),
            Err(PageInputError::Empty)
        );
    }
}
