//! Scroll bars of the document area: a vertical bar on the right while a
//! document is open, and a horizontal bar at the bottom while the pages are
//! wider than the view.
//!
//! The bars are laid out in the viewport's prepaint and painted with the
//! pages, from the same frame's session state, so a thumb never lags the
//! content (smooth scrolling advances in prepaint). Dragging a thumb
//! scrolls; a click on the track pages up or down (left or right). In page
//! mode the vertical thumb stands for the shown row among all rows
//! ([`fastpdf_core::ScrollBars`]).

use fastpdf_core::{ScrollAxis, ScrollBars, ScrollThumb};
use gpui::{Bounds, Pixels, Point, Window, fill, point, px, size};

use crate::theme::Theme;

/// Width of the vertical bar and height of the horizontal one.
pub(crate) const THICKNESS: f32 = 12.0;
/// Shortest thumb, so it stays easy to grab in long documents.
const MIN_THUMB: f32 = 28.0;
/// Space between the thumb and the long edges of its track.
const INSET: f32 = 2.0;

/// One bar as laid out for the current frame, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Bar {
    pub axis: ScrollAxis,
    pub track: Bounds<Pixels>,
    pub thumb: Bounds<Pixels>,
    /// Thumb length as a fraction of the document (the session's).
    size: f64,
}

impl Bar {
    fn new(axis: ScrollAxis, track: Bounds<Pixels>, thumb: ScrollThumb) -> Self {
        let (start, len) = along(axis, track);
        let thumb_len = (thumb.size as f32 * len).max(MIN_THUMB).min(len);
        let free = (len - thumb_len).max(0.0);
        let offset = if thumb.size < 1.0 {
            (thumb.start / (1.0 - thumb.size)) as f32 * free
        } else {
            0.0
        };
        let offset = if offset.is_finite() {
            offset.clamp(0.0, free)
        } else {
            0.0
        };
        let thumb_bounds = match axis {
            ScrollAxis::Vertical => Bounds::new(
                point(track.origin.x + px(INSET), px(start + offset)),
                size(
                    (track.size.width - px(2.0 * INSET)).max(px(1.0)),
                    px(thumb_len),
                ),
            ),
            ScrollAxis::Horizontal => Bounds::new(
                point(px(start + offset), track.origin.y + px(INSET)),
                size(
                    px(thumb_len),
                    (track.size.height - px(2.0 * INSET)).max(px(1.0)),
                ),
            ),
        };
        Self {
            axis,
            track,
            thumb: thumb_bounds,
            size: thumb.size,
        }
    }

    /// The thumb start (as in [`ScrollThumb::start`]) that puts the thumb's
    /// leading edge at `edge`, a window coordinate along the bar.
    pub(crate) fn start_for(&self, edge: f32) -> f64 {
        let (start, len) = along(self.axis, self.track);
        let (_, thumb_len) = along(self.axis, self.thumb);
        let free = len - thumb_len;
        if free <= 0.0 || !edge.is_finite() {
            return 0.0;
        }
        f64::from(((edge - start) / free).clamp(0.0, 1.0)) * (1.0 - self.size)
    }

    /// `p`'s coordinate along the bar.
    pub(crate) fn coordinate(&self, p: Point<Pixels>) -> f32 {
        match self.axis {
            ScrollAxis::Vertical => f32::from(p.y),
            ScrollAxis::Horizontal => f32::from(p.x),
        }
    }
}

/// Start and length of `bounds` along `axis`.
fn along(axis: ScrollAxis, bounds: Bounds<Pixels>) -> (f32, f32) {
    match axis {
        ScrollAxis::Vertical => (f32::from(bounds.origin.y), f32::from(bounds.size.height)),
        ScrollAxis::Horizontal => (f32::from(bounds.origin.x), f32::from(bounds.size.width)),
    }
}

/// What a mouse press on the bars hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Hit {
    /// The thumb, `grab` pixels after its leading edge.
    Thumb { axis: ScrollAxis, grab: f32 },
    /// The track before (above, left of) the thumb.
    Before(ScrollAxis),
    /// The track after the thumb.
    After(ScrollAxis),
    /// A track without a thumb (everything fits), or the corner between
    /// the two bars.
    Nothing,
}

