//! Getting document bytes from disk (ADR 0006).
//!
//! Small files are read into memory: one sequential read is the fastest way
//! to get them, and the file stays unlocked so other programs can replace
//! it. Large files are memory-mapped so opening an 800 MB PDF costs almost
//! nothing up front and only the pages the engine touches are paged in
//! (spec §11) — file-backed pages do not count against the commit charge and
//! the OS can drop them under pressure.
//!
//! Files on network drives (UNC paths and drive letters mapped to a share)
//! are never mapped, whatever their size (R10): when the connection drops,
//! touching a mapped page that has to be read again raises
//! `EXCEPTION_IN_PAGE_ERROR` in whatever code touches it — the engine — and
//! the process ends. They are read instead, under the same size rules.
//!
//! In render-host mode ([`set_handle_only`], ADR 0008 §1.5) FastPDF does not
//! read the file at all: it opens it without `FILE_SHARE_WRITE`, keeps that
//! handle while the document is open (the bytes cannot change under the
//! host), and the remote engine hands a duplicate of the handle to the host,
//! which reads or maps it itself. The bytes are still produced here on first
//! access, should an in-process engine end up with them.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use fastpdf_engine_api::{FileOrigin, SharedBytes};

/// Files up to this size are read; larger files are mapped.
pub const MMAP_THRESHOLD: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStrategy {
    Read,
    Mapped,
    /// Opened for another process (the render host); read here only if an
    /// in-process engine needs the bytes.
    Handle,
}

impl fmt::Display for LoadStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Mapped => "mapped",
            Self::Handle => "handle",
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

static HANDLE_ONLY: AtomicBool = AtomicBool::new(false);

/// Render-host mode (see the module docs). Set once by the application
/// when its engine runs documents in a render host.
pub fn set_handle_only(on: bool) {
    HANDLE_ONLY.store(on, Ordering::Release);
}

pub fn handle_only() -> bool {
    HANDLE_ONLY.load(Ordering::Acquire)
}

/// Loads `path` with the default threshold.
pub fn load(path: &Path) -> Result<LoadedFile, LoadError> {
    load_with_threshold(path, MMAP_THRESHOLD)
}

pub fn load_with_threshold(path: &Path, mmap_threshold: u64) -> Result<LoadedFile, LoadError> {
    let network = is_network_path(path);
    if handle_only()
        && let Some(loaded) = load_handle(path, mmap_threshold, network)?
    {
        return Ok(loaded);
    }
    let len = std::fs::metadata(path)?.len();
    check_len(len)?;
    if len > mmap_threshold && !network {
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

fn check_len(len: u64) -> Result<(), LoadError> {
    if len == 0 {
        return Err(LoadError::Empty);
    }
    if usize::try_from(len).is_err() {
        return Err(LoadError::TooLarge(len));
    }
    Ok(())
}

/// Render-host mode: the file opened for writer-proof sharing, nothing
/// read. `None` when another process has it open for writing (the caller
/// then reads a copy).
fn load_handle(
    path: &Path,
    mmap_threshold: u64,
    network: bool,
) -> Result<Option<LoadedFile>, LoadError> {
    let file = match open_deny_write(path) {
        Ok(file) => file,
        Err(e) if is_sharing_violation(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata()?.len();
    check_len(len)?;
    let origin = Arc::new(FileOrigin::new(file, len, network));
    let lazy = LazyFile {
        origin: Arc::clone(&origin),
        mmap_threshold,
        bytes: OnceLock::new(),
    };
    Ok(Some(LoadedFile {
        bytes: SharedBytes::from_file(lazy, origin),
        strategy: LoadStrategy::Handle,
    }))
}

/// Bytes of a file opened in render-host mode, produced on first access by
/// the same rules as [`load`] (read, or map when large and local).
struct LazyFile {
    origin: Arc<FileOrigin>,
    mmap_threshold: u64,
    bytes: OnceLock<Materialized>,
}

enum Materialized {
    Read(Vec<u8>),
    Mapped(memmap2::Mmap),
    /// Reading failed; the engine sees no bytes and reports a broken file.
    Failed,
}

impl AsRef<[u8]> for LazyFile {
    fn as_ref(&self) -> &[u8] {
        match self.bytes.get_or_init(|| self.materialize()) {
            Materialized::Read(data) => data,
            Materialized::Mapped(map) => map,
            Materialized::Failed => &[],
        }
    }
}

impl LazyFile {
    #[allow(unsafe_code)]
    fn materialize(&self) -> Materialized {
        let origin = &self.origin;
        if origin.len() > self.mmap_threshold && !origin.is_network() {
            // SAFETY: memmap2 requires that the file is not modified while
            // mapped. `origin` holds a handle opened without FILE_SHARE_WRITE
            // for as long as the mapping lives (the mapping is dropped
            // before `origin` with this struct), so no process can open the
            // file for writing (Windows; best-effort elsewhere, ADR 0006).
            if let Ok(map) = unsafe { memmap2::Mmap::map(origin.file()) } {
                return Materialized::Mapped(map);
            }
        }
        match read_all_at(origin.file(), origin.len()) {
            Ok(data) if !data.is_empty() => Materialized::Read(data),
            _ => Materialized::Failed,
        }
    }
}

/// Reads `len` bytes from offset 0 without moving the handle's cursor
/// (the handle may be duplicated into another process).
fn read_all_at(file: &File, len: u64) -> io::Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| io::Error::other("file too large"))?;
    let mut data = vec![0u8; len];
    let mut done = 0;
    while done < len {
        let n = read_at(file, &mut data[done..], done as u64)?;
        if n == 0 {
            break;
        }
        done += n;
    }
    data.truncate(done);
    Ok(data)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(not(any(windows, unix)))]
fn read_at(_file: &File, _buf: &mut [u8], _offset: u64) -> io::Result<usize> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
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

/// Where a path lives, as far as loading is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    /// `\\server\share\...`, `\\?\UNC\server\share\...`.
    Unc,
    /// A drive letter (`C:\...`, `\\?\C:\...`): local or mapped, the volume
    /// decides.
    Drive(u8),
    /// Relative, rooted without a drive, device namespace, volume GUID.
    Other,
}

/// Classifies an absolute path by its prefix alone (no system calls).
fn locate(path: &Path) -> Location {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => Location::Unc,
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Location::Drive(letter.to_ascii_uppercase())
            }
            _ => Location::Other,
        },
        _ => Location::Other,
    }
}

