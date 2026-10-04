use std::error::Error;
use std::fmt;

use crate::PageIndex;

/// Engine-neutral error. Adapters map their engine's errors into this type so
/// no third-party error type crosses the API boundary.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EngineError {
    /// The document is encrypted and needs a user password.
    PasswordRequired,
    /// A password was supplied but did not decrypt the document.
    InvalidPassword,
    /// The file is not a PDF the engine can make sense of.
    Malformed(String),
    /// The engine does not implement this operation or PDF feature.
    Unsupported(String),
    PageOutOfRange {
        page: PageIndex,
        page_count: u32,
    },
    /// The caller asked for something invalid (e.g. a region outside the page).
    InvalidRequest(String),
    /// A guardrail from [`crate::ResourceLimits`] stopped the operation.
    LimitExceeded(LimitKind),
    /// The request was cancelled before it finished.
    Cancelled,
    /// The engine panicked; the panic was contained by the guard layer.
    Panicked(String),
    /// Any other engine failure.
    Internal(String),
}

impl EngineError {
    /// Errors that only affect one operation; the document stays usable.
    pub fn is_recoverable(&self) -> bool {
        !matches!(self, Self::PasswordRequired | Self::InvalidPassword)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PasswordRequired => f.write_str("document requires a password"),
            Self::InvalidPassword => f.write_str("invalid password"),
            Self::Malformed(m) => write!(f, "malformed PDF: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported: {m}"),
            Self::PageOutOfRange { page, page_count } => {
                write!(
                    f,
                    "{page} is out of range (document has {page_count} pages)"
                )
            }
            Self::InvalidRequest(m) => write!(f, "invalid request: {m}"),
            Self::LimitExceeded(kind) => write!(f, "resource limit exceeded: {kind}"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Panicked(m) => write!(f, "engine panicked: {m}"),
            Self::Internal(m) => write!(f, "engine error: {m}"),
        }
    }
}

impl Error for EngineError {}

/// Which guardrail was hit (spec §25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LimitKind {
    BitmapBytes,
    BitmapDimension,
    PageDimension,
    PageCount,
    DecodedImage,
    Nesting,
    Recursion,
    ObjectSize,
    RenderTime,
}

impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BitmapBytes => "bitmap allocation size",
            Self::BitmapDimension => "bitmap dimension",
            Self::PageDimension => "page dimension",
            Self::PageCount => "page count",
            Self::DecodedImage => "decoded image size",
            Self::Nesting => "object nesting depth",
            Self::Recursion => "recursion depth",
            Self::ObjectSize => "object size",
            Self::RenderTime => "render time",
        })
    }
}
