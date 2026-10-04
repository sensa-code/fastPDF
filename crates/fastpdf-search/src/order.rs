use fastpdf_engine_api::PageIndex;

/// Pages in search order: `start`, then alternately after and before it,
/// moving outward until every page has been visited once.
pub fn search_order(start: PageIndex, page_count: u32) -> impl Iterator<Item = PageIndex> {
    let start = i64::from(start.get().min(page_count.saturating_sub(1)));
    let count = i64::from(page_count);
    (0..count.max(0) * 2)
        .map(move |step| {
            // 0, +1, -1, +2, -2, ...
            let offset = (step + 1) / 2;
            if step % 2 == 1 {
                start + offset
            } else {
                start - offset
            }
        })
        .filter(move |&p| (0..count).contains(&p))
        .take(page_count as usize)
        .map(|p| PageIndex::new(p as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(start: u32, count: u32) -> Vec<u32> {
        search_order(PageIndex::new(start), count)
            .map(PageIndex::get)
            .collect()
    }

    #[test]
    fn expands_outward_from_the_current_page() {
        assert_eq!(order(2, 6), vec![2, 3, 1, 4, 0, 5]);
        assert_eq!(order(0, 4), vec![0, 1, 2, 3]);
        assert_eq!(order(3, 4), vec![3, 2, 1, 0]);
        assert_eq!(order(9, 3), vec![2, 1, 0]); // start clamped
        assert!(order(0, 0).is_empty());
    }

    #[test]
    fn visits_every_page_exactly_once() {
        let mut pages = order(777, 2000);
        assert_eq!(pages.len(), 2000);
        pages.sort_unstable();
        pages.dedup();
        assert_eq!(pages.len(), 2000);
    }
}
