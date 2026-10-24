//! Opening a checked external address with its default app (ADR 0015):
//! `ShellExecuteExW` with the verb `open` hands it to the browser or mail
//! app registered for its scheme. FastPDF itself makes no connection. Tests
//! never call [`open_external`]: it starts another program.
#![cfg_attr(windows, allow(unsafe_code))]

use std::fmt;

use crate::uri::ExternalUri;

/// Why an address did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// No app is registered for the scheme (`ERROR_NO_ASSOCIATION`).
    NoHandler,
    /// The user or the shell cancelled (`ERROR_CANCELLED`).
    Cancelled,
    /// Another Win32 error.
    Win32(u32),
    /// Not on Windows, or the launch thread could not run.
    Unsupported,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHandler => f.write_str("no app is registered for this kind of address"),
            Self::Cancelled => f.write_str("opening the address was cancelled"),
            Self::Win32(code) => write!(f, "Windows error {code}"),
            Self::Unsupported => f.write_str("opening addresses is not supported here"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Opens `uri` with the app registered for its scheme and waits until the
/// shell has handed it over. Blocks for a few milliseconds (longer when
/// the app has to start): call it off the UI thread.
pub fn open_external(uri: &ExternalUri) -> Result<(), OpenError> {
    let text = uri.as_str().to_owned();
    // COM must be initialized as single-threaded on the calling thread
    // (Microsoft's guidance for ShellExecuteEx); a short-lived thread of
    // our own leaves the caller's threads as they are.
    std::thread::Builder::new()
        .name("fastpdf-open-link".into())
        .spawn(move || shell_open(&text))
        .map_err(|_| OpenError::Unsupported)?
        .join()
        .unwrap_or(Err(OpenError::Unsupported))
}

#[cfg(windows)]
fn shell_open(text: &str) -> Result<(), OpenError> {
    use windows_sys::Win32::Foundation::{ERROR_CANCELLED, ERROR_NO_ASSOCIATION, GetLastError};
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    /// `SW_SHOWNORMAL`.
    const SHOW_NORMAL: i32 = 1;

    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
    let verb = wide("open");
    let file = wide(text);
    // SAFETY: no pointers; balanced by CoUninitialize below when it
    // succeeds (S_OK or S_FALSE).
    let com = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    let info_size = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    let mut info = SHELLEXECUTEINFOW {
        cbSize: info_size,
        // Wait until the hand-over is done (the thread ends right after);
        // report errors instead of showing the shell's error dialogs.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        nShow: SHOW_NORMAL,
        ..Default::default()
    };
    // SAFETY: `info` is initialized with its size; `verb` and `file` are
    // NUL-terminated UTF-16 that outlive the call.
    let result = if unsafe { ShellExecuteExW(&mut info) } != 0 {
        Ok(())
    } else {
        // SAFETY: no arguments; read right after the failed call.
        Err(match unsafe { GetLastError() } {
            ERROR_NO_ASSOCIATION => OpenError::NoHandler,
            ERROR_CANCELLED => OpenError::Cancelled,
            code => OpenError::Win32(code),
        })
    };
    if com >= 0 {
        // SAFETY: pairs the successful CoInitializeEx on this thread.
        unsafe { CoUninitialize() };
    }
    result
}

#[cfg(not(windows))]
fn shell_open(_text: &str) -> Result<(), OpenError> {
    Err(OpenError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_explain_themselves() {
        assert!(OpenError::NoHandler.to_string().contains("no app"));
        assert_eq!(OpenError::Win32(5).to_string(), "Windows error 5");
    }

    #[test]
    fn launcher_signature_is_stable() {
        // Type check only: tests never open anything.
        let _open: fn(&ExternalUri) -> Result<(), OpenError> = open_external;
    }
}
