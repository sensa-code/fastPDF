//! [`RemoteEngine`] and its documents: `PdfEngine` and `EngineDocument`
//! served by render host processes, one per open document (ADR 0008 §1.1).
//!
//! The caller wraps documents in `open_guarded` exactly as with an
//! in-process engine; the parent-side `GuardedDocument` keeps validating
//! requests and remains a second line of defense.
//!
//! Page geometry is special: FastPDF's UI thread asks for it while laying
//! out pages. It is fetched right after opening (the first pages before
//! `open` returns, the rest by a background thread) into a cache that
//! outlives host restarts, so `page_info` normally answers without any IPC.
//! A page that is not known yet is requested directly from the host's
//! geometry threads — bypassing the admission gate, the state lock and the
//! render queue — and the UI thread waits at most [`UI_GEOMETRY_WAIT`].
//!
//! Errors that come from the host's supervision rather than from the
//! engine are typed so that callers can retry them (ADR 0008 §2):
//! `EngineError::HostExited` when the host died while handling the request
//! (with the reason; `permanent` once the request is not sent to a host
//! again, or the document stopped restarting hosts), and
//! `EngineError::Unavailable` when no host could answer in time (restart
//! failed, no answer, geometry wait exceeded).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{
    CancelToken, DocumentMetadata, DocumentSource, EngineDocument, EngineError, EngineInfo,
    FileOrigin, HostExit, HostExitReason, HostStatus, Link, MemoryPressure, OpenOptions,
    OutlineItem, PageIndex, PageInfo, PdfEngine, PixmapMut, RenderOutcome, RenderRequest,
    ResourceLimits, TextLayer,
};

use crate::HostStats;
use crate::client::{CallError, Connection, Expect, Request, TargetLease, lock};
use crate::config::RemoteConfig;
use crate::gate::Gate;
use crate::geometry::Geometry;
use crate::policy::{DeathCause, Ledger, Subject, Verdict};
use crate::protocol::{
    Command, DocumentRef, MAX_NAME, MAX_SLOT_BYTES, MAX_SLOTS, Open, Payload, Render,
    WireEngineInfo,
};
use crate::win::section::{Access, Section};

/// The first attempt plus retries after host deaths.
const MAX_ATTEMPTS: usize = 3;
/// Pages whose geometry `open` fetches before returning: the first screen
/// (and its thumbnails) never needs a geometry request.
const GEOMETRY_PREFIX: u32 = 64;
/// Pages per background geometry request.
const GEOMETRY_BATCH: u32 = 1024;
/// Longest the UI thread waits in `page_info` for a page that is not known
/// yet (only possible before the background fetch has finished).
pub(crate) const UI_GEOMETRY_WAIT: Duration = Duration::from_secs(1);
/// How often a waiting `page_info` looks for a restarted host.
const GEOMETRY_POLL: Duration = Duration::from_millis(20);
/// Verdict maps kept for late callers of a dead connection.
const KEPT_VERDICTS: usize = 8;
/// Waiting for a background start: its own timeout plus this.
const START_GRACE: Duration = Duration::from_secs(1);
/// Delay before a taken spare host is replaced.
const REFILL_DELAY: Duration = Duration::from_secs(1);

/// A [`PdfEngine`] whose documents live in render host processes.
///
/// Every document gets its own host, created inside a job object with a
/// memory limit and `KILL_ON_JOB_CLOSE` (ADR 0008 §3). A host that
/// crashes, exceeds its memory limit or hangs past a request's deadline is
/// replaced transparently; pages that keep crashing fail permanently and a
/// crash storm stops automatic restarts (see [`crate::CrashPolicy`]).
pub struct RemoteEngine {
    shared: Arc<EngineShared>,
}

impl std::fmt::Debug for RemoteEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEngine")
            .field("engine", &self.shared.info.name)
            .field("program", &self.shared.config.program)
            .finish_non_exhaustive()
    }
}

struct EngineShared {
    config: RemoteConfig,
    info: EngineInfo,
    wire_info: WireEngineInfo,
    spare: Mutex<Spare>,
    /// Signalled when a background start ends.
    spare_done: Condvar,
    generation: AtomicU64,
    documents: Mutex<Vec<Weak<DocShared>>>,
}

/// A started host without a document, ready for the next open or restart
/// (ADR 0008 §5), and the background start of the next one.
#[derive(Default)]
struct Spare {
    conn: Option<Arc<Connection>>,
    /// A host is being started in the background.
    starting: bool,
    /// How the first start ended; `None` while it runs.
    first: Option<Result<(), EngineError>>,
}

