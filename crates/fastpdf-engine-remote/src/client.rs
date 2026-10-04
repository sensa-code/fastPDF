//! One render host process as FastPDF sees it: spawn and handshake, the
//! reply reader thread, request/reply matching, deadlines, tile slots, and
//! the post-mortem when the host dies.
//!
//! Every request registers a [`Pending`] entry before its command is
//! written. The entry owns the request's render target (slot or section)
//! and its gate admission until the host's terminal reply arrives, even when
//! the caller gave up (cancellation): a slot is never reused while the host
//! may still write into it, and an exclusive request keeps everything else
//! out until the host has really finished it. When the host dies, every entry is resolved with
//! [`Delivery::Lost`] and the [`Death`] record says which requests were in
//! flight; the document's crash ledger turns that into verdicts.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read};
use std::os::windows::io::{AsHandle, BorrowedHandle};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{CancelToken, EngineError, PageIndex};

use crate::config::RemoteConfig;
use crate::gate::OwnedPass;
use crate::geometry::Geometry;
use crate::host::ipc_args;
use crate::policy::{DeathCause, InFlight, Subject};
use crate::protocol::{
    BUILD_ID, Command, FrameError, Init, MAX_REPLY_FRAME, MAX_WORKERS, Payload, RenderTarget,
    Reply, SectionRef, SlotSpec, WireEngineInfo, decode_reply, encode_command, read_frame,
};
use crate::win::pipe::{self, ServerPipe};
use crate::win::process::{self, Child, JobLimits, KILLED_BY_PARENT, describe_exit};
use crate::win::section::{Access, Section, View};

/// How often waiting callers look at their cancel token.
const POLL: Duration = Duration::from_millis(3);
/// How often the reader thread checks request deadlines.
const DEADLINE_TICK: Duration = Duration::from_millis(50);
/// A command that cannot be written within this time means the host stopped
/// reading.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Extra wait beyond a request's deadline before its caller stops relying
/// on the reader thread to enforce it.
const GIVE_UP_GRACE: Duration = Duration::from_secs(10);
/// Engine memory usage is refreshed in the background at most this often.
const MEMORY_REFRESH: Duration = Duration::from_millis(500);

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Which reply a request expects; anything else is a protocol violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expect {
    Opened,
    PageInfo(PageIndex),
    PageInfos { first: PageIndex, count: u32 },
    Metadata,
    Rendered,
    TextLayer(PageIndex),
    Outline,
    Links,
    MemoryUsage,
    Trimmed,
}

impl Expect {
    fn accepts(self, payload: &Payload) -> bool {
        match (self, payload) {
            (Self::TextLayer(page), Payload::TextLayer(layer)) => layer.page == page,
            (Self::PageInfos { first, count }, Payload::PageInfos { first: f, pages }) => {
                *f == first && pages.len() == count as usize
            }
            (Self::Opened, Payload::Opened { .. })
            | (Self::PageInfo(_), Payload::PageInfo(_))
            | (Self::Metadata, Payload::Metadata(_))
            | (Self::Rendered, Payload::Rendered(_))
            | (Self::Outline, Payload::Outline(_))
            | (Self::Links, Payload::Links(_))
            | (Self::MemoryUsage, Payload::MemoryUsage(_))
            | (Self::Trimmed, Payload::Trimmed) => true,
            _ => false,
        }
    }
}

/// What a waiting caller receives. Moved once through a channel, so the
/// size difference between the variants does not matter.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Delivery {
    Reply(Result<Payload, EngineError>, Option<TargetLease>),
    /// The host died before answering.
    Lost,
}

/// Why a call did not produce a payload.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The host answered with an engine error.
    Engine(EngineError),
    /// The caller's token was cancelled; the request was abandoned.
    Cancelled,
    /// The host died; ask the document's ledger what to do.
    Lost,
    /// The request could not be made at all.
    Fatal(EngineError),
}

