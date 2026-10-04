use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-unique identifier of an opened document.
///
/// Assigned by the reader core when a document is opened, never by an engine,
/// so cache keys stay valid regardless of which engine produced a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(u64);

impl DocumentId {
    /// Allocates a new identifier, unique for the lifetime of the process.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "doc#{}", self.0)
    }
}

/// Zero-based page index. User-facing page numbers are `index + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct PageIndex(u32);

impl PageIndex {
    pub const FIRST: Self = Self(0);

    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// One-based number shown to users.
    pub const fn display_number(self) -> u32 {
        self.0.saturating_add(1)
    }

    /// The next page, if it exists in a document of `page_count` pages.
    pub fn next(self, page_count: u32) -> Option<Self> {
        let next = self.0.checked_add(1)?;
        (next < page_count).then_some(Self(next))
    }

    pub fn prev(self) -> Option<Self> {
        self.0.checked_sub(1).map(Self)
    }
}

impl fmt::Display for PageIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "page {}", self.display_number())
    }
}

/// A page of a specific document; the identity used by caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageId {
    pub document: DocumentId,
    pub page: PageIndex,
}

impl PageId {
    pub const fn new(document: DocumentId, page: PageIndex) -> Self {
        Self { document, page }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_ids_are_unique() {
        let a = DocumentId::next();
        let b = DocumentId::next();
        assert_ne!(a, b);
    }

    #[test]
    fn page_navigation_respects_bounds() {
        let last = PageIndex::new(2);
        assert_eq!(last.next(3), None);
        assert_eq!(PageIndex::new(1).next(3), Some(last));
        assert_eq!(PageIndex::FIRST.prev(), None);
        assert_eq!(PageIndex::new(u32::MAX).next(u32::MAX), None);
        assert_eq!(PageIndex::new(4).display_number(), 5);
    }
}
