use std::ops::Range;

use fastpdf_engine_api::{PageIndex, PageSize};

/// Rectangle in layout space: points, origin at the document's top-left,
/// y pointing down.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl LayoutRect {
    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn center_y(&self) -> f64 {
        self.y + self.height / 2.0
    }

    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        (x0 < x1 && y0 < y1).then_some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }

    /// Grows the rectangle vertically by `dy` on both sides.
    pub fn expand_y(&self, dy: f64) -> Self {
        Self {
            y: self.y - dy,
            height: self.height + 2.0 * dy,
            ..*self
        }
    }
}

/// A document position that survives relayout: when estimated page sizes
/// are replaced by real ones, the view stays on the same content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollAnchor {
    /// The first page of the row the position is in.
    pub page: PageIndex,
    /// Offset from the row top as a fraction of the row height.
    pub fraction: f64,
}

/// Pages per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Spread {
    /// One page per row.
    #[default]
    Single,
    /// Two pages side by side: 1-2, 3-4, ...
    Double,
    /// Two pages side by side after a first page on its own, like a book's
    /// cover: 1, 2-3, 4-5, ...
    DoubleCover,
}

/// How pages are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutMode {
    pub spread: Spread,
    /// All rows one below the other (scroll mode); `false` shows one row at
    /// a time (page mode).
    pub continuous: bool,
}

impl Default for LayoutMode {
    fn default() -> Self {
        Self {
            spread: Spread::Single,
            continuous: true,
        }
    }
}

/// Where a page sits in its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    /// One page per row, centered.
    Center,
    /// Left of the spine of a two-page row.
    Left,
    /// Right of the spine.
    Right,
}

/// Layout of all pages in rows of one or two pages.
///
/// Opening a PDF must not require resolving every page (spec §11), so pages
/// start with an estimated size (usually page 1's) and are refined as their
/// real sizes become known.
///
/// * One page per row: pages are centered horizontally.
/// * Two pages per row: pages meet at a vertical spine (like a book), the
///   left page's right edge and the right page's left edge one gap apart;
///   a page alone in its row keeps its column. Pages are centered
///   vertically in their row.
/// * Continuous: rows are stacked with gaps. Page mode: only the focus row
///   has geometry ([`DocumentLayout::page_rect`] is `None` for every other
///   page), placed at the top of the layout.
#[derive(Debug, Clone)]
pub struct DocumentLayout {
    sizes: Vec<PageSize>,
    known: Vec<bool>,
    mode: LayoutMode,
    gap: f64,
    /// Continuous mode: the top of every row.
    row_tops: Vec<f64>,
    /// Widest page of one page per row.
    max_width: f64,
    /// Widest page of each column of two-page rows.
    left_width: f64,
    right_width: f64,
    /// Continuous mode: the height of all rows with gaps.
    height: f64,
    /// Page mode: the row shown.
    focus_row: usize,
}

impl DocumentLayout {
    /// One page per row, continuous. `estimate` is a displayed size (after
    /// rotation); `gap` is the spacing between pages and around the
    /// document, in points.
    pub fn new(page_count: u32, estimate: PageSize, gap: f32) -> Self {
        Self::with_mode(page_count, estimate, gap, LayoutMode::default())
    }

    pub fn with_mode(page_count: u32, estimate: PageSize, gap: f32, mode: LayoutMode) -> Self {
        let n = page_count as usize;
        let mut layout = Self {
            sizes: vec![estimate; n],
            known: vec![false; n],
            mode,
            gap: f64::from(gap.max(0.0)),
            row_tops: Vec::new(),
            max_width: 0.0,
            left_width: 0.0,
            right_width: 0.0,
            height: 0.0,
            focus_row: 0,
        };
        layout.relayout();
        layout
    }

    pub fn page_count(&self) -> u32 {
        self.sizes.len() as u32
    }

    pub fn mode(&self) -> LayoutMode {
        self.mode
    }

    /// One row at a time.
    pub fn is_paged(&self) -> bool {
        !self.mode.continuous
    }

