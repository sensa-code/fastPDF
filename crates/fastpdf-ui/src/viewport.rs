//! Painting the document area from a [`DocumentSession`] frame.
//!
//! Prepaint keeps the session's viewport in sync with the element's bounds
//! (minus the scroll bars) and the window scale (resize, DPI change), lays
//! out the scroll bars, takes the frame (with the texture upload budget as
//! its `ready` predicate) and maps search hits and the text selection to
//! view rectangles; paint draws page backgrounds, stand-in tiles, exact
//! tiles, highlights, page errors and the scroll bars.
//!
//! [`DocumentSession`]: fastpdf_core::DocumentSession

use std::time::Instant;

use fastpdf_core::Frame;
use fastpdf_core::ScrollAxis;
use gpui::{
    App, Bounds, ContentMask, DispatchPhase, Entity, MouseMoveEvent, MouseUpEvent, Pixels, Rgba,
    SharedString, TextAlign, TextRun, Window, fill, point, px, size,
};

use crate::bench::BenchEvent;
use crate::reader::{DocState, ReaderView};
use crate::textures::TileImage;
use crate::theme::PageHighlights;

/// Longest page error message painted on a page.
const MAX_ERROR_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HighlightKind {
    SearchHit,
    ActiveHit,
    Selection,
}

/// What the canvas paints in one frame.
pub(crate) struct ViewportFrame {
    /// The part of the canvas the pages are shown in (without the bars).
    content: Bounds<Pixels>,
    frame: Frame<TileImage>,
    /// View rectangles (logical pixels, relative to the viewport).
    highlights: Vec<([f32; 4], HighlightKind)>,
}

fn view_bounds(origin: gpui::Point<Pixels>, rect: [f32; 4]) -> Bounds<Pixels> {
    Bounds::new(
        point(origin.x + px(rect[0]), origin.y + px(rect[1])),
        size(px(rect[2]), px(rect[3])),
    )
}

