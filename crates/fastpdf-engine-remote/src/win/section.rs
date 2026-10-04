//! Pagefile-backed sections: tile slots and document bytes shared between
//! FastPDF and the render host (ADR 0008 §1.4, §1.5).
//!
//! Ownership rules that make the `unsafe` below sound:
//!
//! * **Tile slots.** The parent hands slot `i` to exactly one in-flight
//!   request and takes it back only after that request's terminal reply (or
//!   after the host died). The host additionally refuses a slot that is
//!   already busy, so at most one `&mut [u8]` to a slot exists in the host.
//!   The parent never writes slots and copies pixels out with raw-pointer
//!   copies after the reply; a misbehaving host can at worst produce wrong
//!   pixel values (every byte pattern is a valid `u8`).
//! * **Document bytes.** The parent fills a fresh section, unmaps it, and
//!   keeps only a read-only handle; the host maps it read-only. After the
//!   copy nobody can write the section any more, so handing out `&[u8]` to
//!   the mapping is sound for the mapping's lifetime.
//! * **Document files.** For a document loaded from a file, the parent
//!   hands the host a read-only duplicate of its file handle instead of a
//!   copy (ADR 0008 §1.5). The parent opened the file without write
//!   sharing, and the duplicate refers to the same file object, so nobody
//!   can open the file for writing while the host keeps that handle open;
//!   the host keeps it for as long as it maps the file.

use std::fs::File;
use std::io;
use std::os::windows::fs::FileExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64};

use windows_sys::Win32::Foundation::{DuplicateHandle, FALSE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READONLY, PAGE_READWRITE, UnmapViewOfFile,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::{Claim, check, claim, owned, release};

/// Access rights for mapping a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    ReadWrite,
}

impl Access {
    /// The section access mask (`FILE_MAP_*` doubles as `SECTION_MAP_*`).
    pub(crate) fn mask(self) -> u32 {
        match self {
            Self::Read => FILE_MAP_READ,
            Self::ReadWrite => FILE_MAP_READ | FILE_MAP_WRITE,
        }
    }
}

/// A shared memory section of `len` bytes.
#[derive(Debug)]
pub(crate) struct Section {
    handle: OwnedHandle,
    len: usize,
}

impl Section {
    /// Creates an unnamed, zero-filled, pagefile-backed section.
    pub(crate) fn create(len: usize) -> io::Result<Self> {
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty section"));
        }
        let size = len as u64;
        // SAFETY: plain FFI call; null security attributes and name mean an
        // unnamed section with default security.
        let raw = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                std::ptr::null(),
            )
        };
        // SAFETY: `raw` was just created and has no other owner.
        let handle = unsafe { owned(raw) }?;
        Ok(Self { handle, len })
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }

    /// Maps the whole section.
    pub(crate) fn map(&self, access: Access) -> io::Result<View> {
        self.map_prefix(self.len, access)
    }

    /// Maps the first `len` bytes of the section.
    pub(crate) fn map_prefix(&self, len: usize, access: Access) -> io::Result<View> {
        if len == 0 || len > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mapping outside the section",
            ));
        }
        // SAFETY: plain FFI call on a section handle we own; the result is
        // checked for null below.
        let addr = unsafe { MapViewOfFile(self.handle.as_raw_handle(), access.mask(), 0, 0, len) };
        let ptr = NonNull::new(addr.Value.cast::<u8>()).ok_or_else(io::Error::last_os_error)?;
        Ok(View { ptr, len })
    }

    /// Replaces the handle by one that can only map the section for reading.
    /// Once the caller has no writable view left, the contents are frozen.
    pub(crate) fn into_read_only(self) -> io::Result<Self> {
        let mut out = std::ptr::null_mut();
        // SAFETY: duplicates a handle we own within this process; `out` is a
        // valid place for the new handle value.
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle.as_raw_handle(),
                GetCurrentProcess(),
                &mut out,
                FILE_MAP_READ,
                FALSE,
                0,
            )
        })?;
        // SAFETY: DuplicateHandle succeeded, so `out` is a new handle that
        // only we own. The writable original is closed when `self` drops.
        let handle = unsafe { owned(out) }?;
        Ok(Self {
            handle,
            len: self.len,
        })
    }
}

