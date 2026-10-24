//! Moving the view: the zoom, rotation, page and scroll commands, the
//! zoom new documents start with, scrolling a found text into view, and
//! the jumps Back and Forward return from (`ReaderView::jump`, mouse
//! back/forward buttons).

use fastpdf_core::keymap::ReaderCommand;
use fastpdf_core::{DocumentSession, Jump};
use fastpdf_engine_api::{Destination, PageIndex, PageRect};
use gpui::{DispatchPhase, Entity, MouseButton, MouseDownEvent, NavigationDirection, Window};

use crate::reader::ReaderView;
use crate::settings::DefaultZoom;
use crate::textures::TileImage;

/// Scroll distance of one wheel "line" and of the arrow keys, in logical
/// pixels (three lines per wheel notch at the Windows default).
pub(crate) const LINE_SCROLL_PX: f32 = 33.0;

pub(crate) fn navigate(session: &mut DocumentSession<TileImage>, command: ReaderCommand) {
    use ReaderCommand as C;
    match command {
        C::ZoomIn => session.zoom_in(None),
        C::ZoomOut => session.zoom_out(None),
        C::ActualSize => session.actual_size(),
        C::FitPage => session.fit_page(),
        C::FitWidth => session.fit_width(),
        C::RotateClockwise => session.rotate_clockwise(),
        C::RotateCounterClockwise => session.rotate_counter_clockwise(),
        C::NextPage => session.next_page(),
        C::PreviousPage => session.prev_page(),
        C::PageDown => session.page_down(),
        C::PageUp => session.page_up(),
        C::ScrollDown => scroll_or_turn(session, LINE_SCROLL_PX * 2.0),
        C::ScrollUp => scroll_or_turn(session, -LINE_SCROLL_PX * 2.0),
        _ => {}
    }
}

/// Arrow keys: scroll; in page mode, at the edge of the shown row, turn
/// the page.
fn scroll_or_turn(session: &mut DocumentSession<TileImage>, dy: f32) {
    if !session.layout_mode().continuous && !session.can_scroll(dy > 0.0) {
        session.turn_page(dy > 0.0);
    } else {
        session.scroll_by(0.0, dy);
    }
}

/// Puts a session in the zoom new documents start with.
pub(crate) fn apply_default_zoom(session: &mut DocumentSession<TileImage>, zoom: DefaultZoom) {
    match zoom {
        DefaultZoom::FitWidth => session.fit_width(),
        DefaultZoom::FitPage => session.fit_page(),
        DefaultZoom::ActualSize => session.actual_size(),
    }
}

/// Scrolls so `rect` on `page` is visible, about a third from the top when
/// the view has to move.
pub(crate) fn reveal(session: &mut DocumentSession<TileImage>, page: PageIndex, rect: PageRect) {
    let vp = *session.viewport();
    let fits = |r: [f32; 4]| {
        r[0] >= 0.0 && r[1] >= 0.0 && r[0] + r[2] <= vp.width && r[1] + r[3] <= vp.height
    };
    if session.page_to_view(page, rect).is_some_and(fits) {
        return;
    }
    // Makes the page's size known and puts its top at the top.
    session.go_to_page(page);
    if let Some(r) = session.page_to_view(page, rect) {
        let dx = if r[0] < 0.0 || r[0] + r[2] > vp.width {
            r[0] + r[2] / 2.0 - vp.width / 2.0
        } else {
            0.0
        };
        session.scroll_by(dx, r[1] - vp.height / 3.0);
    }
}

impl ReaderView {
    /// Runs a navigation the user may undo with Back: outline items,
    /// thumbnails, Home/End, search hits and the dev script's `page=` (and
    /// later go-to-page and links) all go through here.
    pub(crate) fn jump(&mut self, kind: Jump, go: impl FnOnce(&mut DocumentSession<TileImage>)) {
        // Nothing moves behind a prompt card (sidebar clicks and search
        // hits do not pass through run_command's guard).
        if self.active_prompt().is_some() {
            return;
        }
        self.stop_view_motion();
        if let Some(session) = self.session_mut() {
            session.jump(kind, go);
        }
    }

    /// Jumps to an outline or link target (its zoom is not applied).
    pub(crate) fn jump_to_destination(&mut self, destination: &Destination) {
        self.jump(Jump::Direct, |s| s.go_to_destination(destination));
    }

    /// Back to the view before the last jump.
    pub(crate) fn go_back(&mut self) {
        self.step_history(true);
    }

    /// Forward again after Back.
    pub(crate) fn go_forward(&mut self) {
        self.step_history(false);
    }

    fn step_history(&mut self, back: bool) {
        self.stop_view_motion();
        let Some(session) = self.session_mut() else {
            return;
        };
        let moved = if back {
            session.go_back()
        } else {
            session.go_forward()
        };
        if moved {
            let (behind, ahead) = session.history_depth();
            log::debug!(
                "navigation: {} to page {} ({behind} back, {ahead} forward)",
                if back { "back" } else { "forward" },
                session.current_page().display_number(),
            );
        }
    }

    /// A wheel animation or page-turn travel in flight would otherwise
    /// carry on from the new place.
    fn stop_view_motion(&mut self) {
        self.smooth_scroll.stop();
        self.page_turn.reset();
    }
}

/// The mouse's back and forward buttons (X1, X2) anywhere in the window.
/// Registered by the document canvas' paint for the next frame: a
/// window-level listener, so the panels that occlude the document (find
/// bar, print, tools, ...) do not swallow the buttons. It runs before the
/// reader root's focus-on-click and suppresses it, so the buttons leave the
/// keyboard focus where it was (e.g. in the find field).
pub(crate) fn listen_navigation_buttons(view: Entity<ReaderView>, window: &mut Window) {
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble {
            return;
        }
        let command = match event.button {
            MouseButton::Navigate(NavigationDirection::Back) => ReaderCommand::NavigateBack,
            MouseButton::Navigate(NavigationDirection::Forward) => ReaderCommand::NavigateForward,
            _ => return,
        };
        view.update(cx, |this, cx| this.run_command(command, window, cx));
        window.prevent_default();
    });
}