/// One request to the host.
pub(crate) struct Request {
    pub(crate) command: Command,
    pub(crate) expect: Expect,
    /// What a crash during the request is blamed on; `None`: never blamed.
    pub(crate) subject: Option<Subject>,
    /// Render target, owned until the host's terminal reply.
    pub(crate) target: Option<TargetLease>,
    /// Gate admission, released with the terminal reply (or the host's
    /// death), not when the caller stops waiting.
    pub(crate) admission: Option<OwnedPass>,
    /// Where geometry answers go, whether or not anyone still waits.
    pub(crate) sink: Option<Arc<Geometry>>,
}

impl Request {
    pub(crate) fn new(command: Command, expect: Expect, subject: Option<Subject>) -> Self {
        Self {
            command,
            expect,
            subject,
            target: None,
            admission: None,
            sink: None,
        }
    }
}

struct Pending {
    reply_to: Option<SyncSender<Delivery>>,
    expect: Expect,
    subject: Option<Subject>,
    deadline: Instant,
    target: Option<TargetLease>,
    // Held, never read: dropping the entry is what releases them.
    _admission: Option<OwnedPass>,
    sink: Option<Arc<Geometry>>,
}

#[derive(Default)]
struct Table {
    entries: HashMap<u64, Pending>,
    closed: bool,
}

/// Why the parent terminated the host itself.
#[derive(Debug, Clone)]
enum KillReason {
    Deadline { id: u64 },
    Protocol(String),
    Unresponsive(String),
    Shutdown,
}

/// Post-mortem of a host connection.
#[derive(Debug, Clone)]
pub(crate) struct Death {
    pub(crate) cause: DeathCause,
    /// Human-readable cause, for errors and the overlay.
    pub(crate) description: String,
    pub(crate) in_flight: Vec<InFlight>,
}

#[derive(Debug, Default)]
struct MemoryProbe {
    value: Option<u64>,
    asked: Option<Instant>,
    waiting: bool,
}

/// A connected render host.
pub(crate) struct Connection {
    generation: u64,
    child: Child,
    commands: Mutex<ServerPipe>,
    table: Mutex<Table>,
    death: OnceLock<Death>,
    kill_reason: Mutex<Option<KillReason>>,
    slots: Option<Arc<SlotPool>>,
    next_id: AtomicU64,
    info: WireEngineInfo,
    memory: Mutex<MemoryProbe>,
    memory_limit: Option<u64>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("generation", &self.generation)
            .field("pid", &self.child.pid())
            .field("alive", &self.is_alive())
            .finish_non_exhaustive()
    }
}

/// `Read` over the reply pipe that keeps watching the host process and
/// runs `tick` while it waits.
struct PipeReader<'a> {
    pipe: &'a ServerPipe,
    process: BorrowedHandle<'a>,
    every: Duration,
    tick: &'a mut dyn FnMut(),
}

impl Read for PipeReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.pipe.read(buf, self.process, self.every, self.tick)
    }
}

fn startup_error(what: &str, e: impl std::fmt::Display) -> EngineError {
    EngineError::Internal(format!("cannot start the render host: {what}: {e}"))
}

