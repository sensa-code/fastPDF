//! Painting the document area from a [`DocumentSession`] frame.
//!
//! Prepaint keeps the session's viewport in sync with the element's bounds
//! and the window scale (resize, DPI change), takes the frame (with the
//! texture upload budget as its `ready` predicate) and maps search hits and
//! the text selection to view rectangles; paint draws page backgrounds,
//! stand-in tiles, exact tiles, highlights and page errors.
//!
//! [`DocumentSession`]: fastpdf_core::DocumentSession

use std::time::Instant;

use fastpdf_core::Frame;
use fastpdf_engine_api::Rgba8;
use gpui::{
    App, Bounds, ContentMask, DispatchPhase, Entity, MouseMoveEvent, MouseUpEvent, Pixels, Rgba,
    SharedString, TextAlign, TextRun, Window, fill, point, px, size,
};

use crate::bench::BenchEvent;
use crate::reader::{DocState, ReaderView};
use crate::textures::TileImage;

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

pub(crate) fn color(c: Rgba8) -> Rgba {
    Rgba {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: f32::from(c.a) / 255.0,
    }
}

impl ReaderView {
    /// Canvas prepaint: sync the viewport and take this frame's content.
    pub(crate) fn prepare_viewport(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Option<ViewportFrame> {
        self.viewport_bounds = Some(bounds);
        // Releases textures retired since the last frame before anything
        // new is uploaded, and resets the upload budget.
        self.textures.begin_frame(self.frame_seq, window);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let scale = window.scale_factor();
        let retire = self.textures.retire_queue();
        let textures = &mut self.textures;
        let DocState::Open(open) = &mut self.doc else {
            return None;
        };
        if width < 1.0 || height < 1.0 {
            return None; // minimized or collapsed: nothing to plan
        }
        let session = &mut open.session;
        let vp = *session.viewport();
        if vp.width != width || vp.height != height || vp.device_scale != scale {
            let before = (session.zoom(), session.current_page());
            session.resize(width, height, scale);
            // Fit modes change the zoom (and maybe the page) shown in the
            // toolbar, which was rendered before this layout.
            if before != (session.zoom(), session.current_page()) {
                window.request_animation_frame();
            }
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
        Some(ViewportFrame { frame, highlights })
    }

    /// Canvas paint.
    pub(crate) fn paint_viewport(
        &mut self,
        bounds: Bounds<Pixels>,
        state: Option<ViewportFrame>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let paper = color(self.options.session.paper);
        let theme = self.theme;
        let mut pending = 0;
        let mut deferred = 0;
        let mut shows_pages = false;
        if let Some(ViewportFrame { frame, highlights }) = &state {
            pending = frame.pending;
            deferred = frame.deferred;
            shows_pages = !frame.pages.is_empty();
            let origin = bounds.origin;
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
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
                        HighlightKind::SearchHit => theme.search_hit,
                        HighlightKind::ActiveHit => theme.search_active,
                        HighlightKind::Selection => theme.selection,
                    };
                    window.paint_quad(fill(view_bounds(origin, *rect), color));
                }
                for page in &frame.pages {
                    if let Some(error) = &page.error {
                        let rect = view_bounds(origin, page.rect);
                        paint_page_error(
                            page.page.display_number(),
                            error,
                            rect,
                            theme.error_text,
                            window,
                            cx,
                        );
                    }
                }
            });
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
    page_number: u32,
    error: &str,
    page: Bounds<Pixels>,
    color: Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    let mut text: String = format!("Page {page_number} could not be rendered: {error}")
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
