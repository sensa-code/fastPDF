use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Immutable, cheaply clonable document bytes.
///
/// The reader core decides how bytes are obtained — a plain read for small
/// files, a memory map for large ones (ADR 0006) — and engines borrow them
/// without copying.
///
/// Bytes that come from a file can carry that file ([`FileOrigin`]): an
/// engine that runs the document in another process (the render host, ADR
/// 0008) hands the file itself over instead of the bytes, and the bytes may
/// then never be read in this process at all (the owner materializes them
/// on first access).
#[derive(Clone)]
pub struct SharedBytes {
    bytes: Arc<dyn AsRef<[u8]> + Send + Sync>,
    origin: Option<Arc<FileOrigin>>,
    /// Length known without touching the bytes (a file's size).
    len: Option<usize>,
}

/// The file some bytes come from, opened read-only without
/// `FILE_SHARE_WRITE`: while this handle is open nobody can change the
/// bytes, so another process may read or map the same file and see exactly
/// what this process would.
#[derive(Debug)]
pub struct FileOrigin {
    file: File,
    len: u64,
    network: bool,
}

impl FileOrigin {
    pub fn new(file: File, len: u64, network: bool) -> Self {
        Self { file, len, network }
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// On a network drive: never memory-mapped (a dropped connection would
    /// fault inside the mapping; ADR 0006, R10).
    pub fn is_network(&self) -> bool {
        self.network
    }
}

impl SharedBytes {
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self::from_owner(bytes)
    }

    /// Wraps any byte owner, e.g. a memory map.
    pub fn from_owner<T: AsRef<[u8]> + Send + Sync + 'static>(owner: T) -> Self {
        Self {
            bytes: Arc::new(owner),
            origin: None,
            len: None,
        }
    }

    /// Bytes of `origin`, produced by `owner` on first access (which may
    /// never happen when an engine uses the file instead).
    pub fn from_file<T: AsRef<[u8]> + Send + Sync + 'static>(
        owner: T,
        origin: Arc<FileOrigin>,
    ) -> Self {
        Self {
            bytes: Arc::new(owner),
            len: usize::try_from(origin.len()).ok(),
            origin: Some(origin),
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        (*self.bytes).as_ref()
    }

    /// The length; for bytes from a file, without reading them.
    pub fn len(&self) -> usize {
        self.len.unwrap_or_else(|| self.as_slice().len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The underlying shared owner, for engines whose API accepts one.
    pub fn as_arc(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        Arc::clone(&self.bytes)
    }

    /// The file these bytes come from, if any.
    pub fn origin(&self) -> Option<&Arc<FileOrigin>> {
        self.origin.as_ref()
    }
}

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl fmt::Debug for SharedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedBytes({} bytes", self.len())?;
        if self.origin.is_some() {
            f.write_str(", from a file")?;
        }
        f.write_str(")")
    }
}

/// Input to [`crate::PdfEngine::open`].
#[derive(Debug, Clone)]
pub struct DocumentSource {
    pub data: SharedBytes,
    /// Where the bytes came from; used for diagnostics only.
    pub path: Option<PathBuf>,
}

impl DocumentSource {
    pub fn from_bytes(data: SharedBytes) -> Self {
        Self { data, path: None }
    }

    pub fn with_path(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_path_buf());
        self
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn shared_bytes_do_not_copy() {
        let a = SharedBytes::from_vec(vec![1, 2, 3]);
        let b = a.clone();
        assert_eq!(a.as_slice().as_ptr(), b.as_slice().as_ptr());
        assert_eq!(b.len(), 3);
        assert!(b.origin().is_none());
    }

    /// An owner that counts how often its bytes are produced.
    struct Counting(Arc<AtomicUsize>, Vec<u8>);

    impl AsRef<[u8]> for Counting {
        fn as_ref(&self) -> &[u8] {
            self.0.fetch_add(1, Ordering::Relaxed);
            &self.1
        }
    }

    #[test]
    fn bytes_from_a_file_know_their_length_without_reading() {
        let path = std::env::temp_dir().join(format!("fastpdf-api-origin-{}", std::process::id()));
        std::fs::write(&path, b"%PDF-1.7").unwrap();
        let file = File::open(&path).unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let origin = Arc::new(FileOrigin::new(file, 8, false));
        let bytes =
            SharedBytes::from_file(Counting(Arc::clone(&reads), b"%PDF-1.7".to_vec()), origin);
        assert_eq!(bytes.len(), 8);
        assert!(format!("{bytes:?}").contains("from a file"));
        assert_eq!(reads.load(Ordering::Relaxed), 0, "len() must not read");
        assert_eq!(bytes.origin().map(|o| o.len()), Some(8));
        assert_eq!(bytes.as_slice(), b"%PDF-1.7");
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        drop(bytes);
        let _ = std::fs::remove_file(path);
    }
}
