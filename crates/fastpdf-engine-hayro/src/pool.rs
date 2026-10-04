//! Per-document render threads.
//!
//! hayro's caches (`RenderCache`, `InterpreterCache`) are `!Send` and borrow
//! the `Pdf`, so they cannot live in the scheduler's worker threads between
//! calls without lifetime erasure. Instead every document owns a small pool
//! of threads whose stack frames own a generation of the `Pdf` *and* the
//! caches that borrow it. Callers (the scheduler's workers, the UI) submit a
//! job and wait for the result. The hop costs a few microseconds.
//!
//! The threads also get a large stack: hayro recurses for nested color
//! spaces, functions and XObjects, and a stack overflow cannot be contained
//! by `catch_unwind`. Together with the depth limit of the static scan this
//! keeps legitimate deep files working and stops hostile ones before they
//! reach the recursion.
//!
//! Threads are spawned on demand (up to `max_threads`), exit after
//! `IDLE_EXIT` without work (dropping their caches; the last one also drops
//! the document's finished blocks), and drop their caches early when the
//! document's cache epoch changes (memory pressure) or after too many
//! distinct pages.

use std::collections::{HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use fastpdf_engine_api::{CancelToken, EngineError, TextLayer};
use hayro::RenderCache;
use hayro::hayro_interpret::InterpreterCache;
use hayro::vello_cpu::{RenderContext, Resources};

use crate::document::{DocInner, Generation, lock};
use crate::render::{RenderJob, Rendered, execute_render};
use crate::stats;
use crate::text::execute_text;

/// Stack reserve of a render thread. Windows commits stack pages lazily, so
/// this costs address space, not memory.
const STACK_BYTES: usize = 64 * 1024 * 1024;
/// Idle time after which a thread exits and frees its caches. They are
/// large: hayro's glyph outlines of a CJK font, or vello's scene buffers
/// after a complex vector page, reach 50-70 MB per thread (benchmark B-5),
/// and an idle reader should not keep them. Respawning a thread takes well
/// under a millisecond; re-parsing a font takes a few.
#[cfg(not(test))]
const IDLE_EXIT: Duration = Duration::from_secs(5);
#[cfg(test)]
const IDLE_EXIT: Duration = Duration::from_millis(200);
/// Distinct pages a thread renders before it starts over with empty caches
/// (fonts and glyph outlines of documents that embed per-page font subsets
/// would otherwise accumulate without bound).
const MAX_PAGES_PER_CACHE: usize = 48;
/// Estimated bytes a render context keeps per pixel of the largest target
/// it rendered (vello keeps its buffers' capacity between renders).
/// Calibrated with benchmark B-5: ~3.3 MiB per thread after A4 pages at
/// 100% (0.9 MP targets).
const CONTEXT_BYTES_PER_PIXEL: u64 = 4;

pub(crate) struct TextJob {
    pub(crate) page: u32,
    pub(crate) cancel: CancelToken,
}

enum Job {
    Render(RenderJob, SyncSender<Result<Rendered, EngineError>>),
    Text(TextJob, SyncSender<Result<TextLayer, EngineError>>),
}

struct PoolState {
    queue: VecDeque<Job>,
    threads: usize,
    /// Threads executing a job. The others are waiting for one or about to
    /// (resetting their caches after a memory trim), so they will pick up
    /// queued jobs without a new thread.
    busy: usize,
    shutdown: bool,
    spawned: u64,
}

struct PoolShared {
    doc: Arc<DocInner>,
    state: Mutex<PoolState>,
    wake: Condvar,
    max_threads: usize,
}

/// Owner handle; dropping the document shuts the pool down.
pub(crate) struct Pool {
    shared: Arc<PoolShared>,
}

impl Pool {
    pub(crate) fn new(doc: Arc<DocInner>) -> Self {
        let max_threads = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 8))
            .unwrap_or(2);
        Self {
            shared: Arc::new(PoolShared {
                doc,
                state: Mutex::new(PoolState {
                    queue: VecDeque::new(),
                    threads: 0,
                    busy: 0,
                    shutdown: false,
                    spawned: 0,
                }),
                wake: Condvar::new(),
                max_threads,
            }),
        }
    }

    pub(crate) fn render(&self, job: RenderJob) -> Result<Rendered, EngineError> {
        let (tx, rx) = sync_channel(1);
        self.submit(Job::Render(job, tx))?;
        rx.recv()
            .unwrap_or_else(|_| Err(EngineError::Internal("render thread exited".into())))
    }

    pub(crate) fn text(&self, job: TextJob) -> Result<TextLayer, EngineError> {
        let (tx, rx) = sync_channel(1);
        self.submit(Job::Text(job, tx))?;
        rx.recv()
            .unwrap_or_else(|_| Err(EngineError::Internal("render thread exited".into())))
    }

    fn submit(&self, job: Job) -> Result<(), EngineError> {
        let mut state = lock(&self.shared.state);
        if state.shutdown {
            return Err(EngineError::Cancelled);
        }
        state.queue.push_back(job);
        // Spawn only when there are more jobs than threads that are not
        // busy. Counting only threads parked on the condvar would spawn
        // extras whenever a trim (`release_idle_threads`) briefly wakes them
        // all, exactly when memory is scarce.
        let available = state.threads.saturating_sub(state.busy);
        if state.queue.len() > available && state.threads < self.shared.max_threads {
            state.threads += 1;
            state.spawned += 1;
            let n = state.spawned;
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name(format!("fastpdf-hayro-{n}"))
                .stack_size(STACK_BYTES)
                .spawn(move || thread_main(&shared));
            if spawned.is_err() {
                state.threads -= 1;
                if state.threads == 0 {
                    // Nobody would ever pick the job up.
                    state.queue.pop_back();
                    return Err(EngineError::Internal("cannot start render thread".into()));
                }
            }
        }
        drop(state);
        self.shared.wake.notify_one();
        Ok(())
    }

    /// Wakes idle threads so they drop their caches (and a replaced `Pdf`
    /// generation) now instead of at their next job; used under memory
    /// pressure after the cache epoch was bumped.
    pub(crate) fn release_idle_threads(&self) {
        self.shared.wake.notify_all();
    }

    pub(crate) fn shutdown(&self) {
        let mut state = lock(&self.shared.state);
        state.shutdown = true;
        // Dropping the queued jobs drops their reply senders, which wakes
        // their callers with an error.
        state.queue.clear();
        drop(state);
        self.shared.wake.notify_all();
    }
}