impl ReaderView {
    /// Canvas prepaint: sync the viewport and take this frame's content.
    pub(crate) fn prepare_viewport(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Option<ViewportFrame> {
        self.report(BenchEvent::Prepaint);
        self.viewport_bounds = Some(bounds);
        // Releases textures retired since the last frame before anything
        // new is uploaded, and resets the upload budget.
        self.textures.begin_frame(self.frame_seq, window);
        let has_doc = matches!(self.doc, DocState::Open(_));
        if !has_doc {
            self.scrollbars.clear();
        }
        let content = self.scrollbars.content_bounds(bounds, has_doc);
        let width = f32::from(content.size.width);
        let height = f32::from(content.size.height);
        let scale = window.scale_factor();
        let retire = self.textures.retire_queue();
        // Wheel notches in flight: this frame's share of the motion, and
        // one more frame while the animation lasts (never when idle).
        let scroll_step = self.smooth_scroll.step(Instant::now());
        if self.smooth_scroll.is_active() {
            window.request_animation_frame();
        }
        let textures = &mut self.textures;
        let DocState::Open(open) = &mut self.doc else {
            return None;
        };
        if width < 1.0 || height < 1.0 {
            return None; // minimized or collapsed: nothing to plan
        }
        let session = &mut open.session;
        if let Some((dx, dy)) = scroll_step {
            let page = session.current_page();
            session.scroll_by(dx, dy);
            // The toolbar's page number was rendered before this step.
            if session.current_page() != page {
                window.request_animation_frame();
            }
        }
        let vp = *session.viewport();
        let before = (session.zoom(), session.current_page());
        if vp.width != width || vp.height != height || vp.device_scale != scale {
            session.resize(width, height, scale);
        }
        // The horizontal bar comes and goes with the pages' width; the view
        // gets shorter or taller by its height. Fit modes never make the
        // pages wider than the view, so this settles in one step.
        let mut bars = session.scroll_bars();
        let content = if bars.horizontal.is_some() != self.scrollbars.horizontal_shown {
            self.scrollbars.horizontal_shown = bars.horizontal.is_some();
            let content = self.scrollbars.content_bounds(bounds, true);
            let height = f32::from(content.size.height);
            session.resize(width, height, scale);
            bars = session.scroll_bars();
            content
        } else {
            content
        };
        self.scrollbars.layout(bounds, bars);
        // Fit modes change the zoom (and maybe the page) shown in the
        // toolbar, which was rendered before this layout.
        if before != (session.zoom(), session.current_page()) {
            window.request_animation_frame();
        }
        // Tiles over this frame's upload budget come back as stand-ins.
        let frame = retire.in_frame(|| session.frame_with(|image| textures.reserve(image)));

        let mut highlights = Vec::new();
        for page in frame.pages.iter().map(|p| p.page) {
            for (rect, active) in self.find.hits.on_page(page) {
                if let Some(r) = session.page_to_view(page, rect) {
                    let kind = if active {
                        HighlightKind::ActiveHit
                    } else {
                        HighlightKind::SearchHit
                    };
                    highlights.push((r, kind));
                }
            }
            for rect in self.selection.rects_on(page) {
                if let Some(r) = session.page_to_view(page, rect) {
                    highlights.push((r, HighlightKind::Selection));
                }
            }
        }
        Some(ViewportFrame {
            content,
            frame,
            highlights,
        })
    }

    /// Canvas paint.
    pub(crate) fn paint_viewport(
        &mut self,
        bounds: Bounds<Pixels>,
        state: Option<ViewportFrame>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.report(BenchEvent::Paint);
        let mode = self.session().map(|s| s.color_mode()).unwrap_or_default();
        // Unrendered paper matches the tiles (black in night mode).
        let paper = crate::theme::paper(self.options.session.paper, mode);
        let marks = PageHighlights::for_mode(mode);
        let theme = self.theme;
        let strings = self.strings();
        let mut pending = 0;
        let mut deferred = 0;
        let mut shows_pages = false;
        if let Some(ViewportFrame {
            content,
            frame,
            highlights,
        }) = &state
        {
            pending = frame.pending;
            deferred = frame.deferred;
            shows_pages = !frame.pages.is_empty();
            let origin = content.origin;
            let mask = ContentMask { bounds: *content };
            window.with_content_mask(Some(mask), |window| {
                for page in &frame.pages {
                    let rect = view_bounds(origin, page.rect);
                    window.paint_quad(fill(rect.dilate(px(1.0)), theme.page_border));
                    window.paint_quad(fill(rect, paper));
                }
                // Stand-ins come first in the frame, exact tiles on top;
                // only the inner part of each tile is shown (gutters).
                for tile in &frame.tiles {
                    let rect = view_bounds(origin, tile.rect);
                    self.textures
                        .paint(&tile.image, rect, Some(tile.src), window);
                }
                for (rect, kind) in highlights {
                    let color = match kind {
                        HighlightKind::SearchHit => marks.search_hit,
                        HighlightKind::ActiveHit => marks.search_active,
                        HighlightKind::Selection => marks.selection,
                    };
                    window.paint_quad(fill(view_bounds(origin, *rect), color));
                }
                self.paint_text_edits(origin, paper, mode, window, cx);
                self.paint_stamps(origin, window);
                for page in &frame.pages {
                    if let Some(error) = &page.error {
                        let rect = view_bounds(origin, page.rect);
                        let number = page.page.display_number();
                        paint_page_error(
                            &strings.page_failure(number, page.cause.as_ref(), error),
                            rect,
                            theme.error_text,
                            window,
                            cx,
                        );
                    }
                }
            });
        }
        if state.is_some() {
            self.scrollbars.paint(&theme, window);
            self.paint_drag_page_label(window, cx);
        }
        // Over budget this frame, or evictions made during it still hold
        // GPU memory: one more frame finishes the job.
        if deferred > 0 || self.textures.has_retired() {
            window.request_animation_frame();
        }
        self.after_paint(shows_pages, pending, deferred, window);
        // Last, so it sits on top of the pages.
        self.overlay.paint(bounds, &theme, window, cx);
    }

    /// While the vertical thumb is dragged: the page it points at, next to
    /// the thumb ("12 / 340").
    fn paint_drag_page_label(&self, window: &mut Window, cx: &mut App) {
        let dragging = self
            .scrollbars
            .drag
            .is_some_and(|d| d.axis == ScrollAxis::Vertical);
        let (Some(bar), Some((page, count))) = (
            self.scrollbars
                .bar(ScrollAxis::Vertical)
                .filter(|_| dragging),
            self.page_indicator(),
        ) else {
            return;
        };
        let text = format!("{} / {}", page.display_number(), count);
        let font = window.text_style().font();
        let run = TextRun {
            len: text.len(),
            font,
            color: self.theme.text.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line =
            window
                .text_system()
                .shape_line(SharedString::from(text), px(13.0), &[run], None);
        let pad = px(6.0);
        let label = Bounds::new(
            point(
                bar.track.origin.x - line.width - pad * 3.0,
                bar.thumb.origin.y + bar.thumb.size.height / 2.0 - px(12.0),
            ),
            size(line.width + pad * 2.0, px(24.0)),
        );
        window.paint_quad(
            fill(label, self.theme.toolbar_bg)
                .corner_radii(px(4.0))
                .border_widths(px(1.0))
                .border_color(self.theme.toolbar_border),
        );
        let origin = point(label.origin.x + pad, label.origin.y + px(4.0));
        if let Err(e) = line.paint(origin, px(16.0), TextAlign::Left, None, window, cx) {
            log::debug!("cannot paint the page label: {e}");
        }
    }

    /// Bench milestones and overlay statistics.
    fn after_paint(
        &mut self,
        shows_pages: bool,
        pending: usize,
        deferred: usize,
        window: &mut Window,
    ) {
        let now = Instant::now();
        if !self.first_paint_reported {
            self.first_paint_reported = true;
            if let Some(hook) = self.bench_hook() {
                window.on_next_frame(move |_, _| hook(BenchEvent::FirstPaint));
            }
        }
        if let DocState::Open(open) = &mut self.doc
            && !open.exact_reported
            && shows_pages
            && pending == 0
            && deferred == 0
        {
            open.exact_reported = true;
            let since_open = open.opened_at.elapsed();
            log::info!(
                "first page fully rendered {:.1} ms after the session started",
                since_open.as_secs_f64() * 1000.0
            );
            if let Some(hook) = self.bench_hook() {
                window.on_next_frame(move |_, _| hook(BenchEvent::FirstPageExact));
            }
        }
        let frame_ms = self
            .render_started
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64() * 1000.0);
        let session_stats = self.session().map(|s| s.stats());
        let texture_stats = self.textures.stats();
        self.overlay
            .record_frame(now, frame_ms, session_stats, texture_stats, pending);
        if self.overlay.wants_memory_sample(now) {
            let memory = self.memory_breakdown();
            self.overlay.set_memory(now, memory);
        }
    }
}

/// While a scroll bar thumb is dragged, follows the mouse everywhere in the
/// window until the button is released (like [`track_selection_drag`]).
pub(crate) fn track_scrollbar_drag(view: Entity<ReaderView>, window: &mut Window) {
    let move_view = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
        if phase == DispatchPhase::Bubble {
            move_view.update(cx, |this, cx| this.on_scrollbar_drag(event.position, cx));
        }
    });
    window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
        if phase == DispatchPhase::Bubble {
            view.update(cx, |this, cx| this.end_scrollbar_drag(cx));
        }
    });
}

