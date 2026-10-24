//! Pointer input on the document area: a press works the scroll bars,
//! places a stamp, starts a line edit or a text selection (a click on a
//! link follows it, `crate::links`); the wheel
//! scrolls (with Ctrl it zooms), a pinch zooms and a dropped file opens.

use std::time::Instant;

use fastpdf_core::ScrollAxis;
use gpui::{
    Context, ExternalPaths, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PinchEvent,
    Pixels, ScrollWheelEvent, Window, px,
};

use crate::navigation::LINE_SCROLL_PX;
use crate::opening::is_pdf;
use crate::reader::{DocState, ReaderView};
use crate::scrollbar::{Drag, Hit};
use crate::select::PagePoint;

/// Ctrl + wheel: logical pixels of wheel travel that double the zoom.
const WHEEL_PX_PER_ZOOM_DOUBLING: f32 = 400.0;

impl ReaderView {
    pub(crate) fn on_document_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.focus(&self.focus, cx);
        if let Some(hit) = self.scrollbars.hit(event.position) {
            self.on_scrollbar_press(hit, cx);
            return;
        }
        if self.text_edit_mouse_down(event.position, window, cx) {
            cx.notify();
            return;
        }
        if self.stamp_mouse_down(event.position) {
            cx.notify();
            return;
        }
        self.link_press(event);
        match self.page_point(event.position) {
            Some(point) => {
                let missing = self.selection.begin(point);
                self.fetch_text_layers(missing, cx);
            }
            None => self.selection.clear(),
        }
        cx.notify();
    }

    /// Drag update from the canvas' window-wide listener.
    pub(crate) fn on_selection_drag(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        self.link_drag(position);
        if let Some(point) = self.page_point(position) {
            let missing = self.selection.extend(point);
            self.fetch_text_layers(missing, cx);
            cx.notify();
        }
    }

    /// The release ending a selection drag: a click (not a drag) clears
    /// the selection, and on a link follows it. Other buttons released
    /// meanwhile do not end it.
    pub(crate) fn on_selection_end(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        self.selection.end();
        let followed = self.link_release(event, window, cx);
        if followed || self.selection.anchor == self.selection.focus {
            self.selection.clear();
        }
        cx.notify();
    }

    fn on_scrollbar_press(&mut self, hit: Hit, cx: &mut Context<'_, Self>) {
        self.smooth_scroll.stop();
        let width = self.session().map_or(0.0, |s| s.viewport().width);
        match hit {
            Hit::Thumb { axis, grab } => self.scrollbars.drag = Some(Drag { axis, grab }),
            Hit::Before(ScrollAxis::Vertical) => {
                if let Some(s) = self.session_mut() {
                    s.page_up();
                }
            }
            Hit::After(ScrollAxis::Vertical) => {
                if let Some(s) = self.session_mut() {
                    s.page_down();
                }
            }
            Hit::Before(ScrollAxis::Horizontal) => {
                if let Some(s) = self.session_mut() {
                    s.scroll_by(-width * 0.9, 0.0);
                }
            }
            Hit::After(ScrollAxis::Horizontal) => {
                if let Some(s) = self.session_mut() {
                    s.scroll_by(width * 0.9, 0.0);
                }
            }
            Hit::Nothing => {}
        }
        cx.notify();
    }

    /// Drag update from the canvas' window-wide listener.
    pub(crate) fn on_scrollbar_drag(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(drag) = self.scrollbars.drag else {
            return;
        };
        let Some(bar) = self.scrollbars.bar(drag.axis) else {
            return;
        };
        let start = bar.start_for(bar.coordinate(position) - drag.grab);
        if let Some(session) = self.session_mut() {
            session.scroll_to(drag.axis, start);
        }
        cx.notify();
    }

    pub(crate) fn end_scrollbar_drag(&mut self, cx: &mut Context<'_, Self>) {
        if self.scrollbars.drag.take().is_some() {
            cx.notify();
        }
    }

    /// Highlights the thumb under the mouse.
    pub(crate) fn on_document_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let hover = match self.scrollbars.hit(event.position) {
            Some(Hit::Thumb { axis, .. }) => Some(axis),
            _ => None,
        };
        if hover != self.scrollbars.hover {
            self.scrollbars.hover = hover;
            cx.notify();
        }
        self.text_edit_hover(event.position, cx);
        self.link_hover(event.position, cx);
    }

    pub(crate) fn on_document_hover(
        &mut self,
        hovered: &bool,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !*hovered && self.scrollbars.hover.take().is_some() {
            cx.notify();
        }
        if !*hovered {
            self.clear_link_hover(cx);
        }
    }

    /// The page-space point under a window position.
    pub(crate) fn page_point(&self, position: gpui::Point<Pixels>) -> Option<PagePoint> {
        let (x, y) = self.local_position(position)?;
        let (page, px, py) = self.session()?.view_to_page(x, y)?;
        Some(PagePoint { page, x: px, y: py })
    }

    fn local_position(&self, position: gpui::Point<Pixels>) -> Option<(f32, f32)> {
        let b = self.viewport_bounds?;
        Some((
            f32::from(position.x - b.origin.x),
            f32::from(position.y - b.origin.y),
        ))
    }

    pub(crate) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let anchor = self.local_position(event.position);
        let notch = crate::smooth_scroll::is_wheel_notch(&event.delta);
        let smooth = self.settings.smooth_scrolling && !event.modifiers.control && notch;
        let delta = event.delta.pixel_delta(px(LINE_SCROLL_PX));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let DocState::Open(open) = &mut self.doc else {
            return;
        };
        let session = &mut open.session;
        // GPUI deltas move the content; the view moves the other way.
        let down = -dy;
        let paged = !session.layout_mode().continuous;
        if event.modifiers.control {
            // Wheel away from the user zooms in, around the cursor.
            let factor = 2f32.powf(dy / WHEEL_PX_PER_ZOOM_DOUBLING);
            session.set_zoom(session.zoom().scaled(factor), anchor);
        } else if paged && down != 0.0 && !session.can_scroll(down > 0.0) {
            // Page mode at the edge of the shown row: wheel travel past it
            // turns the page (`crate::smooth_scroll::PageTurn`).
            if self.page_turn.push(down, notch, Instant::now()) {
                self.smooth_scroll.stop();
                session.turn_page(down > 0.0);
            }
        } else if smooth {
            self.page_turn.reset();
            // Animated by the viewport's prepaint (`crate::smooth_scroll`).
            self.smooth_scroll.add((-dx, -dy), Instant::now());
        } else {
            self.page_turn.reset();
            session.scroll_by(-dx, -dy);
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// A wheel turn of `notches` (positive: down) as a mouse sends it, at
    /// the Windows default of three lines per notch (dev scripts).
    pub(crate) fn wheel_notches(
        &mut self,
        notches: i32,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let position = self
            .viewport_bounds
            .map_or_else(gpui::Point::default, |b| b.center());
        let event = ScrollWheelEvent {
            position,
            delta: gpui::ScrollDelta::Lines(gpui::point(0.0, -3.0 * notches as f32)),
            ..ScrollWheelEvent::default()
        };
        self.on_scroll_wheel(&event, window, cx);
    }

    pub(crate) fn on_pinch(
        &mut self,
        event: &PinchEvent,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let anchor = self.local_position(event.position);
        let Some(session) = self.session_mut() else {
            return;
        };
        let factor = (1.0 + event.delta).clamp(0.5, 2.0);
        session.set_zoom(session.zoom().scaled(factor), anchor);
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn on_drop_paths(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let paths = paths.paths();
        let chosen = paths
            .iter()
            .find(|p| is_pdf(p))
            .or_else(|| paths.first())
            .cloned();
        if let Some(path) = chosen {
            self.open_path(path, window, cx);
        }
    }
}
