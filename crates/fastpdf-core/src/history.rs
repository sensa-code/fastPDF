//! Back and Forward between views: a bounded, per-document history of the
//! places the user jumped away from.
//!
//! Only jumps are recorded (go to page, first/last page, outline items,
//! thumbnails, links, search hits); scrolling, paging, zooming, rotating and
//! layout changes are not. Like a browser, Back remembers where the user
//! actually is (including scrolling done after a jump) so Forward returns
//! there, and a new jump clears Forward. The history lives in the document
//! session and is dropped with it; nothing is persisted.

use std::collections::VecDeque;

use fastpdf_render::DocumentLayout;

use crate::view_state::ViewState;

/// Entries kept per direction (spec §15: every store has a budget).
pub const HISTORY_CAPACITY: usize = 100;

/// Why the view jumped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// Go to page, first/last page, outline item, thumbnail, link.
    Direct,
    /// Showing a search hit: a run of hits shares one entry, so Back
    /// returns to where the search started.
    SearchHit,
}

/// The views Back and Forward return to.
#[derive(Debug, Clone)]
pub struct NavigationHistory {
    /// Oldest first; the next Back target is last.
    back: VecDeque<ViewState>,
    /// The next Forward target is last.
    forward: VecDeque<ViewState>,
    capacity: usize,
    /// Where the last search-hit jump landed; `None` after any other jump,
    /// Back or Forward.
    search_landing: Option<ViewState>,
}

impl NavigationHistory {
    /// An empty history keeping at most `capacity` entries per direction
    /// (at least one). Allocates nothing until the first jump.
    pub fn new(capacity: usize) -> Self {
        Self {
            back: VecDeque::new(),
            forward: VecDeque::new(),
            capacity: capacity.max(1),
            search_landing: None,
        }
    }

    /// Records a jump from `from` to `to`; true when Back can now return to
    /// `from`.
    ///
    /// * A jump that did not move the view records nothing and keeps
    ///   Forward.
    /// * A search hit reached from where the previous search hit landed
    ///   extends that run instead of adding an entry.
    /// * Otherwise `from` becomes the Back target (replacing an entry at
    ///   the same place) and Forward is cleared.
    pub fn record(
        &mut self,
        from: ViewState,
        to: ViewState,
        jump: Jump,
        layout: &DocumentLayout,
    ) -> bool {
        if from.same_place(&to, layout) {
            return false;
        }
        if jump == Jump::SearchHit
            && self
                .search_landing
                .is_some_and(|landing| landing.same_place(&from, layout))
        {
            self.search_landing = Some(to);
            return false;
        }
        if self
            .back
            .back()
            .is_some_and(|last| last.same_place(&from, layout))
        {
            self.back.pop_back();
        }
        push_capped(&mut self.back, from, self.capacity);
        self.forward.clear();
        self.search_landing = (jump == Jump::SearchHit).then_some(to);
        true
    }

    /// The view to go back to from `current`, which becomes the Forward
    /// target. Entries that would show `current` again are skipped, so Back
    /// always changes something.
    pub fn back(&mut self, current: ViewState, layout: &DocumentLayout) -> Option<ViewState> {
        let target = pop_other(&mut self.back, &current, layout)?;
        push_capped(&mut self.forward, current, self.capacity);
        self.search_landing = None;
        Some(target)
    }

    /// The view to go forward to from `current`, which becomes the Back
    /// target again.
    pub fn forward(&mut self, current: ViewState, layout: &DocumentLayout) -> Option<ViewState> {
        let target = pop_other(&mut self.forward, &current, layout)?;
        push_capped(&mut self.back, current, self.capacity);
        self.search_landing = None;
        Some(target)
    }

    /// Whether [`NavigationHistory::back`] from `current` would find a
    /// view to show (peeks; entries that would show `current` again do not
    /// count).
    pub fn can_go_back(&self, current: &ViewState, layout: &DocumentLayout) -> bool {
        has_target(&self.back, current, layout)
    }

    /// Whether [`NavigationHistory::forward`] from `current` would find a
    /// view to show.
    pub fn can_go_forward(&self, current: &ViewState, layout: &DocumentLayout) -> bool {
        has_target(&self.forward, current, layout)
    }

    /// Entries (back, forward), for dev-script checks.
    pub fn depth(&self) -> (usize, usize) {
        (self.back.len(), self.forward.len())
    }
}

fn push_capped(stack: &mut VecDeque<ViewState>, view: ViewState, capacity: usize) {
    stack.push_back(view);
    while stack.len() > capacity {
        stack.pop_front();
    }
}

