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

use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::NonNull;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{DuplicateHandle, FALSE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::{check, claim, owned, release};

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
}