impl Connection {
    /// Starts a host and completes the handshake.
    pub(crate) fn spawn(config: &RemoteConfig, generation: u64) -> Result<Arc<Self>, EngineError> {
        let deadline = Instant::now() + config.startup_timeout;
        let channel = pipe::listen().map_err(|e| startup_error("pipes", e))?;
        let mut args: Vec<OsString> = config.args.clone();
        args.extend(ipc_args(channel.name()));
        let child = process::spawn(
            &config.program,
            &args,
            JobLimits {
                memory: config.memory_limit,
            },
        )
        .map_err(|e| startup_error(&config.program.display().to_string(), e))?;
        if let Err(e) = channel.accept(child.process(), child.pid(), deadline) {
            return Err(startup_failure(&child, &e.to_string()));
        }
        let pipe::Channel {
            commands, replies, ..
        } = channel;

        let slots = if config.slots > 0 {
            Some(
                SlotPool::new(config.slots, config.slot_bytes)
                    .map_err(|e| startup_error("tile slots", e))?,
            )
        } else {
            None
        };
        let slot_spec = match &slots {
            Some(pool) => {
                let handle = child
                    .duplicate_into(pool.section.as_handle(), Access::ReadWrite.mask())
                    .map_err(|e| startup_error("tile slots", e))?;
                Some(SlotSpec {
                    section: SectionRef {
                        handle,
                        len: pool.section.len() as u64,
                    },
                    count: pool.count,
                    slot_bytes: pool.slot_bytes as u64,
                })
            }
            None => None,
        };
        let init = Command::Init(Init {
            build_id: BUILD_ID.to_owned(),
            engine: config.engine.clone(),
            workers: config.host_workers.clamp(1, MAX_WORKERS),
            slots: slot_spec,
        });
        let frame = encode_command(&init).map_err(|e| startup_error("handshake", e))?;
        if let Err(e) = commands.write_all(&frame, child.process(), config.startup_timeout) {
            return Err(startup_failure(&child, &e.to_string()));
        }
        let mut timed_out = false;
        let reply = {
            let mut tick = || {
                if Instant::now() > deadline {
                    timed_out = true;
                    child.kill();
                }
            };
            let mut reader = PipeReader {
                pipe: &replies,
                process: child.process(),
                every: Duration::from_millis(20),
                tick: &mut tick,
            };
            read_frame(&mut reader, MAX_REPLY_FRAME)
        };
        let info = match reply {
            Ok(Some(payload)) => match decode_reply(&payload) {
                Ok(Reply::Ready { build_id, engine }) if build_id == BUILD_ID => engine,
                Ok(Reply::Ready { build_id, .. }) => {
                    return Err(EngineError::Internal(format!(
                        "render host build mismatch: {build_id}, expected {BUILD_ID}"
                    )));
                }
                Ok(Reply::InitFailed(e)) => return Err(e),
                Ok(Reply::Done { .. }) => {
                    return Err(startup_error("handshake", "unexpected reply"));
                }
                Err(e) => return Err(startup_error("handshake", e)),
            },
            Ok(None) | Err(_) if timed_out => {
                return Err(startup_error(
                    "handshake",
                    format!("no answer within {:?}", config.startup_timeout),
                ));
            }
            Ok(None) => return Err(startup_failure(&child, "closed the channel")),
            Err(e) => return Err(startup_failure(&child, &e.to_string())),
        };
        let conn = Arc::new(Self {
            generation,
            child,
            commands: Mutex::new(commands),
            table: Mutex::new(Table::default()),
            death: OnceLock::new(),
            kill_reason: Mutex::new(None),
            slots,
            next_id: AtomicU64::new(1),
            info,
            memory: Mutex::new(MemoryProbe::default()),
            memory_limit: config.memory_limit,
        });
        // The reader holds only a weak reference (and its own process
        // handle): when the last owner drops the connection, `Child` kills
        // the host, the pipe breaks and the reader ends.
        let process = conn
            .child
            .process()
            .try_clone_to_owned()
            .map_err(|e| startup_error("reader thread", e))?;
        let weak = Arc::downgrade(&conn);
        std::thread::Builder::new()
            .name(format!("fastpdf-remote-{}", conn.child.pid()))
            .spawn(move || reader_main(&weak, &replies, process.as_handle()))
            .map_err(|e| startup_error("reader thread", e))?;
        Ok(conn)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.pid()
    }

    pub(crate) fn info(&self) -> &WireEngineInfo {
        &self.info
    }

    pub(crate) fn private_bytes(&self) -> Option<u64> {
        self.is_alive()
            .then(|| self.child.private_bytes())
            .flatten()
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.death.get().is_none()
    }

    pub(crate) fn death(&self) -> Option<&Death> {
        self.death.get()
    }

    pub(crate) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Duplicates a section handle into the host (for `Open`).
    pub(crate) fn share(&self, section: &Section, access: Access) -> Result<SectionRef, CallError> {
        let handle = self
            .child
            .duplicate_into(section.as_handle(), access.mask())
            .map_err(|_| CallError::Lost)?;
        Ok(SectionRef {
            handle,
            len: section.len() as u64,
        })
    }

    /// Terminates the host on purpose; the first reason recorded wins.
    fn kill(&self, reason: KillReason) {
        {
            let mut current = lock(&self.kill_reason);
            if current.is_none() {
                *current = Some(reason);
            }
        }
        self.child.kill();
    }

