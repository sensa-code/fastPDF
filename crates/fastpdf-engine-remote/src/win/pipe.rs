//! The command channel: two one-way, byte-mode named pipes per host
//! (ADR 0008 §1.3).
//!
//! The parent creates both pipes under a random name with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE`, a single instance each and
//! `PIPE_REJECT_REMOTE_CLIENTS`; the host opens them by name. After each
//! connection the parent checks that the client is the host process it just
//! started, so a squatter can at worst make the start fail.
//!
//! No handle is inherited. With inheritance, every `CreateProcess` that
//! another thread runs while the pipe ends are inheritable (for example a
//! `std::process::Command`) takes a copy, and that copy keeps the pipe open
//! after the host died, delaying crash detection.
//!
//! The parent ends use overlapped I/O, so every wait also watches the host
//! process and can run a periodic tick (deadline checks). The host ends are
//! ordinary synchronous handles used through `std::fs::File`.

use std::fs::File;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED, FALSE, GENERIC_READ, GENERIC_WRITE,
    GetLastError, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, ReadFile, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, WaitForMultipleObjects, WaitForSingleObject,
};

use super::{check, owned};

/// Kernel buffer per direction. Replies with text layers are a few hundred
/// KiB; writes larger than the buffer simply block until the reader drains.
const PIPE_BUFFER: u32 = 256 * 1024;
/// How long a read keeps draining after the host process has exited (only
/// matters if something else still holds the host's end of the pipe).
const DRAIN_AFTER_EXIT: u32 = 250;
/// Longest accepted channel name (the part after `\\.\pipe\`).
const MAX_NAME: usize = 96;

/// Parent end of one pipe.
#[derive(Debug)]
pub(crate) struct ServerPipe {
    pipe: OwnedHandle,
    /// Manual-reset event for this pipe's overlapped operations; one
    /// operation at a time per pipe (one reader thread / a writer mutex).
    event: OwnedHandle,
}

/// Both pipes of a new connection, waiting for the host to connect.
#[derive(Debug)]
pub(crate) struct Channel {
    /// Parent writes commands.
    pub(crate) commands: ServerPipe,
    /// Parent reads replies.
    pub(crate) replies: ServerPipe,
    name: String,
}

fn random_name() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    // RandomState is seeded from the OS RNG, so the name is unpredictable;
    // the client PID check is the actual guarantee.
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(seq);
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!(
        "fastpdf-render-{}-{seq}-{:016x}",
        std::process::id(),
        h.finish()
    )
}

/// `\\.\pipe\<name>-c` (commands) or `-r` (replies), NUL-terminated.
fn pipe_path(name: &str, commands: bool) -> Vec<u16> {
    let suffix = if commands { "c" } else { "r" };
    format!(r"\\.\pipe\{name}-{suffix}")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

/// A channel name as passed on the host's command line.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Parent side: creates both pipes of a new channel.
pub(crate) fn listen() -> io::Result<Channel> {
    let mut last = None;
    for _ in 0..8 {
        let name = random_name();
        let created = create_server(&name, true)
            .and_then(|commands| Ok((commands, create_server(&name, false)?)));
        match created {
            Ok((commands, replies)) => {
                return Ok(Channel {
                    commands,
                    replies,
                    name,
                });
            }
            // FIRST_PIPE_INSTANCE: somebody already owns this name.
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("cannot create a private pipe")))
}

fn create_server(name: &str, commands: bool) -> io::Result<ServerPipe> {
    let path = pipe_path(name, commands);
    let direction = if commands {
        PIPE_ACCESS_OUTBOUND
    } else {
        PIPE_ACCESS_INBOUND
    };
    // SAFETY: `path` is NUL-terminated; null security attributes give the
    // default security descriptor.
    let raw = unsafe {
        CreateNamedPipeW(
            path.as_ptr(),
            direction | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            std::ptr::null(),
        )
    };
    // SAFETY: a fresh handle (or a failure value).
    let pipe = unsafe { owned(raw) }?;
    // SAFETY: manual-reset, initially non-signaled, unnamed event.
    let raw = unsafe { CreateEventW(std::ptr::null(), TRUE, FALSE, std::ptr::null()) };
    // SAFETY: a fresh handle (or a failure value).
    let event = unsafe { owned(raw) }?;
    Ok(ServerPipe { pipe, event })
}

impl Channel {
    /// The name the host needs to connect.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Waits until process `pid` (handle `process`) has connected to both
    /// pipes. Fails if the process exits first, if `deadline` passes, or if
    /// any other process connected.
    pub(crate) fn accept(
        &self,
        process: BorrowedHandle<'_>,
        pid: u32,
        deadline: Instant,
    ) -> io::Result<()> {
        self.commands.accept(process, pid, deadline)?;
        self.replies.accept(process, pid, deadline)
    }
}

/// Host side: connects to the channel the parent named on our command line.
pub(crate) fn connect(name: &str) -> io::Result<(File, File)> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad channel name",
        ));
    }
    let open = |commands: bool| -> io::Result<File> {
        let path = pipe_path(name, commands);
        let access = if commands {
            GENERIC_READ
        } else {
            GENERIC_WRITE
        };
        // SAFETY: `path` is NUL-terminated; null security attributes and
        // template. SECURITY_IDENTIFICATION: whoever owns the server end can
        // identify, but not impersonate, the host.
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: a fresh handle (or a failure value).
        unsafe { owned(raw) }.map(File::from)
    };
    let commands = open(true)?;
    let replies = open(false)?;
    Ok((commands, replies))
}

