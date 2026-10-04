//! Bounded worker pool that renders planned tiles by priority (spec §14, §18).
//!
//! * Workers never run on the UI thread.
//! * The worker count is fixed and small: latency matters more than
//!   throughput, and a 32-core machine must not render 32 tiles at once.
//! * Every new plan replaces the queue of its lane: queued jobs the view no
//!   longer needs are discarded, and in-flight ones are cancelled.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fastpdf_engine_api::{
    CancelToken, EngineDocument, EngineError, PixelFormat, Pixmap, RenderRequest, ResourceLimits,
};

use crate::{Priority, RenderJob, TileKey};

/// Independent streams of work. Submitting a plan replaces only the queued
/// work of its own lane, so a new viewport plan never cancels thumbnails or
/// background work (and vice versa). All lanes share one priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    /// Tiles for the document view (P0–P3).
    Viewport,
    /// Sidebar thumbnails (P4).
    Thumbnails,
    /// Anything else (P5).
    Background,
}

/// Scheduler settings.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerConfig {
    pub workers: usize,
    pub pixel_format: PixelFormat,
    pub limits: ResourceLimits,
}

impl SchedulerConfig {
    /// Two workers: B-3/B-4 (docs/benchmarks/b3-b4-tiles-workers.md)
    /// measured no viewport-fill latency gain from 4, 6 or 8 workers, while
    /// every extra render thread keeps its own engine caches (spec §18:
    /// latency first, not throughput).
    pub fn default_workers() -> usize {
        2
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            workers: Self::default_workers(),
            pixel_format: PixelFormat::default(),
            limits: ResourceLimits::default(),
        }
    }
}

/// A finished (or failed) job. Cancelled jobs produce no result.
#[derive(Debug)]
pub struct TileResult<K = TileKey> {
    pub key: K,
    pub lane: Lane,
    pub priority: Priority,
    /// Plan generation the job was submitted with; results from older
    /// generations may be stale.
    pub generation: u64,
    pub result: Result<Pixmap, EngineError>,
    /// Time spent waiting in the queue.
    pub queued: Duration,
    /// Time spent rendering.
    pub rendered: Duration,
}

/// Counters for the development overlay (spec §46).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchedulerStats {
    pub queued: usize,
    pub in_flight: usize,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub discarded: u64,
}

type Sink<K> = Box<dyn Fn(TileResult<K>) + Send + Sync>;

struct Job<K> {
    key: K,
    lane: Lane,
    priority: Priority,
    distance: f32,
    seq: u64,
    generation: u64,
    request: RenderRequest,
    enqueued: Instant,
}

impl<K> Job<K> {
    /// Larger = more urgent (BinaryHeap is a max-heap).
    fn urgency_cmp(&self, other: &Self) -> Ordering {
        other
            .priority
            .cmp(&self.priority)
            .then(other.distance.total_cmp(&self.distance))
            .then(other.seq.cmp(&self.seq))
    }
}

impl<K> PartialEq for Job<K> {
    fn eq(&self, other: &Self) -> bool {
        self.urgency_cmp(other) == Ordering::Equal
    }
}
impl<K> Eq for Job<K> {}
impl<K> PartialOrd for Job<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<K> Ord for Job<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.urgency_cmp(other)
    }
}

struct State<K> {
    queue: BinaryHeap<Job<K>>,
    in_flight: HashMap<K, (Lane, CancelToken)>,
    seq: u64,
    generation: u64,
    shutdown: bool,
    stats: SchedulerStats,
}

impl<K> Default for State<K> {
    fn default() -> Self {
        Self {
            queue: BinaryHeap::new(),
            in_flight: HashMap::new(),
            seq: 0,
            generation: 0,
            shutdown: false,
            stats: SchedulerStats::default(),
        }
    }
}

struct Shared<K> {
    state: Mutex<State<K>>,
    wake: Condvar,
    document: Arc<dyn EngineDocument>,
    config: SchedulerConfig,
    sink: Sink<K>,
}

