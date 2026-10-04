//! Development overlay (spec §46): frame timing, session and texture
//! counters, process memory. Off by default; Ctrl+Shift+D or
//! `FASTPDF_DEV_OVERLAY=1`.
//!
//! It is painted at the end of the document paint with the statistics of
//! that same frame, so it is never a frame behind and never needs a frame of
//! its own: an idle reader stays idle with the overlay on.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use fastpdf_core::SessionStats;
use gpui::{App, Bounds, Pixels, TextAlign, TextRun, Window, fill, font, point, px, size};

use crate::textures::TextureStats;
use crate::theme::{MONO_FONT, Theme};

const WINDOW: Duration = Duration::from_secs(1);
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub(crate) struct DevOverlay {
    pub visible: bool,
    /// (paint time, render-to-paint duration in ms) of recent frames.
    frames: VecDeque<(Instant, f64)>,
    session: Option<SessionStats>,
    textures: TextureStats,
    pending: usize,
    private_bytes: Option<u64>,
    last_memory_sample: Option<Instant>,
}

impl DevOverlay {
    pub(crate) fn new(visible: bool) -> Self {
        Self {
            visible,
            ..Self::default()
        }
    }

    /// Records one painted frame.
    pub(crate) fn record_frame(
        &mut self,
        now: Instant,
        frame_ms: f64,
        session: Option<SessionStats>,
        textures: TextureStats,
        pending: usize,
    ) {
        self.frames.push_back((now, frame_ms));
        while self
            .frames
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > WINDOW)
        {
            self.frames.pop_front();
        }
        self.session = session;
        self.textures = textures;
        self.pending = pending;
        if self.visible
            && self
                .last_memory_sample
                .is_none_or(|t| now.duration_since(t) >= MEMORY_SAMPLE_INTERVAL)
        {
            self.last_memory_sample = Some(now);
            self.private_bytes = fastpdf_core::memory::process_private_bytes();
        }
    }

    fn lines(&self) -> Vec<String> {
        let fps = self.frames.len();
        let last = self.frames.back().map_or(0.0, |(_, ms)| *ms);
        let worst = self.frames.iter().map(|(_, ms)| *ms).fold(0.0, f64::max);
        let mut lines = vec![format!(
            "frames/s {fps:>3}  frame {last:>6.2} ms  worst {worst:>6.2} ms"
        )];
        if let Some(s) = self.session {
            lines.push(format!(
                "tiles {:>4} {:>7}  hit {} miss {} evict {}",
                s.tile_entries,
                mib(s.tile_bytes as u64),
                s.tile_hits,
                s.tile_misses,
                s.tile_evictions
            ));
            let q = s.scheduler;
            lines.push(format!(
                "queue {} running {} pending {}  done {} fail {} cancel {} drop {}",
                q.queued,
                q.in_flight,
                self.pending,
                q.completed,
                q.failed,
                q.cancelled,
                q.discarded
            ));
        } else {
            lines.push("no document".into());
        }
        let t = self.textures;
        lines.push(format!(
            "gpu tiles {:>4} {:>7}  uploaded {} ({})  released {}  deferred {}",
            t.resident,
            mib(t.resident_bytes as u64),
            t.uploads,
            mib(t.upload_bytes),
            t.released,
            t.deferred
        ));
        lines.push(format!(
            "private {}",
            self.private_bytes.map_or_else(|| "?".into(), mib)
        ));
        lines
    }

    /// Paints the overlay in the top-right corner of `area`.
    pub(crate) fn paint(
        &self,
        area: Bounds<Pixels>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut App,
    ) {
        if !self.visible {
            return;
        }
        let font_size = px(12.0);
        let line_height = px(16.0);
        let padding = px(8.0);
        let mono = font(MONO_FONT);
        let shaped: Vec<_> = self
            .lines()
            .into_iter()
            .map(|text| {
                let run = TextRun {
                    len: text.len(),
                    font: mono.clone(),
                    color: theme.overlay_text.into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                window
                    .text_system()
                    .shape_line(text.into(), font_size, &[run], None)
            })
            .collect();
        let text_width = shaped.iter().map(|l| l.width).fold(px(0.0), Pixels::max);
        let panel = size(
            text_width + padding * 2.0,
            line_height * shaped.len() as f32 + padding * 1.5,
        );
        let origin = point(
            area.origin.x + area.size.width - panel.width - px(8.0),
            area.origin.y + px(8.0),
        );
        window.paint_quad(fill(Bounds::new(origin, panel), theme.overlay_bg).corner_radii(px(4.0)));
        for (i, line) in shaped.iter().enumerate() {
            let at = point(
                origin.x + padding,
                origin.y + padding * 0.75 + line_height * i as f32,
            );
            if let Err(e) = line.paint(at, line_height, TextAlign::Left, None, window, cx) {
                log::debug!("overlay text: {e}");
            }
        }
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}
