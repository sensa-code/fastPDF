//! `scroll`: memory behaviour of a scripted reading session (benchmark plan
//! B-5; spec §15, §16, §46).
//!
//! A headless [`DocumentSession`] is driven the way a reader uses the app:
//! scroll from the first page to the last with a fixed step, zoom in to 400%
//! and back out at a few places, then jump back to the start. Every step
//! waits until the visible tiles are rendered. The tile cache is registered
//! with a [`MemoryBudgetManager`], and a [`MemoryMonitor`] watching the
//! document is polled on every frame, exactly as in the app. The report is a
//! time series of process memory, cache, scheduler and engine counters plus
//! a summary. Only memory is evaluated; times are recorded for orientation.

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use fastpdf_cache::{BudgetConfig, MemoryBudgetManager, MemoryPressure};
use fastpdf_core::memory::MemoryMonitor;
use fastpdf_core::{DocumentSession, Frame, SessionConfig, loader};
use fastpdf_engine_api::{
    DocumentSource, EngineDocument, GuardedDocument, OpenOptions, PdfEngine, Pixmap, open_guarded,
};
use fastpdf_render::{TileKey, ZoomLevel};
use serde::Serialize;

use crate::args::Args;
use crate::engines;
use crate::metrics::{self, MachineInfo, MemoryCounters};
use crate::report::{PageError, Status};

const SCHEMA: u32 = 1;
const MIB: f64 = 1024.0 * 1024.0;
/// Default view: a maximized window on a 1080p display.
const DEFAULT_VIEW: (u32, u32) = (1920, 1080);
/// Zoom reached by each zoom excursion (100% → 400% → 100%).
const PEAK_ZOOM: f32 = 4.0;
/// Where the zoom excursions happen, as fractions of the page count.
const EXCURSIONS: [f32; 3] = [0.25, 0.5, 0.75];
/// How often a waiting step re-checks the frame without a wake-up.
const WAKE_POLL: Duration = Duration::from_millis(50);
/// Upper bound on steps, against a layout that never reaches its end.
const MAX_STEPS: u64 = 1_000_000;
/// Preset zoom steps between two zoom levels never exceed this.
const MAX_ZOOM_STEPS: usize = 64;
/// A pixel is ink when one of its color channels is darker than this
/// (the paper is white).
const INK_THRESHOLD: u8 = 0xE0;
/// How long to wait for the engine's threads to exit after the document
/// is dropped.
const DROP_WAIT: Duration = Duration::from_secs(3);
/// How long to wait for running renders after the session is closed.
const IDLE_WAIT: Duration = Duration::from_secs(30);
/// Idle time before the `after_idle` point: longer than the hayro adapter's
/// idle thread exit (5 s).
#[cfg(not(test))]
const IDLE_SETTLE: Duration = Duration::from_secs(6);
#[cfg(test)]
const IDLE_SETTLE: Duration = Duration::from_millis(50);

/// Settings of one run, echoed in the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ScriptConfig {
    /// View size in logical pixels.
    pub(crate) viewport: [u32; 2],
    pub(crate) device_scale: f32,
    pub(crate) zoom_percent: f32,
    pub(crate) peak_zoom_percent: f32,
    /// Scroll step in logical pixels.
    pub(crate) step_px: f32,
    pub(crate) sample_every: u64,
    pub(crate) settle_timeout_s: u64,
    pub(crate) tile_size: u32,
    pub(crate) workers: usize,
    pub(crate) tile_budget_mb: u32,
    pub(crate) soft_limit_mb: u32,
    pub(crate) hard_limit_mb: u32,
    pub(crate) relief_target_percent: u8,
    pub(crate) monitor_interval_ms: u64,
}

impl ScriptConfig {
    pub(crate) fn from_args(args: &Args) -> Result<Self, String> {
        let session = SessionConfig::default();
        let budget = BudgetConfig::default();
        let (w, h) = args.viewport.unwrap_or(DEFAULT_VIEW);
        if w == 0 || h == 0 {
            return Err("--viewport must not be empty".into());
        }
        let zoom = ZoomLevel::new(args.zoom_percent.unwrap_or(100.0) / 100.0).get();
        let soft = args.soft_limit_mb.unwrap_or(to_mb(budget.soft_limit));
        let hard = args.hard_limit_mb.unwrap_or(to_mb(budget.hard_limit));
        if soft >= hard {
            return Err(format!(
                "the soft limit ({soft} MiB) must be below the hard limit ({hard} MiB)"
            ));
        }
        Ok(Self {
            viewport: [w, h],
            device_scale: args.scale,
            zoom_percent: zoom * 100.0,
            peak_zoom_percent: peak_zoom(zoom) * 100.0,
            // Page Down scrolls 90% of the view (DocumentSession::page_down).
            step_px: args.step_px.unwrap_or(h as f32 * 0.9),
            sample_every: u64::from(args.sample_every.max(1)),
            settle_timeout_s: args.settle_secs.max(1),
            tile_size: session.tile_size,
            workers: args.workers.unwrap_or(session.workers).max(1),
            tile_budget_mb: args.tile_budget_mb.unwrap_or(to_mb(session.tile_budget)),
            soft_limit_mb: soft,
            hard_limit_mb: hard,
            relief_target_percent: budget.relief_target_percent,
            monitor_interval_ms: u64::try_from(MemoryMonitor::DEFAULT_INTERVAL.as_millis())
                .unwrap_or(u64::MAX),
        })
    }

