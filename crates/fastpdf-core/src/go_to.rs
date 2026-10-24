//! What the go-to-page field's text names: a page label, a page number, or
//! `#N` for the N-th page whatever its label.
//!
//! A label wins over a number (as in other readers): in a book whose front
//! matter is numbered i–x, `5` is the page labeled 5, and `#5` the fifth
//! page. Full-width forms typed with a CJK input method (`１２`, `＃３`)
//! count as their ASCII counterparts.

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
    let folded: String = input.chars().map(fold_full_width).collect();
    let text = folded.trim();
    if text.is_empty() {
        return Err(PageInputError::Empty);
    }
    if let Some(number) = text.strip_prefix('#') {
        return page_number(number.trim(), page_count).ok_or(PageInputError::NoSuchPage);
    }
    if let Some(page) = labels
        .filter(|labels| !labels.is_plain_numbers())
        .and_then(|labels| labels.find(text))
    {
        return Ok(page);
    }
    page_number(text, page_count).ok_or(PageInputError::NoSuchPage)
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
