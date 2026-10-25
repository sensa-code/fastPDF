//! Reading positions: where each document was left, so it reopens there.
//!
//! A [`PositionRecord`] keeps the view ([`SavedView`]: page, offset in the
//! page, horizontal position, zoom and view rotation), the page count and
//! the file's size and modification time. [`restore_for`] decides how much
//! of it applies when the document opens again: everything when the file
//! is unchanged, the page and offset only (in the default zoom) when it
//! changed, nothing when the page no longer exists.
//!
//! The [`PositionStore`] file lives next to the recent-files list
//! (`<recent stem>-positions.txt`), so whatever moves or disables that list
//! (`FASTPDF_RECENT_FILE`) moves or disables the positions too. It is
//! bounded ([`MAX_ENTRIES`] documents, [`MAX_FILE_BYTES`]) and std only.
//! Every write reads the file again under a lock all FastPDF processes
//! share (`<file>.lock` next to it, [`crate::paths::lock_beside`]), so
//! they merge their entries instead of overwriting each other's, and
//! replaces it through a temporary file flushed to the disk and a rename
//! ([`crate::paths::write_atomic`]); temporary files an interrupted write
//! left behind are removed under that lock. Paths in the file are only
//! compared, never opened. The text format is in `positions/format.rs`.

mod format;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, UNIX_EPOCH};

use crate::recent::{path_key, same_path};
use crate::view_state::{RestoreScope, SavedView, SavedZoom};

/// Documents remembered at most (least recently left dropped first).
pub const MAX_ENTRIES: usize = 200;
/// Larger files are not FastPDF's: they read as empty and are replaced on
/// the next write; writing never produces one.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;
/// Records waiting for a flush at most.
const MAX_PENDING: usize = 16;
/// Longest a flush waits for another process's flush of the same file.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// A file's size and modification time: the same identity, the same
/// content as far as FastPDF can tell without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub len: u64,
    /// Nanoseconds since 1970-01-01.
    pub modified: u64,
}

impl FileIdentity {
    /// The identity of the file at `path`; `None` when it cannot be read.
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            len: meta.len(),
            modified: u64::try_from(since.as_nanos()).ok()?,
        })
    }
}

/// Where one document was left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionRecord {
    pub view: SavedView,
    /// The document's page count when the view was saved.
    pub page_count: u32,
    /// The file when the view was saved; `None` when it was unknown.
    pub identity: Option<FileIdentity>,
}

/// How a remembered position applies to the document as it is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Restore {
    /// The same file: page, offset, horizontal position, zoom, rotation.
    Exact(SavedView),
    /// The file changed: page and offset only, in the default zoom.
    Place(SavedView),
}

impl Restore {
    pub fn view(&self) -> &SavedView {
        match self {
            Self::Exact(view) | Self::Place(view) => view,
        }
    }

    /// What `DocumentSession::restore_saved` applies.
    pub fn scope(&self) -> RestoreScope {
        match self {
            Self::Exact(_) => RestoreScope::Exact,
            Self::Place(_) => RestoreScope::PlaceOnly,
        }
    }

    /// `exact` or `place`, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Exact(_) => "exact",
            Self::Place(_) => "place",
        }
    }
}

/// How `record` applies to the document that now has `page_count` pages
/// and the identity `now`: [`Restore::Exact`] when neither changed,
/// [`Restore::Place`] when something did (or is unknown) but the page still
/// exists, `None` when it does not.
pub fn restore_for(
    record: &PositionRecord,
    page_count: u32,
    now: Option<FileIdentity>,
) -> Option<Restore> {
    let view = record.view.sanitized();
    if view.page.get() >= page_count {
        return None;
    }
    let unchanged = record.page_count == page_count && now.is_some() && record.identity == now;
    Some(if unchanged {
        Restore::Exact(view)
    } else {
        Restore::Place(view)
    })
}

/// `view` with the precision the file keeps (four decimals), so a view
/// that only differs below it compares equal to the one written.
pub fn quantized(view: SavedView) -> SavedView {
    let round = |v: f32| (v * 10_000.0).round() / 10_000.0;
    let view = view.sanitized();
    SavedView {
        offset: round(view.offset),
        x: round(view.x),
        zoom: match view.zoom {
            SavedZoom::Custom(z) => SavedZoom::Custom(round(z)),
            fit => fit,
        },
        ..view
    }
}