/// An overlapped operation in flight. Until it is finished, the kernel may
/// still write into the operation's buffer and `OVERLAPPED`; dropping an
/// unfinished `InFlight` (an early return or unwinding) therefore cancels
/// the operation and waits for it, so neither is released too early.
struct InFlight<'a> {
    pipe: &'a ServerPipe,
    ov: *mut OVERLAPPED,
    done: bool,
}

impl InFlight<'_> {
    /// Waits for the operation and returns its byte count.
    fn finish(mut self) -> io::Result<u32> {
        self.done = true;
        let mut n = 0;
        // SAFETY: `ov` is the live OVERLAPPED of an operation started on
        // this pipe; with bWait = TRUE the call returns only once the
        // operation is complete.
        let ok =
            unsafe { GetOverlappedResult(self.pipe.pipe.as_raw_handle(), self.ov, &mut n, TRUE) };
        if ok != 0 {
            Ok(n)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Cancels the operation and waits until the cancellation took effect.
    /// Data that arrived first is still reported.
    fn cancel(self) -> io::Result<u32> {
        // SAFETY: cancels only the operation identified by `ov` on this pipe.
        unsafe { CancelIoEx(self.pipe.pipe.as_raw_handle(), self.ov) };
        self.finish()
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if !self.done {
            let mut n = 0;
            // SAFETY: as in `cancel` and `finish`; blocking here is what
            // keeps the buffer alive until the kernel is done with it.
            unsafe {
                CancelIoEx(self.pipe.pipe.as_raw_handle(), self.ov);
                GetOverlappedResult(self.pipe.pipe.as_raw_handle(), self.ov, &mut n, TRUE);
            }
        }
    }
}

fn last_error() -> u32 {
    // SAFETY: reads the calling thread's last-error value.
    unsafe { GetLastError() }
}

fn host_gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "render host exited")
}

/// Waits on `event` and `process` for up to `ms`; returns the wait code.
fn wait_two(event: &OwnedHandle, process: BorrowedHandle<'_>, ms: u32) -> u32 {
    let handles = [event.as_raw_handle(), process.as_raw_handle()];
    // SAFETY: two valid handles that outlive the call.
    unsafe { WaitForMultipleObjects(2, handles.as_ptr(), FALSE, ms) }
}

fn millis_until(deadline: Instant) -> u32 {
    let left = deadline.saturating_duration_since(Instant::now());
    u32::try_from(left.as_millis()).unwrap_or(u32::MAX - 1)
}

