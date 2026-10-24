//! Moving the view: the zoom, rotation, page and scroll commands, the
//! zoom new documents start with, and scrolling a found text into view.

use fastpdf_core::DocumentSession;
use fastpdf_core::keymap::ReaderCommand;
use fastpdf_engine_api::{PageIndex, PageRect};

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
        C::FirstPage => session.first_page(),
        C::LastPage => session.last_page(),
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
