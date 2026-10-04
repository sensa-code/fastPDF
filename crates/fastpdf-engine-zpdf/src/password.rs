//! Detects "this encrypted file needs a password" while zpdf opens it.
//!
//! zpdf never reports a missing password. Opened with the empty password, an
//! RC4 / AES-128 (V ≤ 4) document whose `/U` entry does not validate still
//! opens "best-effort" and only logs
//! `encryption key did not validate against /U ...` through `tracing`
//! (zpdf-parser `crypt.rs`, `Decryptor::from_encrypt_dict`); its content then
//! decrypts to garbage. The adapter listens for exactly that event for the
//! duration of the open call so it can return `EngineError::PasswordRequired`
//! instead of rendering garbage. AES-256 (V5) files are detected differently
//! (no decryptor is built at all, see `lib.rs`).
//!
//! The event text is stable because zpdf is pinned to one commit. If tracing
//! were compiled out (a `max_level_off` feature in the final binary) the probe
//! would simply never fire and V ≤ 4 files would open best-effort as before.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// The message zpdf logs when a derived key fails to validate (see module docs).
const UNVERIFIED_KEY_MESSAGE: &str = "did not validate against /U";

/// A minimal `tracing` subscriber that records whether zpdf reported an
/// unverified encryption key. Installed only around `PdfDocument::open*`.
#[derive(Debug, Clone, Default)]
pub(crate) struct PasswordProbe {
    unverified_key: Arc<AtomicBool>,
}

impl PasswordProbe {
    /// True when zpdf opened the file with a key it could not validate.
    pub(crate) fn key_unverified(&self) -> bool {
        self.unverified_key.load(Ordering::Acquire)
    }

    /// Runs `f` with this probe as the thread's tracing subscriber.
    pub(crate) fn observe<T>(&self, f: impl FnOnce() -> T) -> T {
        tracing::subscriber::with_default(self.clone(), f)
    }
}

/// Collects the `message` field of an event.
struct MessageVisitor<'a>(&'a mut String);

impl Visit for MessageVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{value:?}");
        }
    }
}

impl Subscriber for PasswordProbe {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // Only zpdf-parser warnings matter; everything else is discarded.
        *metadata.level() <= Level::WARN && metadata.target().starts_with("zpdf_parser")
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));
        if message.contains(UNVERIFIED_KEY_MESSAGE) {
            self.unverified_key.store(true, Ordering::Release);
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_only_the_unverified_key_warning() {
        let probe = PasswordProbe::default();
        probe.observe(|| {
            tracing::warn!(target: "zpdf_parser::crypt", "something else entirely");
        });
        assert!(!probe.key_unverified());
        probe.observe(|| {
            tracing::warn!(
                target: "zpdf_parser::crypt",
                "encryption key did not validate against /U (V=2 R=3); the PDF may require a password"
            );
        });
        assert!(probe.key_unverified());
    }

    #[test]
    fn ignores_other_targets() {
        let probe = PasswordProbe::default();
        probe.observe(|| {
            tracing::warn!(target: "elsewhere", "key did not validate against /U");
        });
        assert!(!probe.key_unverified());
    }
}
