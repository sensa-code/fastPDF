//! Text selection on the document canvas (spec §8: Text Selection, Copy).
//!
//! The UI only tracks the drag (anchor and focus as page-space points from
//! `DocumentSession::view_to_page`) and the text layers it needs; the
//! selection itself comes from `fastpdf_core::selection`. Text layers are
//! fetched once per page in the background through the shared
//! [`fastpdf_search::TextCache`] and kept here while the document is open,
//! so dragging never re-extracts text.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fastpdf_core::selection::{
    PageSelection, hit_test, select_all, select_from_start, select_range, select_to_end,
    selected_text_pages,
};
use fastpdf_engine_api::{PageIndex, PageRect, TextLayer};

/// Pages a single drag may span; more would mean extracting text for
/// pages the user never sees.
pub(crate) const MAX_SELECTION_PAGES: u32 = 100;
/// Text layers kept for the open document.
const MAX_LAYERS: usize = 64;

/// A point in page space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PagePoint {
    pub page: PageIndex,
    pub x: f32,
    pub y: f32,
}

/// Selection state of the open document.
#[derive(Debug, Default)]
pub(crate) struct TextSelection {
    pub anchor: Option<PagePoint>,
    pub focus: Option<PagePoint>,
    pub dragging: bool,
    /// Select the whole page once its text layer arrives (Ctrl+A).
    pending_all: Option<PageIndex>,
    layers: HashMap<PageIndex, Arc<TextLayer>>,
    loading: HashSet<PageIndex>,
    /// Pages whose text could not be extracted.
    failed: HashSet<PageIndex>,
    pub selection: Vec<PageSelection>,
}

impl TextSelection {
    /// Forgets everything (document closed or replaced).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
        self.dragging = false;
        self.pending_all = None;
        self.selection.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.selection.iter().all(|s| s.rects.is_empty())
    }

    /// Starts a drag at `point`. Returns pages whose text must be fetched.
    pub(crate) fn begin(&mut self, point: PagePoint) -> Vec<PageIndex> {
        self.clear();
        self.anchor = Some(point);
        self.focus = Some(point);
        self.dragging = true;
        self.missing_layers()
    }

    /// Moves the drag's end. Returns pages whose text must be fetched.
    pub(crate) fn extend(&mut self, point: PagePoint) -> Vec<PageIndex> {
        if !self.dragging {
            return Vec::new();
        }
        self.focus = Some(point);
        let missing = self.missing_layers();
        self.recompute();
        missing
    }

    pub(crate) fn end(&mut self) {
        self.dragging = false;
    }

    /// Selects all text of `page`. Returns pages whose text must be fetched.
    pub(crate) fn select_page(&mut self, page: PageIndex) -> Vec<PageIndex> {
        self.clear();
        if let Some(layer) = self.layers.get(&page) {
            self.selection = vec![select_all(layer)];
            return Vec::new();
        }
        self.pending_all = Some(page);
        self.request(page).into_iter().collect()
    }

    /// Marks `page` as being fetched; `None` when it is known or loading.
    fn request(&mut self, page: PageIndex) -> Option<PageIndex> {
        if self.layers.contains_key(&page)
            || self.failed.contains(&page)
            || !self.loading.insert(page)
        {
            return None;
        }
        Some(page)
    }

    /// A text layer arrived (or failed: `None`).
    pub(crate) fn layer_loaded(&mut self, page: PageIndex, layer: Option<Arc<TextLayer>>) {
        self.loading.remove(&page);
        match layer {
            Some(layer) => {
                if self.layers.len() >= MAX_LAYERS {
                    self.evict_far_from(page);
                }
                self.layers.insert(page, layer);
            }
            None => {
                self.failed.insert(page);
            }
        }
        if self.pending_all == Some(page) {
            self.pending_all = None;
            if let Some(layer) = self.layers.get(&page) {
                self.selection = vec![select_all(layer)];
            }
        } else {
            self.recompute();
        }
    }

    /// Keeps the cache bounded: drops the layers farthest from `page` that
    /// the current selection does not use.
    fn evict_far_from(&mut self, page: PageIndex) {
        let in_use: HashSet<PageIndex> = self.selection.iter().map(|s| s.page).collect();
        let mut pages: Vec<PageIndex> = self
            .layers
            .keys()
            .copied()
            .filter(|p| !in_use.contains(p))
            .collect();
        pages.sort_by_key(|p| std::cmp::Reverse(p.get().abs_diff(page.get())));
        for p in pages.into_iter().take(self.layers.len() + 1 - MAX_LAYERS) {
            self.layers.remove(&p);
        }
    }

    /// Pages between anchor and focus whose layers are neither cached nor
    /// loading; marks them loading.
    fn missing_layers(&mut self) -> Vec<PageIndex> {
        let (Some(a), Some(f)) = (self.anchor, self.focus) else {
            return Vec::new();
        };
        let (lo, hi) = order(a.page, f.page);
        let hi = hi
            .get()
            .min(lo.get().saturating_add(MAX_SELECTION_PAGES - 1));
        (lo.get()..=hi)
            .map(PageIndex::new)
            .filter_map(|p| self.request(p))
            .collect()
    }

    fn recompute(&mut self) {
        if let (Some(a), Some(f)) = (self.anchor, self.focus) {
            self.selection = compute(a, f, &self.layers);
        }
    }

    /// Highlight rectangles on `page`.
    pub(crate) fn rects_on(&self, page: PageIndex) -> impl Iterator<Item = PageRect> + '_ {
        self.selection
            .iter()
            .filter(move |s| s.page == page)
            .flat_map(|s| s.rects.iter().copied())
    }

    /// Text for the clipboard.
    pub(crate) fn text(&self) -> String {
        let pairs: Vec<(&TextLayer, &PageSelection)> = self
            .selection
            .iter()
            .filter_map(|s| Some((self.layers.get(&s.page)?.as_ref(), s)))
            .collect();
        selected_text_pages(pairs)
    }
}