    /// Switches the arrangement; the focus row becomes the one holding the
    /// page that was focused.
    pub fn set_mode(&mut self, mode: LayoutMode) {
        if mode == self.mode {
            return;
        }
        let focused = self.focus_page();
        self.mode = mode;
        self.relayout();
        self.set_focus(focused);
    }

    pub fn is_known(&self, page: PageIndex) -> bool {
        self.known.get(page.as_usize()).copied().unwrap_or(false)
    }

    pub fn page_size(&self, page: PageIndex) -> Option<PageSize> {
        self.sizes.get(page.as_usize()).copied()
    }

    /// Records real page sizes; relayouts once for the whole batch.
    /// Returns true if anything moved.
    pub fn set_page_sizes(
        &mut self,
        updates: impl IntoIterator<Item = (PageIndex, PageSize)>,
    ) -> bool {
        let mut changed = false;
        for (page, size) in updates {
            let i = page.as_usize();
            if i >= self.sizes.len() {
                continue;
            }
            self.known[i] = true;
            if self.sizes[i] != size {
                self.sizes[i] = size;
                changed = true;
            }
        }
        if changed {
            self.relayout();
        }
        changed
    }

    /// Width and height of the whole document including outer gaps (page
    /// mode: of the focus row).
    pub fn total_size(&self) -> (f64, f64) {
        let height = if self.sizes.is_empty() {
            0.0
        } else if self.is_paged() {
            self.row_height(self.focus_row) + 2.0 * self.gap
        } else {
            self.height
        };
        (self.content_width() + 2.0 * self.gap, height)
    }

    /// The size that fit-page fits into the view for `page`: the page
    /// itself with one page per row, else its row at the width of both
    /// columns (so every row fits the width).
    pub fn fit_size(&self, page: PageIndex) -> Option<PageSize> {
        let size = self.page_size(page)?;
        if self.mode.spread == Spread::Single {
            return Some(size);
        }
        let row = self.row_index(page.as_usize());
        Some(PageSize::new(
            self.content_width() as f32,
            self.row_height(row) as f32,
        ))
    }

    pub fn page_rect(&self, page: PageIndex) -> Option<LayoutRect> {
        let i = page.as_usize();
        if i >= self.sizes.len() {
            return None;
        }
        let row = self.row_index(i);
        if self.is_paged() && row != self.focus_row {
            return None;
        }
        Some(self.rect_in_row(i, row, self.row_top(row)))
    }

    /// Page mode: where `page` would be if its row were the focus row (for
    /// rendering the next row ahead of a page turn). Continuous mode: the
    /// same as [`DocumentLayout::page_rect`].
    pub fn page_rect_if_focused(&self, page: PageIndex) -> Option<LayoutRect> {
        let i = page.as_usize();
        if i >= self.sizes.len() {
            return None;
        }
        let row = self.row_index(i);
        let top = if self.is_paged() {
            self.gap
        } else {
            self.row_top(row)
        };
        Some(self.rect_in_row(i, row, top))
    }

    /// Pages whose rows intersect `y0..y1`. O(log n). A page of a two-page
    /// row may not itself intersect when it is shorter than its row.
    pub fn pages_intersecting(&self, y0: f64, y1: f64) -> Range<u32> {
        let rows = self.row_count();
        if rows == 0 {
            return 0..0;
        }
        if self.is_paged() {
            let pages = self.row_pages(self.focus_row);
            let (top, bottom) = (self.gap, self.gap + self.row_height(self.focus_row));
            return if bottom > y0 && top < y1 {
                pages
            } else {
                pages.start..pages.start
            };
        }
        // Row tops and bottoms both increase monotonically, so both ends of
        // the range are binary searches.
        let start = first_index(rows, |r| self.row_tops[r] + self.row_height(r) > y0);
        let end = self.row_tops.partition_point(|&top| top < y1).max(start);
        if start == end {
            let page = if start < rows {
                self.row_pages(start).start
            } else {
                self.page_count()
            };
            return page..page;
        }
        self.row_pages(start).start..self.row_pages(end - 1).end
    }

