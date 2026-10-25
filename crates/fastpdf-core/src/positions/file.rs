//! Reading the positions file, and the in-process locks around it.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use super::{MAX_FILE_BYTES, format};

/// The entries of the file at `path`: none when it is missing, too large
/// or not UTF-8; an error when it exists but cannot be read.
pub(super) fn read_entries(path: &Path) -> io::Result<Vec<format::Entry>> {
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
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// [`lock`], giving up at `deadline` with [`io::ErrorKind::WouldBlock`].
pub(super) fn lock_until<T>(mutex: &Mutex<T>, deadline: Instant) -> io::Result<MutexGuard<'_, T>> {
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(e)) => return Ok(e.into_inner()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another flush of the positions is running",
                ));
            }
        }
    }
}