    fn session(&self) -> SessionConfig {
        SessionConfig {
            tile_size: self.tile_size,
            workers: self.workers,
            tile_budget: mb_to_bytes(self.tile_budget_mb),
            ..SessionConfig::default()
        }
    }

    fn budget(&self) -> BudgetConfig {
        BudgetConfig {
            soft_limit: mb_to_bytes(self.soft_limit_mb),
            hard_limit: mb_to_bytes(self.hard_limit_mb),
            relief_target_percent: self.relief_target_percent,
        }
    }

    fn base_zoom(&self) -> ZoomLevel {
        ZoomLevel::new(self.zoom_percent / 100.0)
    }
}

fn to_mb(bytes: usize) -> u32 {
    u32::try_from(bytes / (1024 * 1024)).unwrap_or(u32::MAX)
}

fn mb_to_bytes(mb: u32) -> usize {
    (mb as usize).saturating_mul(1024 * 1024)
}

fn mib(bytes: u64) -> f64 {
    (bytes as f64 / MIB * 10.0).round() / 10.0
}

/// Zoom reached by an excursion from `base`.
fn peak_zoom(base: f32) -> f32 {
    if base < PEAK_ZOOM {
        PEAK_ZOOM
    } else {
        ZoomLevel::new(base * 2.0).get()
    }
}

/// Pages (0-based) at which zoom excursions start.
fn excursion_pages(page_count: u32) -> Vec<u32> {
    let mut pages: Vec<u32> = EXCURSIONS
        .iter()
        .map(|f| (page_count as f32 * f) as u32)
        .collect();
    pages.dedup();
    pages
}

/// Pixels of `src` (`[x, y, w, h]` in image pixels) that are not paper.
fn ink_pixels(image: &Pixmap, src: [f32; 4]) -> u64 {
    let size = image.size();
    let (w, h) = (size.width as usize, size.height as usize);
    let clamp = |v: f32, max: usize| (v.max(0.0) as usize).min(max);
    let (x0, x1) = (clamp(src[0], w), clamp((src[0] + src[2]).ceil(), w));
    let (y0, y1) = (clamp(src[1], h), clamp((src[1] + src[3]).ceil(), h));
    let data = image.data();
    let mut ink = 0;
    for y in y0..y1 {
        let Some(row) = data.get((y * w + x0) * 4..(y * w + x1) * 4) else {
            break;
        };
        ink += row
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[..3].iter().any(|&c| c < INK_THRESHOLD))
            .count() as u64;
    }
    ink
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
#[serde(rename_all = "snake_case")]
enum Pressure {
    #[default]
    Normal,
    Soft,
    Hard,
}

