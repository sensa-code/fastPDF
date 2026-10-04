use fastpdf_engine_api::PageSize;

use crate::{DocumentLayout, LayoutRect, ScaleBucket, ZoomLevel};

/// Logical pixels per PDF point at 100% zoom (96 dpi / 72 pt per inch).
pub const POINTS_TO_PX: f32 = 96.0 / 72.0;

/// The visible part of the document.
///
/// Scroll offsets are in layout points so they stay meaningful across zoom
/// changes; the size is in logical pixels (what the UI toolkit reports).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub width: f32,
    pub height: f32,
    pub zoom: ZoomLevel,
    /// Window scale factor reported by the OS (1.0, 1.25, 1.5, 2.0, ...).
    pub device_scale: f32,
}

impl Viewport {
    pub fn new(width: f32, height: f32, zoom: ZoomLevel, device_scale: f32) -> Self {
        Self {
            scroll_x: 0.0,
            scroll_y: 0.0,
            width: width.max(1.0),
            height: height.max(1.0),
            zoom,
            device_scale: if device_scale.is_finite() && device_scale > 0.0 {
                device_scale
            } else {
                1.0
            },
        }
    }

    /// Logical pixels per layout point.
    pub fn px_per_point(&self) -> f64 {
        f64::from(POINTS_TO_PX * self.zoom.get())
    }

    /// Zoom × device scale; selects the tile bucket.
    pub fn display_scale(&self) -> f32 {
        self.zoom.get() * self.device_scale
    }

    pub fn bucket(&self) -> ScaleBucket {
        ScaleBucket::for_display_scale(self.display_scale())
    }

    /// Visible area in layout points.
    pub fn visible_rect(&self) -> LayoutRect {
        let ppp = self.px_per_point();
        LayoutRect {
            x: self.scroll_x,
            y: self.scroll_y,
            width: f64::from(self.width) / ppp,
            height: f64::from(self.height) / ppp,
        }
    }

    /// Keeps the view inside the document; centers documents narrower than
    /// the view horizontally.
    pub fn clamp_scroll(&mut self, layout: &DocumentLayout) {
        let (doc_w, doc_h) = layout.total_size();
        let visible = self.visible_rect();
        self.scroll_x = if doc_w <= visible.width {
            (doc_w - visible.width) / 2.0
        } else {
            self.scroll_x.clamp(0.0, doc_w - visible.width)
        };
        self.scroll_y = self.scroll_y.clamp(0.0, (doc_h - visible.height).max(0.0));
    }

    /// Changes zoom while keeping the layout point under (`anchor_x`,
    /// `anchor_y`) — logical pixels within the view — fixed on screen.
    pub fn zoom_around(&mut self, zoom: ZoomLevel, anchor_x: f32, anchor_y: f32) {
        let before = self.px_per_point();
        let px = self.scroll_x + f64::from(anchor_x) / before;
        let py = self.scroll_y + f64::from(anchor_y) / before;
        self.zoom = zoom;
        let after = self.px_per_point();
        self.scroll_x = px - f64::from(anchor_x) / after;
        self.scroll_y = py - f64::from(anchor_y) / after;
    }

    /// Zoom at which pages of `layout` fill the view width minus `margin_px`.
    pub fn fit_width_zoom(&self, layout: &DocumentLayout, margin_px: f32) -> ZoomLevel {
        let (doc_w, _) = layout.total_size();
        let usable = (self.width - 2.0 * margin_px).max(1.0);
        ZoomLevel::new(usable / (doc_w as f32 * POINTS_TO_PX))
    }

    /// Zoom at which one page of `page` size fits entirely in the view.
    pub fn fit_page_zoom(&self, page: PageSize, margin_px: f32) -> ZoomLevel {
        let w = (self.width - 2.0 * margin_px).max(1.0) / (page.width.max(1.0) * POINTS_TO_PX);
        let h = (self.height - 2.0 * margin_px).max(1.0) / (page.height.max(1.0) * POINTS_TO_PX);
        ZoomLevel::new(w.min(h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::PageIndex;

    #[test]
    fn visible_rect_scales_with_zoom() {
        let v = Viewport::new(800.0, 600.0, ZoomLevel::new(2.0), 1.5);
        let r = v.visible_rect();
        // POINTS_TO_PX is an f32, so allow for its rounding.
        assert!((r.width - 300.0).abs() < 1e-3);
        assert!((r.height - 225.0).abs() < 1e-3);
        assert_eq!(v.bucket().display_scale(), 3.0);
    }

    #[test]
    fn zoom_around_keeps_anchor_fixed() {
        let mut v = Viewport::new(800.0, 600.0, ZoomLevel::ACTUAL_SIZE, 1.0);
        v.scroll_y = 1000.0;
        let before = v.scroll_y + 300.0 / v.px_per_point();
        v.zoom_around(ZoomLevel::new(3.0), 400.0, 300.0);
        let after = v.scroll_y + 300.0 / v.px_per_point();
        assert!((before - after).abs() < 1e-9);
    }

    #[test]
    fn fit_modes() {
        let layout = DocumentLayout::new(3, PageSize::LETTER, 0.0);
        let v = Viewport::new(816.0, 1056.0, ZoomLevel::ACTUAL_SIZE, 1.0);
        // Letter at 100% is exactly 816 x 1056 logical pixels.
        assert!((v.fit_width_zoom(&layout, 0.0).get() - 1.0).abs() < 1e-6);
        assert!((v.fit_page_zoom(PageSize::LETTER, 0.0).get() - 1.0).abs() < 1e-6);
        let page = layout.page_size(PageIndex::FIRST).unwrap();
        assert!(v.fit_page_zoom(page, 50.0).get() < 1.0);
    }

    #[test]
    fn scroll_is_clamped_to_the_document() {
        let layout = DocumentLayout::new(2, PageSize::LETTER, 0.0);
        let mut v = Viewport::new(400.0, 300.0, ZoomLevel::ACTUAL_SIZE, 1.0);
        v.scroll_y = -50.0;
        v.scroll_x = 1e9;
        v.clamp_scroll(&layout);
        assert_eq!(v.scroll_y, 0.0);
        assert!((v.scroll_x - (612.0 - 300.0)).abs() < 1e-3);
        v.scroll_y = 1e12;
        v.clamp_scroll(&layout);
        assert!((v.scroll_y - (2.0 * 792.0 - 225.0)).abs() < 1e-3);
    }
}
