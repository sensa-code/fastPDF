//! Starting the render host inside a job object (ADR 0008 §3).
//!
//! The host is created directly inside a fresh job
//! (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), so it never runs a single instruction
//! outside its limits and cannot outlive FastPDF even if FastPDF dies in the
//! middle of starting it:
//!
//! * `JOB_OBJECT_LIMIT_PROCESS_MEMORY`: a commit limit; a decompression
//!   bomb or runaway allocation ends the host, not FastPDF.
//! * `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: the parent holds the only job
//!   handle, so the host dies with the parent, even when the parent is
//!   killed.
//! * `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`: no error-reporting
//!   dialog, no waiting for a dump.
//! * `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 1`: the host cannot start processes.
//! * UI restrictions (clipboard, desktop, global atoms, USER handles of
//!   other processes, system parameters); dropped only when the parent's own
//!   job does not allow a nested job with UI limits.
//!
//! A completion port on the job reports `JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT`,
//! the only way to tell an allocation failure from any other fail-fast exit.

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle};
use std::path::Path;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    DuplicateHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::{check, owned};

/// Exit code the parent uses when it terminates a host on purpose.
pub(crate) const KILLED_BY_PARENT: u32 = 0xDEAD;

#[derive(Debug, Clone, Copy)]
pub(crate) struct JobLimits {
    /// Commit limit of the host process in bytes.
    pub(crate) memory: Option<u64>,
}

/// A running host process and its job.
#[derive(Debug)]
pub(crate) struct Child {
    process: OwnedHandle,
    /// Only handle to the job: closing it (dropping `Child`) kills the host.
    _job: OwnedHandle,
    port: OwnedHandle,
    pid: u32,
}

/// Starts `program args...` inside a new job. No handle is inherited.
///
/// The process is created *in* the job (`PROC_THREAD_ATTRIBUTE_JOB_LIST`),
/// so there is no moment in which it runs, or could outlive the parent,
/// outside the job's limits; if the parent dies at any point after this
/// call, closing the job handle kills the host.
pub(crate) fn spawn(
    program: &Path,
    args: &[std::ffi::OsString],
    limits: JobLimits,
) -> io::Result<Child> {
    match spawn_in_job(program, args, limits, true) {
        Ok(child) => Ok(child),
        Err(first) => {
            // A process that already belongs to a job (the parent's) can
            // only be put into a nested job without UI restrictions.
            let child = spawn_in_job(program, args, limits, false).map_err(|_| first)?;
            log::debug!("render host job without UI restrictions");
            Ok(child)
        }
    }
}

fn spawn_in_job(
    program: &Path,
    args: &[std::ffi::OsString],
    limits: JobLimits,
    ui_restrictions: bool,
) -> io::Result<Child> {
    let (job, port) = create_job(limits, ui_restrictions)?;
    let app: Vec<u16> = program.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut cmd = command_line(program, args);
    let jobs: [HANDLE; 1] = [job.as_raw_handle()];

    let mut attrs = AttributeList::with_jobs(&jobs)?;
    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attrs.as_mut_ptr();
    let mut pi = PROCESS_INFORMATION::default();
    // DETACHED_PROCESS: a console-subsystem host gets no console (and no
    // conhost.exe) at all.
    let flags = DETACHED_PROCESS | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
    // SAFETY: `app` and `cmd` are NUL-terminated UTF-16 buffers (`cmd` is
    // writable as CreateProcessW requires); `si` and its attribute list,
    // which points into `jobs`, live until the call returns; `pi` is a valid
    // out-parameter. bInheritHandles = FALSE: the host inherits nothing.
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            FALSE,
            flags,
            std::ptr::null(),
            std::ptr::null(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    let spawn_error = io::Error::last_os_error();
    drop(attrs);
    if ok == 0 {
        return Err(spawn_error);
    }
    // SAFETY: CreateProcessW succeeded; both handles are new and ours.
    let process = unsafe { owned(pi.hProcess) }?;
    // SAFETY: as above. Not needed: the process already runs.
    drop(unsafe { owned(pi.hThread) });
    Ok(Child {
        process,
        _job: job,
        port,
        pid: pi.dwProcessId,
    })
}

fn terminate(process: &OwnedHandle) {
    // SAFETY: terminates a process we own a handle to.
    unsafe {
        TerminateProcess(process.as_raw_handle(), KILLED_BY_PARENT);
    }
}

/// `PROC_THREAD_ATTRIBUTE_LIST` with a job list.
struct AttributeList<'a> {
    /// Pointer-aligned backing store.
    storage: Vec<usize>,
    initialized: bool,
    _lists: std::marker::PhantomData<&'a [HANDLE]>,
}