enum Served {
    Exit,
    NewGeneration,
}

fn thread_main(shared: &PoolShared) {
    let _live = stats::Live::thread();
    loop {
        let generation = shared.doc.current();
        match serve(shared, &generation) {
            Served::Exit => break,
            Served::NewGeneration => {}
        }
    }
}

/// Per-thread hayro state borrowing one `Pdf` generation.
pub(crate) struct ThreadCaches<'p> {
    epoch: u64,
    pub(crate) render: RenderCache<'p>,
    pub(crate) interp: InterpreterCache<'p>,
    pub(crate) context: Option<RenderContext>,
    pub(crate) resources: Resources,
    pages: HashSet<u32>,
    /// Estimated bytes of `context`, counted in the document's gauge.
    context_bytes: u64,
    gauge: Arc<AtomicU64>,
}

impl ThreadCaches<'_> {
    fn new(epoch: u64, gauge: Arc<AtomicU64>) -> Self {
        Self {
            epoch,
            render: RenderCache::new(),
            interp: InterpreterCache::new(),
            context: None,
            resources: Resources::default(),
            pages: HashSet::new(),
            context_bytes: 0,
            gauge,
        }
    }

    pub(crate) fn note_page(&mut self, page: u32) {
        self.pages.insert(page);
    }

    /// Records that the render context now has buffers for a target of
    /// `width x height` pixels.
    pub(crate) fn note_context_size(&mut self, width: u32, height: u32) {
        let bytes = u64::from(width) * u64::from(height) * CONTEXT_BYTES_PER_PIXEL;
        if bytes > self.context_bytes {
            let grown = bytes - self.context_bytes;
            stats::add(&self.gauge, grown);
            stats::add(&stats::counters().context_bytes, grown);
            self.context_bytes = bytes;
        }
    }
}

impl Drop for ThreadCaches<'_> {
    fn drop(&mut self) {
        stats::sub(&self.gauge, self.context_bytes);
        stats::sub(&stats::counters().context_bytes, self.context_bytes);
    }
}

enum Next {
    /// A job; the thread counts as busy until the `Busy` guard is dropped.
    Run(Job),
    /// Woken without work while the cache epoch changed: drop caches now.
    Reset,
    Exit,
}

fn next_job(shared: &PoolShared, epoch_seen: u64) -> Next {
    let mut state = lock(&shared.state);
    loop {
        if state.shutdown {
            state.threads -= 1;
            return Next::Exit;
        }
        if let Some(job) = state.queue.pop_front() {
            state.busy += 1;
            stats::add(&stats::counters().busy_threads, 1);
            return Next::Run(job);
        }
        if shared.doc.cache_epoch() != epoch_seen {
            return Next::Reset;
        }
        let (guard, timeout) = shared
            .wake
            .wait_timeout(state, IDLE_EXIT)
            .unwrap_or_else(|e| e.into_inner());
        state = guard;
        if timeout.timed_out() && state.queue.is_empty() {
            state.threads -= 1;
            let last = state.threads == 0;
            drop(state);
            if last {
                // Nothing is rendering any more, so no neighbouring tile
                // will come for the finished blocks.
                shared.doc.blocks.clear();
            }
            return Next::Exit;
        }
    }
}

/// Marks the thread as no longer busy when dropped.
struct Busy<'s>(&'s PoolShared);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.busy = state.busy.saturating_sub(1);
        stats::sub(&stats::counters().busy_threads, 1);
    }
}

