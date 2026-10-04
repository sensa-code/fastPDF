//! Getting document bytes from disk (ADR 0006).
//!
//! Small files are read into memory: one sequential read is the fastest way
//! to get them, and the file stays unlocked so other programs can replace
//! it. Large files are memory-mapped so opening an 800 MB PDF costs almost
//! nothing up front and only the pages the engine touches are paged in
//! (spec §11) — file-backed pages do not count against the commit charge and
//! the OS can drop them under pressure.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use fastpdf_engine_api::SharedBytes;

/// Files up to this size are read; larger files are mapped.
pub const MMAP_THRESHOLD: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStrategy {
    Read,
    Mapped,
}

impl fmt::Display for LoadStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Mapped => "mapped",
        })
    }
}

#[derive(Debug)]
pub struct LoadedFile {
    pub bytes: SharedBytes,
    pub strategy: LoadStrategy,
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Empty,
    TooLarge(u64),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read file: {e}"),
            Self::Empty => f.write_str("file is empty"),
            Self::TooLarge(len) => write!(f, "file is too large to open ({len} bytes)"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Loads `path` with the default threshold.
pub fn load(path: &Path) -> Result<LoadedFile, LoadError> {
    load_with_threshold(path, MMAP_THRESHOLD)
}

pub fn load_with_threshold(path: &Path, mmap_threshold: u64) -> Result<LoadedFile, LoadError> {
    let len = std::fs::metadata(path)?.len();
    if len == 0 {
        return Err(LoadError::Empty);
    }
    if usize::try_from(len).is_err() {
        return Err(LoadError::TooLarge(len));
    }
    if len > mmap_threshold {
        match map(path) {
            Ok(bytes) => {
                return Ok(LoadedFile {
                    bytes,
                    strategy: LoadStrategy::Mapped,
                });
            }
            // Another process holds the file open for writing, so we cannot
            // guarantee the mapping stays immutable. Fall back to a copy.
            Err(e) if is_sharing_violation(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut file = File::open(path)?;
    let mut data = Vec::with_capacity(len as usize);
    file.read_to_end(&mut data)?;
    if data.is_empty() {
        return Err(LoadError::Empty);
    }
    Ok(LoadedFile {
        bytes: SharedBytes::from_vec(data),
        strategy: LoadStrategy::Read,
    })
}

/// A read-only mapping plus the handle that keeps writers out.
struct MappedFile {
    map: memmap2::Mmap,
    // Held for the lifetime of the mapping: on Windows the handle was opened
    // without FILE_SHARE_WRITE, which keeps other processes from modifying
    // the bytes under the mapping (truncation is already refused by the OS
    // while a view exists).
    _file: File,
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        &self.map
    }
}

#[allow(unsafe_code)]
fn map(path: &Path) -> io::Result<SharedBytes> {
    let file = open_deny_write(path)?;
    // SAFETY: memmap2 requires that the file is not modified while mapped.
    // On Windows `open_deny_write` opened it without FILE_SHARE_WRITE and the
    // handle lives as long as the mapping, so no other process can open it
    // for writing. On other platforms this is best-effort (documented in
    // ADR 0006); FastPDF is Windows-first.
    let map = unsafe { memmap2::Mmap::map(&file)? };
    Ok(SharedBytes::from_owner(MappedFile { map, _file: file }))
}

#[cfg(windows)]
fn open_deny_write(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

#[cfg(not(windows))]
fn open_deny_write(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn is_sharing_violation(e: &io::Error) -> bool {
    use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
    e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
}

#[cfg(not(windows))]
fn is_sharing_violation(_e: &io::Error) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("fastpdf-core-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}-{name}", std::process::id()));
        let mut f = File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn small_files_are_read() {
        let path = temp_file("small.pdf", b"%PDF-1.7 tiny");
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.strategy, LoadStrategy::Read);
        assert_eq!(loaded.bytes.as_slice(), b"%PDF-1.7 tiny");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn large_files_are_mapped_and_locked_for_writing() {
        let data = vec![7u8; 4096];
        let path = temp_file("large.pdf", &data);
        let loaded = load_with_threshold(&path, 1024).unwrap();
        assert_eq!(loaded.strategy, LoadStrategy::Mapped);
        assert_eq!(loaded.bytes.as_slice(), &data[..]);
        #[cfg(windows)]
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        drop(loaded);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_and_missing_files_fail_cleanly() {
        let path = temp_file("empty.pdf", b"");
        assert!(matches!(load(&path), Err(LoadError::Empty)));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(load(&path), Err(LoadError::Io(_))));
    }
}
