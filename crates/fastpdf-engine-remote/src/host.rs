//! The render host process behind [`run_host`](crate::run_host)
//! (ADR 0008 §1.2).
//!
//! The main thread reads commands and queues them on one of four lanes,
//! each with its own threads: renders (as many threads as FastPDF renders
//! tiles at a time, so the engine keeps as many caches as in-process; the
//! parent keeps more requests in flight, which wait here), other slow work
//! (opening, text, outline), single-page geometry (`PageInfo`, which
//! FastPDF's UI thread may be waiting for, so it never queues behind a
//! render) and background geometry batches (`PageInfos`). Only `Cancel` is
//! handled inline, so cancellation reaches a busy worker at once. Every request runs inside `catch_unwind` — on top of the
//! `GuardedDocument` the document is opened with — and ends with exactly one
//! `Done` reply, so the parent never waits for an answer that cannot come.
//! The only way a request goes unanswered is the death of the whole process,
//! which the parent detects.
//!
//! The host exits when the parent closes the command pipe. If the parent
//! dies, the job object kills the host (`KILL_ON_JOB_CLOSE`).

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};

use fastpdf_engine_api::{
    CancelToken, DocumentSource, EngineDocument, EngineError, GuardedDocument, OpenOptions,
    PageIndex, PdfEngine, PixelFormat, PixmapMut, SharedBytes, open_guarded,
};

use crate::protocol::{
    BUILD_ID, Command, DocumentRef, FrameError, Init, MAX_COMMAND_FRAME, Open, PROTOCOL_VERSION,
    Payload, Render, RenderTarget, Reply, WireEngineInfo, decode_command, encode_reply, read_frame,
};
use crate::win;
use crate::win::section::{
    FileView, MappedBytes, ParentFile, ParentSection, SlotTable, TargetView,
};

/// Command-line flag in front of the connection spec.
pub(crate) const IPC_FLAG: &str = "--fastpdf-ipc";

const EXIT_USAGE: u8 = 2;
const EXIT_HANDSHAKE: u8 = 3;
const EXIT_PROTOCOL: u8 = 4;
const EXIT_PIPE: u8 = 5;

/// Engines recurse on hostile input; give them more room than the 2 MiB
/// default (a stack overflow still only ends this process).
const WORKER_STACK: usize = 16 << 20;
/// Threads answering single-page geometry requests: one slow (hostile)
/// page cannot hold up the next request.
const GEOMETRY_THREADS: u32 = 4;
/// Read buffer of the command pipe.
const COMMAND_BUFFER: usize = 64 << 10;
/// Document files up to this size are read rather than mapped, as the
/// loader does in-process (ADR 0006); files on network drives are always
/// read (R10).
const READ_LIMIT: u64 = 64 << 20;

/// Document bytes handed over by the parent, adopted by this process.
enum Adopted {
    Section(ParentSection),
    File {
        file: ParentFile,
        len: u64,
        network: bool,
    },
}

impl Adopted {
    fn take(document: DocumentRef) -> io::Result<Self> {
        Ok(match document {
            DocumentRef::Section(s) => Self::Section(ParentSection::adopt(s.handle, s.len)?),
            DocumentRef::File {
                handle,
                len,
                network,
            } => Self::File {
                file: ParentFile::adopt(handle)?,
                len,
                network,
            },
        })
    }
}

/// The two arguments that tell a host which channel to connect to.
pub(crate) fn ipc_args(channel: &str) -> [OsString; 2] {
    [
        IPC_FLAG.into(),
        format!("{PROTOCOL_VERSION}:{channel}").into(),
    ]
}

/// Inverse of [`ipc_args`]; `None` for anything else.
pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    let mut args = args.into_iter();
    if args.next()?.to_str()? != IPC_FLAG {
        return None;
    }
    let spec = args.next()?.into_string().ok()?;
    if args.next().is_some() {
        return None;
    }
    let (version, channel) = spec.split_once(':')?;
    (version.parse::<u16>().ok()? == PROTOCOL_VERSION && win::pipe::valid_name(channel))
        .then(|| channel.to_owned())
}

