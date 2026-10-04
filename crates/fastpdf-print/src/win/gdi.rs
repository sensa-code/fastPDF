//! Safe RAII wrappers over the Win32 GDI printing calls.
//!
//! All `unsafe` GDI code of the crate lives here. Each handle has exactly one
//! owner whose `Drop` releases it, so every error path (including panics and
//! early returns with `?`) deletes the DC and aborts an unfinished document.

#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_CANCELLED, GetLastError};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DIB_RGB_COLORS, DT_RASPRINTER, DeleteDC,
    GDI_ERROR, GET_DEVICE_CAPS_INDEX, GetDeviceCaps, HALFTONE, HDC, HORZRES, LOGPIXELSX,
    LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, PatBlt,
    RASTERCAPS, RC_STRETCHDIB, RGBQUAD, SRCCOPY, SetBrushOrgEx, SetStretchBltMode, StretchDIBits,
    TECHNOLOGY, VERTRES, WHITENESS,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};

use crate::job::PrintError;
use crate::layout::{DeviceRect, Paper};
use crate::raster::{DibFormat, Packed};

/// A BITMAPINFO with room for a full 8-bit color table.
#[repr(C)]
struct DibInfo {
    header: BITMAPINFOHEADER,
    palette: [RGBQUAD; 256],
}

/// A NUL-terminated UTF-16 copy of `s`; interior NULs are rejected because
/// Win32 would silently truncate at them.
pub(crate) fn wide(s: &OsStr) -> Result<Vec<u16>, PrintError> {
    let mut out: Vec<u16> = s.encode_wide().collect();
    if out.contains(&0) {
        return Err(PrintError::InvalidJob("string contains NUL".into()));
    }
    out.push(0);
    Ok(out)
}

fn last_error(call: &'static str) -> PrintError {
    // SAFETY: GetLastError only reads the calling thread's last-error value.
    let code = unsafe { GetLastError() };
    PrintError::Win32 { call, code }
}

/// An owned printer device context (`CreateDCW` / `DeleteDC`).
#[derive(Debug)]
pub(crate) struct PrinterDc {
    hdc: HDC,
}

impl PrinterDc {
    /// Opens a DC for `printer` with the printer's own default settings; no
    /// printer configuration is read from or written to the user.
    pub(crate) fn open(printer: &str) -> Result<Self, PrintError> {
        let name = wide(OsStr::new(printer))?;
        // SAFETY: `name` is NUL-terminated and outlives the call; null driver,
        // port and DEVMODE select the named printer with its defaults.
        let hdc = unsafe { CreateDCW(null(), name.as_ptr(), null(), null()) };
        if hdc.is_null() {
            return Err(PrintError::PrinterNotFound(printer.to_owned()));
        }
        Ok(Self { hdc })
    }

    fn caps(&self, index: GET_DEVICE_CAPS_INDEX) -> i64 {
        // SAFETY: `self.hdc` is a live DC owned by `self`; GetDeviceCaps only
        // reads device information.
        i64::from(unsafe { GetDeviceCaps(self.hdc, index as i32) })
    }

    /// Sheet geometry and resolution of the device.
    pub(crate) fn paper(&self) -> Result<Paper, PrintError> {
        if self.caps(TECHNOLOGY) != i64::from(DT_RASPRINTER) {
            return Err(PrintError::Unsupported(
                "device is not a raster printer".into(),
            ));
        }
        if self.caps(RASTERCAPS) & i64::from(RC_STRETCHDIB) == 0 {
            return Err(PrintError::Unsupported(
                "printer driver cannot stretch device-independent bitmaps".into(),
            ));
        }
        let paper = Paper {
            dpi_x: u32::try_from(self.caps(LOGPIXELSX)).unwrap_or(0),
            dpi_y: u32::try_from(self.caps(LOGPIXELSY)).unwrap_or(0),
            physical_w: self.caps(PHYSICALWIDTH),
            physical_h: self.caps(PHYSICALHEIGHT),
            offset_x: self.caps(PHYSICALOFFSETX),
            offset_y: self.caps(PHYSICALOFFSETY),
            printable_w: self.caps(HORZRES),
            printable_h: self.caps(VERTRES),
        };
        let sane = paper.dpi_x > 0
            && paper.dpi_y > 0
            && paper.printable_w > 0
            && paper.printable_h > 0
            && paper.physical_w >= paper.printable_w
            && paper.physical_h >= paper.printable_h;
        if !sane {
            return Err(PrintError::Unsupported(format!(
                "printer reports unusable paper metrics {paper:?}"
            )));
        }
        Ok(paper)
    }

