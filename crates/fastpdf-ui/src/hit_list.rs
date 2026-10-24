//! The hits of the running search, in document order, with the active one.
//! The search streams hits page by page, from the start page outward, so
//! pages arrive out of order: hits are kept in one bucket per page, and a
//! page's hits are spliced in once (moving bucket headers, not hits).
//! Lookups by index and by page are binary searches. At most
//! [`SearchQuery::DEFAULT_MAX_HITS`] hits are kept (the search stops there
//! too).

use fastpdf_engine_api::{PageIndex, PageRect};
use fastpdf_search::{SearchHit, SearchQuery};

/// Hits kept at most.
const MAX_HITS: usize = SearchQuery::DEFAULT_MAX_HITS;

/// The hits of one page, in reading order.
#[derive(Debug, Clone, PartialEq)]
struct PageHits {
    page: PageIndex,
    /// Index of its first hit in the whole list.
    first: usize,
    hits: Vec<SearchHit>,
}

/// Where hits went in: `count` hits from index `at` on; the hits that were
/// at `at` and after it moved down by `count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Inserted {
    pub at: usize,
    pub count: usize,
}

/// Hits in document order with the active one.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct HitList {
    /// Sorted by page, no page twice, none empty.
    pages: Vec<PageHits>,
    len: usize,
    active: Option<usize>,
}

impl HitList {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub(crate) fn active(&self) -> Option<&SearchHit> {
        self.get(self.active?)
    }

    /// Hit `index` in document order.
    pub(crate) fn get(&self, index: usize) -> Option<&SearchHit> {
        let bucket = self
            .pages
            .partition_point(|p| p.first <= index)
            .checked_sub(1)?;
        let page = self.pages.get(bucket)?;
        page.hits.get(index - page.first)
    }

    /// Adds hits, keeping document order and the active hit. Hits past
    /// [`MAX_HITS`] are dropped. Returns where they went (`None` when none
    /// did): the search reports one page at a time, so they went in one
    /// place; hits of several pages report the first place and their total.
    pub(crate) fn insert(&mut self, mut found: Vec<SearchHit>) -> Option<Inserted> {
        found.sort_by_key(|h| (h.page, h.start));
        found.truncate(MAX_HITS.saturating_sub(self.len));
        let mut inserted: Option<Inserted> = None;
        while let Some(page) = found.first().map(|h| h.page) {
            let rest = found.split_off(found.partition_point(|h| h.page == page));
            let this = self.insert_page(page, found);
            inserted = Some(match inserted {
                Some(before) => Inserted {
                    at: before.at.min(this.at),
                    count: before.count + this.count,
                },
                None => this,
            });
            found = rest;
        }
        inserted
    }

    /// Adds `found`, hits of `page` in reading order.
    fn insert_page(&mut self, page: PageIndex, mut found: Vec<SearchHit>) -> Inserted {
        let bucket = self.pages.partition_point(|p| p.page < page);
        let count = found.len();
        // The active hit after the insert, when it is on this page.
        let mut active_here = None;
        let at = match self.pages.get_mut(bucket) {
            Some(existing) if existing.page == page => {
                // The page was reported before (the search does not): merge
                // in order and report the page's place.
                let first = existing.first;
                let active = self
                    .active
                    .and_then(|a| a.checked_sub(first))
                    .and_then(|i| existing.hits.get(i))
                    .map(|h| (h.start, h.end));
                existing.hits.append(&mut found);
                existing.hits.sort_by_key(|h| h.start);
                active_here = active.and_then(|key| {
                    let i = existing.hits.iter().position(|h| (h.start, h.end) == key)?;
                    Some(first + i)
                });
                first
            }
            _ => {
                let at = self.pages.get(bucket).map_or(self.len, |next| next.first);
                self.pages.insert(
                    bucket,
                    PageHits {
                        page,
                        first: at,
                        hits: found,
                    },
                );
                at
            }
        };
        for later in self.pages.iter_mut().skip(bucket + 1) {
            later.first += count;
        }
        self.len += count;
        match (active_here, self.active.as_mut()) {
            (Some(index), Some(active)) => *active = index,
            (None, Some(active)) if *active >= at => *active += count,
            _ => {}
        }
        Inserted { at, count }
    }