    /// Closing the document: end the host now.
    pub(crate) fn shutdown(&self) {
        self.kill(KillReason::Shutdown);
    }

    fn write(&self, frame: &[u8]) -> io::Result<()> {
        lock(&self.commands).write_all(frame, self.child.process(), WRITE_TIMEOUT)
    }

    /// A render target for `bytes` bytes: a free slot when it fits (waiting
    /// for one if necessary), else a section of its own.
    pub(crate) fn acquire_target(
        &self,
        bytes: usize,
        cancel: Option<&CancelToken>,
        timeout: Duration,
    ) -> Result<TargetLease, CallError> {
        if let Some(pool) = self.slots.as_ref().filter(|p| bytes <= p.slot_bytes) {
            return pool.acquire(cancel, Instant::now() + timeout);
        }
        let fail = |e: io::Error| {
            CallError::Fatal(EngineError::Internal(format!(
                "cannot allocate a {bytes}-byte render target: {e}"
            )))
        };
        let section = Section::create(bytes).map_err(fail)?;
        let view = section.map(Access::Read).map_err(fail)?;
        let handle = self
            .child
            .duplicate_into(section.as_handle(), Access::ReadWrite.mask())
            .map_err(|_| CallError::Lost)?;
        Ok(TargetLease::Own {
            wire: RenderTarget::Section(SectionRef {
                handle,
                len: bytes as u64,
            }),
            view,
            _section: section,
        })
    }

    /// Registers `request`; `false` if the connection is already closed
    /// (the request, with its admission and target, is dropped then).
    fn register(
        &self,
        id: u64,
        request: Request,
        reply_to: Option<SyncSender<Delivery>>,
        timeout: Duration,
    ) -> bool {
        let Request {
            expect,
            subject,
            target,
            admission,
            sink,
            ..
        } = request;
        let mut table = lock(&self.table);
        if table.closed {
            return false;
        }
        table.entries.insert(
            id,
            Pending {
                reply_to,
                expect,
                subject,
                deadline: Instant::now() + timeout,
                target,
                _admission: admission,
                sink,
            },
        );
        true
    }

    /// Sends `request` and waits for its terminal reply.
    pub(crate) fn call(
        &self,
        request: Request,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<(Payload, Option<TargetLease>), CallError> {
        let id = request.command.id().ok_or_else(|| {
            CallError::Fatal(EngineError::InvalidRequest("request without an id".into()))
        })?;
        let frame = encode_command(&request.command).map_err(|e| {
            CallError::Fatal(EngineError::InvalidRequest(format!(
                "cannot send request: {e}"
            )))
        })?;
        let (tx, rx) = mpsc::sync_channel(1);
        if !self.register(id, request, Some(tx), timeout) {
            return Err(CallError::Lost);
        }
        if let Err(e) = self.write(&frame) {
            // Dead or no longer reading: make sure it is gone; the reader
            // thread then resolves every entry, ours included.
            self.kill(KillReason::Unresponsive(e.to_string()));
        }
        let give_up = Instant::now() + timeout + GIVE_UP_GRACE;
        let mut killed = false;
        loop {
            match rx.recv_timeout(POLL) {
                Ok(Delivery::Reply(Ok(payload), lease)) => return Ok((payload, lease)),
                Ok(Delivery::Reply(Err(e), _)) => return Err(CallError::Engine(e)),
                Ok(Delivery::Lost) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(CallError::Lost);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if cancel.is_some_and(CancelToken::is_cancelled) {
                        // The host stops cooperatively if the engine can. The
                        // entry keeps the target and the admission until the
                        // host has answered; the caller returns now.
                        if let Ok(frame) = encode_command(&Command::Cancel { id }) {
                            let _ = self.write(&frame);
                        }
                        return Err(CallError::Cancelled);
                    }
                    if Instant::now() > give_up {
                        if killed {
                            return Err(CallError::Fatal(EngineError::Internal(
                                "render host does not respond".into(),
                            )));
                        }
                        // The reader thread should have enforced the deadline.
                        killed = true;
                        self.kill(KillReason::Deadline { id });
                    }
                }
            }
        }
    }