/// Host side: a section handle the parent duplicated into this process and
/// sent in a command. The host owns it from then on and closes it on drop.
#[derive(Debug)]
pub(crate) struct ParentSection {
    section: Option<Section>,
    value: u64,
}

impl ParentSection {
    pub(crate) fn adopt(value: u64, len: u64) -> io::Result<Self> {
        let len = usize::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "section too large"))?;
        let raw = claim(value)?;
        // SAFETY: the parent duplicated this handle into our process for
        // this command only (DuplicateHandle), so nothing else in the host
        // owns it, and `claim` refuses a value that is already adopted.
        // Mapping fails cleanly if it is not a section after all.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(Self {
            section: Some(Section { handle, len }),
            value,
        })
    }

    pub(crate) fn section(&self) -> Option<&Section> {
        self.section.as_ref()
    }
}

impl Drop for ParentSection {
    fn drop(&mut self) {
        // Close first, then allow the value to be adopted again.
        drop(self.section.take());
        release(self.value);
    }
}

/// A mapped view of a section; unmapped on drop.
#[derive(Debug)]
pub(crate) struct View {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: a view is plain memory owned by this struct (the mapping stays
// valid until drop). Every access goes through raw copies or through
// slices whose exclusivity the callers guarantee (see the module docs), so
// sharing the address between threads is sound.
unsafe impl Send for View {}
// SAFETY: as above.
unsafe impl Sync for View {}

impl View {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Copies `dst.len()` bytes starting at `offset` out of the view.
    /// Returns false (copying nothing) when the range is out of bounds.
    pub(crate) fn read_into(&self, offset: usize, dst: &mut [u8]) -> bool {
        if offset
            .checked_add(dst.len())
            .is_none_or(|end| end > self.len)
        {
            return false;
        }
        // SAFETY: the range lies inside the mapping (checked above) and
        // `dst` is a distinct local buffer. The other process does not
        // write this range while we copy (slot rules, module docs); if it
        // misbehaves the copy still only reads bytes of mapped memory.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.ptr.as_ptr().add(offset),
                dst.as_mut_ptr(),
                dst.len(),
            );
        }
        true
    }

    /// As [`View::read_into`], swapping the red and blue bytes of every
    /// 4-byte pixel on the way (RGBA to BGRA, or back); bytes after the
    /// last whole pixel are copied unchanged.
    pub(crate) fn read_into_swapping_red_blue(&self, offset: usize, dst: &mut [u8]) -> bool {
        if offset
            .checked_add(dst.len())
            .is_none_or(|end| end > self.len)
        {
            return false;
        }
        // SAFETY: the range lies inside the mapping (checked above).
        let src = unsafe { self.ptr.as_ptr().add(offset) };
        let (pixels, rest) = dst.as_chunks_mut::<4>();
        let whole = pixels.len() * 4;
        for (i, px) in pixels.iter_mut().enumerate() {
            // SAFETY: pixel `i` lies inside the checked range, and `[u8; 4]`
            // has no alignment requirement. As in `read_into`, the bytes are
            // read through a raw pointer while the other process leaves the
            // range alone (slot rules, module docs); `dst` is a distinct
            // local buffer.
            let rgba = unsafe { src.add(i * 4).cast::<[u8; 4]>().read() };
            *px = swap_red_blue(u32::from_le_bytes(rgba)).to_le_bytes();
        }
        // SAFETY: the bytes after the last whole pixel lie inside the
        // checked range too.
        unsafe { std::ptr::copy_nonoverlapping(src.add(whole), rest.as_mut_ptr(), rest.len()) };
        true
    }