pub(crate) fn run<F>(factory: F, args: impl IntoIterator<Item = OsString>) -> ExitCode
where
    F: Fn(&str) -> Option<Box<dyn PdfEngine>>,
{
    win::quiet_crash_dialogs();
    let Some(channel) = parse_args(args) else {
        eprintln!("render host: expected `{IPC_FLAG} <spec>` (started by FastPDF only)");
        return ExitCode::from(EXIT_USAGE);
    };
    let (mut commands, replies) = match win::pipe::connect(&channel) {
        Ok(pipes) => pipes,
        Err(e) => {
            eprintln!("render host: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let writer = Writer(Mutex::new(replies));
    let init = match next_command(&mut commands) {
        Ok(Some(Command::Init(init))) => init,
        _ => return ExitCode::from(EXIT_HANDSHAKE),
    };
    let state = match start(&factory, &init, writer) {
        Ok(state) => state,
        Err((writer, error)) => {
            let _ = writer.send(&encode_reply(&Reply::InitFailed(error)));
            return ExitCode::from(EXIT_HANDSHAKE);
        }
    };
    let lanes = [
        (LaneKind::Render, init.render_threads.max(1)),
        (LaneKind::Work, init.workers.max(1)),
        (LaneKind::Geometry, GEOMETRY_THREADS),
        (LaneKind::Batches, 1),
    ];
    for (kind, threads) in lanes {
        for n in 0..threads {
            let state = Arc::clone(&state);
            let spawned = std::thread::Builder::new()
                .name(format!("fastpdf-render-host-{kind:?}-{n}"))
                .stack_size(WORKER_STACK)
                .spawn(move || {
                    let _ = catch_unwind(AssertUnwindSafe(|| worker(&state, kind)));
                    // A worker never returns. If one unwinds out of its loop,
                    // the request it held would never be answered: end the
                    // process so the parent sees a crash, not a silent hang.
                    std::process::abort();
                });
            if spawned.is_err() && n == 0 {
                return ExitCode::from(EXIT_HANDSHAKE);
            }
        }
    }
    // Most commands are small: one read of the pipe usually brings a whole
    // frame (or several), not a header and then a payload.
    let mut commands = BufReader::with_capacity(COMMAND_BUFFER, commands);
    loop {
        match next_command(&mut commands) {
            Ok(Some(Command::Cancel { id })) => state.cancel(id),
            Ok(Some(Command::Init(_))) => return ExitCode::from(EXIT_PROTOCOL),
            Ok(Some(command)) => state.submit(command),
            // The parent closed the channel: done, whatever is still running.
            Ok(None) => return ExitCode::SUCCESS,
            Err(NextError::Protocol) => return ExitCode::from(EXIT_PROTOCOL),
            Err(NextError::Pipe) => return ExitCode::from(EXIT_PIPE),
        }
    }
}

enum NextError {
    Protocol,
    Pipe,
}

fn next_command(pipe: &mut impl Read) -> Result<Option<Command>, NextError> {
    match read_frame(pipe, MAX_COMMAND_FRAME) {
        Ok(Some(payload)) => decode_command(&payload)
            .map(Some)
            .map_err(|_| NextError::Protocol),
        Ok(None) => Ok(None),
        Err(FrameError::Protocol(_)) => Err(NextError::Protocol),
        Err(FrameError::Io(_)) => Err(NextError::Pipe),
    }
}

/// Reply pipe; whole frames are written under the lock.
struct Writer(Mutex<File>);

impl Writer {
    fn send(&self, frame: &[u8]) -> io::Result<()> {
        let mut pipe = self.0.lock().unwrap_or_else(|e| e.into_inner());
        pipe.write_all(frame)?;
        pipe.flush()
    }
}

/// Handshake: engine, slots, `Ready`.
fn start<F>(factory: &F, init: &Init, writer: Writer) -> Result<Arc<State>, (Writer, EngineError)>
where
    F: Fn(&str) -> Option<Box<dyn PdfEngine>>,
{
    if init.build_id != BUILD_ID {
        let msg = format!("build mismatch: parent {}, host {BUILD_ID}", init.build_id);
        return Err((writer, EngineError::Internal(msg)));
    }
    let engine = match catch_unwind(AssertUnwindSafe(|| factory(&init.engine))) {
        Ok(Some(engine)) => engine,
        Ok(None) => {
            let msg = format!("engine `{}` is not available in this host", init.engine);
            return Err((writer, EngineError::Unsupported(msg)));
        }
        Err(payload) => {
            let msg = panic_message(payload.as_ref());
            return Err((writer, EngineError::Panicked(msg)));
        }
    };
    let slots = match init.slots {
        Some(spec) => match SlotTable::adopt(spec.section.handle, spec.count, spec.slot_bytes) {
            Ok(table) => Some(table),
            Err(e) => {
                let msg = format!("cannot map the tile slots: {e}");
                return Err((writer, EngineError::Internal(msg)));
            }
        },
        None => None,
    };
    let info = engine.info();
    let ready = Reply::Ready {
        build_id: BUILD_ID.to_owned(),
        engine: WireEngineInfo {
            name: info.name.to_owned(),
            version: info.version.to_owned(),
            capabilities: info.capabilities,
        },
    };
    if writer.send(&encode_reply(&ready)).is_err() {
        return Err((writer, EngineError::Internal("reply pipe closed".into())));
    }
    Ok(Arc::new(State {
        engine,
        document: RwLock::new(None),
        slots,
        cancels: Mutex::new(HashMap::new()),
        render: Lane::default(),
        work: Lane::default(),
        geometry: Lane::default(),
        batches: Lane::default(),
        writer,
    }))
}

/// Which threads serve a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneKind {
    /// Tile and page renders, `render_threads` at a time.
    Render,
    /// Everything else that is slow: opening, text layers, outline, links.
    Work,
    Geometry,
    Batches,
}

impl LaneKind {
    fn of(command: &Command) -> Self {
        match command {
            Command::Render(_) => Self::Render,
            Command::PageInfo { .. } => Self::Geometry,
            Command::PageInfos { .. } => Self::Batches,
            _ => Self::Work,
        }
    }
}

#[derive(Default)]
struct Lane {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
}

struct Job {
    id: u64,
    command: Command,
    cancel: CancelToken,
}

struct State {
    engine: Box<dyn PdfEngine>,
    document: RwLock<Option<Arc<GuardedDocument>>>,
    slots: Option<SlotTable>,
    /// Cancel tokens of queued and running requests.
    cancels: Mutex<HashMap<u64, CancelToken>>,
    render: Lane,
    work: Lane,
    geometry: Lane,
    batches: Lane,
    writer: Writer,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn worker(state: &State, lane: LaneKind) {
    loop {
        let job = state.next_job(lane);
        let Job {
            id,
            command,
            cancel,
        } = job;
        // Backstop for panics outside the engine's guard (protocol or slot
        // handling in this file): the request still gets its terminal reply.
        let frame = catch_unwind(AssertUnwindSafe(|| {
            let result = state.execute(command, &cancel);
            encode_reply(&Reply::Done { id, result })
        }))
        .unwrap_or_else(|payload| {
            let msg = format!("render host: {}", panic_message(payload.as_ref()));
            encode_reply(&Reply::Done {
                id,
                result: Err(EngineError::Panicked(msg)),
            })
        });
        lock(&state.cancels).remove(&id);
        if state.writer.send(&frame).is_err() {
            // The parent is gone; nobody is waiting for anything.
            std::process::exit(i32::from(EXIT_PIPE));
        }
    }
}

impl State {
    fn lane(&self, kind: LaneKind) -> &Lane {
        match kind {
            LaneKind::Render => &self.render,
            LaneKind::Work => &self.work,
            LaneKind::Geometry => &self.geometry,
            LaneKind::Batches => &self.batches,
        }
    }

    fn submit(&self, command: Command) {
        let id = command.id().unwrap_or(0);
        let cancel = CancelToken::new();
        lock(&self.cancels).insert(id, cancel.clone());
        let lane = self.lane(LaneKind::of(&command));
        lock(&lane.queue).push_back(Job {
            id,
            command,
            cancel,
        });
        lane.wake.notify_one();
    }

    fn cancel(&self, id: u64) {
        if let Some(token) = lock(&self.cancels).get(&id) {
            token.cancel();
        }
    }

    fn next_job(&self, kind: LaneKind) -> Job {
        let lane = self.lane(kind);
        let mut queue = lock(&lane.queue);
        loop {
            if let Some(job) = queue.pop_front() {
                return job;
            }
            queue = lane.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn document(&self) -> Result<Arc<GuardedDocument>, EngineError> {
        self.document
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| EngineError::InvalidRequest("no document is open in this host".into()))
    }

    fn execute(&self, command: Command, cancel: &CancelToken) -> Result<Payload, EngineError> {
        match command {
            Command::Open(open) => self.open(open),
            Command::PageInfo { page, .. } => {
                self.document()?.page_info(page).map(Payload::PageInfo)
            }
            Command::PageInfos { first, count, .. } => self.page_infos(first, count, cancel),
            Command::Metadata { .. } => self.document()?.metadata().map(Payload::Metadata),
            Command::Render(render) => self.render(render, cancel),
            Command::TextLayer { page, .. } => self
                .document()?
                .text_layer(page, cancel)
                .map(Payload::TextLayer),
            Command::Outline { .. } => self.document()?.outline().map(Payload::Outline),
            Command::Links { page, .. } => self.document()?.links(page).map(Payload::Links),
            Command::MemoryUsage { .. } => Ok(Payload::MemoryUsage(
                self.document().ok().and_then(|d| d.memory_usage()),
            )),
            Command::Trim { pressure, .. } => {
                if let Ok(doc) = self.document() {
                    doc.trim_memory(pressure);
                }
                Ok(Payload::Trimmed)
            }
            Command::Init(_) | Command::Cancel { .. } => {
                Err(EngineError::InvalidRequest("not a request command".into()))
            }
        }
    }

    fn open(&self, open: Open) -> Result<Payload, EngineError> {
        // Take the handle first so it is closed on every path.
        let document =
            open.document.map(Adopted::take).transpose().map_err(|e| {
                EngineError::Internal(format!("cannot take the document bytes: {e}"))
            })?;
        if self.document().is_ok() {
            return Err(EngineError::InvalidRequest(
                "a document is already open in this host".into(),
            ));
        }
        let data = match document {
            Some(Adopted::Section(section)) => {
                let s = section
                    .section()
                    .ok_or_else(|| EngineError::Internal("document section missing".into()))?;
                let bytes = MappedBytes::map(s).map_err(|e| {
                    EngineError::Internal(format!("cannot map the document bytes: {e}"))
                })?;
                // The mapping keeps the section alive; the handle goes now.
                SharedBytes::from_owner(bytes)
            }
            Some(Adopted::File { file, len, network }) => {
                let fail = |e: io::Error| {
                    EngineError::Internal(format!("cannot read the document file: {e}"))
                };
                if network || len <= READ_LIMIT {
                    SharedBytes::from_vec(file.read(len).map_err(fail)?)
                } else {
                    SharedBytes::from_owner(FileView::map(file, len).map_err(fail)?)
                }
            }
            None => SharedBytes::from_vec(Vec::new()),
        };
        let source = DocumentSource {
            data,
            path: open.path,
        };
        let options = OpenOptions {
            password: open.password,
            limits: open.limits,
        };
        let doc = open_guarded(self.engine.as_ref(), source, &options)?;
        let page_count = doc.page_count();
        let mut slot = self.document.write().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err(EngineError::InvalidRequest(
                "a document is already open in this host".into(),
            ));
        }
        *slot = Some(Arc::new(doc));
        Ok(Payload::Opened { page_count })
    }

    /// Exactly `count` results, one per page (out-of-range pages included,
    /// as errors), so the parent can match them to its request.
    fn page_infos(
        &self,
        first: PageIndex,
        count: u32,
        cancel: &CancelToken,
    ) -> Result<Payload, EngineError> {
        let doc = self.document()?;
        let mut pages = Vec::with_capacity(count as usize);
        for i in 0..count {
            cancel.check()?;
            let page = PageIndex::new(first.get().saturating_add(i));
            pages.push(doc.page_info(page));
        }
        Ok(Payload::PageInfos { first, pages })
    }

    fn render(&self, render: Render, cancel: &CancelToken) -> Result<Payload, EngineError> {
        let own = match render.target {
            RenderTarget::Section(s) => Some(
                ParentSection::adopt(s.handle, s.len)
                    .map_err(|e| EngineError::Internal(format!("cannot take the target: {e}")))?,
            ),
            RenderTarget::Slot(_) => None,
        };
        let doc = self.document()?;
        let size = render.request.region.size();
        // Bounds width * height * 4 before any arithmetic on it.
        doc.limits().check_bitmap(size)?;
        let len = size.width as usize * size.height as usize * PixelFormat::BYTES_PER_PIXEL;
        let outcome = match (render.target, own) {
            (RenderTarget::Slot(index), _) => {
                let slots = self.slots.as_ref().ok_or_else(|| {
                    EngineError::InvalidRequest("this host has no tile slots".into())
                })?;
                let mut claim = slots.claim(index, len).map_err(|e| {
                    EngineError::InvalidRequest(format!("tile slot {index}: {e:?}"))
                })?;
                let mut target = PixmapMut::from_slice(claim.bytes(), size, render.format)?;
                doc.render(&render.request, &mut target, cancel)?
            }
            (RenderTarget::Section(_), Some(section)) => {
                let mut view = TargetView::map(&section, len)
                    .map_err(|e| EngineError::Internal(format!("cannot map the target: {e}")))?;
                let mut target = PixmapMut::from_slice(view.bytes(), size, render.format)?;
                doc.render(&render.request, &mut target, cancel)?
            }
            (RenderTarget::Section(_), None) => {
                return Err(EngineError::Internal("render target missing".into()));
            }
        };
        Ok(Payload::Rendered(outcome))
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsHandle;
    use std::time::{Duration, Instant};

    use fastpdf_engine_api::{EngineCapabilities, EngineInfo};

    use super::*;
    use crate::protocol::{Init, decode_reply, encode_command};
    use crate::win::process::current_process;

    struct NoEngine;

    impl PdfEngine for NoEngine {
        fn info(&self) -> EngineInfo {
            EngineInfo {
                name: "none",
                version: "0",
                capabilities: EngineCapabilities::default(),
            }
        }

        fn open(
            &self,
            _source: DocumentSource,
            _options: &OpenOptions,
        ) -> Result<Box<dyn EngineDocument>, EngineError> {
            Err(EngineError::Unsupported("no documents".into()))
        }
    }

    /// The host loop runs on a thread of this test process; the test plays
    /// the parent. Closing the channel must end the host by itself (no job
    /// object involved).
    #[test]
    fn the_host_ends_when_the_parent_closes_the_channel() {
        let channel = win::pipe::listen().unwrap();
        let args = ipc_args(channel.name()).to_vec();
        let host = std::thread::spawn(move || {
            let code = run(|_| Some(Box::new(NoEngine) as Box<dyn PdfEngine>), args);
            format!("{code:?}")
        });
        let me = current_process();
        let deadline = Instant::now() + Duration::from_secs(5);
        channel
            .accept(me.as_handle(), std::process::id(), deadline)
            .unwrap();
        let init = Command::Init(Init {
            build_id: BUILD_ID.into(),
            engine: "none".into(),
            workers: 1,
            render_threads: 1,
            slots: None,
        });
        channel
            .commands
            .write_all(
                &encode_command(&init).unwrap(),
                me.as_handle(),
                Duration::from_secs(5),
            )
            .unwrap();
        let mut reader = ReplyReader {
            channel: &channel,
            process: me.as_handle(),
        };
        let ready = read_frame(&mut reader, crate::protocol::MAX_REPLY_FRAME)
            .ok()
            .flatten()
            .and_then(|p| decode_reply(&p).ok());
        assert!(matches!(ready, Some(Reply::Ready { .. })), "{ready:?}");
        let closed = Instant::now();
        drop(channel);
        let code = host.join().unwrap();
        assert_eq!(code, format!("{:?}", ExitCode::SUCCESS));
        assert!(closed.elapsed() < Duration::from_secs(2));
    }

    struct ReplyReader<'a> {
        channel: &'a win::pipe::Channel,
        process: std::os::windows::io::BorrowedHandle<'a>,
    }

    impl std::io::Read for ReplyReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.channel.replies.read(buf, self.process, None, &mut || {
                Some(Duration::from_millis(20))
            })
        }
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn connection_arguments_round_trip() {
        let args = ipc_args("fastpdf-render-12-3-00ff");
        assert_eq!(
            parse_args(args.to_vec()).as_deref(),
            Some("fastpdf-render-12-3-00ff")
        );
    }

    #[test]
    fn foreign_arguments_are_rejected() {
        assert_eq!(parse_args(os(&[])), None);
        assert_eq!(parse_args(os(&["--render-host"])), None);
        assert_eq!(parse_args(os(&[IPC_FLAG])), None);
        assert_eq!(parse_args(os(&[IPC_FLAG, "1"])), None);
        assert_eq!(parse_args(os(&[IPC_FLAG, "999:abc"])), None);
        assert_eq!(parse_args(os(&[IPC_FLAG, "1:..\\x"])), None);
        assert_eq!(parse_args(os(&[IPC_FLAG, "1:"])), None);
        assert_eq!(parse_args(os(&[IPC_FLAG, "1:abc", "extra"])), None);
        assert_eq!(
            parse_args(os(&[IPC_FLAG, "1:abc-1"])).as_deref(),
            Some("abc-1")
        );
    }
}