impl From<MemoryPressure> for Pressure {
    fn from(p: MemoryPressure) -> Self {
        match p {
            MemoryPressure::Normal => Self::Normal,
            MemoryPressure::Soft => Self::Soft,
            MemoryPressure::Hard => Self::Hard,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Open,
    Scroll,
    ZoomIn,
    ZoomOut,
    JumpBack,
}

/// Memory the engine holds outside FastPDF's budgeted caches (hayro only).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
struct EngineMemory {
    block_cache_mb: f64,
    decoded_content_mb: f64,
    /// Estimated buffers of the render threads' contexts.
    context_mb: f64,
    /// System fonts mapped by the font resolver (file-backed, shared).
    mapped_fonts_mb: f64,
    /// Decode budget held by renders in progress.
    decode_mb: f64,
    decode_waits: u64,
    documents: u64,
    generations: u64,
    reopens: u64,
    threads: u64,
    busy_threads: u64,
    soft_trims: u64,
    hard_trims: u64,
}

#[cfg(feature = "engine-hayro")]
fn engine_memory(engine: &str) -> Option<EngineMemory> {
    (engine == "hayro").then(|| {
        let m = fastpdf_engine_hayro::diagnostics::memory();
        EngineMemory {
            block_cache_mb: mib(m.block_cache_bytes),
            decoded_content_mb: mib(m.decoded_content_bytes),
            context_mb: mib(m.context_bytes),
            mapped_fonts_mb: mib(m.mapped_font_bytes),
            decode_mb: mib(m.decode_bytes),
            decode_waits: m.decode_waits,
            documents: m.live_documents,
            generations: m.live_generations,
            reopens: m.reopens,
            threads: m.render_threads,
            busy_threads: m.busy_threads,
            soft_trims: m.soft_trims,
            hard_trims: m.hard_trims,
        }
    })
}

#[cfg(not(feature = "engine-hayro"))]
fn engine_memory(_engine: &str) -> Option<EngineMemory> {
    None
}

/// Budgets of the engine-internal caches: (block cache, decoded content).
#[cfg(feature = "engine-hayro")]
fn engine_budgets(engine: &str) -> Option<(u64, u64)> {
    use fastpdf_engine_hayro::diagnostics;
    (engine == "hayro").then_some((
        diagnostics::BLOCK_CACHE_BUDGET,
        diagnostics::CONTENT_STREAM_BUDGET,
    ))
}

#[cfg(not(feature = "engine-hayro"))]
fn engine_budgets(_engine: &str) -> Option<(u64, u64)> {
    None
}

/// One point of the time series. Memory in MiB.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct Sample {
    step: u64,
    phase: Phase,
    /// 1-based page at the top third of the view.
    page: u32,
    zoom_percent: u32,
    t_s: f64,
    settled: bool,
    /// Committed private bytes of the process.
    private_mb: f64,
    working_set_mb: f64,
    /// `private` minus the registered caches: what the memory monitor
    /// treats as memory outside the budgeted caches.
    external_mb: f64,
    /// `EngineDocument::memory_usage`: the engine's own estimate of its
    /// internal caches for this document.
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_reported_mb: Option<f64>,
    tile_mb: f64,
    tile_entries: usize,
    tile_evictions: u64,
    rendered_tiles: u64,
    queued: usize,
    in_flight: usize,
    cancelled: u64,
    discarded: u64,
    failed: u64,
    pressure: Pressure,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<EngineMemory>,
}

/// A memory-monitor poll that found pressure and relieved it.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct ReliefEvent {
    step: u64,
    pressure: Pressure,
    freed_mb: f64,
    private_before_mb: f64,
    private_after_mb: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ScrollBack {
    /// Every visible tile was rendered at the exact resolution in time.
    settled: bool,
    exact_tiles: usize,
    standin_tiles: usize,
    pending_tiles: usize,
    /// Tiles rendered while returning: the first page's tiles had been
    /// evicted and came back.
    rerendered_tiles: u64,
    /// Non-paper pixels in the exact tiles; zero means a blank page.
    ink_pixels: u64,
    blank: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
struct MemoryPoint {
    private_mb: f64,
    working_set_mb: f64,
    /// `EngineDocument::memory_usage` while the document is open.
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_reported_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<EngineMemory>,
}

impl MemoryPoint {
    fn now(engine: &str, doc: Option<&GuardedDocument>) -> Self {
        let m = metrics::memory().unwrap_or_default();
        Self {
            private_mb: mib(m.private),
            working_set_mb: mib(m.working_set),
            engine_reported_mb: doc.and_then(GuardedDocument::memory_usage).map(mib),
            engine: engine_memory(engine),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Reliefs {
    soft: u64,
    hard: u64,
    freed_mb: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EngineSummary {
    block_cache_budget_mb: f64,
    max_block_cache_mb: f64,
    block_cache_held: bool,
    content_budget_mb: f64,
    max_decoded_content_mb: f64,
    max_threads: u64,
    max_generations: u64,
    reopens: u64,
    soft_trims: u64,
    hard_trims: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Summary {
    steps: u64,
    scroll_steps: u64,
    zoom_steps: u64,
    /// Steps whose visible tiles were not all rendered within the timeout.
    unsettled_steps: u64,
    last_page: u32,
    elapsed_s: f64,
    max_private_mb: f64,
    max_private_step: u64,
    final_private_mb: f64,
    max_working_set_mb: f64,
    final_working_set_mb: f64,
    /// Peaks as reported by the OS for the whole process lifetime.
    os_peak_working_set_mb: f64,
    os_peak_private_mb: f64,
    max_external_mb: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_engine_reported_mb: Option<f64>,
    tile_budget_mb: f64,
    max_tile_mb: f64,
    tile_budget_held: bool,
    tile_evictions: u64,
    rendered_tiles: u64,
    reliefs: Reliefs,
    max_pressure: Pressure,
    scroll_back: ScrollBack,
    /// After `IDLE_SETTLE` without input, tiles still cached: what an open,
    /// idle reader holds once the engine released its working memory.
    after_idle: MemoryPoint,
    /// After `DocumentSession::close` (every tile released).
    after_close: MemoryPoint,
    /// After the session and the document were dropped.
    after_drop: MemoryPoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<EngineSummary>,
}

/// Result of a `scroll` run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ScrollReport {
    schema: u32,
    mode: &'static str,
    file: String,
    file_bytes: Option<u64>,
    load_strategy: Option<String>,
    engine: String,
    engine_version: String,
    pub(crate) status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    page_count: Option<u32>,
    machine: MachineInfo,
    config: ScriptConfig,
    summary: Option<Summary>,
    reliefs: Vec<ReliefEvent>,
    page_errors: Vec<PageError>,
    samples: Vec<Sample>,
}

impl ScrollReport {
    fn one_line(&self) -> String {
        let Some(s) = &self.summary else {
            return format!(
                "scroll: {} {:?}: {}",
                self.file,
                self.status,
                self.error.as_deref().unwrap_or("")
            );
        };
        format!(
            "scroll: {} pages, {} steps ({} unsettled), private max {} / final {} MiB, \
             working set max {} MiB, tiles max {} / budget {} MiB, {} evictions, \
             reliefs soft {} hard {}, scroll-back re-rendered {} tiles, ink {} px",
            self.page_count.unwrap_or(0),
            s.steps,
            s.unsettled_steps,
            s.max_private_mb,
            s.final_private_mb,
            s.max_working_set_mb,
            s.max_tile_mb,
            s.tile_budget_mb,
            s.tile_evictions,
            s.reliefs.soft,
            s.reliefs.hard,
            s.scroll_back.rerendered_tiles,
            s.scroll_back.ink_pixels,
        )
    }
}

/// `fastpdf-bench scroll <file>`.
pub(crate) fn run(file: &Path, args: &Args) -> Result<ExitCode, String> {
    let config = ScriptConfig::from_args(args)?;
    let engine = engines::select(args.engine.as_deref())?;
    let report = scroll(engine.as_ref(), file, args.password.clone(), config);
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    match &args.out {
        Some(path) => {
            std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
        }
        None => println!("{text}"),
    }
    eprintln!("{}", report.one_line());
    Ok(match report.status {
        Status::Ok | Status::Partial => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    })
}

pub(crate) fn scroll(
    engine: &dyn PdfEngine,
    file: &Path,
    password: Option<String>,
    config: ScriptConfig,
) -> ScrollReport {
    let info = engine.info();
    let mut report = ScrollReport {
        schema: SCHEMA,
        mode: "scroll",
        file: file.to_string_lossy().replace('\\', "/"),
        file_bytes: None,
        load_strategy: None,
        engine: info.name.to_owned(),
        engine_version: info.version.to_owned(),
        status: Status::Ok,
        error: None,
        page_count: None,
        machine: metrics::machine(),
        config,
        summary: None,
        reliefs: Vec::new(),
        page_errors: Vec::new(),
        samples: Vec::new(),
    };
    let loaded = match loader::load(file) {
        Ok(l) => l,
        Err(e) => {
            report.status = Status::LoadError;
            report.error = Some(e.to_string());
            return report;
        }
    };
    report.file_bytes = Some(loaded.bytes.len() as u64);
    report.load_strategy = Some(loaded.strategy.to_string());
    let options = OpenOptions {
        password,
        ..OpenOptions::default()
    };
    let source = DocumentSource::from_bytes(loaded.bytes).with_path(file);
    let doc = match open_guarded(engine, source, &options) {
        Ok(doc) => Arc::new(doc),
        Err(e) => {
            report.status = Status::OpenError;
            report.error = Some(e.to_string());
            return report;
        }
    };
    report.page_count = Some(doc.page_count());

    let config = report.config.clone();
    let mut script = Script::new(doc, &config, info.name);
    let back = script.run();
    script.finish(back, &mut report);
    report
}

/// Bookkeeping of a run.
#[derive(Debug, Default)]
struct Recorder {
    samples: Vec<Sample>,
    reliefs: Vec<ReliefEvent>,
    page_errors: Vec<PageError>,
    error_pages: HashSet<u32>,
    scroll_steps: u64,
    zoom_steps: u64,
    unsettled: u64,
    last_page: u32,
    max_private: u64,
    max_private_step: u64,
    max_working_set: u64,
    max_external: u64,
    max_tile: usize,
    max_pressure: Pressure,
    max_block: f64,
    max_content: f64,
    max_threads: u64,
    max_generations: u64,
    max_engine_reported: Option<u64>,
    freed_bytes: u64,
}

impl Recorder {
    fn track(&mut self, step: u64, mem: MemoryCounters, external: u64, tile: usize) {
        if mem.private > self.max_private {
            self.max_private = mem.private;
            self.max_private_step = step;
        }
        self.max_working_set = self.max_working_set.max(mem.working_set);
        self.max_external = self.max_external.max(external);
        self.max_tile = self.max_tile.max(tile);
    }

    fn track_engine(&mut self, engine: Option<EngineMemory>) {
        if let Some(e) = engine {
            self.max_block = self.max_block.max(e.block_cache_mb);
            self.max_content = self.max_content.max(e.decoded_content_mb);
            self.max_threads = self.max_threads.max(e.threads);
            self.max_generations = self.max_generations.max(e.generations);
        }
    }
}

struct Script<'c> {
    config: &'c ScriptConfig,
    engine: &'static str,
    doc: Arc<GuardedDocument>,
    session: DocumentSession<Arc<Pixmap>>,
    monitor: MemoryMonitor,
    wake: Receiver<()>,
    started: Instant,
    step: u64,
    rec: Recorder,
}

impl<'c> Script<'c> {
    fn new(doc: Arc<GuardedDocument>, config: &'c ScriptConfig, engine: &'static str) -> Self {
        let (wake_tx, wake) = mpsc::channel();
        let session = DocumentSession::new(
            Arc::clone(&doc),
            config.session(),
            (config.viewport[0] as f32, config.viewport[1] as f32),
            config.device_scale,
            Arc::new,
            move || {
                let _ = wake_tx.send(());
            },
            // Evicted tiles only need dropping (no GPU textures here).
            |_: Vec<(TileKey, Arc<Pixmap>)>| {},
        );
        // Wired exactly like the reader window (fastpdf-ui `reader.rs`).
        let manager = Arc::new(MemoryBudgetManager::new(config.budget()));
        manager.register(session.tile_cache().budgeted());
        let mut monitor = MemoryMonitor::new(manager);
        monitor.watch(&doc);
        Self {
            config,
            engine,
            doc,
            session,
            monitor,
            wake,
            started: Instant::now(),
            step: 0,
            rec: Recorder::default(),
        }
    }

    fn run(&mut self) -> ScrollBack {
        let base = self.config.base_zoom();
        self.session.set_zoom(base, None);
        self.session.first_page();
        self.complete_step(Phase::Open, true);

        let mut excursions = excursion_pages(self.session.page_count()).into_iter();
        let mut next = excursions.next();
        while self.step < MAX_STEPS {
            let page = self.session.current_page().get();
            while next.is_some_and(|p| page >= p) {
                self.zoom_excursion(base);
                next = excursions.next();
            }
            let before = self.session.viewport().scroll_y;
            self.session.scroll_by(0.0, self.config.step_px);
            if self.session.viewport().scroll_y <= before {
                break; // the last page is in view
            }
            self.complete_step(Phase::Scroll, false);
        }
        // Documents shorter than a few views end before every excursion.
        while next.is_some() {
            self.zoom_excursion(base);
            next = excursions.next();
        }
        self.jump_back()
    }

    /// Zooms in preset steps to the peak and back to `base` around the
    /// view center, settling every step.
    fn zoom_excursion(&mut self, base: ZoomLevel) {
        let peak = peak_zoom(base.get());
        for _ in 0..MAX_ZOOM_STEPS {
            if self.session.zoom().get() >= peak * 0.999 {
                break;
            }
            self.session.zoom_in(None);
            self.complete_step(Phase::ZoomIn, true);
        }
        for _ in 0..MAX_ZOOM_STEPS {
            let zoom = self.session.zoom();
            if zoom.get() <= base.get() * 1.001 {
                break;
            }
            if zoom.zoom_out().get() < base.get() {
                self.session.set_zoom(base, None);
            } else {
                self.session.zoom_out(None);
            }
            self.complete_step(Phase::ZoomOut, true);
        }
    }

    fn jump_back(&mut self) -> ScrollBack {
        let before = self.session.stats().scheduler.completed;
        self.session.first_page();
        let frame = self.complete_step(Phase::JumpBack, true);
        let after = self.session.stats().scheduler.completed;
        let exact: Vec<_> = frame.tiles.iter().filter(|t| t.exact).collect();
        let ink: u64 = exact.iter().map(|t| ink_pixels(&t.image, t.src)).sum();
        ScrollBack {
            settled: frame.pending == 0,
            exact_tiles: exact.len(),
            standin_tiles: frame.tiles.len() - exact.len(),
            pending_tiles: frame.pending,
            rerendered_tiles: after.saturating_sub(before),
            ink_pixels: ink,
            blank: ink == 0,
        }
    }

    fn complete_step(&mut self, phase: Phase, force_sample: bool) -> Frame<Arc<Pixmap>> {
        self.step += 1;
        match phase {
            Phase::Scroll => self.rec.scroll_steps += 1,
            Phase::ZoomIn | Phase::ZoomOut => self.rec.zoom_steps += 1,
            Phase::Open | Phase::JumpBack => {}
        }
        let (frame, settled) = self.settle();
        if !settled {
            self.rec.unsettled += 1;
        }
        self.observe(phase, &frame, settled, force_sample || !settled);
        frame
    }

    /// Paints until every visible tile is exact or the timeout passes.
    fn settle(&mut self) -> (Frame<Arc<Pixmap>>, bool) {
        let deadline = Instant::now() + Duration::from_secs(self.config.settle_timeout_s);
        loop {
            let frame = self.session.frame();
            self.poll_memory();
            if frame.pending == 0 {
                return (frame, true);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (frame, false);
            }
            // The session's wake hook fires when a tile arrives; the poll
            // interval covers tiles that must be re-planned by `frame`.
            let _ = self.wake.recv_timeout(left.min(WAKE_POLL));
            while self.wake.try_recv().is_ok() {}
        }
    }

    /// The app polls the monitor whenever work happened; it rate-limits
    /// itself to one sample per interval.
    fn poll_memory(&mut self) {
        let before = metrics::memory().unwrap_or_default().private;
        let Some(relief) = self.monitor.poll() else {
            return;
        };
        if relief.pressure > MemoryPressure::Normal {
            self.rec.freed_bytes += relief.freed as u64;
            let after = metrics::memory().unwrap_or_default().private;
            self.rec.reliefs.push(ReliefEvent {
                step: self.step,
                pressure: relief.pressure.into(),
                freed_mb: mib(relief.freed as u64),
                private_before_mb: mib(before),
                private_after_mb: mib(after),
            });
        }
        self.rec.max_pressure = self.rec.max_pressure.max(relief.pressure.into());
    }

    fn observe(&mut self, phase: Phase, frame: &Frame<Arc<Pixmap>>, settled: bool, force: bool) {
        for page in &frame.pages {
            if let Some(error) = &page.error
                && self.rec.error_pages.insert(page.page.get())
            {
                self.rec.page_errors.push(PageError {
                    page: page.page.display_number(),
                    stage: "render".into(),
                    error: error.clone(),
                });
            }
        }
        let page = self.session.current_page().display_number();
        self.rec.last_page = self.rec.last_page.max(page);
        let mem = metrics::memory().unwrap_or_default();
        let stats = self.session.stats();
        let cached = self.monitor.manager().cache_bytes() as u64;
        let external = mem.private.saturating_sub(cached);
        let engine = engine_memory(self.engine);
        let reported = self.doc.memory_usage();
        self.rec.track(self.step, mem, external, stats.tile_bytes);
        self.rec.track_engine(engine);
        if let Some(r) = reported {
            self.rec.max_engine_reported = Some(self.rec.max_engine_reported.unwrap_or(0).max(r));
        }
        if !(force || self.step.is_multiple_of(self.config.sample_every)) {
            return;
        }
        self.rec.samples.push(Sample {
            step: self.step,
            phase,
            page,
            zoom_percent: self.session.zoom().percent(),
            t_s: (self.started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
            settled,
            private_mb: mib(mem.private),
            working_set_mb: mib(mem.working_set),
            external_mb: mib(external),
            engine_reported_mb: reported.map(mib),
            tile_mb: mib(stats.tile_bytes as u64),
            tile_entries: stats.tile_entries,
            tile_evictions: stats.tile_evictions,
            rendered_tiles: stats.scheduler.completed,
            queued: stats.scheduler.queued,
            in_flight: stats.scheduler.in_flight,
            cancelled: stats.scheduler.cancelled,
            discarded: stats.scheduler.discarded,
            failed: stats.scheduler.failed,
            pressure: self.monitor.last_pressure().into(),
            engine,
        });
    }

    /// Summarizes, then closes and drops everything to see what is freed.
    fn finish(self, scroll_back: ScrollBack, report: &mut ScrollReport) {
        let Self {
            config,
            engine,
            doc,
            mut session,
            monitor,
            started,
            step,
            rec,
            ..
        } = self;
        let elapsed = started.elapsed().as_secs_f64();
        let last = metrics::memory().unwrap_or_default();
        let stats = session.stats();
        let final_engine = engine_memory(engine);

        // The reader stops: prefetch finishes and is collected, then
        // nothing happens.
        wait_until_idle(&session, engine);
        let _ = session.frame();
        wait_until_idle(&session, engine);
        std::thread::sleep(IDLE_SETTLE);
        let after_idle = MemoryPoint::now(engine, Some(&doc));

        session.close();
        // Renders already running finish (hayro cannot interrupt them);
        // measure once they have, so the point shows what an open document
        // without tiles keeps.
        wait_until_idle(&session, engine);
        let after_close = MemoryPoint::now(engine, Some(&doc));
        drop(session); // joins the scheduler's workers
        drop(monitor);
        drop(doc);
        wait_for_engine_exit(engine);
        let after_drop = MemoryPoint::now(engine, None);

        let budget = mb_to_bytes(config.tile_budget_mb);
        let reliefs = Reliefs {
            soft: count(&rec.reliefs, Pressure::Soft),
            hard: count(&rec.reliefs, Pressure::Hard),
            freed_mb: mib(rec.freed_bytes),
        };
        let engine_summary = engine_budgets(engine).map(|(block, content)| EngineSummary {
            block_cache_budget_mb: mib(block),
            max_block_cache_mb: rec.max_block,
            block_cache_held: rec.max_block <= mib(block),
            content_budget_mb: mib(content),
            max_decoded_content_mb: rec.max_content,
            max_threads: rec.max_threads,
            max_generations: rec.max_generations,
            reopens: final_engine.map_or(0, |e| e.reopens),
            soft_trims: final_engine.map_or(0, |e| e.soft_trims),
            hard_trims: final_engine.map_or(0, |e| e.hard_trims),
        });
        report.summary = Some(Summary {
            steps: step,
            scroll_steps: rec.scroll_steps,
            zoom_steps: rec.zoom_steps,
            unsettled_steps: rec.unsettled,
            last_page: rec.last_page,
            elapsed_s: (elapsed * 10.0).round() / 10.0,
            max_private_mb: mib(rec.max_private),
            max_private_step: rec.max_private_step,
            final_private_mb: mib(last.private),
            max_working_set_mb: mib(rec.max_working_set),
            final_working_set_mb: mib(last.working_set),
            os_peak_working_set_mb: mib(last.peak_working_set.max(rec.max_working_set)),
            os_peak_private_mb: mib(last.peak_private.max(rec.max_private)),
            max_external_mb: mib(rec.max_external),
            max_engine_reported_mb: rec.max_engine_reported.map(mib),
            tile_budget_mb: mib(budget as u64),
            max_tile_mb: mib(rec.max_tile as u64),
            tile_budget_held: rec.max_tile <= budget,
            tile_evictions: stats.tile_evictions,
            rendered_tiles: stats.scheduler.completed,
            reliefs,
            max_pressure: rec.max_pressure,
            scroll_back,
            after_idle,
            after_close,
            after_drop,
            engine: engine_summary,
        });
        if rec.unsettled > 0 || !rec.page_errors.is_empty() {
            report.status = Status::Partial;
        }
        report.reliefs = rec.reliefs;
        report.page_errors = rec.page_errors;
        report.samples = rec.samples;
    }
}

fn count(events: &[ReliefEvent], pressure: Pressure) -> u64 {
    events.iter().filter(|e| e.pressure == pressure).count() as u64
}

/// Waits (bounded) until the session's scheduler has nothing queued or
/// rendering and the engine's render threads are idle too (a cancelled
/// tile's shared block keeps rendering).
fn wait_until_idle<V: Clone + Send + 'static>(session: &DocumentSession<V>, engine: &str) {
    let deadline = Instant::now() + IDLE_WAIT;
    while Instant::now() < deadline {
        let s = session.stats().scheduler;
        let engine_busy = engine_memory(engine).is_some_and(|m| m.busy_threads > 0);
        if s.queued == 0 && s.in_flight == 0 && !engine_busy {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The document's render threads exit asynchronously after it is dropped.
fn wait_for_engine_exit(engine: &str) {
    let deadline = Instant::now() + DROP_WAIT;
    while Instant::now() < deadline {
        match engine_memory(engine) {
            Some(m) if m.threads > 0 || m.documents > 0 => {}
            Some(_) => return,
            None => {
                std::thread::sleep(Duration::from_millis(250));
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpdf_engine_api::{PixelFormat, PixelSize, ResourceLimits, Rgba8};

    fn args(cmd: &str) -> Args {
        crate::args::parse(cmd.split_whitespace().map(std::ffi::OsString::from)).unwrap()
    }

    #[test]
    fn config_defaults_follow_the_app() {
        let c = ScriptConfig::from_args(&args("scroll a.pdf")).unwrap();
        assert_eq!(c.viewport, [1920, 1080]);
        assert_eq!((c.zoom_percent, c.peak_zoom_percent), (100.0, 400.0));
        assert_eq!(c.step_px, 972.0);
        assert_eq!(c.tile_budget_mb, 128);
        assert_eq!((c.soft_limit_mb, c.hard_limit_mb), (320, 512));
        assert_eq!(c.session().tile_budget, 128 * 1024 * 1024);
        assert_eq!(c.budget(), BudgetConfig::default());

        let c =
            ScriptConfig::from_args(&args("scroll a.pdf --zoom 500 --tile-budget-mb 16")).unwrap();
        assert_eq!((c.zoom_percent, c.peak_zoom_percent), (500.0, 1000.0));
        assert_eq!(c.session().tile_budget, 16 * 1024 * 1024);
        assert!(ScriptConfig::from_args(&args("scroll a.pdf --soft-limit-mb 600")).is_err());
        assert!(ScriptConfig::from_args(&args("scroll a.pdf --viewport 0x10")).is_err());
    }

    #[test]
    fn excursions_are_spread_over_the_document() {
        assert_eq!(excursion_pages(2000), vec![500, 1000, 1500]);
        assert_eq!(excursion_pages(20), vec![5, 10, 15]);
        assert_eq!(excursion_pages(1), vec![0]);
    }

    #[test]
    fn ink_counts_dark_pixels_inside_the_source_rect() {
        let limits = ResourceLimits::default();
        let mut pm = Pixmap::new(PixelSize::new(8, 8), PixelFormat::default(), &limits).unwrap();
        let mut target = pm.as_mut();
        target.fill(Rgba8::WHITE);
        // A 2x2 black square at (5, 5).
        let data = target.data_mut();
        for (x, y) in [(5, 5), (6, 5), (5, 6), (6, 6)] {
            data[(y * 8 + x) * 4..(y * 8 + x) * 4 + 3].fill(0);
        }
        assert_eq!(ink_pixels(&pm, [0.0, 0.0, 8.0, 8.0]), 4);
        assert_eq!(ink_pixels(&pm, [0.0, 0.0, 5.5, 5.5]), 1);
        assert_eq!(ink_pixels(&pm, [0.0, 0.0, 4.0, 8.0]), 0);
        assert_eq!(ink_pixels(&pm, [-3.0, -3.0, 100.0, 100.0]), 4);
    }

    /// A small document whose pages each carry a black bar.
    #[cfg(feature = "engine-hayro")]
    fn bars_pdf(pages: usize) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        let mut object = |out: &mut Vec<u8>, body: String| {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{body}\nendobj\n", offsets.len()).as_bytes());
        };
        object(&mut out, "<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
        object(
            &mut out,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {pages} >>",
                kids.join(" ")
            ),
        );
        for i in 0..pages {
            object(
                &mut out,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents {} 0 R >>",
                    4 + 2 * i
                ),
            );
            let content = format!("0 0 0 rg 20 20 {} 260 re f", 40 + 10 * i);
            object(
                &mut out,
                format!(
                    "<< /Length {} >>\nstream\n{content}\nendstream",
                    content.len()
                ),
            );
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
        for offset in &offsets {
            out.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[cfg(feature = "engine-hayro")]
    #[test]
    fn scripted_session_keeps_budgets_and_rerenders_after_eviction() {
        let path =
            std::env::temp_dir().join(format!("fastpdf-bench-scroll-{}.pdf", std::process::id()));
        std::fs::write(&path, bars_pdf(12)).unwrap();
        // A 2 MiB tile budget holds less than two pages; memory limits
        // below the test process's size make the monitor relieve pressure.
        let mut config = ScriptConfig::from_args(&args(
            "scroll x.pdf --viewport 400x300 --tile-budget-mb 2 --soft-limit-mb 1 \
             --hard-limit-mb 2 --sample-every 4 --settle 20",
        ))
        .unwrap();
        config.tile_size = 128;
        let engine = engines::select(Some("hayro")).unwrap();
        let report = scroll(engine.as_ref(), &path, None, config);
        let _ = std::fs::remove_file(&path);

        assert_eq!(report.status, Status::Ok, "{:?}", report.error);
        let s = report.summary.as_ref().unwrap();
        assert_eq!(s.unsettled_steps, 0);
        assert!(s.scroll_steps > 5, "{s:?}");
        // Three excursions of eight preset steps in and eight out.
        assert_eq!(s.zoom_steps, 48);
        assert_eq!(s.last_page, 12);
        assert!(s.tile_budget_held && s.max_tile_mb <= 2.0, "{s:?}");
        assert!(s.tile_evictions > 0);
        // The first poll happens on the first frame; the test process is a
        // few MiB, so it lands at soft or hard pressure.
        assert!(s.reliefs.soft + s.reliefs.hard >= 1, "{:?}", report.reliefs);
        assert!(s.max_pressure > Pressure::Normal);
        assert!(s.scroll_back.settled && s.scroll_back.rerendered_tiles > 0);
        assert!(!s.scroll_back.blank && s.scroll_back.exact_tiles > 0);
        assert!(s.max_engine_reported_mb.is_some_and(|mb| mb > 0.0));
        let e = s.engine.as_ref().unwrap();
        assert!(
            e.block_cache_held && e.soft_trims + e.hard_trims >= 1,
            "{e:?}"
        );
        assert!(!report.samples.is_empty());
        assert!(report.samples.iter().all(|x| x.tile_mb <= 2.0));
    }
}
