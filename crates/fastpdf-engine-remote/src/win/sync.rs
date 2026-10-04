//! Kernel events and a semaphore shared with the render host: the slot
//! channel's wake-ups (`channel.rs`).
//!
//! The parent creates them and duplicates them into the host with the
//! least access the host needs: `EVENT_MODIFY_STATE` for the completion
//! events (it only sets them) and `SYNCHRONIZE` for the request semaphore
//! (it only waits on it). The host adds a semaphore of its own for renders
//! that arrive as commands. Waits never poll; an idle host blocks on the
//! semaphores without a timeout.

use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{FALSE, TRUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateSemaphoreW, INFINITE, ReleaseSemaphore, SetEvent, WaitForMultipleObjects,
    WaitForSingleObject,
};

use super::{Claim, check, owned};

/// A kernel event.
#[derive(Debug)]
pub(crate) struct Event {
    // Declared before the claim: closed before the value is released.
    handle: OwnedHandle,
    _claim: Option<Claim>,
}

impl Event {
    /// An unnamed event; `manual`: it stays set until reset, otherwise one
    /// wait consumes it.
    pub(crate) fn new(manual: bool) -> io::Result<Self> {
        let reset = if manual { TRUE } else { FALSE };
        // SAFETY: null attributes and name: an unnamed event, not set.
        let raw = unsafe { CreateEventW(std::ptr::null(), reset, FALSE, std::ptr::null()) };
        Ok(Self {
            // SAFETY: a fresh handle (or a failure value).
            handle: unsafe { owned(raw) }?,
            _claim: None,
        })
    }

    /// Host side: an event the parent duplicated into this process.
    pub(crate) fn adopt(value: u64) -> io::Result<Self> {
        let (claim, raw) = Claim::take(value)?;
        // SAFETY: the parent duplicated this handle into our process for the
        // handshake only, and `Claim::take` refuses a value that is already
        // adopted. Setting a handle that is not an event fails cleanly.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(Self {
            handle,
            _claim: Some(claim),
        })
    }

    pub(crate) fn set(&self) -> io::Result<()> {
        // SAFETY: sets an event we own.
        check(unsafe { SetEvent(self.handle.as_raw_handle()) })
    }

    /// Consumes a set auto-reset event without blocking; true if it was set.
    pub(crate) fn try_consume(&self) -> bool {
        // SAFETY: a zero-timeout wait on an event we own.
        unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }

    pub(crate) fn handle(&self) -> BorrowedHandle<'_> {
        // SAFETY: the handle is owned by `self` and outlives the borrow.
        unsafe { BorrowedHandle::borrow_raw(self.handle.as_raw_handle()) }
    }
}

/// A counting semaphore: one count per posted slot render.
#[derive(Debug)]
pub(crate) struct Semaphore {
    handle: OwnedHandle,
    _claim: Option<Claim>,
}

impl Semaphore {
    pub(crate) fn new(max: u32) -> io::Result<Self> {
        let max = i32::try_from(max.max(1)).unwrap_or(i32::MAX);
        // SAFETY: null attributes and name: an unnamed semaphore at zero.
        let raw = unsafe { CreateSemaphoreW(std::ptr::null(), 0, max, std::ptr::null()) };
        Ok(Self {
            // SAFETY: a fresh handle (or a failure value).
            handle: unsafe { owned(raw) }?,
            _claim: None,
        })
    }

    /// Host side: the semaphore the parent duplicated into this process.
    pub(crate) fn adopt(value: u64) -> io::Result<Self> {
        let (claim, raw) = Claim::take(value)?;
        // SAFETY: as `Event::adopt`.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(Self {
            handle,
            _claim: Some(claim),
        })
    }

    pub(crate) fn release(&self) -> io::Result<()> {
        // SAFETY: releases one count of a semaphore we own; the previous
        // count is not wanted.
        check(unsafe { ReleaseSemaphore(self.handle.as_raw_handle(), 1, std::ptr::null_mut()) })
    }

    pub(crate) fn handle(&self) -> BorrowedHandle<'_> {
        // SAFETY: the handle is owned by `self` and outlives the borrow.
        unsafe { BorrowedHandle::borrow_raw(self.handle.as_raw_handle()) }
    }
}

/// `wait_any` without a timeout.
pub(crate) const FOREVER: u32 = INFINITE;

/// How a wait on several handles ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waited {
    /// The handle at this index was signalled (the lowest one if several).
    Signalled(usize),
    TimedOut,
    Failed,
}

/// Waits until one of `handles` is signalled, at most `ms` milliseconds.
pub(crate) fn wait_any(handles: &[BorrowedHandle<'_>], ms: u32) -> Waited {
    const MAX: usize = 64; // MAXIMUM_WAIT_OBJECTS
    if handles.is_empty() || handles.len() > MAX {
        return Waited::Failed;
    }
    // On the stack: every tile render waits here, on both sides.
    let mut raw = [std::ptr::null_mut(); MAX];
    for (r, h) in raw.iter_mut().zip(handles) {
        *r = h.as_raw_handle();
    }
    let count = handles.len();
    // SAFETY: the first `count` entries of `raw` are valid handles, borrowed
    // from `handles` for the duration of the call.
    let r = unsafe { WaitForMultipleObjects(count as u32, raw.as_ptr(), FALSE, ms) };
    match r.checked_sub(WAIT_OBJECT_0) {
        Some(i) if (i as usize) < count => Waited::Signalled(i as usize),
        _ if r == windows_sys::Win32::Foundation::WAIT_TIMEOUT => Waited::TimedOut,
        _ => Waited::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_semaphores_wake_waiters() {
        let auto = Event::new(false).unwrap();
        let manual = Event::new(true).unwrap();
        assert_eq!(
            wait_any(&[auto.handle(), manual.handle()], 0),
            Waited::TimedOut
        );
        auto.set().unwrap();
        assert_eq!(
            wait_any(&[auto.handle(), manual.handle()], 0),
            Waited::Signalled(0)
        );
        // Auto-reset: the wait consumed it.
        assert!(!auto.try_consume());
        manual.set().unwrap();
        assert_eq!(
            wait_any(&[auto.handle(), manual.handle()], 0),
            Waited::Signalled(1)
        );
        assert_eq!(
            wait_any(&[auto.handle(), manual.handle()], 0),
            Waited::Signalled(1)
        );
        auto.set().unwrap();
        assert!(auto.try_consume());
        assert!(!auto.try_consume());

        let sem = Semaphore::new(4).unwrap();
        sem.release().unwrap();
        sem.release().unwrap();
        // One count per wait.
        assert_eq!(wait_any(&[sem.handle()], FOREVER), Waited::Signalled(0));
        assert_eq!(wait_any(&[sem.handle()], 0), Waited::Signalled(0));
        assert_eq!(wait_any(&[sem.handle()], 0), Waited::TimedOut);
        assert_eq!(wait_any(&[], 0), Waited::Failed);
    }
}