    /// The first page of the row containing `y`, or of the nearest row.
    pub fn page_at(&self, y: f64) -> PageIndex {
        if self.sizes.is_empty() {
            return PageIndex::FIRST;
        }
        PageIndex::new(self.row_pages(self.row_at(y)).start)
    }

    /// The page of the row containing `y` nearest to `x`.
    pub fn page_at_point(&self, x: f64, y: f64) -> PageIndex {
        if self.sizes.is_empty() {
            return PageIndex::FIRST;
        }
        let row = self.row_at(y);
        let pages = self.row_pages(row);
        let top = self.row_top(row);
        let distance = |p: u32| {
            let r = self.rect_in_row(p as usize, row, top);
            if x < r.x {
                r.x - x
            } else if x > r.right() {
                x - r.right()
            } else {
                0.0
            }
        };
        let page = pages
            .clone()
            .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
            .unwrap_or(pages.start);
        PageIndex::new(page)
    }

    pub fn anchor_at(&self, y: f64) -> ScrollAnchor {
        if self.sizes.is_empty() {
            return ScrollAnchor {
                page: PageIndex::FIRST,
                fraction: 0.0,
            };
        }
        let row = self.row_at(y);
        let height = self.row_height(row).max(1.0);
        ScrollAnchor {
            page: PageIndex::new(self.row_pages(row).start),
            fraction: (y - self.row_top(row)) / height,
        }
    }

    pub fn y_for_anchor(&self, anchor: ScrollAnchor) -> f64 {
        if self.sizes.is_empty() {
            return 0.0;
        }
        let i = anchor.page.as_usize().min(self.sizes.len() - 1);
        let row = self.row_index(i);
        self.row_top(row) + anchor.fraction * self.row_height(row)
    }

    /// Top of the row holding `page` (page mode: of the focus row, at the
    /// top of the layout).
    pub fn row_top_of(&self, page: PageIndex) -> Option<f64> {
        let i = page.as_usize();
        (i < self.sizes.len()).then(|| self.row_top(self.row_index(i)))
    }

    pub fn row_count(&self) -> usize {
        let n = self.sizes.len();
        match self.mode.spread {
            Spread::Single => n,
            Spread::Double => n.div_ceil(2),
            Spread::DoubleCover if n == 0 => 0,
            Spread::DoubleCover => 1 + n / 2,
        }
    }

    /// The row holding `page` (clamped to the last row).
    pub fn row_of(&self, page: PageIndex) -> usize {
        self.row_index(page.as_usize().min(self.sizes.len().saturating_sub(1)))
    }

    /// Pages of `row`, in reading order.
    pub fn row_pages(&self, row: usize) -> Range<u32> {
        let n = self.sizes.len();
        let (start, len) = match self.mode.spread {
            Spread::Single => (row, 1),
            Spread::Double => (2 * row, 2),
            Spread::DoubleCover if row == 0 => (0, 1),
            Spread::DoubleCover => (2 * row - 1, 2),
        };
        let start = start.min(n);
        start as u32..(start + len).min(n) as u32
    }

    /// The first page of the row after `page`'s.
    pub fn next_row_page(&self, page: PageIndex) -> Option<PageIndex> {
        let next = self.row_of(page) + 1;
        (next < self.row_count()).then(|| PageIndex::new(self.row_pages(next).start))
    }

    /// The first page of the row before `page`'s.
    pub fn prev_row_page(&self, page: PageIndex) -> Option<PageIndex> {
        let row = self.row_of(page);
        (row > 0).then(|| PageIndex::new(self.row_pages(row - 1).start))
    }

    /// Page mode: the row shown.
    pub fn focus_row(&self) -> usize {
        self.focus_row
    }

    /// The first page of the focus row.
    pub fn focus_page(&self) -> PageIndex {
        PageIndex::new(self.row_pages(self.focus_row).start)
    }

    /// Makes the row holding `page` the focus row; true if it changed.
    pub fn set_focus(&mut self, page: PageIndex) -> bool {
        let row = self.row_of(page);
        let changed = row != self.focus_row;
        self.focus_row = row;
        changed
    }

    fn row_index(&self, i: usize) -> usize {
        match self.mode.spread {
            Spread::Single => i,
            Spread::Double => i / 2,
            Spread::DoubleCover => i.div_ceil(2),
        }
    }