impl<'a> AttributeList<'a> {
    fn with_jobs(jobs: &'a [HANDLE]) -> io::Result<Self> {
        const COUNT: u32 = 1;
        let mut size = 0usize;
        // SAFETY: size query with a null list; fails with
        // ERROR_INSUFFICIENT_BUFFER and writes the size.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), COUNT, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut list = Self {
            storage: vec![0usize; size.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
            _lists: std::marker::PhantomData,
        };
        // SAFETY: `storage` holds at least `size` pointer-aligned bytes.
        check(unsafe {
            InitializeProcThreadAttributeList(list.as_mut_ptr(), COUNT, 0, &mut size)
        })?;
        list.initialized = true;
        // SAFETY: the list was initialized for one attribute; `jobs`
        // outlives the list (lifetime 'a) as UpdateProcThreadAttribute
        // requires.
        check(unsafe {
            UpdateProcThreadAttribute(
                list.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                jobs.as_ptr().cast::<c_void>(),
                std::mem::size_of_val(jobs),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        })?;
        Ok(list)
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for AttributeList<'_> {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: the storage holds a list that
            // InitializeProcThreadAttributeList initialized; deleted once.
            unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
        }
    }
}

fn create_job(limits: JobLimits, ui: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    // SAFETY: unnamed job with default security.
    let job = unsafe { owned(CreateJobObjectW(std::ptr::null(), std::ptr::null())) }?;
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    info.BasicLimitInformation.ActiveProcessLimit = 1;
    if let Some(bytes) = limits.memory {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        info.ProcessMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
    }
    set_job_info(&job, JobObjectExtendedLimitInformation, &info)?;
    if ui {
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        };
        set_job_info(&job, JobObjectBasicUIRestrictions, &ui)?;
    }
    // SAFETY: creates a new completion port (no file handle associated).
    let port = unsafe {
        owned(CreateIoCompletionPort(
            INVALID_HANDLE_VALUE,
            std::ptr::null_mut(),
            0,
            1,
        ))
    }?;
    let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        CompletionKey: std::ptr::null_mut(),
        CompletionPort: port.as_raw_handle(),
    };
    set_job_info(&job, JobObjectAssociateCompletionPortInformation, &assoc)?;
    Ok((job, port))
}

fn set_job_info<T>(job: &OwnedHandle, class: i32, info: &T) -> io::Result<()> {
    // SAFETY: `info` points to a properly initialized structure of the type
    // that `class` names, and its exact size is passed along.
    check(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            class,
            (info as *const T).cast::<c_void>(),
            std::mem::size_of::<T>() as u32,
        )
    })
}

impl Child {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn process(&self) -> BorrowedHandle<'_> {
        use std::os::windows::io::AsHandle;
        self.process.as_handle()
    }

    /// Ends the host now (deadline, protocol violation, shutdown).
    pub(crate) fn kill(&self) {
        terminate(&self.process);
    }

    /// Waits up to `timeout` for the host to exit; returns its exit code.
    pub(crate) fn wait(&self, timeout: Duration) -> Option<u32> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: waits on a process handle we own.
        let r = unsafe { WaitForSingleObject(self.process.as_raw_handle(), ms) };
        if r == WAIT_OBJECT_0 {
            let mut code = 0u32;
            // SAFETY: reads the exit code of a process handle we own into a
            // local.
            let ok = unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) };
            (ok != 0).then_some(code)
        } else {
            None
        }
    }

    /// True once the job reported that the host hit its memory limit.
    /// Drains the job's notification queue.
    pub(crate) fn hit_memory_limit(&self) -> bool {
        let mut hit = false;
        loop {
            let mut message = 0u32;
            let mut key = 0usize;
            let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
            // SAFETY: polls our own completion port (timeout 0) into locals;
            // job notifications carry no OVERLAPPED to free.
            let ok = unsafe {
                GetQueuedCompletionStatus(
                    self.port.as_raw_handle(),
                    &mut message,
                    &mut key,
                    &mut ov,
                    0,
                )
            };
            if ok == 0 {
                return hit;
            }
            hit |= message == JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT;
        }
    }

    /// Committed private bytes of the host.
    pub(crate) fn private_bytes(&self) -> Option<u64> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..PROCESS_MEMORY_COUNTERS_EX::default()
        };
        // SAFETY: `counters` is a PROCESS_MEMORY_COUNTERS_EX whose size is
        // passed in `cb`, as the EX variant of the call expects.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                self.process.as_raw_handle(),
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX)
                    .cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
        (ok != 0).then_some(counters.PrivateUsage as u64)
    }

    /// Duplicates `handle` into the host with `access` rights and returns
    /// the value it has there (to be sent in a command). The host owns the
    /// copy from then on.
    pub(crate) fn duplicate_into(
        &self,
        handle: BorrowedHandle<'_>,
        access: u32,
    ) -> io::Result<u64> {
        let mut target: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a handle we own into the host process, for which
        // CreateProcessW gave us a full-access handle; `target` is a local.
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle.as_raw_handle(),
                self.process.as_raw_handle(),
                &mut target,
                access,
                FALSE,
                0,
            )
        })?;
        Ok(target as usize as u64)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Closing the job kills the host anyway (KILL_ON_JOB_CLOSE);
        // terminating first makes that independent of field drop order.
        terminate(&self.process);
    }
}

