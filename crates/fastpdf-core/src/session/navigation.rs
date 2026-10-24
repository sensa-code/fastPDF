//! Capturing and restoring views, and the jumps Back and Forward undo.
//!
//! Every navigation the user may want to undo (go to page, first/last
//! page, outline items, thumbnails, links, search hits) runs through
//! [`DocumentSession::jump`], which records the view it left in the
//! session's [`NavigationHistory`](crate::history::NavigationHistory).
//! Ordinary movement (scrolling, paging, zooming, rotating, changing the
//! layout) calls the session directly and is not recorded.

use fastpdf_engine_api::PageIndex;
use fastpdf_render::{ScrollAnchor, ZoomLevel};

use super::{DocumentSession, ZoomMode};
use crate::history::Jump;
use crate::view_state::{RestoreScope, SavedView, SavedZoom, ViewState};

#[cfg(test)]
mod tests;

impl<V: Clone + Send + 'static> DocumentSession<V> {
    /// Where the view is now.
    pub fn view_state(&self) -> ViewState {
        ViewState {
            anchor: self.layout.anchor_at(self.viewport.scroll_y),
            page: self.current_page(),
            scroll_x: self.viewport.scroll_x,
            zoom: self.viewport.zoom,
            zoom_mode: self.zoom_mode,
            layout: self.layout.mode(),
            rotation: self.rotation,
        }
    }

    /// Returns to `state`: its zoom and zoom mode, and its exact position
    /// when the layout and rotation are still the ones it was taken in;
    /// otherwise the page it showed. The layout and rotation themselves
    /// stay as they are.
    pub fn restore_view(&mut self, state: &ViewState) {
        self.zoom_mode = state.zoom_mode;
        if state.zoom_mode == ZoomMode::Custom {
            self.viewport.zoom = state.zoom;
        }
        let arranged = state.layout == self.layout.mode() && state.rotation == self.rotation;
        if arranged && state.anchor.fraction.is_finite() && state.scroll_x.is_finite() {
            // Resolves the row's page sizes (page mode: shows the row)
            // before the anchor is turned into a position.
            self.go_to_page(state.anchor.page);
            self.viewport.scroll_y = self.layout.y_for_anchor(state.anchor);
            self.viewport.scroll_x = state.scroll_x;
        } else {
            self.go_to_page(state.page);
        }
        self.apply_zoom_mode();
        self.viewport.clamp_scroll(&self.layout);
    }

    /// The view in its persisted, layout-independent form.
    ///
    /// `go_to_page` leaves the view one gap above its row, which the anchor
    /// gives to the end of the row before (fraction 1). Saved like that,
    /// another layout would show the wrong row, so a view in the gap below
    /// a row is saved as the top of the next one (a small negative offset).
    pub fn saved_view(&self) -> SavedView {
        let mut state = self.view_state();
        let layout = &self.layout;
        if layout.mode().continuous
            && state.anchor.fraction >= 1.0
            && let Some(next) = layout.next_row_page(state.anchor.page)
        {
            let top = layout.y_for_anchor(ScrollAnchor {
                page: next,
                fraction: 0.0,
            });
            let bottom = layout.y_for_anchor(ScrollAnchor {
                page: next,
                fraction: 1.0,
            });
            state.anchor = ScrollAnchor {
                page: next,
                fraction: (self.viewport.scroll_y - top) / (bottom - top).max(1.0),
            };
        }
        SavedView::from_state(&state, layout.total_size().0)
    }

    /// Shows a saved view (sanitized first): page and offset, plus the
    /// horizontal position, zoom and rotation for [`RestoreScope::Exact`].
    /// Works before the first frame, so the first one plans the saved page.
    pub fn restore_saved(&mut self, saved: &SavedView, scope: RestoreScope) {
        let saved = saved.sanitized();
        let exact = scope == RestoreScope::Exact;
        if exact {
            if saved.rotation != self.rotation {
                self.set_rotation(saved.rotation);
            }
            match saved.zoom {
                SavedZoom::FitWidth => self.zoom_mode = ZoomMode::FitWidth,
                SavedZoom::FitPage => self.zoom_mode = ZoomMode::FitPage,
                SavedZoom::Custom(zoom) => {
                    self.zoom_mode = ZoomMode::Custom;
                    self.viewport.zoom = ZoomLevel::new(zoom);
                }
            }
        }
        let last = self.page_count().saturating_sub(1);
        let page = PageIndex::new(saved.page.get().min(last));
        let anchor = ScrollAnchor {
            page,
            fraction: f64::from(saved.offset),
        };
        self.go_to_page(page);
        self.viewport.scroll_y = self.layout.y_for_anchor(anchor);
        // Fit page fits the restored page, which is current only now.
        self.apply_zoom_mode();
        if exact {
            self.viewport.scroll_x = f64::from(saved.x) * self.layout.total_size().0;
        }
        self.viewport.clamp_scroll(&self.layout);
    }

    /// Runs a navigation the user may undo with Back (`go` moves the view)
    /// and records the view it left. False when nothing new was recorded:
    /// the view did not move, or the search hit continues a run of hits.
    pub fn jump(&mut self, kind: Jump, go: impl FnOnce(&mut Self)) -> bool {
        let before = self.view_state();
        go(self);
        let after = self.view_state();
        self.history.record(before, after, kind, &self.layout)
    }

    /// Returns to the view before the last jump; false when there is none.
    pub fn go_back(&mut self) -> bool {
        let current = self.view_state();
        match self.history.back(current, &self.layout) {
            Some(target) => {
                self.restore_view(&target);
                true
            }
            None => false,
        }
    }

    /// Undoes the last Back; false when there is nothing to go forward to.
    pub fn go_forward(&mut self) -> bool {
        let current = self.view_state();
        match self.history.forward(current, &self.layout) {
            Some(target) => {
                self.restore_view(&target);
                true
            }
            None => false,
        }
    }

    /// Whether Back would change the view.
    pub fn can_go_back(&self) -> bool {
        self.history.can_go_back(&self.view_state(), &self.layout)
    }

    /// Whether Forward would change the view.
    pub fn can_go_forward(&self) -> bool {
        self.history
            .can_go_forward(&self.view_state(), &self.layout)
    }

    /// History entries (back, forward), for dev-script checks.
    pub fn history_depth(&self) -> (usize, usize) {
        self.history.depth()
    }
}