    fn column(&self, i: usize) -> Column {
        match self.mode.spread {
            Spread::Single => Column::Center,
            Spread::Double if i.is_multiple_of(2) => Column::Left,
            Spread::Double => Column::Right,
            Spread::DoubleCover if !i.is_multiple_of(2) => Column::Left,
            Spread::DoubleCover => Column::Right,
        }
    }

    /// The tallest page of `row`.
    fn row_height(&self, row: usize) -> f64 {
        self.row_pages(row)
            .map(|p| f64::from(self.sizes[p as usize].height))
            .fold(0.0, f64::max)
    }

    fn row_top(&self, row: usize) -> f64 {
        if self.is_paged() {
            self.gap
        } else {
            self.row_tops.get(row).copied().unwrap_or(self.gap)
        }
    }

    /// The row containing `y`, or the nearest one (page mode: the focus row).
    fn row_at(&self, y: f64) -> usize {
        if self.is_paged() {
            return self.focus_row;
        }
        let rows = self.row_count();
        self.row_tops
            .partition_point(|&top| top <= y)
            .saturating_sub(1)
            .min(rows.saturating_sub(1))
    }

    fn rect_in_row(&self, i: usize, row: usize, top: f64) -> LayoutRect {
        let size = self.sizes[i];
        let (width, height) = (f64::from(size.width), f64::from(size.height));
        LayoutRect {
            x: self.page_x(i, width),
            y: top + (self.row_height(row) - height) / 2.0,
            width,
            height,
        }
    }

    fn page_x(&self, i: usize, width: f64) -> f64 {
        match self.column(i) {
            Column::Center => self.gap + (self.max_width - width) / 2.0,
            Column::Left => self.gap + self.left_width - width,
            Column::Right => self.gap + self.left_width + self.inner_gap(),
        }
    }

    /// Space between the two pages of a row (none when one column is empty,
    /// e.g. a one-page document with a cover).
    fn inner_gap(&self) -> f64 {
        if self.left_width > 0.0 && self.right_width > 0.0 {
            self.gap
        } else {
            0.0
        }
    }

    fn content_width(&self) -> f64 {
        match self.mode.spread {
            Spread::Single => self.max_width,
            Spread::Double | Spread::DoubleCover => {
                self.left_width + self.inner_gap() + self.right_width
            }
        }
    }

    fn relayout(&mut self) {
        self.max_width = 0.0;
        self.left_width = 0.0;
        self.right_width = 0.0;
        for i in 0..self.sizes.len() {
            let width = f64::from(self.sizes[i].width);
            let column = match self.column(i) {
                Column::Center => &mut self.max_width,
                Column::Left => &mut self.left_width,
                Column::Right => &mut self.right_width,
            };
            *column = column.max(width);
        }
        let rows = self.row_count();
        self.row_tops.clear();
        if self.mode.continuous {
            self.row_tops.reserve(rows);
            let mut y = self.gap;
            for row in 0..rows {
                self.row_tops.push(y);
                y += self.row_height(row) + self.gap;
            }
            self.height = if rows == 0 { 0.0 } else { y };
        } else {
            self.height = 0.0;
        }
        self.focus_row = self.focus_row.min(rows.saturating_sub(1));
    }
}