/// A real (waitable, duplicable) handle to the current process; tests use
/// it where a "host that never exits" is needed.
#[cfg(test)]
pub(crate) fn current_process() -> OwnedHandle {
    use windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS;
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicates the current-process pseudo handle into a real one.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentProcess(),
            GetCurrentProcess(),
            &mut out,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    assert!(ok != 0, "DuplicateHandle failed");
    // SAFETY: a fresh handle that only we own.
    unsafe { owned(out) }.unwrap_or_else(|e| panic!("{e}"))
}

/// Windows command line for `program args...` (MSVC CRT quoting rules).
pub(crate) fn command_line(program: &Path, args: &[std::ffi::OsString]) -> Vec<u16> {
    let mut cmd: Vec<u16> = Vec::new();
    // argv[0] is not unescaped by the CRT: quote it verbatim.
    cmd.push(u16::from(b'"'));
    cmd.extend(program.as_os_str().encode_wide());
    cmd.push(u16::from(b'"'));
    for arg in args {
        cmd.push(u16::from(b' '));
        push_quoted(&mut cmd, arg);
    }
    cmd.push(0);
    cmd
}

fn push_quoted(out: &mut Vec<u16>, arg: &OsStr) {
    let wide: Vec<u16> = arg.encode_wide().collect();
    let special = |c: u16| matches!(c, 0x20 | 0x09 | 0x0A | 0x0B | 0x22);
    if !wide.is_empty() && !wide.iter().any(|&c| special(c)) {
        out.extend_from_slice(&wide);
        return;
    }
    out.push(u16::from(b'"'));
    let mut backslashes = 0usize;
    for &c in &wide {
        if c == u16::from(b'\\') {
            backslashes += 1;
        } else {
            if c == u16::from(b'"') {
                // Escape the run of backslashes and the quote itself.
                out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(c);
    }
    // Backslashes before the closing quote must be doubled.
    out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
    out.push(u16::from(b'"'));
}

/// Human-readable cause for a host exit code.
pub(crate) fn describe_exit(code: u32) -> String {
    let what = match code {
        0 => "exited",
        KILLED_BY_PARENT => "was terminated",
        0xC000_00FD => "stack overflow",
        0xC000_0409 => "fail-fast abort (panic=abort, abort() or allocation failure)",
        0xC000_0005 => "access violation",
        0xC000_0017 | 0xC000_012D => "out of memory",
        0xC000_001D => "illegal instruction",
        0xC000_0094 => "integer division by zero",
        0xC000_0374 => "heap corruption",
        0xC000_0420 => "assertion failure",
        0xC000_0044 => "quota exceeded",
        0xC000_013A => "terminated (Ctrl+C)",
        _ => "exited unexpectedly",
    };
    format!("{what} (exit code {code:#010x})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    #[test]
    fn command_lines_quote_like_the_crt_expects() {
        let args: Vec<std::ffi::OsString> = [
            "--render-host",
            "",
            "a b",
            r#"say "hi""#,
            r"C:\dir\",
            r"C:\dir with space\",
            r#"back\\"quote"#,
        ]
        .iter()
        .map(Into::into)
        .collect();
        let line = command_line(Path::new(r"C:\Program Files\FastPDF\fastpdf.exe"), &args);
        let expected = utf16(concat!(
            r#""C:\Program Files\FastPDF\fastpdf.exe" --render-host "" "a b" "#,
            r#""say \"hi\"" C:\dir\ "C:\dir with space\\" "back\\\\\"quote""#
        ));
        assert_eq!(
            String::from_utf16_lossy(&line),
            String::from_utf16_lossy(&expected)
        );
    }

    #[test]
    fn exit_codes_have_readable_causes() {
        assert!(describe_exit(0xC000_00FD).contains("stack overflow"));
        assert!(describe_exit(0xC000_0409).contains("fail-fast"));
        assert!(describe_exit(7).contains("0x00000007"));
    }
}
