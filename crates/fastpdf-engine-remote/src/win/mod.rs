//! Win32 plumbing for out-of-process rendering: named pipes, shared memory
//! sections, and the job-object-wrapped host process.
//!
//! These modules are the only place in the crate with `unsafe` code. Every
//! block states why it is sound; the common rules are:
//!
//! * a raw handle becomes an [`OwnedHandle`] only right after the call that
//!   created it (or after the parent handed it over), so it is closed
//!   exactly once;
//! * overlapped I/O never outlives its buffer or `OVERLAPPED`: an armed
//!   guard cancels and waits for the operation on every exit path;
//! * shared memory is reached through raw-pointer copies or through slices
//!   whose exclusivity the caller guarantees (slot ownership rules in
//!   `section.rs`).

#![allow(unsafe_code)]

pub(crate) mod pipe;
pub(crate) mod process;
pub(crate) mod section;

use std::collections::HashSet;
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX, SetErrorMode,
};
use windows_sys::core::BOOL;

/// `Ok(())` for a non-zero `BOOL`, the thread's last error otherwise.
fn check(ok: BOOL) -> io::Result<()> {
    if ok != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Wraps a handle that a Win32 call just returned, or reports the last
/// error when the call failed (null or `INVALID_HANDLE_VALUE`).
///
/// # Safety
///
/// `handle` must be a fresh handle owned by nobody else (or a failure
/// value), so that the returned `OwnedHandle` is its only owner.
unsafe fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the caller guarantees the handle is valid and unowned.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Raw handle values that the host adopted from its parent (command line or
/// commands) and has not closed yet. Adopting a value that is already owned
/// would create a second owner of one handle, so it is refused.
static ADOPTED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

/// Reserves `value` for adoption; fails if it is null, not a plausible
/// kernel handle, or currently adopted.
fn claim(value: u64) -> io::Result<RawHandle> {
    let plausible = value != 0 && value <= u64::from(u32::MAX) && value.is_multiple_of(4);
    let raw = usize::try_from(value)
        .ok()
        .filter(|_| plausible)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a handle value"))?;
    let mut adopted = ADOPTED.lock().unwrap_or_else(|e| e.into_inner());
    if !adopted.get_or_insert_with(HashSet::new).insert(value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "handle value already adopted",
        ));
    }
    Ok(std::ptr::without_provenance_mut(raw))
}

/// Forgets an adopted value after its handle was closed.
fn release(value: u64) {
    let mut adopted = ADOPTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(set) = adopted.as_mut() {
        set.remove(&value);
    }
}

/// An adopted handle value, released on drop. Owners declare it after the
/// handle itself, so the handle is closed before the value can be adopted
/// again.
#[derive(Debug)]
struct Claim(u64);

impl Claim {
    fn take(value: u64) -> io::Result<(Self, RawHandle)> {
        let raw = claim(value)?;
        Ok((Self(value), raw))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        release(self.0);
    }
}

/// Host process only: crashes end the process silently instead of showing
/// Windows Error Reporting or critical-error dialogs (the job object also
/// sets `DIE_ON_UNHANDLED_EXCEPTION`).
pub(crate) fn quiet_crash_dialogs() {
    // SAFETY: SetErrorMode only changes a process-wide flag word.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    }
}
