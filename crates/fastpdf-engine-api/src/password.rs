//! The open password of an encrypted document (ADR 0014).
//!
//! A password must never reach logs, `Debug` output, files, argv or the
//! environment. [`Password`] makes the safe thing the default: its `Debug`
//! is redacted (so the derived `Debug` of [`crate::OpenOptions`] and of every
//! message that carries one stays safe), it has no `Display`, and its bytes
//! are overwritten when it is dropped.
//!
//! Wiping is best effort and std only: copies the engine derives from the
//! password (decryption keys), OS pipe buffers and crash dumps are out of
//! reach. The goal is that FastPDF's own buffers do not keep the password
//! after the document closes.

use std::fmt;

/// An open password. Moved, not copied, from the text field to the engine.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Password(String);

impl Password {
    /// Longest password the reader accepts from its text field. Far below
    /// any real password and below the render host's wire cap.
    pub const MAX_BYTES: usize = 1024;

    /// The password for the engine call that needs it. Do not keep, log or
    /// format the returned text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl From<String> for Password {
    /// Takes the buffer over without copying it.
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&str> for Password {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        wipe_string(&mut self.0);
    }
}

/// Overwrites `bytes` with zeros in a way the optimizer keeps.
pub fn wipe_bytes(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(&*bytes);
}

/// Overwrites the whole buffer of `text`, spare capacity included, and leaves
/// it empty with its capacity kept (so a field can reuse it).
pub fn wipe_string(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    wipe_vec(&mut bytes);
    bytes.clear();
    // An empty vector is valid UTF-8, so this never falls back.
    *text = String::from_utf8(bytes).unwrap_or_default();
}

/// Overwrites the contents and the spare capacity of `bytes`; the length is
/// unchanged. Spare capacity can still hold text that was cut or cleared.
pub fn wipe_vec(bytes: &mut Vec<u8>) {
    wipe_bytes(bytes);
    let spare = bytes.spare_capacity_mut();
    for byte in spare.iter_mut() {
        byte.write(0);
    }
    std::hint::black_box(&*spare);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenOptions;

    #[test]
    fn debug_never_shows_the_password() {
        let password = Password::from("zq9-secret");
        assert_eq!(format!("{password:?}"), "Password(<redacted>)");
        let options = OpenOptions {
            password: Some(Password::from("zq9-secret")),
            ..OpenOptions::default()
        };
        let text = format!("{options:?} {options:#?}");
        assert!(!text.contains("zq9"), "{text}");
        assert!(text.contains("<redacted>"));
    }

    #[test]
    fn wipe_string_empties_and_keeps_the_capacity() {
        let mut text = String::with_capacity(64);
        text.push_str("測試密碼2026");
        let capacity = text.capacity();
        wipe_string(&mut text);
        assert!(text.is_empty());
        assert_eq!(text.capacity(), capacity);
        text.push_str("next");
        assert_eq!(text, "next");
    }

    #[test]
    fn wipe_vec_zeroes_the_contents() {
        let mut bytes = b"secret".to_vec();
        wipe_vec(&mut bytes);
        assert_eq!(bytes, vec![0; 6]);
        let mut slice = *b"abc";
        wipe_bytes(&mut slice);
        assert_eq!(slice, [0; 3]);
    }

    #[test]
    fn clones_compare_equal_and_survive_the_original() {
        let original = Password::from(String::from("pw"));
        let copy = original.clone();
        assert_eq!(original, copy);
        assert_ne!(original, Password::from("other"));
        drop(original);
        assert_eq!(copy.as_str(), "pw");
        assert_eq!(copy.len(), 2);
        assert!(!copy.is_empty());
        assert!(Password::default().is_empty());
    }
}
