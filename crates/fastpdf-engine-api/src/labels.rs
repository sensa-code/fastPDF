//! Page labels (ISO 32000-2 §12.4.2): the document's own page numbering,
//! such as `i`, `ii`, `iii` for front matter, `1`, `2`, ... for the body and
//! `A-1` for an appendix.
//!
//! A document defines ranges; each range starts at a page and numbers its
//! pages with a style, a prefix and a start value. [`PageLabels`] keeps only
//! the ranges and formats a label on demand, so a document with a million
//! pages costs its ranges, not a million strings.
//!
//! The ranges come from hostile files: [`PageLabels::new`] normalizes them
//! (order, duplicates, out-of-range pages, start values, prefix text and
//! length) and every layer that receives labels calls it again.

mod numbering;
#[cfg(test)]
mod tests;

use crate::PageIndex;
use crate::display::clean_display;

/// How the numeric part of a label is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PageLabelStyle {
    /// No numeric part: every page of the range is labeled with the prefix
    /// alone. Also used for unknown styles.
    #[default]
    None,
    /// `1, 2, 3` (`/S /D`).
    Decimal,
    /// `I, II, III` (`/S /R`).
    UpperRoman,
    /// `i, ii, iii` (`/S /r`).
    LowerRoman,
    /// `A, B, ..., Z, AA, BB` (`/S /A`).
    UpperLetters,
    /// `a, b, ..., z, aa, bb` (`/S /a`).
    LowerLetters,
}

/// One labeling range: from `first_page` up to the next range's first page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageLabelRange {
    pub first_page: PageIndex,
    pub style: PageLabelStyle,
    /// Text before the number (`/P`).
    pub prefix: String,
    /// Number of `first_page` (`/St`), at least 1.
    pub start: u32,
}

/// The page labels of one document, normalized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageLabels {
    ranges: Vec<PageLabelRange>,
    page_count: u32,
}

impl PageLabels {
    /// Ranges kept at most (the ones that start first).
    pub const MAX_RANGES: usize = 4096;
    /// Longest prefix kept, in bytes of UTF-8.
    pub const MAX_PREFIX_BYTES: usize = 128;
    /// Longest text [`PageLabels::find`] looks at: no label is longer.
    const MAX_LABEL_BYTES: usize = Self::MAX_PREFIX_BYTES + numbering::MAX_NUMERIC_CHARS;

    /// Normalizes `ranges` for a document of `page_count` pages: ranges that
    /// start past the last page are dropped, the rest are sorted by their
    /// first page (of several ranges starting on the same page the first
    /// one given wins) and cut to [`Self::MAX_RANGES`]; start values below 1
    /// become 1; prefixes are made safe to display
    /// ([`crate::display::clean_display`]) and cut to
    /// [`Self::MAX_PREFIX_BYTES`]. Idempotent.
    pub fn new(mut ranges: Vec<PageLabelRange>, page_count: u32) -> Self {
        ranges.retain(|r| r.first_page.get() < page_count);
        ranges.sort_by_key(|r| r.first_page);
        ranges.dedup_by_key(|r| r.first_page);
        ranges.truncate(Self::MAX_RANGES);
        for range in &mut ranges {
            range.start = range.start.max(1);
            range.prefix = clean_display(&range.prefix, Self::MAX_PREFIX_BYTES);
        }
        Self { ranges, page_count }
    }

    pub fn ranges(&self) -> &[PageLabelRange] {
        &self.ranges
    }

    pub fn into_ranges(self) -> Vec<PageLabelRange> {
        self.ranges
    }

    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    /// True when the document defines no labels.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// True when every page's label is just its page number (or there are
    /// no labels), so showing labels would add nothing.
    pub fn is_plain_numbers(&self) -> bool {
        match self.ranges.first() {
            None => true,
            Some(first) if first.first_page != PageIndex::FIRST => false,
            Some(_) => self.ranges.iter().all(|r| {
                r.style == PageLabelStyle::Decimal
                    && r.prefix.is_empty()
                    && u64::from(r.start) == u64::from(r.first_page.get()) + 1
            }),
        }
    }

    /// The label of `page`; `None` for pages before the first range (the
    /// document gives them no label) and past the last page. O(log ranges).
    pub fn label(&self, page: PageIndex) -> Option<String> {
        if page.get() >= self.page_count {
            return None;
        }
        let index = self
            .ranges
            .partition_point(|r| r.first_page <= page)
            .checked_sub(1)?;
        let range = self.ranges.get(index)?;
        let n = u64::from(range.start) + u64::from(page.get() - range.first_page.get());
        let mut label = String::with_capacity(range.prefix.len() + 8);
        label.push_str(&range.prefix);
        numbering::format(range.style, n, &mut label);
        Some(label)
    }

    /// The page labeled `text`: an exact match first, then one that differs
    /// only in ASCII case (`III` finds `iii`). When several pages carry the
    /// label, the first one in document order. A prefix-only range is found
    /// by its prefix and answers its first page. O(ranges × label length).
    pub fn find(&self, text: &str) -> Option<PageIndex> {
        if text.is_empty() || text.len() > Self::MAX_LABEL_BYTES {
            return None;
        }
        [true, false]
            .into_iter()
            .find_map(|exact| self.find_with(text, exact))
    }

    fn find_with(&self, text: &str, exact: bool) -> Option<PageIndex> {
        self.ranges.iter().enumerate().find_map(|(i, range)| {
            let end = self
                .ranges
                .get(i + 1)
                .map_or(self.page_count, |next| next.first_page.get());
            let rest = strip_prefix(text, &range.prefix, exact)?;
            let n = if range.style == PageLabelStyle::None {
                rest.is_empty().then_some(u64::from(range.start))?
            } else {
                let n = numbering::parse(range.style, rest)?;
                let mut canonical = String::new();
                numbering::format(range.style, n, &mut canonical);
                let same = if exact {
                    canonical == rest
                } else {
                    canonical.eq_ignore_ascii_case(rest)
                };
                same.then_some(n)?
            };
            let offset = n.checked_sub(u64::from(range.start))?;
            let first = range.first_page.get();
            (offset < u64::from(end - first)).then(|| PageIndex::new(first + offset as u32))
        })
    }
}

/// `text` without `prefix`, compared exactly or ignoring ASCII case.
fn strip_prefix<'a>(text: &'a str, prefix: &str, exact: bool) -> Option<&'a str> {
    if exact {
        return text.strip_prefix(prefix);
    }
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| text.get(prefix.len()..))
        .flatten()
}