    /// Starts a print job (`StartDocW`). With `output`, the spooler writes the
    /// job to that file instead of the printer port.
    pub(crate) fn start_doc(
        &self,
        name: &str,
        output: Option<&Path>,
    ) -> Result<Document<'_>, PrintError> {
        let doc_name = wide(OsStr::new(name))?;
        let output = output.map(|p| wide(p.as_os_str())).transpose()?;
        let info = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: doc_name.as_ptr(),
            lpszOutput: output.as_ref().map_or(null(), |o| o.as_ptr()),
            lpszDatatype: null(),
            fwType: 0,
        };
        // SAFETY: `self.hdc` is live; `info` and the strings it points to stay
        // alive for the duration of the call (StartDocW copies them).
        let job = unsafe { StartDocW(self.hdc, &info) };
        if job <= 0 {
            return Err(match last_error("StartDocW") {
                // The user dismissed the file prompt of a file-producing
                // queue (port PORTPROMPT: or FILE: without `output`).
                PrintError::Win32 {
                    code: ERROR_CANCELLED,
                    ..
                } => PrintError::Cancelled,
                error => error,
            });
        }
        Ok(Document {
            dc: self,
            open: true,
        })
    }
}

impl Drop for PrinterDc {
    fn drop(&mut self) {
        // SAFETY: `hdc` came from CreateDCW, is owned solely by `self` and is
        // deleted exactly once, here (after any Document borrowing it ended).
        unsafe {
            DeleteDC(self.hdc);
        }
    }
}

/// A started print job. Dropping it without [`Document::finish`] aborts the
/// job (`AbortDoc`), discarding everything spooled so far.
#[derive(Debug)]
pub(crate) struct Document<'a> {
    dc: &'a PrinterDc,
    open: bool,
}