    /// Activates the first hit at or after `page` (wrapping to the first),
    /// unless one is active already. Returns true when it changed.
    pub(crate) fn activate_from(&mut self, page: PageIndex) -> bool {
        if self.active.is_some() || self.len == 0 {
            return false;
        }
        let bucket = self.pages.partition_point(|p| p.page < page);
        self.active = Some(self.pages.get(bucket).map_or(0, |p| p.first));
        true
    }

    /// Moves to the next (or previous) hit, wrapping around.
    pub(crate) fn step(&mut self, forward: bool) -> Option<&SearchHit> {
        let len = self.len;
        if len == 0 {
            return None;
        }
        let next = match (self.active, forward) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(i), true) => (i + 1) % len,
            (Some(i), false) => (i + len - 1) % len,
        };
        self.active = Some(next);
        self.get(next)
    }

    /// Where the hit at byte `start` of `page`'s search text is now (hits
    /// move down as earlier pages come in).
    pub(crate) fn index_of(&self, page: PageIndex, start: usize) -> Option<usize> {
        let bucket = self
            .pages
            .binary_search_by_key(&page, |p| p.page)
            .ok()
            .and_then(|i| self.pages.get(i))?;
        let i = bucket.hits.binary_search_by_key(&start, |h| h.start).ok()?;
        Some(bucket.first + i)
    }

    /// Makes hit `index` (clamped to the list) the active one.
    pub(crate) fn select(&mut self, index: usize) -> Option<&SearchHit> {
        let index = index.min(self.len.checked_sub(1)?);
        self.active = Some(index);
        self.get(index)
    }

    /// Highlight rectangles on `page`; the bool marks the active hit.
    pub(crate) fn on_page(&self, page: PageIndex) -> impl Iterator<Item = (PageRect, bool)> + '_ {
        let bucket = self
            .pages
            .binary_search_by_key(&page, |p| p.page)
            .ok()
            .and_then(|i| self.pages.get(i));
        let active = self.active;
        bucket.into_iter().flat_map(move |p| {
            p.hits.iter().enumerate().flat_map(move |(i, h)| {
                let is_active = active == Some(p.first + i);
                h.rects.iter().map(move |r| (*r, is_active))
            })
        })
    }

    /// Bytes the hits keep on the heap (logged when a search ends).
    pub(crate) fn heap_bytes(&self) -> usize {
        let hit = std::mem::size_of::<SearchHit>();
        let rect = std::mem::size_of::<PageRect>();
        self.pages.capacity() * std::mem::size_of::<PageHits>()
            + self
                .pages
                .iter()
                .map(|p| {
                    p.hits.capacity() * hit
                        + p.hits
                            .iter()
                            .map(|h| h.rects.capacity() * rect + h.context.heap_bytes())
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(page: u32, start: usize) -> SearchHit {
        SearchHit {
            page: PageIndex::new(page),
            start,
            end: start + 2,
            rects: vec![PageRect::new(start as f32, 0.0, start as f32 + 10.0, 10.0)],
            context: Default::default(),
        }
    }

    fn order(list: &HitList) -> Vec<(u32, usize)> {
        (0..list.len())
            .filter_map(|i| list.get(i).map(|h| (h.page.get(), h.start)))
            .collect()
    }

    #[test]
    fn hits_stay_in_document_order_and_keep_the_active_one() {
        let mut list = HitList::default();
        // The search starts at page 5 and wraps around.
        list.insert(vec![hit(5, 30), hit(5, 10)]);
        assert!(list.activate_from(PageIndex::new(5)));
        assert_eq!(
            list.active().map(|h| (h.page.get(), h.start)),
            Some((5, 10))
        );
        list.insert(vec![hit(7, 0)]);
        list.insert(vec![hit(1, 4)]);
        list.insert(vec![hit(2, 0)]);
        // Earlier hits shift the index, not the active hit.
        assert_eq!(
            list.active().map(|h| (h.page.get(), h.start)),
            Some((5, 10))
        );
        assert!(!list.activate_from(PageIndex::FIRST), "already active");
        let order: Vec<_> = (0..list.len())
            .map(|_| list.step(true).map(|h| (h.page.get(), h.start)))
            .collect();
        assert_eq!(
            order,
            vec![
                Some((5, 30)),
                Some((7, 0)),
                Some((1, 4)),
                Some((2, 0)),
                Some((5, 10))
            ]
        );
        assert_eq!(list.step(false).map(|h| h.page.get()), Some(2));
    }

    #[test]
    fn activation_wraps_when_no_hit_follows_the_page() {
        let mut list = HitList::default();
        list.insert(vec![hit(0, 0)]);
        list.insert(vec![hit(1, 0)]);
        assert!(list.activate_from(PageIndex::new(9)));
        assert_eq!(list.active_index(), Some(0));
        let mut empty = HitList::default();
        assert!(!empty.activate_from(PageIndex::FIRST));
        assert!(empty.step(true).is_none());
        assert_eq!(empty.insert(Vec::new()), None);
    }

    #[test]
    fn page_rects_mark_the_active_hit() {
        let mut list = HitList::default();
        list.insert(vec![hit(3, 0), hit(3, 20)]);
        list.insert(vec![hit(4, 0)]);
        list.step(true);
        list.step(true); // second hit on page 3
        let rects: Vec<_> = list.on_page(PageIndex::new(3)).collect();
        assert_eq!(rects.len(), 2);
        assert!(!rects[0].1 && rects[1].1);
        assert_eq!(list.on_page(PageIndex::new(9)).count(), 0);
    }

    #[test]
    fn hits_that_wrap_highlight_every_line() {
        // A word broken over two lines has a rectangle on each.
        let wrapped = SearchHit {
            page: PageIndex::new(3),
            start: 40,
            end: 46,
            rects: vec![
                PageRect::new(500.0, 100.0, 520.0, 114.0),
                PageRect::new(72.0, 120.0, 86.0, 134.0),
            ],
            context: Default::default(),
        };
        let mut list = HitList::default();
        list.insert(vec![hit(3, 60), wrapped.clone(), hit(3, 0)]);
        assert!(list.activate_from(PageIndex::new(3)));
        list.step(true);
        assert_eq!(list.active(), Some(&wrapped));
        // Both of its rectangles are drawn as the active hit, in order.
        let rects: Vec<_> = list.on_page(PageIndex::new(3)).collect();
        assert_eq!(
            rects,
            vec![
                (PageRect::new(0.0, 0.0, 10.0, 10.0), false),
                (wrapped.rects[0], true),
                (wrapped.rects[1], true),
                (PageRect::new(60.0, 0.0, 70.0, 10.0), false),
            ]
        );
    }

    #[test]
    fn buckets_keep_document_order_and_indices() {
        let mut list = HitList::default();
        for page in [10, 11, 9, 12, 8, 13, 0] {
            list.insert(vec![hit(page, 5), hit(page, 1)]);
        }
        let expected: Vec<(u32, usize)> = [0, 8, 9, 10, 11, 12, 13]
            .into_iter()
            .flat_map(|p| [(p, 1), (p, 5)])
            .collect();
        assert_eq!(order(&list), expected);
        assert_eq!(list.len(), 14);
        assert!(list.get(14).is_none());
        // A page reported twice is merged in order, the active hit kept.
        for _ in 0..6 {
            list.step(true);
        }
        assert_eq!(list.active().map(|h| (h.page.get(), h.start)), Some((9, 5)));
        list.insert(vec![hit(9, 3)]);
        assert_eq!(list.active().map(|h| (h.page.get(), h.start)), Some((9, 5)));
        assert_eq!(list.active_index(), Some(6));
        assert_eq!(
            order(&list)[4..7],
            [(9, 1), (9, 3), (9, 5)],
            "{:?}",
            order(&list)
        );
    }

    #[test]
    fn insert_reports_position() {
        let mut list = HitList::default();
        assert_eq!(
            list.insert(vec![hit(5, 0), hit(5, 9)]),
            Some(Inserted { at: 0, count: 2 })
        );
        // After: appended.
        assert_eq!(
            list.insert(vec![hit(6, 0)]),
            Some(Inserted { at: 2, count: 1 })
        );
        // Before: at the top, everything else moves down.
        list.step(true);
        list.step(true);
        assert_eq!(
            list.insert(vec![hit(4, 0), hit(4, 2), hit(4, 4)]),
            Some(Inserted { at: 0, count: 3 })
        );
        assert_eq!(list.active().map(|h| (h.page.get(), h.start)), Some((5, 9)));
        // Between two pages.
        list.insert(vec![hit(8, 0)]);
        assert_eq!(
            list.insert(vec![hit(7, 1)]),
            Some(Inserted { at: 6, count: 1 })
        );
    }

    #[test]
    fn cap_truncates_at_max() {
        let mut list = HitList::default();
        let many: Vec<SearchHit> = (0..MAX_HITS - 1).map(|i| hit(2, i)).collect();
        list.insert(many);
        assert_eq!(
            list.insert(vec![hit(3, 0), hit(3, 5), hit(3, 9)]),
            Some(Inserted {
                at: MAX_HITS - 1,
                count: 1
            })
        );
        assert_eq!(list.len(), MAX_HITS);
        assert_eq!(list.insert(vec![hit(1, 0)]), None);
        assert_eq!(list.len(), MAX_HITS);
    }

    #[test]
    fn hits_are_found_again_after_earlier_pages_came_in() {
        let mut list = HitList::default();
        list.insert(vec![hit(20, 4), hit(20, 9)]);
        assert_eq!(list.index_of(PageIndex::new(20), 9), Some(1));
        list.insert(vec![hit(19, 0), hit(19, 5), hit(19, 7)]);
        list.insert(vec![hit(21, 3)]);
        assert_eq!(list.index_of(PageIndex::new(20), 9), Some(4));
        for i in 0..list.len() {
            let h = list.get(i).map(|h| (h.page, h.start));
            let found = h.and_then(|(page, start)| list.index_of(page, start));
            assert_eq!(found, Some(i));
        }
        assert_eq!(list.index_of(PageIndex::new(20), 5), None);
        assert_eq!(list.index_of(PageIndex::new(3), 0), None);
        assert_eq!(HitList::default().index_of(PageIndex::FIRST, 0), None);
    }

    #[test]
    fn select_clamps() {
        let mut list = HitList::default();
        assert!(list.select(3).is_none());
        list.insert(vec![hit(1, 0), hit(1, 4)]);
        assert_eq!(list.select(9).map(|h| h.start), Some(4));
        assert_eq!(list.active_index(), Some(1));
        assert_eq!(list.select(0).map(|h| h.start), Some(0));
    }

    #[test]
    fn heap_bytes_grow_with_hits_and_snippets() {
        let mut list = HitList::default();
        let empty = list.heap_bytes();
        list.insert(vec![hit(1, 0)]);
        let one = list.heap_bytes();
        assert!(one > empty);
        let mut long = hit(2, 0);
        long.context.text = "x".repeat(200).into();
        list.insert(vec![long]);
        assert!(list.heap_bytes() >= one + 200);
    }
}