    /// Copies `src` into the view at `offset`. Returns false (copying
    /// nothing) when the range is out of bounds.
    pub(crate) fn write_from(&self, offset: usize, src: &[u8]) -> bool {
        if offset
            .checked_add(src.len())
            .is_none_or(|end| end > self.len)
        {
            return false;
        }
        // SAFETY: the range lies inside the mapping (checked above), the
        // view was mapped writable by the caller, and nobody else uses the
        // freshly created section yet (document upload).
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.as_ptr().add(offset), src.len());
        }
        true
    }

    /// Copies `src` into the view at `offset`, for bytes the other process
    /// reads (the slot channel's control blocks). Returns false (copying
    /// nothing) when the range is out of bounds. The view must have been
    /// mapped writable.
    pub(crate) fn write_shared(&self, offset: usize, src: &[u8]) -> bool {
        if offset
            .checked_add(src.len())
            .is_none_or(|end| end > self.len)
        {
            return false;
        }
        // SAFETY: the range lies inside the mapping (checked above) and the
        // caller mapped the view writable. We write through a raw pointer
        // and never hold a reference into the range, so the other process
        // reading (or, misbehaving, writing) it at the same time can only
        // leave arbitrary byte values, which every reader validates.
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.as_ptr().add(offset), src.len());
        }
        true
    }

    /// The 32-bit word at `offset`, as an atomic shared with the other
    /// process (the slot channel's control blocks). `None` when the word is
    /// out of bounds or misaligned.
    pub(crate) fn atomic_u32(&self, offset: usize) -> Option<&AtomicU32> {
        if !offset.is_multiple_of(4) || offset.checked_add(4).is_none_or(|end| end > self.len) {
            return None;
        }
        // SAFETY: the word lies inside the mapping, which stays valid while
        // `self` (borrowed by the result) lives, and is 4-aligned (views
        // start at a page boundary). Both processes access these words only
        // atomically; a misbehaving one can only store arbitrary values,
        // which every reader validates.
        Some(unsafe { AtomicU32::from_ptr(self.ptr.as_ptr().add(offset).cast()) })
    }

    /// As [`View::atomic_u32`], for a 64-bit word.
    pub(crate) fn atomic_u64(&self, offset: usize) -> Option<&AtomicU64> {
        if !offset.is_multiple_of(8) || offset.checked_add(8).is_none_or(|end| end > self.len) {
            return None;
        }
        // SAFETY: as in `atomic_u32`, with 8-byte alignment.
        Some(unsafe { AtomicU64::from_ptr(self.ptr.as_ptr().add(offset).cast()) })
    }

    /// A mutable slice over `offset..offset + len`.
    ///
    /// # Safety
    ///
    /// The view must be writable, and the caller must guarantee that no
    /// other reference to this range exists while the slice lives (the
    /// host's slot table enforces this).
    // Shared memory is interior-mutable by nature; exclusivity is the
    // caller's contract, as for `UnsafeCell::get`.
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn slice_mut(&self, offset: usize, len: usize) -> Option<&mut [u8]> {
        let end = offset.checked_add(len)?;
        if end > self.len {
            return None;
        }
        // SAFETY: in bounds (checked above); exclusivity is guaranteed by
        // the caller.
        Some(unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(offset), len) })
    }
}

/// One pixel read as a little-endian word, its bytes 0 and 2 swapped:
/// RGBA becomes BGRA and back. Written with masks and shifts so the copy
/// loop vectorizes.
fn swap_red_blue(px: u32) -> u32 {
    (px & 0xFF00_FF00) | ((px >> 16) & 0xFF) | ((px & 0xFF) << 16)
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: `ptr` is the base address MapViewOfFile returned and the
        // view is unmapped exactly once, here.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr.as_ptr().cast(),
            });
        }
    }
}

/// Read-only document bytes mapped in the host; usable as `SharedBytes`.
#[derive(Debug)]
pub(crate) struct MappedBytes {
    view: View,
}

impl MappedBytes {
    /// Maps a document section that the parent has frozen (read-only handle,
    /// no writable view left; module docs).
    pub(crate) fn map(section: &Section) -> io::Result<Self> {
        Ok(Self {
            view: section.map(Access::Read)?,
        })
    }
}

impl AsRef<[u8]> for MappedBytes {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: the view covers `len` mapped, readable bytes for as long as
        // `self` lives, and the section can no longer be written by anyone
        // (module docs), so the slice is never mutated while borrowed.
        unsafe { std::slice::from_raw_parts(self.view.ptr.as_ptr(), self.view.len) }
    }
}

