//! Painting the document area from a [`DocumentSession`] frame.
//!
//! Prepaint keeps the session's viewport in sync with the element's bounds
//! and the window scale (resize, DPI change) and takes the frame; paint
//! releases retired textures, then draws page backgrounds, stand-in tiles,
//! exact tiles (within the upload budget) and page errors.

use std::time::Instant;

use fastpdf_core::Frame;
use fastpdf_engine_api::Rgba8;
use gpui::{
    App, Bounds, ContentMask, Pixels, Rgba, SharedString, TextAlign, TextRun, Window, fill, point,
    px, size,
};

use crate::bench::BenchEvent;
use crate::reader::{DocState, ReaderView};
use crate::textures::TileImage;

/// Longest page error message painted on a page.
const MAX_ERROR_CHARS: usize = 160;

fn view_bounds(origin: gpui::Point<Pixels>, rect: [f32; 4]) -> Bounds<Pixels> {
    Bounds::new(
        point(origin.x + px(rect[0]), origin.y + px(rect[1])),
        size(px(rect[2]), px(rect[3])),
    )
}

fn color(c: Rgba8) -> Rgba {
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
    ) -> Option<Frame<TileImage>> {
        self.viewport_bounds = Some(bounds);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        let scale = window.scale_factor();
        let retire = self.textures.retire_queue();
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
        retire.set_in_frame(true);
        let frame = session.frame();
        retire.set_in_frame(false);
        Some(frame)
    }

    /// Canvas paint.
    pub(crate) fn paint_viewport(
        &mut self,
        bounds: Bounds<Pixels>,
        frame: Option<Frame<TileImage>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Release evicted textures before anything new is uploaded.
        self.textures.begin_frame(window);
        let paper = color(self.options.session.paper);
        let theme = self.theme;
        let mut deferred = false;
        let mut pending = 0;
        if let Some(frame) = &frame {
            pending = frame.pending;
            let origin = bounds.origin;
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for page in &frame.pages {
                    let rect = view_bounds(origin, page.rect);
                    window.paint_quad(fill(rect.dilate(px(1.0)), theme.page_border));
                    window.paint_quad(fill(rect, paper));
                }
                // Stand-ins come first in the frame, exact tiles on top.
                for tile in &frame.tiles {
                    let rect = view_bounds(origin, tile.rect);
                    if !self.textures.paint(&tile.image, rect, window) {
                        deferred = true;
                    }
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
        if deferred {
            // Upload budget reached: continue next frame.
            window.request_animation_frame();
        }
        self.after_paint(frame.as_ref(), pending, deferred, window);
        // Last, so it sits on top of the pages.
        self.overlay.paint(bounds, &theme, window, cx);
    }

    /// Bench milestones and overlay statistics.
    fn after_paint(
        &mut self,
        frame: Option<&Frame<TileImage>>,
        pending: usize,
        deferred: bool,
        window: &mut Window,
    ) {
        let now = Instant::now();
        if !self.first_paint_reported {
            self.first_paint_reported = true;
            if let Some(hook) = self.bench_hook() {
                window.on_next_frame(move |_, _| hook(BenchEvent::FirstPaint));
            }
        }
        let shows_pages = frame.is_some_and(|f| !f.pages.is_empty());
        if let DocState::Open(open) = &mut self.doc
            && !open.exact_reported
            && shows_pages
            && pending == 0
            && !deferred
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
    }
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