fn order(a: PageIndex, b: PageIndex) -> (PageIndex, PageIndex) {
    if a <= b { (a, b) } else { (b, a) }
}

/// The selection between two page-space points, in document order. Pages
/// whose layers are missing are skipped (they fill in when they arrive).
fn compute(
    anchor: PagePoint,
    focus: PagePoint,
    layers: &HashMap<PageIndex, Arc<TextLayer>>,
) -> Vec<PageSelection> {
    let (first, last) = if (anchor.page, anchor.y, anchor.x) <= (focus.page, focus.y, focus.x) {
        (anchor, focus)
    } else {
        (focus, anchor)
    };
    if first.page == last.page {
        let Some(layer) = layers.get(&first.page) else {
            return Vec::new();
        };
        let (Some(a), Some(b)) = (
            hit_test(layer, anchor.x, anchor.y),
            hit_test(layer, focus.x, focus.y),
        ) else {
            return Vec::new();
        };
        return vec![select_range(layer, a, b)];
    }
    let end = last
        .page
        .get()
        .min(first.page.get().saturating_add(MAX_SELECTION_PAGES - 1));
    let mut out = Vec::new();
    for page in (first.page.get()..=end).map(PageIndex::new) {
        let Some(layer) = layers.get(&page) else {
            continue;
        };
        let selection = if page == first.page {
            hit_test(layer, first.x, first.y).map(|p| select_to_end(layer, p))
        } else if page == last.page {
            hit_test(layer, last.x, last.y).map(|p| select_from_start(layer, p))
        } else {
            Some(select_all(layer))
        };
        out.extend(selection);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::TextSpan;

    /// A page with two lines: "Hello world" at y 10..20, "第二行" at y 30..40.
    fn layer(page: u32) -> Arc<TextLayer> {
        let span = |text: &str, x: f32, y: f32| {
            let n = text.chars().count();
            TextSpan {
                text: text.into(),
                bounds: PageRect::new(x, y, x + 10.0 * n as f32, y + 10.0),
                char_bounds: (0..n)
                    .map(|i| {
                        let x0 = x + 10.0 * i as f32;
                        PageRect::new(x0, y, x0 + 10.0, y + 10.0)
                    })
                    .collect(),
            }
        };
        Arc::new(TextLayer {
            page: PageIndex::new(page),
            spans: vec![span("Hello world", 0.0, 10.0), span("第二行", 0.0, 30.0)],
        })
    }

    fn pt(page: u32, x: f32, y: f32) -> PagePoint {
        PagePoint {
            page: PageIndex::new(page),
            x,
            y,
        }
    }

    #[test]
    fn drag_on_one_page_selects_and_copies_once_the_layer_arrives() {
        let mut s = TextSelection::default();
        assert_eq!(s.begin(pt(0, 1.0, 15.0)), vec![PageIndex::FIRST]);
        assert!(s.extend(pt(0, 15.0, 35.0)).is_empty(), "already loading");
        assert!(s.is_empty(), "nothing until the text arrives");
        s.layer_loaded(PageIndex::FIRST, Some(layer(0)));
        assert!(!s.is_empty());
        assert_eq!(s.text(), "Hello world\n第二");
        // Dragging backwards selects the same text.
        s.begin(pt(0, 15.0, 35.0));
        s.extend(pt(0, 1.0, 15.0));
        assert_eq!(s.text(), "Hello world\n第二");
    }

    #[test]
    fn drag_across_pages_selects_tail_middle_and_head() {
        let mut s = TextSelection::default();
        let pages = s.begin(pt(2, 61.0, 15.0));
        assert_eq!(pages, vec![PageIndex::new(2)]);
        let more = s.extend(pt(4, 9.0, 35.0));
        assert_eq!(more, vec![PageIndex::new(3), PageIndex::new(4)]);
        for p in 2..=4 {
            s.layer_loaded(PageIndex::new(p), Some(layer(p)));
        }
        let pages: Vec<u32> = s.selection.iter().map(|x| x.page.get()).collect();
        assert_eq!(pages, vec![2, 3, 4]);
        assert_eq!(
            s.text(),
            "world\n第二行\nHello world\n第二行\nHello world\n第"
        );
        assert_eq!(s.rects_on(PageIndex::new(3)).count(), 2);
    }

    #[test]
    fn select_page_waits_for_its_layer_and_failures_are_remembered() {
        let mut s = TextSelection::default();
        assert_eq!(s.select_page(PageIndex::new(1)), vec![PageIndex::new(1)]);
        s.layer_loaded(PageIndex::new(1), Some(layer(1)));
        assert_eq!(s.text(), "Hello world\n第二行");
        // A page without extractable text is not asked for again.
        assert_eq!(s.begin(pt(5, 0.0, 0.0)), vec![PageIndex::new(5)]);
        s.layer_loaded(PageIndex::new(5), None);
        assert!(s.begin(pt(5, 0.0, 0.0)).is_empty());
        assert!(s.is_empty());
        s.clear();
        assert!(s.text().is_empty());
    }

    #[test]
    fn the_layer_cache_is_bounded_and_keeps_selected_pages() {
        let mut s = TextSelection::default();
        s.select_page(PageIndex::new(500));
        s.layer_loaded(PageIndex::new(500), Some(layer(500)));
        for p in 0..(MAX_LAYERS as u32 + 10) {
            s.request(PageIndex::new(p));
            s.layer_loaded(PageIndex::new(p), Some(layer(p)));
        }
        assert!(s.layers.len() <= MAX_LAYERS);
        assert!(s.layers.contains_key(&PageIndex::new(500)), "in use");
    }
}