fn validate(config: &RemoteConfig) -> Result<(), EngineError> {
    let bad = |what: &str| {
        Err(EngineError::InvalidRequest(format!(
            "remote engine configuration: {what}"
        )))
    };
    if config.engine.is_empty() || config.engine.len() > MAX_NAME {
        return bad("engine name");
    }
    if config.slots > MAX_SLOTS {
        return bad("too many slots");
    }
    if config.slots > 0 && (config.slot_bytes == 0 || config.slot_bytes as u64 > MAX_SLOT_BYTES) {
        return bad("slot size");
    }
    Ok(())
}

/// `EngineInfo` wants `&'static str`; the strings come from the host. Each
/// distinct value is leaked once (names are short and few).
fn intern(s: &str) -> &'static str {
    static TABLE: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut table = lock(&TABLE);
    if let Some(found) = table.iter().copied().find(|t| *t == s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    table.push(leaked);
    leaked
}

impl RemoteEngine {
    /// Starts a render host (which proves that the executable runs and
    /// speaks this protocol) and keeps it ready for the first document.
    /// Blocks until the host is ready; see [`RemoteEngine::start`].
    pub fn new(config: RemoteConfig) -> Result<Self, EngineError> {
        validate(&config)?;
        let first = Connection::spawn(&config, 1)?;
        let wire_info = first.info().clone();
        let info = EngineInfo {
            name: intern(&wire_info.name),
            version: intern(&wire_info.version),
            capabilities: wire_info.capabilities,
        };
        let spare = Spare {
            conn: Some(first),
            starting: false,
            first: Some(Ok(())),
        };
        Ok(Self {
            shared: EngineShared::new(config, info, wire_info, spare),
        })
    }

    /// Like [`RemoteEngine::new`] without waiting: the first host starts on
    /// a background thread, so FastPDF's own start-up does not wait for it
    /// (ADR 0008 §5). `expected` is the engine the host must run (the same
    /// engine in-process); a host reporting anything else is refused.
    /// Opening a document waits for the start; [`RemoteEngine::ready`]
    /// tells whether it worked.
    pub fn start(config: RemoteConfig, expected: EngineInfo) -> Result<Self, EngineError> {
        validate(&config)?;
        let wire_info = WireEngineInfo {
            name: expected.name.to_owned(),
            version: expected.version.to_owned(),
            capabilities: expected.capabilities,
        };
        let shared = EngineShared::new(config, expected, wire_info, Spare::default());
        shared.start_spare(true);
        Ok(Self { shared })
    }

    /// Waits until the first host has started (at most the configured
    /// start-up timeout) and returns how that went.
    pub fn ready(&self) -> Result<(), EngineError> {
        let shared = &self.shared;
        let mut spare = lock(&shared.spare);
        let deadline = Instant::now() + shared.config.startup_timeout + START_GRACE;
        while spare.first.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return Err(EngineError::Unavailable(
                    "the render host did not start in time".into(),
                ));
            }
            spare = shared
                .spare_done
                .wait_timeout(spare, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        spare.first.clone().unwrap_or(Ok(()))
    }

    /// Process id of the started host kept in reserve, if any.
    pub fn spare_pid(&self) -> Option<u32> {
        lock(&self.shared.spare)
            .conn
            .as_ref()
            .filter(|c| c.is_alive())
            .map(|c| c.pid())
    }

    /// Supervision state of the host of every open document.
    pub fn host_stats(&self) -> Vec<HostStats> {
        let documents: Vec<Arc<DocShared>> = lock(&self.shared.documents)
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        documents.iter().map(|d| d.stats()).collect()
    }
}

impl PdfEngine for RemoteEngine {
    fn info(&self) -> EngineInfo {
        self.shared.info.clone()
    }

    fn open(
        &self,
        source: DocumentSource,
        options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        let shared = DocShared::open(&self.shared, source, options)?;
        let mut documents = lock(&self.shared.documents);
        documents.retain(|d| d.strong_count() > 0);
        documents.push(Arc::downgrade(&shared));
        Ok(Box::new(RemoteDocument { shared }))
    }
}