impl ServerPipe {
    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED {
            hEvent: self.event.as_raw_handle(),
            ..OVERLAPPED::default()
        }
    }

    /// Waits for the client to connect, then checks it is process `pid`.
    fn accept(&self, process: BorrowedHandle<'_>, pid: u32, deadline: Instant) -> io::Result<()> {
        let mut ov = self.overlapped();
        let ov_ptr: *mut OVERLAPPED = &mut ov;
        // SAFETY: `ov` stays in place until the operation finished (the
        // `InFlight` guard below waits for it on every path).
        let ok = unsafe { ConnectNamedPipe(self.pipe.as_raw_handle(), ov_ptr) };
        if ok == 0 {
            match last_error() {
                ERROR_PIPE_CONNECTED => {}
                ERROR_IO_PENDING => {
                    let op = InFlight {
                        pipe: self,
                        ov: ov_ptr,
                        done: false,
                    };
                    match wait_two(&self.event, process, millis_until(deadline)) {
                        WAIT_OBJECT_0 => {
                            op.finish()?;
                        }
                        w if w == WAIT_OBJECT_0 + 1 => {
                            let _ = op.cancel();
                            return Err(host_gone());
                        }
                        _ => {
                            let _ = op.cancel();
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "render host did not connect",
                            ));
                        }
                    }
                }
                e => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
        let mut client = 0u32;
        // SAFETY: queries our own server handle into a local.
        check(unsafe { GetNamedPipeClientProcessId(self.pipe.as_raw_handle(), &mut client) })?;
        if client == pid {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("process {client} connected to the render pipe instead of {pid}"),
            ))
        }
    }

    /// Reads up to `buf.len()` bytes; `Ok(0)` means the host closed its end.
    ///
    /// While waiting, `process` (the host) is watched: once it has exited,
    /// whatever it wrote before dying is still drained, then the read fails.
    /// `tick` runs every `tick_every` while no data arrives.
    pub(crate) fn read(
        &self,
        buf: &mut [u8],
        process: BorrowedHandle<'_>,
        tick_every: Duration,
        tick: &mut dyn FnMut(),
    ) -> io::Result<usize> {
        let mut ov = self.overlapped();
        let ov_ptr: *mut OVERLAPPED = &mut ov;
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: `buf` (borrowed for the whole call) and `ov` (a local that
        // is never moved) outlive the operation: the `InFlight` guard below
        // waits for it on every exit path, unwinding included.
        let ok = unsafe {
            ReadFile(
                self.pipe.as_raw_handle(),
                buf.as_mut_ptr(),
                len,
                std::ptr::null_mut(),
                ov_ptr,
            )
        };
        if ok == 0 {
            match last_error() {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE => return Ok(0),
                e => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
        let op = InFlight {
            pipe: self,
            ov: ov_ptr,
            done: false,
        };
        let tick_ms = u32::try_from(tick_every.as_millis())
            .unwrap_or(u32::MAX)
            .max(1);
        let result = if ok != 0 {
            op.finish()
        } else {
            loop {
                match wait_two(&self.event, process, tick_ms) {
                    WAIT_OBJECT_0 => break op.finish(),
                    w if w == WAIT_OBJECT_0 + 1 => {
                        // The host is gone and its end of the pipe closed
                        // with it, so the read completes at once with buffered
                        // data or ERROR_BROKEN_PIPE. Do not wait forever in
                        // case another process got hold of that end.
                        // SAFETY: waits on our own event handle.
                        let r = unsafe {
                            WaitForSingleObject(self.event.as_raw_handle(), DRAIN_AFTER_EXIT)
                        };
                        break if r == WAIT_OBJECT_0 {
                            op.finish()
                        } else {
                            op.cancel()
                        };
                    }
                    WAIT_TIMEOUT => tick(),
                    _ => break op.cancel(),
                }
            }
        };
        match result {
            Ok(n) => Ok(n as usize),
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
            Err(e) if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) => Err(host_gone()),
            Err(e) => Err(e),
        }
    }

    /// Writes all of `data`, giving up when the host exits or after
    /// `timeout` (a host that stopped reading).
    pub(crate) fn write_all(
        &self,
        mut data: &[u8],
        process: BorrowedHandle<'_>,
        timeout: Duration,
    ) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        while !data.is_empty() {
            let mut ov = self.overlapped();
            let ov_ptr: *mut OVERLAPPED = &mut ov;
            let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
            // SAFETY: `data` (borrowed for the whole call) and `ov` (a local
            // that is never moved) outlive the operation: the `InFlight`
            // guard below waits for it on every exit path.
            let ok = unsafe {
                WriteFile(
                    self.pipe.as_raw_handle(),
                    data.as_ptr(),
                    len,
                    std::ptr::null_mut(),
                    ov_ptr,
                )
            };
            if ok == 0 {
                match last_error() {
                    ERROR_IO_PENDING => {}
                    ERROR_BROKEN_PIPE | ERROR_NO_DATA => return Err(host_gone()),
                    e => return Err(io::Error::from_raw_os_error(e as i32)),
                }
            }
            let op = InFlight {
                pipe: self,
                ov: ov_ptr,
                done: false,
            };
            let written = if ok != 0 {
                op.finish()?
            } else {
                match wait_two(&self.event, process, millis_until(deadline)) {
                    WAIT_OBJECT_0 => op.finish().map_err(|e| match e.raw_os_error() {
                        Some(c) if c == ERROR_BROKEN_PIPE as i32 || c == ERROR_NO_DATA as i32 => {
                            host_gone()
                        }
                        _ => e,
                    })?,
                    w if w == WAIT_OBJECT_0 + 1 => {
                        let _ = op.cancel();
                        return Err(host_gone());
                    }
                    WAIT_TIMEOUT => {
                        let _ = op.cancel();
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "render host stopped reading commands",
                        ));
                    }
                    _ => {
                        let err = io::Error::last_os_error();
                        let _ = op.cancel();
                        return Err(err);
                    }
                }
            };
            if written == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            data = &data[(written as usize).min(data.len())..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::windows::io::AsHandle;

    use super::*;
    use crate::win::process::current_process;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn channel_carries_bytes_both_ways() {
        let ch = listen().unwrap();
        let (mut host_in, mut host_out) = connect(ch.name()).unwrap();
        let me = current_process();
        ch.accept(me.as_handle(), std::process::id(), soon())
            .unwrap();
        ch.commands
            .write_all(b"hello host", me.as_handle(), Duration::from_secs(5))
            .unwrap();
        let mut buf = [0u8; 10];
        host_in.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello host");

        host_out.write_all(b"hi parent").unwrap();
        let mut got = Vec::new();
        while got.len() < 9 {
            let mut chunk = [0u8; 16];
            let n = ch
                .replies
                .read(
                    &mut chunk,
                    me.as_handle(),
                    Duration::from_millis(5),
                    &mut || {},
                )
                .unwrap();
            got.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(got, b"hi parent");

        // Closing the host end reads as end of stream.
        drop(host_out);
        let mut chunk = [0u8; 4];
        let n = ch
            .replies
            .read(
                &mut chunk,
                me.as_handle(),
                Duration::from_millis(5),
                &mut || {},
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_foreign_client_is_refused() {
        let ch = listen().unwrap();
        let _ends = connect(ch.name()).unwrap();
        let me = current_process();
        let err = ch.accept(me.as_handle(), 4, soon()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // The single instance is taken: nobody else can connect either.
        assert!(connect(ch.name()).is_err());
    }

    #[test]
    fn accepting_gives_up_at_the_deadline() {
        let ch = listen().unwrap();
        let me = current_process();
        let deadline = Instant::now() + Duration::from_millis(50);
        let err = ch
            .accept(me.as_handle(), std::process::id(), deadline)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn channel_names_are_validated() {
        assert!(valid_name(&random_name()));
        assert!(!valid_name(""));
        assert!(!valid_name(r"..\evil"));
        assert!(!valid_name(&"a".repeat(MAX_NAME + 1)));
    }

    #[test]
    fn reads_tick_while_waiting() {
        let ch = listen().unwrap();
        let (_host_in, host_out) = connect(ch.name()).unwrap();
        let me = current_process();
        ch.accept(me.as_handle(), std::process::id(), soon())
            .unwrap();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            let mut host_out = host_out;
            host_out.write_all(b"x").unwrap();
            host_out
        });
        let mut ticks = 0;
        let mut b = [0u8; 1];
        let n = ch
            .replies
            .read(
                &mut b,
                me.as_handle(),
                Duration::from_millis(5),
                &mut || ticks += 1,
            )
            .unwrap();
        assert_eq!((n, b[0]), (1, b'x'));
        assert!(ticks >= 3, "only {ticks} ticks");
        drop(writer.join());
    }

    #[test]
    fn writes_time_out_when_nobody_reads() {
        let ch = listen().unwrap();
        let _host_ends = connect(ch.name()).unwrap(); // open but never read
        let me = current_process();
        ch.accept(me.as_handle(), std::process::id(), soon())
            .unwrap();
        let big = vec![0u8; 4 * PIPE_BUFFER as usize];
        let err = ch
            .commands
            .write_all(&big, me.as_handle(), Duration::from_millis(100))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }
}