    /// Sends `request` without waiting for its reply.
    fn send_detached(&self, request: Request, timeout: Duration) -> bool {
        let Some(id) = request.command.id() else {
            return false;
        };
        let Ok(frame) = encode_command(&request.command) else {
            return false;
        };
        if !self.register(id, request, None, timeout) {
            return false;
        }
        if let Err(e) = self.write(&frame) {
            self.kill(KillReason::Unresponsive(e.to_string()));
            return false;
        }
        true
    }

    /// Asks for one page's geometry without waiting; the answer lands in
    /// `sink`. Not gated: the host serves it on its geometry threads, never
    /// behind render work.
    pub(crate) fn request_geometry(
        &self,
        page: PageIndex,
        sink: &Arc<Geometry>,
        timeout: Duration,
    ) -> bool {
        let id = self.next_id();
        let mut request = Request::new(
            Command::PageInfo { id, page },
            Expect::PageInfo(page),
            Some(Subject::Page(page.get())),
        );
        request.sink = Some(Arc::clone(sink));
        self.send_detached(request, timeout)
    }

    /// Forwards a memory-pressure notice.
    pub(crate) fn trim(&self, pressure: fastpdf_engine_api::MemoryPressure, timeout: Duration) {
        let id = self.next_id();
        let request = Request::new(Command::Trim { id, pressure }, Expect::Trimmed, None);
        self.send_detached(request, timeout);
    }

    /// The engine's last reported memory usage; refreshed in the background
    /// so this never blocks (the overlay calls it on the UI thread).
    pub(crate) fn memory_usage(&self, timeout: Duration) -> Option<u64> {
        let mut probe = lock(&self.memory);
        let stale = probe.asked.is_none_or(|t| t.elapsed() >= MEMORY_REFRESH);
        if stale && !probe.waiting && self.is_alive() {
            probe.waiting = true;
            probe.asked = Some(Instant::now());
            let value = probe.value;
            drop(probe);
            let id = self.next_id();
            let request = Request::new(Command::MemoryUsage { id }, Expect::MemoryUsage, None);
            if !self.send_detached(request, timeout) {
                lock(&self.memory).waiting = false;
            }
            return value;
        }
        probe.value
    }

    // --- reader thread (see `reader_main`) ---------------------------------

    fn check_deadlines(&self) {
        let now = Instant::now();
        let overdue = lock(&self.table)
            .entries
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .min_by_key(|(_, p)| p.deadline)
            .map(|(id, _)| *id);
        if let Some(id) = overdue {
            self.kill(KillReason::Deadline { id });
        }
    }

    fn deliver(&self, id: u64, result: Result<Payload, EngineError>) -> Result<(), String> {
        let pending = lock(&self.table)
            .entries
            .remove(&id)
            .ok_or_else(|| format!("reply to unknown request {id}"))?;
        if let Ok(payload) = &result
            && !pending.expect.accepts(payload)
        {
            return Err(format!("reply to request {id} does not match its command"));
        }
        if pending.expect == Expect::MemoryUsage {
            let mut probe = lock(&self.memory);
            probe.waiting = false;
            if let Ok(Payload::MemoryUsage(value)) = &result {
                probe.value = *value;
            }
        }
        if let Some(sink) = &pending.sink {
            match (&result, pending.expect) {
                (Ok(Payload::PageInfo(info)), Expect::PageInfo(page)) => {
                    sink.store(page.get(), &Ok(*info));
                }
                (Err(e), Expect::PageInfo(page)) if *e != EngineError::Cancelled => {
                    sink.store(page.get(), &Err(e.clone()));
                }
                (Ok(Payload::PageInfos { first, pages }), _) => {
                    sink.store_batch(first.get(), pages);
                }
                _ => {}
            }
        }
        if let Some(tx) = pending.reply_to {
            // A caller that gave up (cancelled) dropped its receiver: the
            // failed send drops the delivery and with it the render target,
            // which returns the slot now that the host is done with it.
            let _ = tx.try_send(Delivery::Reply(result, pending.target));
        }
        // `pending` drops here, releasing the request's gate admission.
        Ok(())
    }