impl EngineShared {
    fn new(
        config: RemoteConfig,
        info: EngineInfo,
        wire_info: WireEngineInfo,
        spare: Spare,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            info,
            wire_info,
            spare: Mutex::new(spare),
            spare_done: Condvar::new(),
            generation: AtomicU64::new(2),
            documents: Mutex::new(Vec::new()),
        })
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed)
    }

    fn spawn(&self) -> Result<Arc<Connection>, EngineError> {
        let conn = Connection::spawn(&self.config, self.next_generation())?;
        if *conn.info() != self.wire_info {
            conn.shutdown();
            return Err(EngineError::Internal(format!(
                "render host runs engine {} {}, expected {} {}",
                conn.info().name,
                conn.info().version,
                self.wire_info.name,
                self.wire_info.version
            )));
        }
        Ok(conn)
    }

    /// A live host without a document: the spare if there is one, else
    /// the one being started in the background (rather than a second
    /// start next to it), else a new one.
    fn host(self: &Arc<Self>) -> Result<Arc<Connection>, EngineError> {
        let ready = {
            let mut spare = lock(&self.spare);
            let deadline = Instant::now() + self.config.startup_timeout + START_GRACE;
            while spare.starting && spare.conn.is_none() {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                spare = self
                    .spare_done
                    .wait_timeout(spare, deadline - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            spare.conn.take()
        };
        let conn = match ready {
            Some(conn) if conn.is_alive() => conn,
            _ => self.spawn()?,
        };
        if self.config.keep_spare {
            self.refill();
        }
        Ok(conn)
    }

    /// A new spare after one was taken, started [`REFILL_DELAY`] later so
    /// that its start-up never competes with the document being opened or
    /// reopened (its first pages render meanwhile). A host needed within
    /// that window is started on the spot, as without a spare.
    fn refill(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("fastpdf-remote-refill".into())
            .spawn(move || {
                std::thread::sleep(REFILL_DELAY);
                if let Some(me) = me.upgrade() {
                    me.start_spare(false);
                }
            });
        if spawned.is_err() {
            self.start_spare(false);
        }
    }

    /// Starts a spare host in the background unless one is ready or
    /// starting. `first`: this is the engine's first host, whose outcome
    /// [`RemoteEngine::ready`] reports.
    fn start_spare(self: &Arc<Self>, first: bool) {
        {
            let mut spare = lock(&self.spare);
            if spare.starting || spare.conn.is_some() {
                return;
            }
            spare.starting = true;
        }
        let me = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("fastpdf-remote-spare".into())
            .spawn(move || {
                let result = me.spawn();
                {
                    let mut spare = lock(&me.spare);
                    spare.starting = false;
                    if first {
                        spare.first = Some(result.as_ref().map(drop).map_err(Clone::clone));
                    }
                    match result {
                        Ok(conn) => spare.conn = Some(conn),
                        Err(e) => log::warn!("render host start failed: {e}"),
                    }
                }
                me.spare_done.notify_all();
            });
        if let Err(e) = started {
            let mut spare = lock(&self.spare);
            spare.starting = false;
            if first {
                spare.first = Some(Err(EngineError::Internal(format!(
                    "cannot start the render host: no thread: {e}"
                ))));
            }
            drop(spare);
            self.spare_done.notify_all();
        }
    }
}

/// Crash bookkeeping visible without the state lock (the overlay reads it
/// on the UI thread while a restart may hold the state lock).
#[derive(Debug, Default, Clone)]
struct Summary {
    restarts: u32,
    crashes: u32,
    last_crash: Option<String>,
    last_reason: Option<HostExitReason>,
    /// Pages that failed permanently, with what they did to the host.
    failed_pages: Vec<(u32, HostExitReason)>,
    disabled: bool,
}

struct DocState {
    conn: Option<Arc<Connection>>,
    ledger: Ledger,
    /// Verdicts per dead connection generation.
    verdicts: Vec<(u64, HashMap<u64, Verdict>)>,
    restarts: u32,
    last_crash: Option<String>,
    last_reason: Option<HostExitReason>,
    /// How each subject last brought a host down.
    reasons: HashMap<Subject, HostExitReason>,
    closed: bool,
}

/// `EngineError::HostExited`; an unknown reason counts as a crash.
fn host_exit(reason: Option<HostExitReason>, permanent: bool) -> EngineError {
    EngineError::HostExited(HostExit {
        reason: reason.unwrap_or(HostExitReason::Crashed { exit_code: None }),
        permanent,
    })
}

/// Where a document's bytes are, for every host that opens it.
enum DocBytes {
    /// An empty source.
    Empty,
    /// A frozen, read-only copy: sources that are not a file.
    Section(Section),
    /// The file itself, which the loader keeps open without write sharing:
    /// each host gets a read-only duplicate of the handle and reads or maps
    /// the file, so FastPDF neither copies nor reads it (ADR 0008 §1.5).
    File(Arc<FileOrigin>),
}

struct DocShared {
    engine: Arc<EngineShared>,
    /// Document bytes; shared again with every new host.
    bytes: DocBytes,
    path: Option<PathBuf>,
    password: Option<String>,
    limits: ResourceLimits,
    page_count: u32,
    timeout: Duration,
    open_timeout: Duration,
    state: Mutex<DocState>,
    /// The live connection, for calls that must not block (memory usage,
    /// trim, stats).
    current: Mutex<Option<Arc<Connection>>>,
    summary: Mutex<Summary>,
    gate: Arc<Gate>,
    /// Page geometry; survives host restarts.
    geometry: Arc<Geometry>,
}

/// Copies the document into a section and freezes it.
fn upload(bytes: &[u8]) -> Result<Option<Section>, EngineError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let fail = |e: io::Error| {
        EngineError::Internal(format!(
            "cannot share the document with the render host: {e}"
        ))
    };
    let section = Section::create(bytes.len()).map_err(fail)?;
    {
        let view = section.map(Access::ReadWrite).map_err(fail)?;
        if !view.write_from(0, bytes) {
            return Err(fail(io::Error::other("section too small")));
        }
    }
    section.into_read_only().map(Some).map_err(fail)
}