impl<K> Shared<K> {
    fn lock(&self) -> MutexGuard<'_, State<K>> {
        // Workers never panic while holding the lock (engine calls happen
        // outside it), but recover anyway rather than cascade.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Renders jobs on a fixed pool of background threads. `K` identifies a
/// job (a [`TileKey`] for the document view, a thumbnail key, ...).
pub struct RenderScheduler<K = TileKey> {
    shared: Arc<Shared<K>>,
    workers: Vec<JoinHandle<()>>,
}

impl<K: Copy + Eq + Hash + Send + 'static> fmt::Debug for RenderScheduler<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderScheduler")
            .field("workers", &self.workers.len())
            .field("stats", &self.stats())
            .finish()
    }
}

impl<K: Copy + Eq + Hash + Send + 'static> RenderScheduler<K> {
    /// Starts the worker pool. `sink` receives results on worker threads and
    /// should only hand them off (e.g. push to a channel and wake the UI).
    pub fn new(
        document: Arc<dyn EngineDocument>,
        config: SchedulerConfig,
        sink: impl Fn(TileResult<K>) + Send + Sync + 'static,
    ) -> Self {
        let workers = config.workers.max(1);
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            document,
            config,
            sink: Box::new(sink),
        });
        let workers = (0..workers)
            .filter_map(|i| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("fastpdf-render-{i}"))
                    .spawn(move || worker_loop(&shared))
                    .ok()
            })
            .collect();
        Self { shared, workers }
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Replaces the pending viewport work with `plan`; shorthand for
    /// `submit(Lane::Viewport, plan)`.
    pub fn submit_plan(&self, plan: Vec<RenderJob<K>>) -> u64 {
        self.submit(Lane::Viewport, plan)
    }

    /// Replaces the pending work of `lane` with `plan` (already filtered of
    /// jobs whose results the caller has cached): queued jobs of that lane
    /// are discarded, in-flight ones not in the plan are cancelled, in-flight
    /// ones still wanted keep running. Returns the new plan generation.
    pub fn submit(&self, lane: Lane, plan: Vec<RenderJob<K>>) -> u64 {
        let mut state = self.shared.lock();
        state.generation += 1;
        let generation = state.generation;

        let wanted: HashSet<K> = plan.iter().map(|t| t.key).collect();
        for (key, (job_lane, token)) in &state.in_flight {
            if *job_lane == lane && !wanted.contains(key) {
                token.cancel();
            }
        }
        let before = state.queue.len();
        state.queue.retain(|job| job.lane != lane);
        let discarded = (before - state.queue.len()) as u64;
        state.stats.discarded += discarded;

        let now = Instant::now();
        for job in plan {
            if state.in_flight.contains_key(&job.key) {
                continue; // already rendering; keep it
            }
            state.seq += 1;
            let seq = state.seq;
            state.queue.push(Job {
                key: job.key,
                lane,
                priority: job.priority,
                distance: job.distance,
                seq,
                generation,
                request: job.request,
                enqueued: now,
            });
        }
        drop(state);
        self.shared.wake.notify_all();
        generation
    }

    /// Drops all queued work and cancels everything in flight, in every lane.
    pub fn cancel_all(&self) {
        for lane in [Lane::Viewport, Lane::Thumbnails, Lane::Background] {
            self.submit(lane, Vec::new());
        }
    }

    pub fn stats(&self) -> SchedulerStats {
        let state = self.shared.lock();
        SchedulerStats {
            queued: state.queue.len(),
            in_flight: state.in_flight.len(),
            ..state.stats
        }
    }

    /// True when nothing is queued or rendering.
    pub fn is_idle(&self) -> bool {
        let state = self.shared.lock();
        state.queue.is_empty() && state.in_flight.is_empty()
    }

    /// True when `lane` has nothing queued or rendering.
    pub fn is_lane_idle(&self, lane: Lane) -> bool {
        let state = self.shared.lock();
        !state.queue.iter().any(|j| j.lane == lane)
            && !state.in_flight.values().any(|(l, _)| *l == lane)
    }
}