/// Smallest index in `0..len` for which the monotonic predicate holds.
fn first_index(len: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, len);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> DocumentLayout {
        DocumentLayout::new(4, PageSize::new(600.0, 800.0), 10.0)
    }

    fn mode(spread: Spread, continuous: bool) -> LayoutMode {
        LayoutMode { spread, continuous }
    }

    #[test]
    fn pages_are_stacked_with_gaps() {
        let l = layout();
        assert_eq!(l.page_rect(PageIndex::new(1)).unwrap().y, 820.0);
        assert_eq!(l.total_size(), (620.0, 10.0 + 4.0 * 810.0));
        assert!(!l.is_known(PageIndex::FIRST));
    }

    #[test]
    fn visible_page_range() {
        let l = layout();
        assert_eq!(l.pages_intersecting(0.0, 5.0), 0..0);
        assert_eq!(l.pages_intersecting(0.0, 11.0), 0..1);
        assert_eq!(l.pages_intersecting(805.0, 815.0), 0..1);
        assert_eq!(l.pages_intersecting(812.0, 818.0), 1..1); // inside the gap
        assert_eq!(l.pages_intersecting(500.0, 1700.0), 0..3);
        assert_eq!(l.pages_intersecting(10_000.0, 20_000.0), 4..4);
    }

    #[test]
    fn refined_sizes_keep_the_anchor() {
        let mut l = layout();
        let y = l.page_rect(PageIndex::new(2)).unwrap().y + 400.0;
        let anchor = l.anchor_at(y);
        assert_eq!(anchor.page, PageIndex::new(2));
        assert!((anchor.fraction - 0.5).abs() < 1e-9);

        // Page 0 turns out to be landscape and narrower: everything shifts up.
        assert!(l.set_page_sizes([(PageIndex::FIRST, PageSize::new(500.0, 400.0))]));
        assert!(l.is_known(PageIndex::FIRST));
        let new_y = l.y_for_anchor(anchor);
        assert_eq!(new_y, y - 400.0);
        assert_eq!(l.page_at(new_y), PageIndex::new(2));
        // Narrower page is centered against the widest one.
        assert_eq!(l.page_rect(PageIndex::FIRST).unwrap().x, 10.0 + 50.0);
    }

    #[test]
    fn unchanged_sizes_do_not_relayout() {
        let mut l = layout();
        assert!(!l.set_page_sizes([(PageIndex::FIRST, PageSize::new(600.0, 800.0))]));
        assert!(l.is_known(PageIndex::FIRST));
        assert!(!l.set_page_sizes([(PageIndex::new(99), PageSize::LETTER)]));
    }

    #[test]
    fn large_documents_are_cheap() {
        let l = DocumentLayout::new(1_000_000, PageSize::LETTER, 8.0);
        let mid = l.page_rect(PageIndex::new(500_000)).unwrap();
        let range = l.pages_intersecting(mid.y, mid.y + 1000.0);
        assert_eq!(range.start, 500_000);
        assert_eq!(range.end, 500_002);
    }

    #[test]
    fn two_pages_meet_at_the_spine() {
        let l = DocumentLayout::with_mode(
            5,
            PageSize::new(600.0, 800.0),
            10.0,
            mode(Spread::Double, true),
        );
        assert_eq!(l.row_count(), 3);
        assert_eq!(l.row_pages(0), 0..2);
        assert_eq!(l.row_pages(2), 4..5);
        let left = l.page_rect(PageIndex::new(2)).unwrap();
        let right = l.page_rect(PageIndex::new(3)).unwrap();
        assert_eq!((left.x, left.y), (10.0, 820.0));
        assert_eq!((right.x, right.y), (620.0, 820.0));
        // The last page is alone but stays in the left column.
        assert_eq!(l.page_rect(PageIndex::new(4)).unwrap().x, 10.0);
        assert_eq!(
            l.total_size(),
            (10.0 + 600.0 + 10.0 + 600.0 + 10.0, 10.0 + 3.0 * 810.0)
        );
        assert_eq!(l.pages_intersecting(825.0, 900.0), 2..4);
        assert_eq!(l.page_at(1000.0), PageIndex::new(2));
        assert_eq!(l.page_at_point(700.0, 1000.0), PageIndex::new(3));
        assert_eq!(l.page_at_point(0.0, 1000.0), PageIndex::new(2));
        assert_eq!(l.next_row_page(PageIndex::new(3)), Some(PageIndex::new(4)));
        assert_eq!(l.prev_row_page(PageIndex::new(3)), Some(PageIndex::FIRST));
        assert_eq!(l.next_row_page(PageIndex::new(4)), None);
    }

    #[test]
    fn a_cover_page_sits_alone_on_the_right() {
        let l = DocumentLayout::with_mode(
            4,
            PageSize::new(600.0, 800.0),
            10.0,
            mode(Spread::DoubleCover, true),
        );
        assert_eq!(l.row_count(), 3);
        assert_eq!(
            (l.row_pages(0), l.row_pages(1), l.row_pages(2)),
            (0..1, 1..3, 3..4)
        );
        assert_eq!(l.page_rect(PageIndex::FIRST).unwrap().x, 620.0);
        assert_eq!(l.page_rect(PageIndex::new(1)).unwrap().x, 10.0);
        assert_eq!(l.page_rect(PageIndex::new(2)).unwrap().x, 620.0);
        assert_eq!(l.page_rect(PageIndex::new(3)).unwrap().x, 10.0);
        // A one-page document has no left column and no inner gap.
        let one = DocumentLayout::with_mode(
            1,
            PageSize::new(600.0, 800.0),
            10.0,
            mode(Spread::DoubleCover, true),
        );
        assert_eq!(one.total_size().0, 620.0);
        assert_eq!(one.page_rect(PageIndex::FIRST).unwrap().x, 10.0);
        assert_eq!(
            DocumentLayout::with_mode(0, PageSize::LETTER, 8.0, mode(Spread::DoubleCover, true))
                .row_count(),
            0
        );
    }

    #[test]
    fn shorter_pages_are_centered_in_their_row() {
        let mut l = DocumentLayout::with_mode(
            2,
            PageSize::new(600.0, 800.0),
            10.0,
            mode(Spread::Double, true),
        );
        l.set_page_sizes([(PageIndex::new(1), PageSize::new(800.0, 600.0))]);
        let landscape = l.page_rect(PageIndex::new(1)).unwrap();
        assert_eq!((landscape.x, landscape.y), (620.0, 110.0));
        assert_eq!(
            l.fit_size(PageIndex::new(1)),
            Some(PageSize::new(1410.0, 800.0))
        );
        assert_eq!(l.row_top_of(PageIndex::new(1)), Some(10.0));
    }

    #[test]
    fn page_mode_shows_only_the_focus_row() {
        let mut l = DocumentLayout::with_mode(
            5,
            PageSize::new(600.0, 800.0),
            10.0,
            mode(Spread::Single, false),
        );
        assert!(l.is_paged());
        assert_eq!(l.total_size(), (620.0, 820.0));
        assert!(l.page_rect(PageIndex::new(1)).is_none());
        assert!(l.set_focus(PageIndex::new(3)));
        assert!(!l.set_focus(PageIndex::new(3)));
        let rect = l.page_rect(PageIndex::new(3)).unwrap();
        assert_eq!((rect.x, rect.y), (10.0, 10.0));
        assert!(l.page_rect(PageIndex::FIRST).is_none());
        assert_eq!(l.pages_intersecting(0.0, 1e9), 3..4);
        assert_eq!(l.pages_intersecting(900.0, 1e9), 3..3);
        assert_eq!(l.page_at(1e6), PageIndex::new(3));
        assert_eq!(l.focus_page(), PageIndex::new(3));
        // The next page can be rendered ahead of a page turn.
        assert_eq!(l.page_rect_if_focused(PageIndex::new(4)), Some(rect));
        let anchor = l.anchor_at(410.0);
        assert_eq!(anchor.page, PageIndex::new(3));
        assert!((l.y_for_anchor(anchor) - 410.0).abs() < 1e-9);
    }

    #[test]
    fn switching_modes_keeps_the_focused_page() {
        let mut l = DocumentLayout::with_mode(
            6,
            PageSize::new(600.0, 800.0),
            10.0,
            mode(Spread::Single, false),
        );
        l.set_focus(PageIndex::new(4));
        l.set_mode(mode(Spread::Double, false));
        assert_eq!(l.focus_row(), 2);
        assert_eq!(l.focus_page(), PageIndex::new(4));
        assert_eq!(l.pages_intersecting(0.0, 1e9), 4..6);
        l.set_mode(mode(Spread::DoubleCover, false));
        assert_eq!(l.focus_page(), PageIndex::new(3));
        l.set_mode(mode(Spread::DoubleCover, true));
        assert!(l.page_rect(PageIndex::FIRST).is_some());
    }
}