fn subject_name(s: Subject) -> String {
    match s {
        Subject::Open => "the document".to_owned(),
        Subject::Page(p) => format!("page {}", PageIndex::new(p).display_number()),
        Subject::Metadata => "the document metadata".to_owned(),
        Subject::Outline => "the outline".to_owned(),
        Subject::Geometry => "the page geometry".to_owned(),
    }
}

fn mismatch() -> EngineError {
    EngineError::Internal("render host answered with the wrong payload".into())
}

impl DocShared {
    fn open(
        engine: &Arc<EngineShared>,
        source: DocumentSource,
        options: &OpenOptions,
    ) -> Result<Arc<Self>, EngineError> {
        let DocumentSource { data, path } = source;
        let bytes = match data.origin() {
            // Not `data.as_slice()`: a file source has not been read, and
            // must not be.
            Some(origin) => DocBytes::File(Arc::clone(origin)),
            None => upload(data.as_slice())?.map_or(DocBytes::Empty, DocBytes::Section),
        };
        // The host has its own access to the bytes now.
        drop(data);
        let config = &engine.config;
        let mut doc = Self {
            engine: Arc::clone(engine),
            bytes,
            path,
            password: options.password.clone(),
            limits: options.limits.clone(),
            page_count: 0,
            timeout: config.request_timeout(&options.limits),
            open_timeout: config.open_timeout(&options.limits),
            state: Mutex::new(DocState {
                conn: None,
                ledger: Ledger::new(config.crash_policy.clone()),
                verdicts: Vec::new(),
                restarts: 0,
                last_crash: None,
                last_reason: None,
                reasons: HashMap::new(),
                closed: false,
            }),
            current: Mutex::new(None),
            summary: Mutex::new(Summary::default()),
            gate: Arc::new(Gate::default()),
            geometry: Arc::new(Geometry::new(0)),
        };
        let conn = engine.host()?;
        doc.page_count = match doc.open_on(&conn) {
            Ok(n) => n,
            Err(e) => {
                conn.shutdown();
                return Err(match e {
                    CallError::Engine(e) | CallError::Fatal(e) => e,
                    CallError::Cancelled => EngineError::Cancelled,
                    // Opening is not retried: the user may open it again.
                    CallError::Lost => {
                        if let Some(d) = conn.death() {
                            log::warn!("opening the document failed: {}", d.description);
                        }
                        host_exit(conn.death().map(|d| d.reason), true)
                    }
                });
            }
        };
        doc.geometry = Arc::new(Geometry::new(doc.page_count));
        doc.state.get_mut().unwrap_or_else(|e| e.into_inner()).conn = Some(Arc::clone(&conn));
        *doc.current.get_mut().unwrap_or_else(|e| e.into_inner()) = Some(conn);
        let doc = Arc::new(doc);
        // The first screen's geometry before returning; the rest in the
        // background, so a huge document does not delay its first page.
        if let Err(e) = doc.fetch_geometry(0, GEOMETRY_PREFIX) {
            doc.close();
            return Err(e);
        }
        if doc.geometry.missing() > 0 {
            let weak = Arc::downgrade(&doc);
            let spawned = std::thread::Builder::new()
                .name("fastpdf-remote-geometry".into())
                .spawn(move || fetch_remaining_geometry(&weak));
            if let Err(e) = spawned {
                log::warn!("no background geometry thread ({e}); pages are fetched on demand");
            }
        }
        Ok(doc)
    }

