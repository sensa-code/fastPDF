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
    /// The engine cannot answer right now: its render host is restarting or
    /// not responding, or the answer did not arrive in time (ADR 0008).
    /// Asking again later may succeed.
    Unavailable(String),
    /// The render host process ended while it handled this request
    /// (ADR 0008).
    HostExited(HostExit),
}

impl EngineError {
    /// Errors that only affect one operation; the document stays usable.
    pub fn is_recoverable(&self) -> bool {
        !matches!(self, Self::PasswordRequired | Self::InvalidPassword)
    }

    /// Failures of the moment rather than answers about the document: the
    /// same request may succeed later, so they must not be cached or shown
    /// as final (retry with a backoff instead).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Unavailable(_) => true,
            Self::HostExited(exit) => !exit.permanent,
            _ => false,
        }
    }
}

/// How a render host ended while handling a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostExit {
    pub reason: HostExitReason,
    /// The request brought hosts down repeatedly and will not be sent to a
    /// host again for this document (or the document stopped restarting
    /// hosts altogether).
    pub permanent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostExitReason {
    /// The process ended on its own: a crash, an abort, a stack overflow.
    Crashed { exit_code: Option<u32> },
    /// The process exceeded its memory limit.
    MemoryLimit,
    /// The request took longer than its deadline and the host was stopped.
    Deadline,
}

impl fmt::Display for HostExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason {
            HostExitReason::Crashed {
                exit_code: Some(code),
            } => write!(f, "the render host crashed (exit code {code:#010x})")?,
            HostExitReason::Crashed { exit_code: None } => {
                f.write_str("the render host crashed")?;
            }
            HostExitReason::MemoryLimit => {
                f.write_str("the render host exceeded its memory limit")?;
            }
            HostExitReason::Deadline => {
                f.write_str("the request took too long; the render host was stopped")?;
            }
        }
        if self.permanent {
            f.write_str("; no longer retried")?;
        }
        Ok(())
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
            Self::Unavailable(m) => write!(f, "temporarily unavailable: {m}"),
            Self::HostExited(exit) => exit.fmt(f),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_are_the_retryable_ones() {
        assert!(EngineError::Unavailable("restarting".into()).is_transient());
        let crash = HostExit {
            reason: HostExitReason::Crashed {
                exit_code: Some(0xC000_0409),
            },
            permanent: false,
        };
        assert!(EngineError::HostExited(crash).is_transient());
        let given_up = HostExit {
            permanent: true,
            ..crash
        };
        assert!(!EngineError::HostExited(given_up).is_transient());
        assert!(!EngineError::Malformed("x".into()).is_transient());
        assert!(!EngineError::Panicked("x".into()).is_transient());
        assert!(!EngineError::Cancelled.is_transient());
    }

    #[test]
    fn host_exits_name_their_reason() {
        let text =
            |reason, permanent| EngineError::HostExited(HostExit { reason, permanent }).to_string();
        assert!(
            text(
                HostExitReason::Crashed {
                    exit_code: Some(0xC000_00FD)
                },
                false
            )
            .contains("0xc00000fd")
        );
        assert!(text(HostExitReason::MemoryLimit, false).contains("memory limit"));
        assert!(text(HostExitReason::Deadline, true).ends_with("no longer retried"));
    }
}
