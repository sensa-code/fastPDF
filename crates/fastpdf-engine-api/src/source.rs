use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Immutable, cheaply clonable document bytes.
///
/// The reader core decides how bytes are obtained — a plain read for small
/// files, a memory map for large ones (ADR 0006) — and engines borrow them
/// without copying.
#[derive(Clone)]
pub struct SharedBytes(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl SharedBytes {
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self(Arc::new(bytes))
    }

    /// Wraps any byte owner, e.g. a memory map.
    pub fn from_owner<T: AsRef<[u8]> + Send + Sync + 'static>(owner: T) -> Self {
        Self(Arc::new(owner))
    }

    pub fn as_slice(&self) -> &[u8] {
        (*self.0).as_ref()
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The underlying shared owner, for engines whose API accepts one.
    pub fn as_arc(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        Arc::clone(&self.0)
    }
}

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl fmt::Debug for SharedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedBytes({} bytes)", self.len())
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
    use super::*;

    #[test]
    fn shared_bytes_do_not_copy() {
        let a = SharedBytes::from_vec(vec![1, 2, 3]);
        let b = a.clone();
        assert_eq!(a.as_slice().as_ptr(), b.as_slice().as_ptr());
        assert_eq!(b.len(), 3);
    }
}