/// Host side: a read-only handle to the document file, duplicated into
/// this process by the parent (module docs, "Document files").
#[derive(Debug)]
pub(crate) struct ParentFile {
    // Declared before the claim: closed before the value is released.
    file: File,
    _claim: Claim,
}

impl ParentFile {
    pub(crate) fn adopt(value: u64) -> io::Result<Self> {
        let (claim, raw) = Claim::take(value)?;
        // SAFETY: the parent duplicated this handle into our process for
        // this command only (DuplicateHandle), so nothing else in the host
        // owns it, and `Claim::take` refuses a value that is already
        // adopted. Reads and mapping fail cleanly if it is not a file.
        let file = unsafe { File::from_raw_handle(raw) };
        Ok(Self {
            file,
            _claim: claim,
        })
    }

    /// Reads the first `len` bytes with positional reads (the file
    /// position belongs to the parent's file object as well).
    pub(crate) fn read(&self, len: u64) -> io::Result<Vec<u8>> {
        let too_large = || io::Error::new(io::ErrorKind::OutOfMemory, "document too large");
        let len = usize::try_from(len).map_err(|_| too_large())?;
        let mut data = Vec::new();
        data.try_reserve_exact(len).map_err(|_| too_large())?;
        data.resize(len, 0);
        let mut done = 0;
        while let Some(rest) = data.get_mut(done..).filter(|r| !r.is_empty()) {
            match self.file.seek_read(rest, done as u64) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(data)
    }
}

/// Host side: the document file mapped read-only.
#[derive(Debug)]
pub(crate) struct FileView {
    view: View,
    // Keeps writers out for as long as the view exists (module docs).
    _file: ParentFile,
}

impl FileView {
    /// Maps the first `len` bytes of `file`; fails if the file is shorter.
    pub(crate) fn map(file: ParentFile, len: u64) -> io::Result<Self> {
        let len = usize::try_from(len)
            .ok()
            .filter(|&l| l > 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad document length"))?;
        // SAFETY: plain FFI call on a file handle we own; a maximum size of
        // zero maps the file at its current size, a null name makes the
        // section unnamed.
        let raw = unsafe {
            CreateFileMappingW(
                file.file.as_raw_handle(),
                std::ptr::null(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            )
        };
        // SAFETY: `raw` was just created and has no other owner.
        let handle = unsafe { owned(raw) }?;
        let section = Section { handle, len };
        // The view keeps the section alive after its handle closes.
        let view = section.map(Access::Read)?;
        Ok(Self { view, _file: file })
    }
}

impl AsRef<[u8]> for FileView {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: the view covers `len` mapped, readable bytes for as long as
        // `self` lives, and nobody can write the file while `_file` is open
        // (module docs), so the slice is never mutated while borrowed.
        unsafe { std::slice::from_raw_parts(self.view.ptr.as_ptr(), self.view.len) }
    }
}

/// Host side: the tile slot section with one busy flag per slot, so at most
/// one `&mut [u8]` per slot exists in the host at any time.
#[derive(Debug)]
pub(crate) struct SlotTable {
    view: View,
    slot_bytes: usize,
    busy: Mutex<Vec<bool>>,
}

/// Why a slot cannot be used for a render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotError {
    NoSuchSlot,
    TooSmall,
    Busy,
}

impl SlotTable {
    /// Maps the slot section the parent duplicated into this process.
    pub(crate) fn adopt(handle: u64, count: u32, slot_bytes: u64) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "bad slot section");
        let slot_bytes = usize::try_from(slot_bytes).map_err(|_| invalid())?;
        let count = usize::try_from(count).map_err(|_| invalid())?;
        let len = count.checked_mul(slot_bytes).ok_or_else(invalid)?;
        let section = ParentSection::adopt(handle, len as u64)?;
        let view = section
            .section()
            .ok_or_else(invalid)?
            .map(Access::ReadWrite)?;
        // The view keeps the section alive; the handle is no longer needed.
        drop(section);
        Ok(Self {
            view,
            slot_bytes,
            busy: Mutex::new(vec![false; count]),
        })
    }

    /// Reserves slot `index` for a render of `len` bytes.
    pub(crate) fn claim(&self, index: u32, len: usize) -> Result<SlotClaim<'_>, SlotError> {
        if len > self.slot_bytes {
            return Err(SlotError::TooSmall);
        }
        let index = usize::try_from(index).map_err(|_| SlotError::NoSuchSlot)?;
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        let flag = busy.get_mut(index).ok_or(SlotError::NoSuchSlot)?;
        if *flag {
            return Err(SlotError::Busy);
        }
        *flag = true;
        Ok(SlotClaim {
            table: self,
            index,
            len,
        })
    }
}