impl<K> Drop for RenderScheduler<K> {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock();
            state.shutdown = true;
            state.queue.clear();
            for (_, token) in state.in_flight.values() {
                token.cancel();
            }
        }
        self.shared.wake.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker_loop<K: Copy + Eq + Hash>(shared: &Shared<K>) {
    loop {
        let (job, cancel) = {
            let mut state = shared.lock();
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(job) = state.queue.pop() {
                    let cancel = CancelToken::new();
                    state.in_flight.insert(job.key, (job.lane, cancel.clone()));
                    break (job, cancel);
                }
                state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };

        let queued = job.enqueued.elapsed();
        let started = Instant::now();
        let result = render_job(shared, &job.request, &cancel);
        let rendered = started.elapsed();

        {
            let mut state = shared.lock();
            state.in_flight.remove(&job.key);
            match &result {
                Ok(_) => state.stats.completed += 1,
                Err(EngineError::Cancelled) => state.stats.cancelled += 1,
                Err(_) => state.stats.failed += 1,
            }
        }
        if matches!(result, Err(EngineError::Cancelled)) {
            continue;
        }
        (shared.sink)(TileResult {
            key: job.key,
            lane: job.lane,
            priority: job.priority,
            generation: job.generation,
            result,
            queued,
            rendered,
        });
    }
}