    fn post_mortem(&self, end: ReadEnd) {
        match &end {
            ReadEnd::Violation(why) => self.kill(KillReason::Protocol(why.clone())),
            // The pipe ended but the process may still be running (it closed
            // its end, or a read failed): it is of no use any more.
            ReadEnd::Closed | ReadEnd::Io(_) => {
                // Normally the host is exiting; give it a moment to finish so
                // its own exit code is kept.
                if self.child.wait(Duration::from_millis(500)).is_none() {
                    let why = match &end {
                        ReadEnd::Io(e) => e.clone(),
                        _ => "closed the reply pipe".into(),
                    };
                    self.kill(KillReason::Unresponsive(why));
                }
            }
        }
        let exit = self.child.wait(Duration::from_secs(5));
        let memory_limit = self.child.hit_memory_limit();
        let reason = lock(&self.kill_reason).clone();
        let killed_by_us = exit == Some(KILLED_BY_PARENT);
        let (cause, description) = match reason {
            Some(KillReason::Shutdown) => (DeathCause::Shutdown, "document closed".to_owned()),
            Some(KillReason::Deadline { id }) if killed_by_us => (
                DeathCause::Deadline { culprit: id },
                "a request overran its deadline; the render host was terminated".to_owned(),
            ),
            Some(KillReason::Protocol(why)) if killed_by_us => (
                DeathCause::Crash,
                format!("the render host sent an invalid reply ({why}) and was terminated"),
            ),
            Some(KillReason::Unresponsive(why)) if killed_by_us => (
                DeathCause::Crash,
                format!("the render host stopped responding ({why}) and was terminated"),
            ),
            _ if memory_limit => (
                DeathCause::Crash,
                match self.memory_limit {
                    Some(limit) => format!(
                        "the render host exceeded its memory limit ({} MiB)",
                        limit >> 20
                    ),
                    None => "the render host exceeded its memory limit".to_owned(),
                },
            ),
            _ => (
                DeathCause::Crash,
                match exit {
                    Some(code) => format!("the render host {}", describe_exit(code)),
                    None => "the render host vanished".to_owned(),
                },
            ),
        };
        if cause != DeathCause::Shutdown {
            log::warn!(
                "render host {} (pid {}): {description}",
                self.generation,
                self.child.pid()
            );
        }
        let entries = {
            let mut table = lock(&self.table);
            let entries = std::mem::take(&mut table.entries);
            let in_flight = entries
                .iter()
                .map(|(id, p)| InFlight {
                    id: *id,
                    subject: p.subject,
                })
                .collect();
            // Recorded before `closed` is visible: a caller that finds the
            // table closed always finds the death record.
            let _ = self.death.set(Death {
                cause,
                description,
                in_flight,
            });
            table.closed = true;
            entries
        };
        if let Some(pool) = &self.slots {
            pool.close();
        }
        for (_, pending) in entries {
            if let Some(tx) = pending.reply_to {
                let _ = tx.try_send(Delivery::Lost);
            }
        }
    }
}

/// Reply reader thread of one connection.
fn reader_main(conn: &Weak<Connection>, replies: &ServerPipe, process: BorrowedHandle<'_>) {
    let end = catch_unwind(AssertUnwindSafe(|| read_loop(conn, replies, process)))
        .unwrap_or_else(|_| ReadEnd::Violation("reply reader panicked".into()));
    // Nobody to tell if the connection is already gone.
    if let Some(conn) = conn.upgrade() {
        conn.post_mortem(end);
    }
}