    /// Fetches the geometry of pages `first..first + count` into the cache
    /// (the host's background geometry lane), with the usual admission,
    /// restart and retry rules.
    fn fetch_geometry(&self, first: u32, count: u32) -> Result<(), EngineError> {
        let count = count.min(self.page_count.saturating_sub(first));
        if count == 0 {
            return Ok(());
        }
        let first = PageIndex::new(first);
        let sink = Arc::clone(&self.geometry);
        self.call(Some(Subject::Geometry), None, |_, id| {
            let mut request = Request::new(
                Command::PageInfos { id, first, count },
                Expect::PageInfos { first, count },
                Some(Subject::Geometry),
            );
            request.sink = Some(Arc::clone(&sink));
            Ok(request)
        })
        .map(drop)
    }

    /// `page_info` for FastPDF's UI thread: never waits for the gate, the
    /// state lock or render work, never restarts a host, and gives up after
    /// [`UI_GEOMETRY_WAIT`].
    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        if let Some(known) = self.geometry.get(page.get()) {
            return known;
        }
        {
            let summary = lock(&self.summary);
            if let Some((_, reason)) = summary.failed_pages.iter().find(|(p, _)| *p == page.get()) {
                return Err(host_exit(Some(*reason), true));
            }
            if summary.disabled {
                return Err(host_exit(summary.last_reason, true));
            }
        }
        let deadline = Instant::now() + UI_GEOMETRY_WAIT;
        let mut asked = None;
        loop {
            // A host that died is replaced by whoever needs it next (the
            // background geometry thread or a render); ask the new one too.
            let conn = lock(&self.current).clone().filter(|c| c.is_alive());
            if let Some(conn) = conn
                && asked != Some(conn.generation())
            {
                asked = Some(conn.generation());
                conn.request_geometry(page, &self.geometry, self.timeout);
            }
            let step = (Instant::now() + GEOMETRY_POLL).min(deadline);
            if let Some(known) = self.geometry.wait(page.get(), step) {
                return known;
            }
            if Instant::now() >= deadline {
                return Err(EngineError::Unavailable(format!(
                    "the size of {} is not known yet: the render host is busy or restarting",
                    subject_name(Subject::Page(page.get()))
                )));
            }
        }
    }

    /// Opens the document in `conn` and returns its page count.
    fn open_on(&self, conn: &Connection) -> Result<u32, CallError> {
        let document = match &self.bytes {
            DocBytes::Empty => None,
            DocBytes::Section(s) => Some(DocumentRef::Section(conn.share(s, Access::Read)?)),
            DocBytes::File(origin) => Some(conn.share_file(origin)?),
        };
        let id = conn.next_id();
        let command = Command::Open(Open {
            id,
            document,
            path: self.path.clone(),
            password: self.password.clone(),
            limits: self.limits.clone(),
        });
        let request = Request::new(command, Expect::Opened, Some(Subject::Open));
        let (payload, _) = conn.call(request, self.open_timeout, None)?;
        match payload {
            Payload::Opened { page_count }
                if page_count > 0 && page_count <= self.limits.max_page_count =>
            {
                Ok(page_count)
            }
            _ => Err(CallError::Fatal(EngineError::Internal(
                "render host reported an invalid page count".into(),
            ))),
        }
    }

    fn check_page(&self, page: PageIndex) -> Result<(), EngineError> {
        if page.get() < self.page_count {
            Ok(())
        } else {
            Err(EngineError::PageOutOfRange {
                page,
                page_count: self.page_count,
            })
        }
    }

    /// Runs one request, restarting the host and retrying as the crash
    /// ledger decides.
    fn call<F>(
        &self,
        subject: Option<Subject>,
        cancel: Option<&CancelToken>,
        mut build: F,
    ) -> Result<(Payload, Option<TargetLease>), EngineError>
    where
        F: FnMut(&Connection, u64) -> Result<Request, CallError>,
    {
        let mut exclusive_next = false;
        for _ in 0..MAX_ATTEMPTS {
            if let Some(cancel) = cancel {
                cancel.check()?;
            }
            let exclusive = {
                let st = lock(&self.state);
                if st.closed {
                    return Err(EngineError::Cancelled);
                }
                if let Some(s) = subject
                    && st.ledger.is_permanent(s)
                {
                    return Err(self.permanent_error(s, &st));
                }
                exclusive_next || subject.is_some_and(|s| st.ledger.is_suspect(s))
            };
            let pass = self.gate.enter(exclusive, cancel)?;
            let conn = self.connection()?;
            let id = conn.next_id();
            let attempt = build(&conn, id).and_then(|mut request| {
                request.subject = subject;
                // The request owns its admission from here on: it is released
                // with the host's terminal reply, even if we stop waiting.
                request.admission = Some(pass);
                conn.call(request, self.timeout, cancel)
            });
            match attempt {
                Ok(done) => {
                    if exclusive && let Some(s) = subject {
                        let mut st = lock(&self.state);
                        st.ledger.cleared(s);
                        self.publish(&st);
                    }
                    return Ok(done);
                }
                Err(CallError::Engine(e) | CallError::Fatal(e)) => return Err(e),
                Err(CallError::Cancelled) => return Err(EngineError::Cancelled),
                Err(CallError::Lost) => match self.verdict(&conn, id, subject) {
                    Verdict::Retry { exclusive } => exclusive_next = exclusive,
                    Verdict::Culprit => return Err(self.culprit_error(&conn, subject)),
                    Verdict::Disabled => return Err(self.disabled_error()),
                    Verdict::Shutdown => return Err(EngineError::Cancelled),
                },
            }
        }
        Err(EngineError::Unavailable(
            "the render host kept failing; request abandoned".into(),
        ))
    }

    /// The live connection, restarting the host (and reopening the
    /// document) when the previous one died.
    fn connection(&self) -> Result<Arc<Connection>, EngineError> {
        let mut st = lock(&self.state);
        if let Some(conn) = &st.conn
            && conn.is_alive()
        {
            return Ok(Arc::clone(conn));
        }
        if let Some(dead) = st.conn.take() {
            self.account(&mut st, &dead);
        }
        if st.closed {
            return Err(EngineError::Cancelled);
        }
        if st.ledger.is_disabled() {
            return Err(self.disabled_error_locked(&st));
        }
        if st.ledger.is_permanent(Subject::Open) {
            return Err(self.permanent_error(Subject::Open, &st));
        }
        let conn = self.engine.host().map_err(|e| {
            EngineError::Unavailable(format!("cannot restart the render host: {e}"))
        })?;
        match self.open_on(&conn) {
            Ok(n) if n == self.page_count => {}
            Ok(n) => {
                conn.shutdown();
                return Err(EngineError::Internal(format!(
                    "the restarted render host sees {n} pages instead of {}",
                    self.page_count
                )));
            }
            Err(CallError::Lost) => {
                self.account(&mut st, &conn);
                // Blamed on the document itself: final once reopening is
                // not tried any more.
                let permanent = st.ledger.is_disabled() || st.ledger.is_permanent(Subject::Open);
                return Err(host_exit(conn.death().map(|d| d.reason), permanent));
            }
            Err(CallError::Engine(e) | CallError::Fatal(e)) => {
                conn.shutdown();
                return Err(e);
            }
            Err(CallError::Cancelled) => {
                conn.shutdown();
                return Err(EngineError::Cancelled);
            }
        }
        st.restarts += 1;
        st.conn = Some(Arc::clone(&conn));
        *lock(&self.current) = Some(Arc::clone(&conn));
        self.publish(&st);
        log::info!(
            "render host restarted (pid {}, restart {})",
            conn.pid(),
            st.restarts
        );
        Ok(conn)
    }

    /// Feeds a dead connection into the ledger, once.
    fn account(&self, st: &mut DocState, conn: &Connection) {
        let generation = conn.generation();
        if st.verdicts.iter().any(|(g, _)| *g == generation) {
            return;
        }
        let Some(death) = conn.death() else {
            return;
        };
        let was_disabled = st.ledger.is_disabled();
        let verdicts = st
            .ledger
            .host_died(Instant::now(), death.cause, &death.in_flight);
        if death.cause != DeathCause::Shutdown {
            st.last_crash = Some(death.description.clone());
            st.last_reason = Some(death.reason);
            for f in &death.in_flight {
                if let Some(s) = f.subject
                    && verdicts.get(&f.id) == Some(&Verdict::Culprit)
                {
                    st.reasons.insert(s, death.reason);
                }
            }
        }
        if st.ledger.is_disabled() && !was_disabled {
            log::error!(
                "render host crashed {} times; automatic restarts stopped",
                st.ledger.total_crashes()
            );
        }
        st.verdicts.push((generation, verdicts));
        if st.verdicts.len() > KEPT_VERDICTS {
            st.verdicts.remove(0);
        }
        if st
            .conn
            .as_ref()
            .is_some_and(|c| c.generation() == generation)
        {
            st.conn = None;
        }
        {
            let mut current = lock(&self.current);
            if current
                .as_ref()
                .is_some_and(|c| c.generation() == generation)
            {
                *current = None;
            }
        }
        self.publish(st);
    }

    fn verdict(&self, conn: &Connection, id: u64, subject: Option<Subject>) -> Verdict {
        let mut st = lock(&self.state);
        self.account(&mut st, conn);
        let found = st
            .verdicts
            .iter()
            .find(|(g, _)| *g == conn.generation())
            .and_then(|(_, v)| v.get(&id).copied());
        found.unwrap_or_else(|| {
            // Not in flight when the host died (it failed before sending).
            if st.closed {
                Verdict::Shutdown
            } else if st.ledger.is_disabled() {
                Verdict::Disabled
            } else {
                Verdict::Retry {
                    exclusive: subject.is_some_and(|s| st.ledger.is_suspect(s)),
                }
            }
        })
    }

    /// The request brought its host down: transient the first time (the
    /// next attempt runs alone), final once the ledger gave up on it.
    fn culprit_error(&self, conn: &Connection, subject: Option<Subject>) -> EngineError {
        let st = lock(&self.state);
        let permanent =
            st.ledger.is_disabled() || subject.is_some_and(|s| st.ledger.is_permanent(s));
        host_exit(conn.death().map(|d| d.reason), permanent)
    }

    fn permanent_error(&self, s: Subject, st: &DocState) -> EngineError {
        host_exit(st.reasons.get(&s).copied().or(st.last_reason), true)
    }

    fn disabled_error(&self) -> EngineError {
        let st = lock(&self.state);
        self.disabled_error_locked(&st)
    }

    fn disabled_error_locked(&self, st: &DocState) -> EngineError {
        host_exit(st.last_reason, true)
    }

    fn publish(&self, st: &DocState) {
        let no_code = HostExitReason::Crashed { exit_code: None };
        *lock(&self.summary) = Summary {
            restarts: st.restarts,
            crashes: st.ledger.total_crashes(),
            last_crash: st.last_crash.clone(),
            last_reason: st.last_reason,
            failed_pages: st
                .ledger
                .failed_pages()
                .into_iter()
                .map(|p| {
                    let reason = st.reasons.get(&Subject::Page(p)).copied();
                    (p, reason.or(st.last_reason).unwrap_or(no_code))
                })
                .collect(),
            disabled: st.ledger.is_disabled(),
        };
    }

    fn stats(&self) -> HostStats {
        let conn = lock(&self.current).clone().filter(|c| c.is_alive());
        let summary = lock(&self.summary).clone();
        HostStats {
            pid: conn.as_ref().map(|c| c.pid()),
            private_bytes: conn.as_ref().and_then(|c| c.private_bytes()),
            restarts: summary.restarts,
            crashes: summary.crashes,
            last_crash: summary.last_crash,
            failed_pages: summary
                .failed_pages
                .into_iter()
                .map(|(p, _)| PageIndex::new(p))
                .collect(),
            disabled: summary.disabled,
            geometry_missing: self.geometry.missing(),
        }
    }

    fn host_status(&self) -> HostStatus {
        let s = self.stats();
        HostStatus {
            pid: s.pid,
            private_bytes: s.private_bytes,
            restarts: s.restarts,
            crashes: s.crashes,
            last_crash: s.last_crash,
            failed_pages: s.failed_pages,
            restarts_disabled: s.disabled,
        }
    }

    fn close(&self) {
        let conn = {
            let mut st = lock(&self.state);
            st.closed = true;
            st.conn.take()
        };
        lock(&self.current).take();
        if let Some(conn) = conn {
            conn.shutdown();
        }
    }
}

