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
//! `IDLE_EXIT` without work (dropping their caches), and drop their caches
//! early when the document's cache epoch changes (memory pressure) or after
//! too many distinct pages.

use std::collections::{HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use fastpdf_engine_api::{CancelToken, EngineError, TextLayer};
use hayro::RenderCache;
use hayro::hayro_interpret::InterpreterCache;
use hayro::vello_cpu::{RenderContext, Resources};

use crate::document::{DocInner, Generation, lock};
use crate::render::{RenderJob, Rendered, execute_render};
use crate::text::execute_text;

/// Stack reserve of a render thread. Windows commits stack pages lazily, so
/// this costs address space, not memory.
const STACK_BYTES: usize = 64 * 1024 * 1024;
/// Idle time after which a thread exits and frees its caches.
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// Distinct pages a thread renders before it starts over with empty caches
/// (fonts and glyph outlines of documents that embed per-page font subsets
/// would otherwise accumulate without bound).
const MAX_PAGES_PER_CACHE: usize = 48;

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
    idle: usize,
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
                    idle: 0,
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
        // Spawn when no thread is idle (each idle thread takes one job).
        if state.idle < state.queue.len() && state.threads < self.shared.max_threads {
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
}

impl ThreadCaches<'_> {
    fn new(epoch: u64) -> Self {
        Self {
            epoch,
            render: RenderCache::new(),
            interp: InterpreterCache::new(),
            context: None,
            resources: Resources::default(),
            pages: HashSet::new(),
        }
    }

    pub(crate) fn note_page(&mut self, page: u32) {
        self.pages.insert(page);
    }
}

enum Next {
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
            return Next::Run(job);
        }
        if shared.doc.cache_epoch() != epoch_seen {
            return Next::Reset;
        }
        state.idle += 1;
        let (guard, timeout) = shared
            .wake
            .wait_timeout(state, IDLE_EXIT)
            .unwrap_or_else(|e| e.into_inner());
        state = guard;
        state.idle -= 1;
        if timeout.timed_out() && state.queue.is_empty() {
            state.threads -= 1;
            return Next::Exit;
        }
    }
}

fn serve(shared: &PoolShared, generation: &Generation) -> Served {
    let doc = &*shared.doc;
    let mut caches = ThreadCaches::new(doc.cache_epoch());
    loop {
        let job = match next_job(shared, caches.epoch) {
            Next::Run(job) => job,
            Next::Reset => {
                if doc.reopen_pending() || doc.current_id() != generation.id {
                    return Served::NewGeneration;
                }
                caches = ThreadCaches::new(doc.cache_epoch());
                continue;
            }
            Next::Exit => return Served::Exit,
        };
        let epoch = doc.cache_epoch();
        if caches.epoch != epoch || caches.pages.len() >= MAX_PAGES_PER_CACHE {
            caches = ThreadCaches::new(epoch);
        }
        let panicked = match job {
            Job::Render(job, reply) => {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    execute_render(doc, generation, &mut caches, &job)
                }));
                let panicked = r.is_err();
                let _ = reply.send(r.unwrap_or_else(|p| Err(panic_error(p.as_ref()))));
                panicked
            }
            Job::Text(job, reply) => {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    execute_text(doc, generation, &mut caches, &job)
                }));
                let panicked = r.is_err();
                let _ = reply.send(r.unwrap_or_else(|p| Err(panic_error(p.as_ref()))));
                panicked
            }
        };
        if panicked {
            // hayro state touched by the panic may be inconsistent and its
            // locks poisoned: start over with fresh caches and a fresh `Pdf`.
            caches = ThreadCaches::new(doc.cache_epoch());
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