fn read_loop(
    conn: &Weak<Connection>,
    replies: &ServerPipe,
    process: BorrowedHandle<'_>,
) -> ReadEnd {
    let mut tick = || {
        if let Some(conn) = conn.upgrade() {
            conn.check_deadlines();
        }
    };
    let mut reader = PipeReader {
        pipe: replies,
        process,
        every: DEADLINE_TICK,
        tick: &mut tick,
    };
    loop {
        let payload = match read_frame(&mut reader, MAX_REPLY_FRAME) {
            Ok(Some(payload)) => payload,
            Ok(None) => return ReadEnd::Closed,
            Err(FrameError::Protocol(e)) => return ReadEnd::Violation(e.to_string()),
            Err(FrameError::Io(e)) => return ReadEnd::Io(e.to_string()),
        };
        let Some(conn) = conn.upgrade() else {
            return ReadEnd::Closed;
        };
        match decode_reply(&payload) {
            Ok(Reply::Done { id, result }) => {
                if let Err(why) = conn.deliver(id, result) {
                    return ReadEnd::Violation(why);
                }
            }
            Ok(_) => return ReadEnd::Violation("unexpected handshake reply".into()),
            Err(e) => return ReadEnd::Violation(e.to_string()),
        }
    }
}

enum ReadEnd {
    /// The host closed its end of the pipe (normally because it died).
    Closed,
    Io(String),
    Violation(String),
}

fn startup_failure(child: &Child, what: &str) -> EngineError {
    let exit = child.wait(Duration::from_secs(2));
    if child.hit_memory_limit() {
        return startup_error("handshake", "the host exceeded its memory limit");
    }
    match exit {
        Some(code) => startup_error("handshake", format!("the host {}", describe_exit(code))),
        None => startup_error("handshake", what),
    }
}

// --- render targets ----------------------------------------------------------

/// Fixed-size tile slots in one shared section (ADR 0008 §1.4).
pub(crate) struct SlotPool {
    section: Section,
    view: View,
    slot_bytes: usize,
    count: u32,
    state: Mutex<PoolState>,
    cv: Condvar,
}

struct PoolState {
    free: Vec<u32>,
    closed: bool,
}

impl SlotPool {
    fn new(count: u32, slot_bytes: usize) -> io::Result<Arc<Self>> {
        let len = (count as usize)
            .checked_mul(slot_bytes)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "slot section too large"))?;
        let section = Section::create(len)?;
        // The parent only ever reads slots.
        let view = section.map(Access::Read)?;
        Ok(Arc::new(Self {
            section,
            view,
            slot_bytes,
            count,
            state: Mutex::new(PoolState {
                free: (0..count).rev().collect(),
                closed: false,
            }),
            cv: Condvar::new(),
        }))
    }

    fn acquire(
        self: &Arc<Self>,
        cancel: Option<&CancelToken>,
        deadline: Instant,
    ) -> Result<TargetLease, CallError> {
        let mut st = lock(&self.state);
        loop {
            if st.closed {
                return Err(CallError::Lost);
            }
            if let Some(index) = st.free.pop() {
                return Ok(TargetLease::Slot(SlotLease {
                    pool: Arc::clone(self),
                    index,
                }));
            }
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(CallError::Cancelled);
            }
            if Instant::now() > deadline {
                return Err(CallError::Fatal(EngineError::Internal(
                    "no free render slot".into(),
                )));
            }
            st = self
                .cv
                .wait_timeout(st, POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn close(&self) {
        lock(&self.state).closed = true;
        self.cv.notify_all();
    }
}

pub(crate) struct SlotLease {
    pool: Arc<SlotPool>,
    index: u32,
}

impl Drop for SlotLease {
    fn drop(&mut self) {
        lock(&self.pool.state).free.push(self.index);
        self.pool.cv.notify_one();
    }
}

/// Where one render's pixels live until the caller has copied them out.
pub(crate) enum TargetLease {
    Slot(SlotLease),
    /// A section for one render that does not fit in a slot.
    Own {
        wire: RenderTarget,
        view: View,
        _section: Section,
    },
}

impl TargetLease {
    pub(crate) fn wire(&self) -> RenderTarget {
        match self {
            Self::Slot(lease) => RenderTarget::Slot(lease.index),
            Self::Own { wire, .. } => *wire,
        }
    }

    /// Copies the rendered pixels into `dst`.
    pub(crate) fn copy_to(&self, dst: &mut [u8]) -> bool {
        match self {
            Self::Slot(lease) => {
                let offset = lease.index as usize * lease.pool.slot_bytes;
                dst.len() <= lease.pool.slot_bytes && lease.pool.view.read_into(offset, dst)
            }
            Self::Own { view, .. } => view.read_into(0, dst),
        }
    }
}