/// A thumb being dragged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Drag {
    pub axis: ScrollAxis,
    /// Pointer offset from the thumb's leading edge when it was grabbed.
    pub grab: f32,
}

/// The bars of the document area.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ScrollBarState {
    vertical_track: Option<Bounds<Pixels>>,
    horizontal_track: Option<Bounds<Pixels>>,
    vertical: Option<Bar>,
    horizontal: Option<Bar>,
    /// Room for the horizontal bar is reserved (the pages were wider than
    /// the view in the last layout).
    pub horizontal_shown: bool,
    /// The thumb under the mouse.
    pub hover: Option<ScrollAxis>,
    pub drag: Option<Drag>,
}

impl ScrollBarState {
    /// The part of the document `area` the pages are shown in: all of it
    /// without a document, else minus the bars.
    pub(crate) fn content_bounds(&self, area: Bounds<Pixels>, has_doc: bool) -> Bounds<Pixels> {
        if !has_doc {
            return area;
        }
        let width = (area.size.width - px(THICKNESS)).max(px(1.0));
        let height = if self.horizontal_shown {
            (area.size.height - px(THICKNESS)).max(px(1.0))
        } else {
            area.size.height
        };
        Bounds::new(area.origin, size(width, height))
    }

    /// Lays the bars out over the document `area` for the session's thumbs.
    pub(crate) fn layout(&mut self, area: Bounds<Pixels>, bars: ScrollBars) {
        let t = px(THICKNESS);
        let bottom = if self.horizontal_shown { t } else { px(0.0) };
        let vertical_track = Bounds::new(
            point(area.origin.x + area.size.width - t, area.origin.y),
            size(t, (area.size.height - bottom).max(px(0.0))),
        );
        self.vertical_track = Some(vertical_track);
        self.vertical = bars
            .vertical
            .map(|thumb| Bar::new(ScrollAxis::Vertical, vertical_track, thumb));
        if self.horizontal_shown {
            let horizontal_track = Bounds::new(
                point(area.origin.x, area.origin.y + area.size.height - t),
                size((area.size.width - t).max(px(0.0)), t),
            );
            self.horizontal_track = Some(horizontal_track);
            self.horizontal = bars
                .horizontal
                .map(|thumb| Bar::new(ScrollAxis::Horizontal, horizontal_track, thumb));
        } else {
            self.horizontal_track = None;
            self.horizontal = None;
        }
        // A thumb that went away is neither hovered nor dragged any more.
        if self.hover.is_some_and(|axis| self.bar(axis).is_none()) {
            self.hover = None;
        }
        if self.drag.is_some_and(|d| self.bar(d.axis).is_none()) {
            self.drag = None;
        }
    }