fn render_job<K>(
    shared: &Shared<K>,
    request: &RenderRequest,
    cancel: &CancelToken,
) -> Result<Pixmap, EngineError> {
    cancel.check()?;
    let mut pixmap = Pixmap::new(
        request.region.size(),
        shared.config.pixel_format,
        &shared.config.limits,
    )?;
    shared
        .document
        .render(request, &mut pixmap.as_mut(), cancel)?;
    Ok(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentLayout, PlanConfig, PlannedTile, Viewport, ZoomLevel, plan_tiles};
    use fastpdf_engine_api::{
        DocumentId, PageIndex, PageInfo, PageSize, PixmapMut, RenderOutcome, Rotation,
    };
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
    use std::sync::mpsc;

    /// Renders after a delay so cancellation can be observed; fails page 3.
    struct SlowDoc {
        delay: Duration,
        renders: AtomicU32,
    }

    impl EngineDocument for SlowDoc {
        fn page_count(&self) -> u32 {
            100
        }

        fn page_info(&self, _: PageIndex) -> Result<PageInfo, EngineError> {
            Ok(PageInfo {
                size: PageSize::LETTER,
                rotation: Rotation::R0,
            })
        }

        fn render(
            &self,
            request: &RenderRequest,
            target: &mut PixmapMut<'_>,
            cancel: &CancelToken,
        ) -> Result<RenderOutcome, EngineError> {
            self.renders.fetch_add(1, AtomicOrdering::Relaxed);
            let deadline = Instant::now() + self.delay;
            while Instant::now() < deadline {
                cancel.check()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            if request.page == PageIndex::new(3) {
                return Err(EngineError::Malformed("broken page".into()));
            }
            target.fill(request.background);
            Ok(RenderOutcome::default())
        }
    }

    fn plan_at(scroll_y: f64) -> Vec<PlannedTile> {
        let layout = DocumentLayout::new(100, PageSize::LETTER, 8.0);
        let mut viewport = Viewport::new(1280.0, 720.0, ZoomLevel::ACTUAL_SIZE, 1.0);
        viewport.scroll_y = scroll_y;
        let info = |_| {
            Some(PageInfo {
                size: PageSize::LETTER,
                rotation: Rotation::R0,
            })
        };
        plan_tiles(
            DocumentId::from_raw(7),
            &layout,
            &viewport,
            info,
            &PlanConfig::default(),
        )
    }

    fn scheduler(
        delay_ms: u64,
        workers: usize,
    ) -> (RenderScheduler, mpsc::Receiver<TileResult>, Arc<SlowDoc>) {
        let doc = Arc::new(SlowDoc {
            delay: Duration::from_millis(delay_ms),
            renders: AtomicU32::new(0),
        });
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let config = SchedulerConfig {
            workers,
            ..SchedulerConfig::default()
        };
        let s = RenderScheduler::new(doc.clone(), config, move |r| {
            let _ = tx.lock().unwrap().send(r);
        });
        (s, rx, doc)
    }

    #[test]
    fn renders_every_planned_tile_once() {
        let (s, rx, _) = scheduler(0, 2);
        let plan = plan_at(0.0);
        let n = plan.len();
        s.submit_plan(plan);
        let results: Vec<_> = (0..n)
            .map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap())
            .collect();
        assert!(results.iter().all(|r| r.result.is_ok()));
        let unique: HashSet<_> = results.iter().map(|r| r.key).collect();
        assert_eq!(unique.len(), n);
        assert_eq!(s.stats().completed, n as u64);
    }

    #[test]
    fn first_results_are_visible_tiles() {
        let (s, rx, _) = scheduler(1, 1);
        s.submit_plan(plan_at(0.0));
        let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(first.priority, Priority::Visible);
    }

    #[test]
    fn fast_scrolling_cancels_stale_work() {
        let (s, rx, doc) = scheduler(30, 2);
        s.submit_plan(plan_at(0.0));
        std::thread::sleep(Duration::from_millis(10));
        // The user flings to page ~60; nothing near page 1 is wanted anymore.
        let far = plan_at(60.0 * 800.0);
        let far_keys: HashSet<_> = far.iter().map(|t| t.key).collect();
        s.submit_plan(far);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !s.is_idle() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let delivered: Vec<_> = rx.try_iter().collect();
        assert!(delivered.iter().all(|r| far_keys.contains(&r.key)));
        let stats = s.stats();
        assert!(stats.discarded > 0);
        assert!(stats.cancelled >= 1, "{stats:?}");
        // Far fewer renders than both plans combined.
        assert!(doc.renders.load(AtomicOrdering::Relaxed) as usize <= far_keys.len() + 2);
    }

    #[test]
    fn failures_are_reported_not_fatal() {
        let (s, rx, _) = scheduler(0, 2);
        // Page 3 starts at 8 + 3 * 800.
        let plan = plan_at(3.0 * 800.0 + 8.0);
        let n = plan.len();
        s.submit_plan(plan);
        let results: Vec<_> = (0..n)
            .map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap())
            .collect();
        assert!(results.iter().any(|r| r.result.is_err()));
        assert!(results.iter().any(|r| r.result.is_ok()));
        assert!(s.stats().failed > 0);
    }

    #[test]
    fn lanes_do_not_cancel_each_other() {
        let (s, rx, _) = scheduler(5, 1);
        let thumbs: Vec<PlannedTile> = plan_at(0.0)
            .into_iter()
            .take(3)
            .map(|mut t| {
                t.priority = Priority::Thumbnail;
                t.key.tile_size = 1; // distinct keys from the viewport plan
                t
            })
            .collect();
        s.submit(Lane::Thumbnails, thumbs);
        // A new (empty) viewport plan must not discard the thumbnails.
        s.submit_plan(Vec::new());
        let results: Vec<_> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap())
            .collect();
        assert!(
            results
                .iter()
                .all(|r| r.lane == Lane::Thumbnails && r.result.is_ok())
        );
        assert!(s.is_lane_idle(Lane::Thumbnails));
    }

    #[test]
    fn drop_joins_workers() {
        let (s, _rx, _) = scheduler(50, 4);
        s.submit_plan(plan_at(0.0));
        let started = Instant::now();
        drop(s);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
