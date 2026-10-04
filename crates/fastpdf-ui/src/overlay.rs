//! Development overlay (spec §46): frame timing, session and texture
//! counters and a memory breakdown. Off by default; Ctrl+Shift+D or
//! `FASTPDF_DEV_OVERLAY=1`.
//!
//! It is painted at the end of the document paint with the statistics of
//! that same frame, so it is never a frame behind and never needs a frame of
//! its own: an idle reader stays idle with the overlay on.
//!
//! The memory breakdown lists every cache registered with the memory budget
//! manager (tiles, thumbnails, text), the engine's own estimate for the open
//! document (`EngineDocument::memory_usage`), the GPU atlas share of tiles,
//! the budget pressure and the process's private bytes; what is left of the
//! private bytes ("other") is memory nobody accounts for: GPU driver and
//! GPUI allocations, fonts, allocator slack, code.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use fastpdf_cache::{CacheSnapshot, MemoryPressure};
use fastpdf_core::SessionStats;
use gpui::{App, Bounds, Pixels, TextAlign, TextRun, Window, fill, font, point, px, size};

use crate::reader::ReaderView;
use crate::textures::TextureStats;
use crate::theme::{MONO_FONT, Theme};

const WINDOW: Duration = Duration::from_secs(1);
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// Where the memory goes, sampled for the overlay and dev scripts.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemoryBreakdown {
    /// Caches registered with the budget manager, largest first.
    pub caches: Vec<CacheSnapshot>,
    /// Engine-internal bytes of the open document: `None` without a
    /// document, `Some(None)` when the engine cannot tell.
    pub engine: Option<Option<u64>>,
    pub gpu_tiles: usize,
    pub gpu_bytes: usize,
    pub pressure: MemoryPressure,
    pub soft_limit: usize,
    pub hard_limit: usize,
    pub private: Option<u64>,
}

impl MemoryBreakdown {
    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let cache_bytes: usize = self.caches.iter().map(|c| c.stats.bytes).sum();
        for c in &self.caches {
            lines.push(format!(
                "cache {:<10} {:>5} entries {:>9} / {}",
                c.name,
                c.stats.entries,
                mib(c.stats.bytes as u64),
                mib(c.stats.budget as u64)
            ));
        }
        let engine = match self.engine {
            None => "no document".to_string(),
            Some(None) => "n/a".to_string(),
            Some(Some(bytes)) => mib(bytes),
        };
        lines.push(format!(
            "engine {engine}  gpu tiles {} {}",
            self.gpu_tiles,
            mib(self.gpu_bytes as u64)
        ));
        lines.push(format!(
            "pressure {:?}  caches {}  (soft {} / hard {})",
            self.pressure,
            mib(cache_bytes as u64),
            mib(self.soft_limit as u64),
            mib(self.hard_limit as u64)
        ));
        let accounted = cache_bytes as u64 + self.engine.flatten().unwrap_or(0);
        lines.push(match self.private {
            Some(private) => format!(
                "private {}  other {}",
                mib(private),
                mib(private.saturating_sub(accounted))
            ),
            None => "private ?".to_string(),
        });
        lines
    }
}

#[derive(Debug, Default)]
pub(crate) struct DevOverlay {
    pub visible: bool,
    /// (paint time, render-to-paint duration in ms) of recent frames.
    frames: VecDeque<(Instant, f64)>,
    session: Option<SessionStats>,
    textures: TextureStats,
    pending: usize,
    memory: Option<MemoryBreakdown>,
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
    }

    /// True when the memory breakdown should be sampled again.
    pub(crate) fn wants_memory_sample(&self, now: Instant) -> bool {
        self.visible
            && self
                .last_memory_sample
                .is_none_or(|t| now.duration_since(t) >= MEMORY_SAMPLE_INTERVAL)
    }

    pub(crate) fn set_memory(&mut self, now: Instant, memory: MemoryBreakdown) {
        self.last_memory_sample = Some(now);
        self.memory = Some(memory);
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
                "tiles {:>4} {:>7}  hit {} miss {} evict {}  thumbs {} {}",
                s.tile_entries,
                mib(s.tile_bytes as u64),
                s.tile_hits,
                s.tile_misses,
                s.tile_evictions,
                s.thumbnail_entries,
                mib(s.thumbnail_bytes as u64)
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
        match &self.memory {
            Some(memory) => lines.extend(memory.lines()),
            None => lines.push("memory ?".into()),
        }
        lines
    }

    /// Paints the overlay in the bottom-right corner of `area`, clear of the
    /// find bar and the print panel at the top.
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
            (area.origin.y + area.size.height - panel.height - px(8.0)).max(area.origin.y),
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

impl ReaderView {
    /// Samples the memory breakdown. Cheap: cache counters, one call into
    /// the engine, one process counter.
    pub(crate) fn memory_breakdown(&self) -> MemoryBreakdown {
        let manager = self.memory.manager();
        let mut caches = manager.snapshot();
        caches.sort_by(|a, b| b.stats.bytes.cmp(&a.stats.bytes).then(a.name.cmp(&b.name)));
        let config = manager.config();
        let textures = self.textures.stats();
        MemoryBreakdown {
            caches,
            engine: self.session().map(|s| {
                use fastpdf_engine_api::EngineDocument;
                s.document().memory_usage()
            }),
            gpu_tiles: textures.resident,
            gpu_bytes: textures.resident_bytes,
            pressure: self.memory.last_pressure(),
            soft_limit: config.soft_limit,
            hard_limit: config.hard_limit,
            private: fastpdf_core::memory::process_private_bytes(),
        }
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_cache::CacheStats;

    const MB: usize = 1024 * 1024;

    #[test]
    fn breakdown_accounts_for_caches_and_engine() {
        let cache = |name: &str, bytes, budget, entries| CacheSnapshot {
            name: name.into(),
            retention: 0,
            stats: CacheStats {
                bytes,
                budget,
                entries,
                ..CacheStats::default()
            },
        };
        let memory = MemoryBreakdown {
            caches: vec![
                cache("tiles", 96 * MB, 128 * MB, 40),
                cache("text", 4 * MB, 32 * MB, 300),
            ],
            engine: Some(Some(20 * MB as u64)),
            gpu_tiles: 40,
            gpu_bytes: 96 * MB,
            pressure: MemoryPressure::Normal,
            soft_limit: 320 * MB,
            hard_limit: 512 * MB,
            private: Some(250 * MB as u64),
        };
        let lines = memory.lines();
        assert!(lines[0].contains("tiles") && lines[0].contains("96.0 MiB / 128.0 MiB"));
        assert!(lines[2].starts_with("engine 20.0 MiB"));
        assert!(lines[3].contains("caches 100.0 MiB"));
        assert_eq!(lines[4], "private 250.0 MiB  other 130.0 MiB");
        let unknown = MemoryBreakdown {
            engine: Some(None),
            ..MemoryBreakdown::default()
        };
        assert!(unknown.lines()[0].starts_with("engine n/a"));
    }
}
