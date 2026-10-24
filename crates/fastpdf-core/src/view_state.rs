//! Where the view is: the exact in-memory [`ViewState`] that Back and
//! Forward return to, and the layout-independent [`SavedView`] that reading
//! positions keep between runs.
//!
//! Both are captured and applied by the session
//! (`DocumentSession::view_state`, `restore_view`, `saved_view`,
//! `restore_saved`), so every feature that jumps somewhere or comes back
//! later shares one notion of "the same place".

use fastpdf_engine_api::{PageIndex, Rotation};
use fastpdf_render::{DocumentLayout, LayoutMode, ScrollAnchor, ZoomLevel};

use crate::session::ZoomMode;

/// Two views closer than this (layout points, both axes) are the same place.
const SAME_PLACE_POINTS: f64 = 2.0;

/// A view of one document in one session, exact enough to come back to.
///
/// Positions are in layout points (zoom-independent); the anchor keeps the
/// view on the same content while estimated page sizes become real ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// The row at the top of the view and how far into it the view starts.
    pub anchor: ScrollAnchor,
    /// The current page (top third of the view); where the view goes when
    /// the anchor no longer applies (another layout or rotation).
    pub page: PageIndex,
    /// Horizontal scroll, in layout points.
    pub scroll_x: f64,
    pub zoom: ZoomLevel,
    pub zoom_mode: ZoomMode,
    /// The arrangement the anchor refers to.
    pub layout: LayoutMode,
    /// The view rotation the anchor refers to.
    pub rotation: Rotation,
}

impl ViewState {
    /// Whether both views show the same place, ignoring the zoom.
    ///
    /// Views in the arrangement `layout` currently has are compared by
    /// position: the same row in page mode, and top and left within 2
    /// layout points (so landing at the end of the row above equals the top
    /// of the next row). Views of another arrangement or rotation can only
    /// be compared by their page.
    pub fn same_place(&self, other: &Self, layout: &DocumentLayout) -> bool {
        let comparable = self.layout == other.layout
            && self.rotation == other.rotation
            && self.layout == layout.mode();
        if !comparable {
            return self.page == other.page;
        }
        if layout.is_paged() && layout.row_of(self.anchor.page) != layout.row_of(other.anchor.page)
        {
            return false;
        }
        let dy = layout.y_for_anchor(self.anchor) - layout.y_for_anchor(other.anchor);
        let dx = self.scroll_x - other.scroll_x;
        // NaN compares false: a broken view is never the same place.
        dy.abs() <= SAME_PLACE_POINTS && dx.abs() <= SAME_PLACE_POINTS
    }

    /// Whether restoring `other` would change nothing visible: the same
    /// place and the same zoom (a fit mode is the same whatever its factor,
    /// which only follows the window size).
    pub fn same_view(&self, other: &Self, layout: &DocumentLayout) -> bool {
        let same_zoom = self.zoom_mode == other.zoom_mode
            && (self.zoom_mode != ZoomMode::Custom
                || (self.zoom.get() - other.zoom.get()).abs() <= 1e-4);
        same_zoom && self.same_place(other, layout)
    }
}

/// The zoom of a [`SavedView`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SavedZoom {
    FitWidth,
    FitPage,
    /// A zoom factor (1.0 = 100%).
    Custom(f32),
}

impl SavedZoom {
    /// The saved form of a zoom mode and its current factor.
    pub fn of(mode: ZoomMode, zoom: ZoomLevel) -> Self {
        match mode {
            ZoomMode::FitWidth => Self::FitWidth,
            ZoomMode::FitPage => Self::FitPage,
            ZoomMode::Custom => Self::Custom(zoom.get()),
        }
    }
}

/// A view kept between runs, independent of the layout and window size.
///
/// Values may come from a file another program wrote: apply them only
/// through [`SavedView::sanitized`] (`restore_saved` does).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedView {
    /// The first page of the row at the top of the view.
    pub page: PageIndex,
    /// How far below that row's top the view starts, as a fraction of the
    /// row height ([`ScrollAnchor::fraction`]), `-1..=2`.
    pub offset: f32,
    /// Horizontal scroll as a fraction of the document width, `0..=1`.
    pub x: f32,
    pub zoom: SavedZoom,
    pub rotation: Rotation,
}

impl SavedView {
    /// The persisted form of `state`; `doc_width` is the document width in
    /// layout points (`DocumentLayout::total_size`).
    pub fn from_state(state: &ViewState, doc_width: f64) -> Self {
        let x = if doc_width > 0.0 {
            state.scroll_x / doc_width
        } else {
            0.0
        };
        Self {
            page: state.anchor.page,
            offset: state.anchor.fraction as f32,
            x: x as f32,
            zoom: SavedZoom::of(state.zoom_mode, state.zoom),
            rotation: state.rotation,
        }
        .sanitized()
    }

    /// Finite values in their ranges: a non-finite offset or position
    /// becomes 0, a zoom factor goes through [`ZoomLevel::new`]. The page
    /// is clamped to the document when it is applied.
    pub fn sanitized(self) -> Self {
        let finite_or_zero = |v: f32| if v.is_finite() { v } else { 0.0 };
        Self {
            offset: finite_or_zero(self.offset).clamp(-1.0, 2.0),
            x: finite_or_zero(self.x).clamp(0.0, 1.0),
            zoom: match self.zoom {
                SavedZoom::Custom(z) => SavedZoom::Custom(ZoomLevel::new(z).get()),
                fit => fit,
            },
            ..self
        }
    }
}