fn serve(shared: &PoolShared, generation: &Generation) -> Served {
    let doc = &*shared.doc;
    let mut caches = ThreadCaches::new(doc.cache_epoch(), doc.context_gauge());
    loop {
        let (job, busy) = match next_job(shared, caches.epoch) {
            Next::Run(job) => (job, Busy(shared)),
            Next::Reset => {
                if doc.reopen_pending() || doc.current_id() != generation.id {
                    return Served::NewGeneration;
                }
                caches = ThreadCaches::new(doc.cache_epoch(), doc.context_gauge());
                continue;
            }
            Next::Exit => return Served::Exit,
        };
        let epoch = doc.cache_epoch();
        if caches.epoch != epoch || caches.pages.len() >= MAX_PAGES_PER_CACHE {
            caches = ThreadCaches::new(epoch, doc.context_gauge());
        }
        // The thread stops counting as busy before it replies, so a caller
        // that submits its next job right away does not spawn a thread.
        let panicked = match job {
            Job::Render(job, reply) => {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    execute_render(doc, generation, &mut caches, &job)
                }));
                let panicked = r.is_err();
                drop(busy);
                let _ = reply.send(r.unwrap_or_else(|p| Err(panic_error(p.as_ref()))));
                panicked
            }
            Job::Text(job, reply) => {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    execute_text(doc, generation, &mut caches, &job)
                }));
                let panicked = r.is_err();
                drop(busy);
                let _ = reply.send(r.unwrap_or_else(|p| Err(panic_error(p.as_ref()))));
                panicked
            }
        };
        if panicked {
            // hayro state touched by the panic may be inconsistent and its
            // locks poisoned: start over with fresh caches and a fresh `Pdf`.
            caches = ThreadCaches::new(doc.cache_epoch(), doc.context_gauge());
            doc.request_reopen();
        }
        if doc.current_id() != generation.id || panicked {
            drop(caches);
            return Served::NewGeneration;
        }
    }
}

fn panic_error(payload: &(dyn std::any::Any + Send)) -> EngineError {
    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    };
    EngineError::Panicked(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::open_pdf;
    use fastpdf_engine_api::{
        PixelRect, RenderScale, ResourceLimits, Rgba8, Rotation, SharedBytes,
    };

    /// One 100x100 pt page with a black square.
    fn tiny_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
            "<< /Length 20 >>\nstream\n0 g 10 10 50 50 re f\nendstream",
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        let size = objects.len() + 1;
        out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for offset in offsets {
            out.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        out
    }

    #[test]
    fn idle_pools_release_threads_and_blocks() {
        use fastpdf_engine_api::{
            CancelToken, PageIndex, PageSize, PixelFormat, Pixmap, RenderRequest,
        };

        let bytes = SharedBytes::from_vec(tiny_pdf());
        let pdf = open_pdf(&bytes, "").unwrap();
        let doc = Arc::new(DocInner::new(
            bytes,
            String::new(),
            ResourceLimits::default(),
            pdf,
        ));
        let pool = Pool::new(Arc::clone(&doc));
        let request = RenderRequest::full_page(
            PageIndex::FIRST,
            PageSize::new(100.0, 100.0),
            Rotation::R0,
            Rotation::R0,
            RenderScale::new(4.0).unwrap(),
        )
        .with_region(PixelRect::new(0, 0, 64, 64));
        let limits = ResourceLimits::default();
        let mut tile = Pixmap::new(request.region.size(), PixelFormat::default(), &limits).unwrap();
        crate::render::render(
            &doc,
            &pool,
            &request,
            &mut tile.as_mut(),
            &CancelToken::new(),
        )
        .unwrap();
        // The tile came out of a cached block.
        assert!(doc.blocks.bytes() > 64 * 64 * 4);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while lock(&pool.shared.state).threads > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(lock(&pool.shared.state).threads, 0);
        assert_eq!(doc.blocks.bytes(), 0);
        // The pool still works afterwards.
        crate::render::render(
            &doc,
            &pool,
            &request,
            &mut tile.as_mut(),
            &CancelToken::new(),
        )
        .unwrap();
    }

    #[test]
    fn sequential_jobs_and_trims_use_one_thread() {
        let bytes = SharedBytes::from_vec(tiny_pdf());
        let pdf = open_pdf(&bytes, "").unwrap();
        let doc = Arc::new(DocInner::new(
            bytes,
            String::new(),
            ResourceLimits::default(),
            pdf,
        ));
        let pool = Pool::new(Arc::clone(&doc));
        let job = RenderJob {
            page: 0,
            scale: RenderScale::new(1.0).unwrap(),
            rotation: Rotation::R0,
            rect: PixelRect::new(0, 0, 64, 64),
            background: Rgba8::WHITE,
            annotations: true,
            cancel: None,
        };
        for i in 0..40 {
            let rendered = pool.render(job.clone()).unwrap();
            assert_eq!(rendered.data.len(), 64 * 64 * 4);
            if i % 4 == 0 {
                // What `trim_memory(Soft)` does.
                doc.bump_cache_epoch();
                pool.release_idle_threads();
            }
        }
        let state = lock(&pool.shared.state);
        assert_eq!((state.threads, state.spawned), (1, 1));
        assert_eq!(state.busy, 0);
    }
}