/// Whether `stack` holds an entry that would change the view.
fn has_target(stack: &VecDeque<ViewState>, current: &ViewState, layout: &DocumentLayout) -> bool {
    stack.iter().any(|view| !view.same_view(current, layout))
}

/// Pops the newest entry that would change the view, dropping the ones
/// that would not.
fn pop_other(
    stack: &mut VecDeque<ViewState>,
    current: &ViewState,
    layout: &DocumentLayout,
) -> Option<ViewState> {
    while let Some(view) = stack.pop_back() {
        if !view.same_view(current, layout) {
            return Some(view);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ZoomMode;
    use fastpdf_engine_api::{PageIndex, PageSize, Rotation};
    use fastpdf_render::{ScrollAnchor, ZoomLevel};

    fn layout() -> DocumentLayout {
        DocumentLayout::new(200, PageSize::LETTER, 8.0)
    }

    /// The view `fraction` into page `page`'s row.
    fn at(page: u32, fraction: f64) -> ViewState {
        ViewState {
            anchor: ScrollAnchor {
                page: PageIndex::new(page),
                fraction,
            },
            page: PageIndex::new(page),
            scroll_x: 0.0,
            zoom: ZoomLevel::ACTUAL_SIZE,
            zoom_mode: ZoomMode::FitWidth,
            layout: layout().mode(),
            rotation: Rotation::R0,
        }
    }

    fn history() -> NavigationHistory {
        NavigationHistory::new(HISTORY_CAPACITY)
    }

    #[test]
    fn jumps_record_where_they_left_and_clear_forward() {
        let l = layout();
        let mut h = history();
        let here = at(30, 0.0);
        assert!(!h.can_go_back(&here, &l) && !h.can_go_forward(&here, &l));
        assert!(h.record(at(0, 0.0), here, Jump::Direct, &l));
        assert_eq!(h.depth(), (1, 0));
        assert!(h.can_go_back(&here, &l));
        assert_eq!(h.back(here, &l), Some(at(0, 0.0)));
        assert!(h.can_go_forward(&at(0, 0.0), &l));
        assert!(h.record(at(0, 0.0), at(50, 0.0), Jump::Direct, &l));
        assert_eq!(h.depth(), (1, 0), "a new jump clears Forward");
    }

    #[test]
    fn back_and_forward_walk_both_stacks() {
        let l = layout();
        let mut h = history();
        let (a, b, c, d) = (at(1, 0.0), at(20, 0.5), at(40, 0.0), at(60, 0.0));
        h.record(a, b, Jump::Direct, &l);
        h.record(b, c, Jump::Direct, &l);
        assert_eq!(h.back(c, &l), Some(b));
        assert_eq!(h.back(b, &l), Some(a));
        assert_eq!(h.back(a, &l), None);
        assert_eq!(h.depth(), (0, 2));
        assert_eq!(h.forward(a, &l), Some(b));
        assert_eq!(h.depth(), (1, 1));
        h.record(b, d, Jump::Direct, &l);
        assert_eq!(h.depth(), (2, 0));
        assert_eq!(h.forward(d, &l), None);
        assert_eq!(h.back(d, &l), Some(b));
    }

    #[test]
    fn back_remembers_scrolling_after_the_jump() {
        let l = layout();
        let mut h = history();
        h.record(at(1, 0.0), at(20, 0.0), Jump::Direct, &l);
        // The user scrolled on before going back: Forward returns there.
        let scrolled = at(21, 0.4);
        assert_eq!(h.back(scrolled, &l), Some(at(1, 0.0)));
        assert_eq!(h.forward(at(1, 0.0), &l), Some(scrolled));
    }

    #[test]
    fn jumps_that_do_not_move_are_not_recorded() {
        let l = layout();
        let mut h = history();
        h.record(at(1, 0.0), at(20, 0.0), Jump::Direct, &l);
        h.back(at(20, 0.0), &l);
        assert!(!h.record(at(1, 0.0), at(1, 0.001), Jump::Direct, &l));
        assert_eq!(h.depth(), (0, 1), "Forward is kept");
        assert!(!h.record(at(5, 0.0), at(5, 0.0), Jump::SearchHit, &l));
        assert_eq!(h.depth(), (0, 1));
    }

    #[test]
    fn search_hits_share_one_entry_until_the_user_moves_away() {
        let l = layout();
        let mut h = history();
        let start = at(3, 0.2);
        assert!(h.record(start, at(20, 0.3), Jump::SearchHit, &l));
        assert!(!h.record(at(20, 0.3), at(25, 0.3), Jump::SearchHit, &l));
        assert!(!h.record(at(25, 0.3), at(31, 0.3), Jump::SearchHit, &l));
        assert_eq!(h.depth(), (1, 0));
        // Scrolled away from the last hit: the next hit is a new entry.
        assert!(h.record(at(33, 0.0), at(40, 0.3), Jump::SearchHit, &l));
        assert_eq!(h.depth(), (2, 0));
        assert_eq!(h.back(at(40, 0.3), &l), Some(at(33, 0.0)));
        assert_eq!(h.back(at(33, 0.0), &l), Some(start));
        // A direct jump ends the run.
        let mut h = history();
        h.record(start, at(20, 0.3), Jump::SearchHit, &l);
        h.record(at(20, 0.3), at(90, 0.0), Jump::Direct, &l);
        assert!(h.record(at(90, 0.0), at(25, 0.3), Jump::SearchHit, &l));
        assert_eq!(h.depth(), (3, 0));
    }

    #[test]
    fn duplicate_origins_are_not_stacked() {
        let l = layout();
        let mut h = history();
        h.record(at(1, 0.0), at(20, 0.0), Jump::Direct, &l);
        // Scrolled back to the origin by hand, then jumped again.
        let zoomed = ViewState {
            zoom_mode: ZoomMode::Custom,
            zoom: ZoomLevel::new(2.0),
            ..at(1, 0.0)
        };
        h.record(zoomed, at(40, 0.0), Jump::Direct, &l);
        assert_eq!(h.depth(), (1, 0));
        assert_eq!(
            h.back(at(40, 0.0), &l),
            Some(zoomed),
            "the newest view wins"
        );
    }

    #[test]
    fn back_skips_entries_at_the_current_place() {
        let l = layout();
        let mut h = history();
        h.record(at(1, 0.0), at(20, 0.0), Jump::Direct, &l);
        h.record(at(20, 0.0), at(40, 0.0), Jump::Direct, &l);
        // Scrolled by hand from 40 back to 20: Back goes on to 1.
        assert_eq!(h.back(at(20, 0.0), &l), Some(at(1, 0.0)));
        assert_eq!(h.depth(), (0, 1));
        // Nothing but the current place left: Back does nothing.
        let mut h = history();
        h.record(at(1, 0.0), at(20, 0.0), Jump::Direct, &l);
        assert_eq!(h.back(at(1, 0.0), &l), None);
        assert_eq!(h.depth(), (0, 0));
        // The buttons agree: nothing but the current place is no target.
        let mut h = history();
        h.record(at(1, 0.0), at(20, 0.0), Jump::Direct, &l);
        assert!(!h.can_go_back(&at(1, 0.0), &l));
        assert!(h.can_go_back(&at(1, 0.3), &l));
        h.record(at(20, 0.0), at(40, 0.0), Jump::Direct, &l);
        assert!(h.can_go_back(&at(20, 0.0), &l), "an older entry differs");
        // Same place at another zoom still changes the view.
        let mut h = history();
        h.record(at(1, 0.0), at(20, 0.0), Jump::Direct, &l);
        let zoomed = ViewState {
            zoom_mode: ZoomMode::Custom,
            zoom: ZoomLevel::new(2.0),
            ..at(1, 0.0)
        };
        assert_eq!(h.back(zoomed, &l), Some(at(1, 0.0)));
    }

    #[test]
    fn capacity_drops_the_oldest() {
        let l = layout();
        let mut h = history();
        for page in 0..150 {
            assert!(h.record(at(page, 0.0), at(page + 1, 0.0), Jump::Direct, &l));
        }
        assert_eq!(h.depth(), (HISTORY_CAPACITY, 0));
        let mut oldest = None;
        let mut current = at(150, 0.0);
        while let Some(view) = h.back(current, &l) {
            oldest = Some(view);
            current = view;
        }
        assert_eq!(oldest, Some(at(50, 0.0)), "pages 0..50 were dropped");
        assert_eq!(h.depth(), (0, HISTORY_CAPACITY));
        let mut tiny = NavigationHistory::new(0);
        tiny.record(at(1, 0.0), at(2, 0.0), Jump::Direct, &l);
        tiny.record(at(2, 0.0), at(3, 0.0), Jump::Direct, &l);
        assert_eq!(tiny.depth(), (1, 0));
    }
}