/// A document open in a render host.
struct RemoteDocument {
    shared: Arc<DocShared>,
}

impl Drop for RemoteDocument {
    fn drop(&mut self) {
        self.shared.close();
    }
}

impl EngineDocument for RemoteDocument {
    fn page_count(&self) -> u32 {
        self.shared.page_count
    }

    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        self.shared.check_page(page)?;
        self.shared.page_info(page)
    }

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        let (payload, _) = self.shared.call(Some(Subject::Metadata), None, |_, id| {
            Ok(Request::new(
                Command::Metadata { id },
                Expect::Metadata,
                None,
            ))
        })?;
        match payload {
            Payload::Metadata(m) => Ok(m),
            _ => Err(mismatch()),
        }
    }

    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        cancel.check()?;
        if request.region.is_empty() || target.size() != request.region.size() {
            return Err(EngineError::InvalidRequest(
                "target size does not match region size".into(),
            ));
        }
        self.shared.check_page(request.page)?;
        let bytes = target.data().len();
        let format = target.format();
        let timeout = self.shared.timeout;
        let (payload, lease) = self.shared.call(
            Some(Subject::Page(request.page.get())),
            Some(cancel),
            |conn, id| {
                let lease = conn.acquire_target(bytes, Some(cancel), timeout)?;
                let command = Command::Render(Render {
                    id,
                    request: request.clone(),
                    format,
                    target: lease.wire(),
                });
                let mut req = Request::new(command, Expect::Rendered, None);
                req.target = Some(lease);
                Ok(req)
            },
        )?;
        let Payload::Rendered(outcome) = payload else {
            return Err(mismatch());
        };
        match lease {
            Some(lease) if lease.copy_to(target.data_mut()) => Ok(outcome),
            _ => Err(EngineError::Internal("render target lost".into())),
        }
    }

    fn text_layer(&self, page: PageIndex, cancel: &CancelToken) -> Result<TextLayer, EngineError> {
        self.shared.check_page(page)?;
        let (payload, _) =
            self.shared
                .call(Some(Subject::Page(page.get())), Some(cancel), |_, id| {
                    Ok(Request::new(
                        Command::TextLayer { id, page },
                        Expect::TextLayer(page),
                        None,
                    ))
                })?;
        match payload {
            Payload::TextLayer(layer) => Ok(layer),
            _ => Err(mismatch()),
        }
    }

    fn outline(&self) -> Result<Vec<OutlineItem>, EngineError> {
        let (payload, _) = self.shared.call(Some(Subject::Outline), None, |_, id| {
            Ok(Request::new(Command::Outline { id }, Expect::Outline, None))
        })?;
        match payload {
            Payload::Outline(items) => Ok(items),
            _ => Err(mismatch()),
        }
    }

    fn links(&self, page: PageIndex) -> Result<Vec<Link>, EngineError> {
        self.shared.check_page(page)?;
        let (payload, _) = self
            .shared
            .call(Some(Subject::Page(page.get())), None, |_, id| {
                Ok(Request::new(
                    Command::Links { id, page },
                    Expect::Links,
                    None,
                ))
            })?;
        match payload {
            Payload::Links(links) => Ok(links),
            _ => Err(mismatch()),
        }
    }

    fn trim_memory(&self, pressure: MemoryPressure) {
        // Not under the `current` lock: the write may block briefly.
        let conn = lock(&self.shared.current).clone();
        if let Some(conn) = conn {
            conn.trim(pressure, self.shared.timeout);
        }
    }

    fn memory_usage(&self) -> Option<u64> {
        let conn = lock(&self.shared.current).clone()?;
        conn.memory_usage(self.shared.timeout)
    }

    fn host_status(&self) -> Option<HostStatus> {
        Some(self.shared.host_status())
    }
}

/// Background geometry thread of one document: fills the cache batch by
/// batch, then ends. Stops early (leaving the rest to on-demand requests)
/// when the document closes or the host keeps failing.
fn fetch_remaining_geometry(doc: &Weak<DocShared>) {
    let mut from = 0;
    loop {
        let Some(doc) = doc.upgrade() else {
            return;
        };
        let gap = doc
            .geometry
            .next_gap(from, GEOMETRY_BATCH)
            .or_else(|| doc.geometry.next_gap(0, GEOMETRY_BATCH));
        let Some((first, count)) = gap else {
            return;
        };
        if let Err(e) = doc.fetch_geometry(first, count) {
            log::debug!("background page geometry stopped: {e}");
            return;
        }
        from = first.saturating_add(count);
    }
}