/// While a selection drag is active, follows the mouse everywhere in the
/// window (not only over the document area) until the button is released.
/// Listeners registered during paint live for one frame; the next paint
/// registers them again while the drag lasts.
pub(crate) fn track_selection_drag(view: Entity<ReaderView>, window: &mut Window) {
    let move_view = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
        if phase == DispatchPhase::Bubble {
            move_view.update(cx, |this, cx| this.on_selection_drag(event.position, cx));
        }
    });
    window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
        if phase == DispatchPhase::Bubble {
            view.update(cx, |this, cx| this.on_selection_end(cx));
        }
    });
}

fn paint_page_error(
    message: &str,
    page: Bounds<Pixels>,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    let mut text: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_ERROR_CHARS)
        .collect();
    if text.is_empty() {
        text.push(' ');
    }
    let font = window.text_style().font();
    let run = TextRun {
        len: text.len(),
        font,
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let font_size = px(14.0);
    let line = window
        .text_system()
        .shape_line(SharedString::from(text), font_size, &[run], None);
    let origin = point(page.origin.x + px(16.0), page.origin.y + px(16.0));
    let width = (page.size.width - px(32.0)).max(px(1.0));
    if let Err(e) = line.paint(origin, px(20.0), TextAlign::Left, Some(width), window, cx) {
        log::debug!("cannot paint page error text: {e}");
    }
}