/// Exclusive use of one slot; released on drop.
#[derive(Debug)]
pub(crate) struct SlotClaim<'a> {
    table: &'a SlotTable,
    index: usize,
    len: usize,
}

impl SlotClaim<'_> {
    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        let offset = self.index * self.table.slot_bytes;
        // SAFETY: the view was mapped writable (`adopt`); `index < count`
        // and `len <= slot_bytes`, so the range is in bounds; this slot's
        // busy flag stays set until the claim drops, so no other claim (and
        // no other reference to the range) exists in the host, and the
        // returned slice borrows `self` mutably. The parent never writes
        // slots (module docs).
        unsafe { self.table.view.slice_mut(offset, self.len) }.unwrap_or_default()
    }
}

impl Drop for SlotClaim<'_> {
    fn drop(&mut self) {
        let mut busy = self.table.busy.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flag) = busy.get_mut(self.index) {
            *flag = false;
        }
    }
}

/// Host side: a writable mapping of the section the parent made for one
/// render that does not fit in a slot.
#[derive(Debug)]
pub(crate) struct TargetView {
    view: View,
}

impl TargetView {
    pub(crate) fn map(section: &ParentSection, len: usize) -> io::Result<Self> {
        let section = section
            .section()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no section"))?;
        Ok(Self {
            view: section.map_prefix(len, Access::ReadWrite)?,
        })
    }

    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        let len = self.view.len();
        // SAFETY: the view is writable and is the host's only mapping of a
        // section made for this one request; the slice borrows `self`
        // mutably, and the parent reads the section only after our reply.
        unsafe { self.view.slice_mut(0, len) }.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_of_one_section_share_memory() {
        let section = Section::create(8192).unwrap();
        let a = section.map(Access::ReadWrite).unwrap();
        let b = section.map(Access::Read).unwrap();
        assert!(a.write_from(4096, b"pixels"));
        let mut out = [0u8; 6];
        assert!(b.read_into(4096, &mut out));
        assert_eq!(&out, b"pixels");
        // Out-of-range copies are refused instead of touching memory.
        assert!(!b.read_into(8190, &mut out));
        assert!(!a.write_from(usize::MAX, b"x"));
        // SAFETY: test-only exclusive access to a fresh writable view.
        let slice = unsafe { a.slice_mut(0, 4) }.unwrap();
        slice.copy_from_slice(b"abcd");
        assert!(b.read_into(0, &mut out[..4]));
        assert_eq!(&out[..4], b"abcd");
        // SAFETY: as above.
        assert!(unsafe { a.slice_mut(8000, 500) }.is_none());
    }

    #[test]
    fn copies_out_can_swap_red_and_blue() {
        let section = Section::create(8192).unwrap();
        let a = section.map(Access::ReadWrite).unwrap();
        let b = section.map(Access::Read).unwrap();
        // Two RGBA pixels and two stray bytes.
        let rgba = [10, 20, 30, 255, 1, 2, 3, 4, 7, 8];
        assert!(a.write_from(100, &rgba));
        let mut out = [0u8; 10];
        assert!(b.read_into_swapping_red_blue(100, &mut out));
        assert_eq!(out, [30, 20, 10, 255, 3, 2, 1, 4, 7, 8]);
        // A larger run, as a tile row: swapping twice gives the pixels back.
        let row: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
        assert!(a.write_from(4096, &row));
        let mut bgra = vec![0u8; row.len()];
        assert!(b.read_into_swapping_red_blue(4096, &mut bgra));
        let (src_px, _) = row.as_chunks::<4>();
        let (dst_px, _) = bgra.as_chunks::<4>();
        for (s, d) in src_px.iter().zip(dst_px) {
            assert_eq!(*d, [s[2], s[1], s[0], s[3]]);
        }
        assert!(a.write_from(0, &bgra));
        let mut back = vec![0u8; row.len()];
        assert!(b.read_into_swapping_red_blue(0, &mut back));
        assert_eq!(back, row);
        // Out of range: refused, nothing copied.
        let mut short = [9u8; 8];
        assert!(!b.read_into_swapping_red_blue(8188, &mut short));
        assert_eq!(short, [9; 8]);
    }