/// [`is_network_path`] with the volume query injected (for tests).
fn is_network_with(path: &Path, drive_is_remote: impl Fn(u8) -> bool) -> bool {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match locate(&absolute) {
        Location::Unc => true,
        Location::Drive(letter) => drive_is_remote(letter),
        Location::Other => false,
    }
}

/// True for files on a network share: UNC paths (`\\server\share\...`,
/// `\\?\UNC\...`) and drive letters whose volume root is a remote drive
/// (`GetDriveTypeW` reports `DRIVE_REMOTE`, i.e. mapped network drives).
pub fn is_network_path(path: &Path) -> bool {
    is_network_with(path, drive_is_remote)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn drive_is_remote(letter: u8) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    /// `DRIVE_REMOTE` (WinBase.h); its windows-sys home is a feature this
    /// crate does not otherwise need.
    const DRIVE_REMOTE: u32 = 4;
    let root: [u16; 4] = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: `root` is a NUL-terminated wide string that outlives the call.
    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
}

#[cfg(not(windows))]
fn drive_is_remote(_letter: u8) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
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

    #[cfg(windows)]
    #[test]
    fn paths_are_classified_by_their_prefix() {
        let p = |s: &str| locate(Path::new(s));
        assert_eq!(p(r"\\server\share\docs\a.pdf"), Location::Unc);
        assert_eq!(p("//server/share/a.pdf"), Location::Unc);
        assert_eq!(p(r"\\?\UNC\server\share\a.pdf"), Location::Unc);
        assert_eq!(p(r"C:\docs\a.pdf"), Location::Drive(b'C'));
        assert_eq!(p("z:/a.pdf"), Location::Drive(b'Z'));
        assert_eq!(p(r"\\?\D:\docs\a.pdf"), Location::Drive(b'D'));
        assert_eq!(p(r"\\.\COM1"), Location::Other);
        assert_eq!(
            p(r"\\?\Volume{12345678-1234-1234-1234-123456789abc}\a.pdf"),
            Location::Other
        );
        assert_eq!(p(r"docs\a.pdf"), Location::Other);
    }

    #[cfg(windows)]
    #[test]
    fn network_paths_are_unc_or_remote_drives() {
        // Z: plays a mapped network drive, C: a local disk.
        let remote = |letter: u8| letter == b'Z';
        assert!(is_network_with(Path::new(r"\\nas\scans\a.pdf"), remote));
        assert!(is_network_with(
            Path::new(r"\\?\UNC\nas\scans\a.pdf"),
            remote
        ));
        assert!(is_network_with(Path::new(r"Z:\scans\a.pdf"), remote));
        assert!(is_network_with(Path::new(r"\\?\z:\scans\a.pdf"), remote));
        assert!(!is_network_with(Path::new(r"C:\docs\a.pdf"), remote));
        assert!(!is_network_with(Path::new(r"\\.\C:\docs\a.pdf"), remote));
        // Relative paths are resolved first; the test runs on a local disk.
        assert!(!is_network_with(Path::new("a.pdf"), |_| false));
        // The real query: the temp directory is local.
        assert!(!is_network_path(&std::env::temp_dir().join("a.pdf")));
    }

    #[test]
    fn handle_only_mode_opens_without_reading() {
        let data = b"%PDF-1.7 handle".to_vec();
        let path = temp_file("handle.pdf", &data);
        let loaded = load_handle(&path, MMAP_THRESHOLD, false).unwrap().unwrap();
        assert_eq!(loaded.strategy, LoadStrategy::Handle);
        assert_eq!(loaded.bytes.len(), data.len());
        let origin = loaded.bytes.origin().cloned().unwrap();
        assert_eq!(origin.len(), data.len() as u64);
        assert!(!origin.is_network());
        // Writers stay out while the document is open.
        #[cfg(windows)]
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        // An in-process engine still gets the bytes, on first access.
        assert_eq!(loaded.bytes.as_slice(), &data[..]);
        drop((loaded, origin));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handle_only_mode_maps_large_local_files_lazily() {
        let data = vec![3u8; 8192];
        let path = temp_file("handle-large.pdf", &data);
        let loaded = load_handle(&path, 1024, false).unwrap().unwrap();
        assert_eq!(loaded.bytes.as_slice(), &data[..]);
        drop(loaded);
        // A file on a network drive of the same size is read, not mapped.
        let net = load_handle(&path, 1024, true).unwrap().unwrap();
        assert!(net.bytes.origin().is_some_and(|o| o.is_network()));
        assert_eq!(net.bytes.as_slice(), &data[..]);
        drop(net);
        std::fs::remove_file(path).unwrap();
    }
}