/// The reading positions file and the records not written to it yet.
///
/// [`remember`](Self::remember) is cheap (UI thread); [`flush`](Self::flush)
/// does the file work (a background thread, or once while quitting).
/// [`lookup`](Self::lookup) answers from the records not written yet first,
/// so a document closed and opened again at once finds where it was left.
pub struct PositionStore {
    path: PathBuf,
    /// Newest first, one per document, at most [`MAX_PENDING`].
    pending: Mutex<Vec<format::Entry>>,
    /// Held while a flush reads and replaces the file.
    write: Mutex<()>,
}

impl std::fmt::Debug for PositionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PositionStore")
            .field("path", &self.path)
            .field("pending", &lock(&self.pending).len())
            .finish()
    }
}

impl PositionStore {
    /// The store next to the recent-files list `recent_file`:
    /// `recent.txt` gives `recent-positions.txt` in the same directory.
    /// Nothing is read or written yet.
    pub fn next_to(recent_file: &Path) -> Self {
        let stem = recent_file
            .file_stem()
            .map_or_else(|| "recent".into(), |s| s.to_string_lossy());
        Self::at(recent_file.with_file_name(format!("{stem}-positions.txt")))
    }

    /// The store kept in `path`.
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            pending: Mutex::new(Vec::new()),
            write: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where `doc` was left: a record not written yet, else the file's.
    /// `None` when there is none, the file cannot be read, or `doc` is a
    /// path the file cannot hold: such a path was never remembered, and
    /// comparing it (lossily, as text) could name another file, e.g. an
    /// unpaired UTF-16 surrogate against a real U+FFFD.
    pub fn lookup(&self, doc: &Path) -> Option<PositionRecord> {
        format::writable(doc)?;
        if let Some((_, record)) = lock(&self.pending).iter().find(|(p, _)| same_path(p, doc)) {
            return Some(*record);
        }
        read_entries(&self.path)
            .ok()?
            .into_iter()
            .find(|(p, _)| same_path(p, doc))
            .map(|(_, record)| record)
    }

    /// Keeps `record` (at the file's precision) for `doc` until the next
    /// [`flush`](Self::flush). False, and nothing kept, when the file
    /// cannot hold `doc`'s path (not UTF-8, a control character, longer
    /// than 2048 characters).
    pub fn remember(&self, doc: &Path, record: PositionRecord) -> bool {
        if format::writable(doc).is_none() {
            return false;
        }
        let record = PositionRecord {
            view: quantized(record.view),
            ..record
        };
        let mut pending = lock(&self.pending);
        pending.retain(|(p, _)| !same_path(p, doc));
        pending.insert(0, (doc.to_path_buf(), record));
        pending.truncate(MAX_PENDING);
        true
    }

    /// Writes the records remembered so far: under the lock every FastPDF
    /// process takes for this file, reads it again (others may have written
    /// it), puts the records first, drops duplicates and the oldest entries
    /// past [`MAX_ENTRIES`], and replaces the file. Returns how many
    /// documents the file holds now; does nothing (0) when no record
    /// waits. A file that exists but cannot be read is left alone, and a
    /// lock held elsewhere for longer than half a second gives up: the
    /// error is returned and the records wait for the next flush.
    pub fn flush(&self) -> io::Result<usize> {
        let _writing = lock(&self.write);
        let snapshot = lock(&self.pending).clone();
        if snapshot.is_empty() {
            return Ok(0);
        }
        let _shared = crate::paths::lock_beside(&self.path, LOCK_WAIT)?;
        // Every writer of this file holds the lock: a temporary file seen
        // now belongs to a write that was interrupted.
        crate::paths::remove_stale_temps(&self.path);
        let mut seen = HashSet::new();
        let merged: Vec<format::Entry> = snapshot
            .iter()
            .cloned()
            .chain(read_entries(&self.path)?)
            .filter(|(path, _)| seen.insert(path_key(path)))
            .take(MAX_ENTRIES)
            .collect();
        let (text, documents) = format::to_text(&merged);
        crate::paths::write_atomic(&self.path, &text)?;
        // Records remembered again meanwhile wait for the next flush.
        lock(&self.pending).retain(|entry| {
            !snapshot
                .iter()
                .any(|s| same_path(&s.0, &entry.0) && s.1 == entry.1)
        });
        Ok(documents)
    }
}

/// The entries of the file at `path`: none when it is missing, too large
/// or not UTF-8; an error when it exists but cannot be read.
fn read_entries(path: &Path) -> io::Result<Vec<format::Entry>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8(bytes)
        .map(|text| format::parse(&text))
        .unwrap_or_default())
}

/// A poisoned lock still guards plain data: keep using it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