    #[test]
    fn read_only_sections_cannot_be_mapped_writable() {
        let section = Section::create(4096).unwrap();
        {
            let w = section.map(Access::ReadWrite).unwrap();
            assert!(w.write_from(0, b"frozen"));
        }
        let frozen = section.into_read_only().unwrap();
        assert!(frozen.map(Access::ReadWrite).is_err());
        let bytes = MappedBytes::map(&frozen).unwrap();
        assert_eq!(&bytes.as_ref()[..6], b"frozen");
        assert_eq!(bytes.as_ref().len(), 4096);
    }

    #[test]
    fn slot_claims_are_exclusive_and_bounded() {
        let section = Section::create(4 * 64).unwrap();
        let table = SlotTable {
            view: section.map(Access::ReadWrite).unwrap(),
            slot_bytes: 64,
            busy: Mutex::new(vec![false; 4]),
        };
        let mut a = table.claim(1, 64).unwrap();
        assert_eq!(table.claim(1, 8).map(drop), Err(SlotError::Busy));
        assert_eq!(table.claim(4, 8).map(drop), Err(SlotError::NoSuchSlot));
        assert_eq!(table.claim(0, 65).map(drop), Err(SlotError::TooSmall));
        a.bytes().fill(7);
        drop(a);
        let reader = section.map(Access::Read).unwrap();
        let mut out = [0u8; 64];
        assert!(reader.read_into(64, &mut out));
        assert!(out.iter().all(|&b| b == 7));
        let mut b = table.claim(1, 16).unwrap();
        assert_eq!(b.bytes().len(), 16);
    }

    #[test]
    fn empty_sections_are_refused() {
        assert!(Section::create(0).is_err());
    }

    /// A file opened like the loader does: read access, no write sharing.
    fn deny_write(path: &std::path::Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    /// What the parent does for the host, within one process.
    fn hand_over(file: &File) -> u64 {
        use std::os::windows::io::IntoRawHandle;
        file.try_clone().unwrap().into_raw_handle() as usize as u64
    }

    #[test]
    fn document_files_are_read_or_mapped_and_stay_unwritable() {
        let dir = std::env::temp_dir().join(format!("fastpdf-file-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.pdf");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let parent = deny_write(&path);

        let read = ParentFile::adopt(hand_over(&parent)).unwrap();
        assert_eq!(read.read(data.len() as u64).unwrap(), data);
        // Positional reads ignore (and survive) the shared file position.
        assert_eq!(read.read(10).unwrap(), &data[..10]);
        assert!(read.read(data.len() as u64 + 1).is_err(), "short file");
        drop(read);

        let view = FileView::map(ParentFile::adopt(hand_over(&parent)).unwrap(), 300_000).unwrap();
        // The parent closes its handle; the host's view keeps writers out.
        drop(parent);
        assert_eq!(view.as_ref(), &data[..]);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        drop(view);
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_ok());

        let parent = deny_write(&path);
        assert!(FileView::map(ParentFile::adopt(hand_over(&parent)).unwrap(), 400_000).is_err());
        assert!(FileView::map(ParentFile::adopt(hand_over(&parent)).unwrap(), 0).is_err());
        drop(parent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_handle_value_is_adopted_once() {
        let path = std::env::temp_dir().join(format!("fastpdf-adopt-{}.bin", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let file = deny_write(&path);
        let value = hand_over(&file);
        let first = ParentFile::adopt(value).unwrap();
        assert!(ParentFile::adopt(value).is_err());
        drop(first);
        assert!(ParentFile::adopt(0).is_err());
        drop(file);
        let _ = std::fs::remove_file(&path);
    }
}