    /// No document: no bars.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn bar(&self, axis: ScrollAxis) -> Option<Bar> {
        match axis {
            ScrollAxis::Vertical => self.vertical,
            ScrollAxis::Horizontal => self.horizontal,
        }
    }

    /// What a press at `p` hits; `None` outside the bars.
    pub(crate) fn hit(&self, p: Point<Pixels>) -> Option<Hit> {
        let tracks = [
            (self.vertical_track, self.vertical),
            (self.horizontal_track, self.horizontal),
        ];
        for (track, bar) in tracks {
            let Some(track) = track.filter(|t| t.contains(&p)) else {
                continue;
            };
            let Some(bar) = bar else {
                return Some(Hit::Nothing);
            };
            debug_assert_eq!(bar.track, track);
            let c = bar.coordinate(p);
            let (edge, len) = along(bar.axis, bar.thumb);
            return Some(if c < edge {
                Hit::Before(bar.axis)
            } else if c > edge + len {
                Hit::After(bar.axis)
            } else {
                Hit::Thumb {
                    axis: bar.axis,
                    grab: c - edge,
                }
            });
        }
        self.corner()
            .filter(|c| c.contains(&p))
            .map(|_| Hit::Nothing)
    }

    /// The square between the two bars.
    fn corner(&self) -> Option<Bounds<Pixels>> {
        let v = self.vertical_track?;
        let h = self.horizontal_track?;
        Some(Bounds::new(
            point(v.origin.x, h.origin.y),
            size(v.size.width, h.size.height),
        ))
    }

    /// Paints the tracks and thumbs.
    pub(crate) fn paint(&self, theme: &Theme, window: &mut Window) {
        let tracks = [self.vertical_track, self.horizontal_track, self.corner()];
        for track in tracks.into_iter().flatten() {
            window.paint_quad(fill(track, theme.scrollbar_track));
        }
        for bar in [self.vertical, self.horizontal].into_iter().flatten() {
            let active =
                self.hover == Some(bar.axis) || self.drag.is_some_and(|d| d.axis == bar.axis);
            let color = if active {
                theme.scrollbar_thumb_active
            } else {
                theme.scrollbar_thumb
            };
            window.paint_quad(fill(bar.thumb, color).corner_radii(px(4.0)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Bounds<Pixels> {
        Bounds::new(point(px(100.0), px(50.0)), size(px(800.0), px(600.0)))
    }

    fn bars(vertical: Option<(f64, f64)>, horizontal: Option<(f64, f64)>) -> ScrollBars {
        let thumb = |(start, size)| ScrollThumb { start, size };
        ScrollBars {
            vertical: vertical.map(thumb),
            horizontal: horizontal.map(thumb),
        }
    }

    #[test]
    fn the_vertical_bar_takes_the_right_edge() {
        let mut s = ScrollBarState::default();
        let content = s.content_bounds(area(), true);
        assert_eq!(content.size.width, px(800.0 - THICKNESS));
        assert_eq!(content.size.height, px(600.0));
        assert_eq!(s.content_bounds(area(), false), area());

        s.layout(area(), bars(Some((0.25, 0.5)), None));
        let bar = s.bar(ScrollAxis::Vertical).expect("laid out");
        assert_eq!(bar.track.origin.x, px(100.0 + 800.0 - THICKNESS));
        // Half the 600-px track, starting half way into the free 300 px.
        assert_eq!(bar.thumb.size.height, px(300.0));
        assert_eq!(bar.thumb.origin.y, px(50.0 + 150.0));
        // Dragging the thumb back to where it is gives the same start.
        assert!((bar.start_for(200.0) - 0.25).abs() < 1e-6);
        assert_eq!(bar.start_for(-1e9), 0.0);
        assert!((bar.start_for(1e9) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn presses_hit_thumb_track_or_nothing() {
        let mut s = ScrollBarState {
            horizontal_shown: true,
            ..ScrollBarState::default()
        };
        s.layout(area(), bars(Some((0.0, 0.1)), Some((0.5, 0.5))));
        let x = 100.0 + 800.0 - THICKNESS / 2.0;
        let at = |s: &ScrollBarState, x: f32, y: f32| s.hit(point(px(x), px(y)));
        assert_eq!(
            at(&s, x, 60.0),
            Some(Hit::Thumb {
                axis: ScrollAxis::Vertical,
                grab: 10.0
            })
        );
        assert_eq!(at(&s, x, 500.0), Some(Hit::After(ScrollAxis::Vertical)));
        let y = 50.0 + 600.0 - THICKNESS / 2.0;
        assert_eq!(at(&s, 150.0, y), Some(Hit::Before(ScrollAxis::Horizontal)));
        assert_eq!(at(&s, x, y), Some(Hit::Nothing), "the corner");
        assert_eq!(at(&s, 400.0, 300.0), None, "the pages");
        // Everything fits horizontally: the track stays but has no thumb.
        s.layout(area(), bars(Some((0.0, 0.1)), None));
        assert_eq!(at(&s, 150.0, y), Some(Hit::Nothing));
    }

    #[test]
    fn tiny_thumbs_stay_grabbable() {
        let mut s = ScrollBarState::default();
        s.layout(area(), bars(Some((0.999, 0.0001)), None));
        let bar = s.bar(ScrollAxis::Vertical).expect("laid out");
        assert_eq!(bar.thumb.size.height, px(MIN_THUMB));
        // At the end of the document the thumb sits at the end of the track.
        let end = bar.thumb.origin.y + bar.thumb.size.height;
        assert!((f32::from(end) - 650.0).abs() < 1.0, "{end:?}");
        // A drag or hover state of a bar that disappeared is dropped.
        s.hover = Some(ScrollAxis::Vertical);
        s.drag = Some(Drag {
            axis: ScrollAxis::Vertical,
            grab: 3.0,
        });
        s.layout(area(), bars(None, None));
        assert_eq!((s.hover, s.drag), (None, None));
    }
}
