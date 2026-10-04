//! Printer enumeration through the spooler (`EnumPrintersW`,
//! `GetDefaultPrinterW`). Read-only: no printer setting is changed.

#![allow(unsafe_code)]

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
use windows_sys::Win32::Graphics::Printing::{
    EnumPrintersW, GetDefaultPrinterW, PRINTER_ATTRIBUTE_FAX, PRINTER_ENUM_CONNECTIONS,
    PRINTER_ENUM_LOCAL, PRINTER_INFO_2W,
};

use crate::job::PrinterInfo;

/// Ports of queues that never reach paper.
const VIRTUAL_PORTS: [&str; 5] = ["PORTPROMPT:", "FILE:", "NUL:", "XPSPORT:", "SHRFAX:"];
/// Driver name fragments of well-known virtual printers.
const VIRTUAL_DRIVERS: [&str; 4] = ["PDF", "XPS", "ONENOTE", "FAX"];

/// Reads a NUL-terminated UTF-16 string that the spooler wrote into `buf`.
/// Pointers outside the buffer (which the API never returns) read as empty.
fn string_in(buf: &[u64], ptr: *const u16) -> String {
    let start = buf.as_ptr() as usize;
    let end = start + std::mem::size_of_val(buf);
    let p = ptr as usize;
    if ptr.is_null() || p < start || p >= end || !(p - start).is_multiple_of(2) {
        return String::new();
    }
    let max_units = (end - p) / 2;
    // SAFETY: `p` lies inside `buf`, is 2-byte aligned, and at most
    // `max_units` u16 values remain before the end of the buffer.
    let units = unsafe { std::slice::from_raw_parts(ptr, max_units) };
    let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..len])
}

/// Raw `EnumPrintersW` level-2 records: the buffer and the record count.
/// The buffer is `u64`-backed so the records (which hold pointers) are
/// aligned.
fn enum_printers() -> Option<(Vec<u64>, u32)> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut buf: Vec<u64> = Vec::new();
    // The list can grow between the size query and the call: retry.
    for _ in 0..4 {
        let size = u32::try_from(std::mem::size_of_val(buf.as_slice())).ok()?;
        let ptr = if buf.is_empty() {
            null_mut()
        } else {
            buf.as_mut_ptr().cast()
        };
        let (mut needed, mut returned) = (0u32, 0u32);
        // SAFETY: `ptr` is null with size 0 (a size query) or points to
        // `size` writable, 8-byte aligned bytes owned by `buf`.
        let ok = unsafe {
            EnumPrintersW(
                flags,
                std::ptr::null(),
                2,
                ptr,
                size,
                &mut needed,
                &mut returned,
            )
        };
        if ok != 0 {
            return Some((buf, returned));
        }
        // SAFETY: reads the calling thread's last-error value.
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || needed <= size {
            return None;
        }
        buf = vec![0u64; (needed as usize).div_ceil(8)];
    }
    None
}

/// All local printers and printer connections. Read-only. Level-2 queries
/// ask print servers for driver and port, so a dead network printer can make
/// this slow: call it off the UI thread.
pub(crate) fn printers() -> Vec<PrinterInfo> {
    let Some((buf, returned)) = enum_printers() else {
        return Vec::new();
    };
    let record = std::mem::size_of::<PRINTER_INFO_2W>();
    let count = (returned as usize).min(std::mem::size_of_val(buf.as_slice()) / record);
    // SAFETY: the spooler wrote `returned` consecutive PRINTER_INFO_2W records
    // at the start of the aligned buffer (count is also bounded by its size).
    let records =
        unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<PRINTER_INFO_2W>(), count) };
    let default = default_printer();
    records
        .iter()
        .map(|r| {
            let name = string_in(&buf, r.pPrinterName);
            let driver = string_in(&buf, r.pDriverName);
            let port = string_in(&buf, r.pPortName);
            let is_virtual = r.Attributes & PRINTER_ATTRIBUTE_FAX != 0
                || VIRTUAL_PORTS.iter().any(|v| port.eq_ignore_ascii_case(v))
                || VIRTUAL_DRIVERS
                    .iter()
                    .any(|v| driver.to_ascii_uppercase().contains(v));
            PrinterInfo {
                is_default: default.as_deref() == Some(name.as_str()),
                name,
                is_virtual,
                driver,
                port,
            }
        })
        .collect()
}

/// The user's default printer, if one is set.
pub(crate) fn default_printer() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: a null buffer with length 0 asks only for the required length.
    let ok = unsafe { GetDefaultPrinterW(null_mut(), &mut len) };
    // SAFETY: reads the calling thread's last-error value.
    if ok != 0 || len == 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds `len` writable u16 values, as the API requires.
    if unsafe { GetDefaultPrinterW(buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let end = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}
