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
    pub page: PageIndex,
    /// Offset from the page top as a fraction of the page height.
    pub fraction: f64,
}

/// Continuous vertical layout of all pages.
///
/// Opening a PDF must not require resolving every page (spec §11), so pages
/// start with an estimated size (usually page 1's) and are refined as their
/// real sizes become known. Pages are centered horizontally.
#[derive(Debug, Clone)]
pub struct DocumentLayout {
    sizes: Vec<PageSize>,
    known: Vec<bool>,
    tops: Vec<f64>,
    gap: f64,
    max_width: f64,
    height: f64,
}

impl DocumentLayout {
    /// `sizes` are displayed sizes (after rotation); `gap` is the spacing
    /// between pages and around the document, in points.
    pub fn new(page_count: u32, estimate: PageSize, gap: f32) -> Self {
        let n = page_count as usize;
        let mut layout = Self {
            sizes: vec![estimate; n],
            known: vec![false; n],
            tops: Vec::with_capacity(n),
            gap: f64::from(gap.max(0.0)),
            max_width: 0.0,
            height: 0.0,
        };
        layout.relayout();
        layout
    }

    pub fn page_count(&self) -> u32 {
        self.sizes.len() as u32
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

    /// Width and height of the whole document including outer gaps.
    pub fn total_size(&self) -> (f64, f64) {
        (self.max_width + 2.0 * self.gap, self.height)
    }

    pub fn page_rect(&self, page: PageIndex) -> Option<LayoutRect> {
        let i = page.as_usize();
        let size = self.sizes.get(i)?;
        let width = f64::from(size.width);
        Some(LayoutRect {
            x: self.gap + (self.max_width - width) / 2.0,
            y: self.tops[i],
            width,
            height: f64::from(size.height),
        })
    }

    /// Pages whose vertical extent intersects `y0..y1`. O(log n).
    pub fn pages_intersecting(&self, y0: f64, y1: f64) -> Range<u32> {
        // Tops and bottoms both increase monotonically, so both ends of the
        // range are binary searches.
        let start = first_index(self.sizes.len(), |i| self.bottom(i) > y0);
        let end = self.tops.partition_point(|&top| top < y1).max(start);
        start as u32..end as u32
    }

    /// The page containing `y`, or the nearest one.
    pub fn page_at(&self, y: f64) -> PageIndex {
        let i = self.tops.partition_point(|&top| top <= y);
        PageIndex::new(i.saturating_sub(1).min(self.sizes.len().saturating_sub(1)) as u32)
    }

    pub fn anchor_at(&self, y: f64) -> ScrollAnchor {
        let page = self.page_at(y);
        let i = page.as_usize();
        let height = f64::from(self.sizes.get(i).map_or(1.0, |s| s.height)).max(1.0);
        let top = self.tops.get(i).copied().unwrap_or(0.0);
        ScrollAnchor {
            page,
            fraction: (y - top) / height,
        }
    }

    pub fn y_for_anchor(&self, anchor: ScrollAnchor) -> f64 {
        let i = anchor
            .page
            .as_usize()
            .min(self.sizes.len().saturating_sub(1));
        match (self.tops.get(i), self.sizes.get(i)) {
            (Some(top), Some(size)) => top + anchor.fraction * f64::from(size.height),
            _ => 0.0,
        }
    }

    fn bottom(&self, i: usize) -> f64 {
        self.tops[i] + f64::from(self.sizes[i].height)
    }

    fn relayout(&mut self) {
        self.tops.clear();
        let mut y = self.gap;
        let mut max_width: f64 = 0.0;
        for size in &self.sizes {
            self.tops.push(y);
            y += f64::from(size.height) + self.gap;
            max_width = max_width.max(f64::from(size.width));
        }
        self.max_width = max_width;
        self.height = if self.sizes.is_empty() { 0.0 } else { y };
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
}