/// How much of a [`SavedView`] `restore_saved` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreScope {
    /// Everything: page, offset, horizontal position, zoom and rotation.
    Exact,
    /// Page and offset only; the zoom and rotation the view has stay (for
    /// a file that changed since the view was saved).
    PlaceOnly,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::PageSize;
    use fastpdf_render::Spread;

    fn state(layout: &DocumentLayout, page: u32, fraction: f64) -> ViewState {
        ViewState {
            anchor: ScrollAnchor {
                page: PageIndex::new(page),
                fraction,
            },
            page: PageIndex::new(page),
            scroll_x: 0.0,
            zoom: ZoomLevel::ACTUAL_SIZE,
            zoom_mode: ZoomMode::FitWidth,
            layout: layout.mode(),
            rotation: Rotation::R0,
        }
    }

    #[test]
    fn same_place_tolerates_go_to_page_landing() {
        let layout = DocumentLayout::new(50, PageSize::LETTER, 8.0);
        // go_to_page(k) puts the view one gap above row k: the anchor is the
        // end of row k-1, the same place as just above row k.
        let gap = 8.0 / 792.0;
        let landing = state(&layout, 9, 1.0);
        let top = state(&layout, 10, -gap);
        assert!(landing.same_place(&top, &layout));
        assert!(landing.same_place(&state(&layout, 10, -gap + 1.5 / 792.0), &layout));
        assert!(!landing.same_place(&state(&layout, 10, -gap + 3.0 / 792.0), &layout));
        assert!(!state(&layout, 10, 0.1).same_place(&top, &layout));
        let left = ViewState {
            scroll_x: 10.0,
            ..top
        };
        assert!(!left.same_place(&top, &layout));

        // Page mode: every row starts at the same y, so rows must match.
        let paged = DocumentLayout::with_mode(
            50,
            PageSize::LETTER,
            8.0,
            LayoutMode {
                spread: Spread::Single,
                continuous: false,
            },
        );
        let a = state(&paged, 3, 0.0);
        assert!(!a.same_place(&state(&paged, 4, 0.0), &paged));
        assert!(a.same_place(&state(&paged, 3, 0.001), &paged));

        // Another arrangement: only the page counts.
        let other = ViewState {
            layout: paged.mode(),
            anchor: ScrollAnchor {
                page: PageIndex::new(10),
                fraction: 0.7,
            },
            ..top
        };
        assert!(other.same_place(&top, &layout));
        let rotated = ViewState {
            rotation: Rotation::R90,
            page: PageIndex::new(11),
            ..top
        };
        assert!(!rotated.same_place(&top, &layout));

        let broken = state(&layout, 10, f64::NAN);
        assert!(!broken.same_place(&broken, &layout));
    }

    #[test]
    fn same_view_compares_custom_zoom_only() {
        let layout = DocumentLayout::new(50, PageSize::LETTER, 8.0);
        let fit = state(&layout, 5, 0.2);
        let resized = ViewState {
            zoom: ZoomLevel::new(1.7),
            ..fit
        };
        assert!(fit.same_view(&resized, &layout));
        let custom = ViewState {
            zoom_mode: ZoomMode::Custom,
            zoom: ZoomLevel::new(2.0),
            ..fit
        };
        assert!(!fit.same_view(&custom, &layout));
        assert!(fit.same_place(&custom, &layout));
        let zoomed = ViewState {
            zoom: ZoomLevel::new(2.5),
            ..custom
        };
        assert!(!custom.same_view(&zoomed, &layout));
        assert!(custom.same_view(&custom, &layout));
    }

    #[test]
    fn saved_views_are_sanitized() {
        let wild = SavedView {
            page: PageIndex::new(7),
            offset: f32::NAN,
            x: f32::INFINITY,
            zoom: SavedZoom::Custom(0.0),
            rotation: Rotation::R270,
        }
        .sanitized();
        assert_eq!(wild.offset, 0.0);
        assert_eq!(wild.x, 0.0);
        assert_eq!(wild.zoom, SavedZoom::Custom(ZoomLevel::MIN));
        assert_eq!(
            (wild.page, wild.rotation),
            (PageIndex::new(7), Rotation::R270)
        );

        let out_of_range = SavedView {
            offset: -5.0,
            x: 1.5,
            zoom: SavedZoom::Custom(f32::NAN),
            ..wild
        }
        .sanitized();
        assert_eq!((out_of_range.offset, out_of_range.x), (-1.0, 1.0));
        assert_eq!(out_of_range.zoom, SavedZoom::Custom(1.0));
        let far = SavedView {
            offset: 9.0,
            x: -0.5,
            zoom: SavedZoom::Custom(f32::INFINITY),
            ..wild
        }
        .sanitized();
        assert_eq!((far.offset, far.x), (2.0, 0.0));
        assert_eq!(far.zoom, SavedZoom::Custom(ZoomLevel::MAX));
        let fit = SavedView {
            zoom: SavedZoom::FitPage,
            ..wild
        };
        assert_eq!(fit.sanitized().zoom, SavedZoom::FitPage);
    }

    #[test]
    fn saved_views_measure_x_against_the_document_width() {
        let layout = DocumentLayout::new(50, PageSize::LETTER, 8.0);
        let view = ViewState {
            scroll_x: 157.0,
            zoom_mode: ZoomMode::Custom,
            zoom: ZoomLevel::new(2.5),
            ..state(&layout, 12, 0.25)
        };
        let saved = SavedView::from_state(&view, 628.0);
        assert_eq!(saved.page, PageIndex::new(12));
        assert_eq!((saved.offset, saved.x), (0.25, 0.25));
        assert_eq!(saved.zoom, SavedZoom::Custom(2.5));
        // A centered narrow document scrolls to a negative x: kept at 0.
        let centered = ViewState {
            scroll_x: -40.0,
            ..view
        };
        assert_eq!(SavedView::from_state(&centered, 628.0).x, 0.0);
        assert_eq!(SavedView::from_state(&view, 0.0).x, 0.0);
    }
}