impl Document<'_> {
    pub(crate) fn start_page(&mut self) -> Result<(), PrintError> {
        // SAFETY: the DC is live (borrowed from its owner) and a document is
        // in progress.
        if unsafe { StartPage(self.dc.hdc) } <= 0 {
            return Err(last_error("StartPage"));
        }
        // Halftone gives the best quality when the device stretches a band
        // (render DPI capped below the device DPI). The brush origin must be
        // reset after selecting it. Failures only lower quality.
        // SAFETY: plain state setters on a live DC; the output point is
        // optional and passed as null.
        unsafe {
            SetStretchBltMode(self.dc.hdc, HALFTONE);
            SetBrushOrgEx(self.dc.hdc, 0, 0, null_mut());
        }
        Ok(())
    }

    pub(crate) fn end_page(&mut self) -> Result<(), PrintError> {
        // SAFETY: live DC with a page in progress.
        if unsafe { EndPage(self.dc.hdc) } <= 0 {
            return Err(last_error("EndPage"));
        }
        Ok(())
    }

    /// Stretches a packed top-down band of `width` x `height` pixels onto
    /// `dest` (device units of the printable area).
    pub(crate) fn draw_dib(
        &mut self,
        dest: DeviceRect,
        bits: &[u8],
        width: u32,
        height: u32,
        packed: Packed,
    ) -> Result<(), PrintError> {
        let bytes_per_pixel = match packed.format {
            DibFormat::Gray8 => 1,
            DibFormat::Bgr24 => 3,
        };
        let fits = |v: i64| i32::try_from(v).is_ok();
        let valid = width > 0
            && height > 0
            && i32::try_from(width).is_ok()
            && i32::try_from(height).is_ok()
            && packed.stride.is_multiple_of(4)
            && packed.stride >= width as usize * bytes_per_pixel
            && packed.stride.checked_mul(height as usize) == Some(packed.len)
            && bits.len() >= packed.len
            && fits(dest.x)
            && fits(dest.y)
            && fits(dest.w)
            && fits(dest.h);
        if !valid {
            return Err(PrintError::InvalidJob(
                "band does not match its bitmap".into(),
            ));
        }
        let mut info = DibInfo {
            header: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative height: rows are stored top-down.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: (bytes_per_pixel * 8) as u16,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            palette: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }; 256],
        };
        if packed.format == DibFormat::Gray8 {
            info.header.biClrUsed = 256;
            for (level, entry) in (0..=u8::MAX).zip(info.palette.iter_mut()) {
                entry.rgbBlue = level;
                entry.rgbGreen = level;
                entry.rgbRed = level;
            }
        }
        // SAFETY: live DC. `info` starts with a BITMAPINFOHEADER followed by
        // the color table the header declares (256 entries for 8-bit, none
        // for 24-bit), which is the layout StretchDIBits reads through the
        // BITMAPINFO pointer. `bits` holds `height` rows of `stride` bytes as
        // the header describes (checked above). Both outlive the call, and
        // all coordinates fit in i32.
        let lines = unsafe {
            StretchDIBits(
                self.dc.hdc,
                dest.x as i32,
                dest.y as i32,
                dest.w as i32,
                dest.h as i32,
                0,
                0,
                width as i32,
                height as i32,
                bits.as_ptr().cast(),
                (&raw const info).cast::<BITMAPINFO>(),
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        if lines == 0 || lines == GDI_ERROR {
            return Err(last_error("StretchDIBits"));
        }
        Ok(())
    }

    /// Paints `dest` paper-white (`PatBlt` with `WHITENESS`), covering
    /// whatever was drawn there earlier on this page. The device clips what
    /// lies outside the printable area.
    pub(crate) fn erase(&mut self, dest: DeviceRect) -> Result<(), PrintError> {
        let fits = |v: i64| i32::try_from(v).is_ok();
        if !(fits(dest.x) && fits(dest.y) && fits(dest.w) && fits(dest.h)) {
            return Err(PrintError::InvalidJob(
                "erase rectangle out of range".into(),
            ));
        }
        // SAFETY: live DC with a page in progress; coordinates fit in i32.
        let ok = unsafe {
            PatBlt(
                self.dc.hdc,
                dest.x as i32,
                dest.y as i32,
                dest.w as i32,
                dest.h as i32,
                WHITENESS,
            )
        };
        if ok == 0 {
            return Err(last_error("PatBlt"));
        }
        Ok(())
    }

    /// Ends the job (`EndDoc`) so the spooler prints or writes it.
    pub(crate) fn finish(mut self) -> Result<(), PrintError> {
        // SAFETY: live DC with a document in progress.
        if unsafe { EndDoc(self.dc.hdc) } <= 0 {
            // `open` stays set: dropping `self` aborts what is left of it.
            return Err(last_error("EndDoc"));
        }
        self.open = false;
        Ok(())
    }
}

impl Drop for Document<'_> {
    fn drop(&mut self) {
        if self.open {
            // SAFETY: live DC with an unfinished document: AbortDoc deletes
            // the spool job, including a page that was started but not ended.
            unsafe {
                AbortDoc(self.dc.hdc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_are_terminated_and_reject_nul() {
        assert_eq!(wide(OsStr::new("ab")).unwrap(), vec![97, 98, 0]);
        assert!(wide(OsStr::new("a\0b")).is_err());
    }

    #[test]
    fn unknown_printers_are_reported_not_opened() {
        assert_eq!(
            PrinterDc::open("FastPDF test printer that does not exist").err(),
            Some(PrintError::PrinterNotFound(
                "FastPDF test printer that does not exist".into()
            ))
        );
    }
}
