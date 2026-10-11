//! Digital signatures (ADR 0013 §5, `docs/digital-signatures.md`): the
//! certificates the user can sign with, and the CMS signature a PDF
//! signature dictionary holds.
//!
//! Everything goes through Windows: the current user's personal store
//! (`MY`), where smart cards such as the Citizen Digital Certificate
//! (自然人憑證) put their certificates, and crypt32's message encoder, which
//! asks the card's driver for the PIN itself. FastPDF never sees a private
//! key or a PIN, and nothing here uses the network: certificate chains are
//! built from what Windows has cached.
//!
//! The signature is what PAdES baseline B-B asks for: CMS `SignedData`,
//! detached, SHA-256, with the content type, message digest and ESS
//! signing-certificate-v2 signed attributes and no signing time (PAdES
//! keeps it in the signature dictionary's `/M`); it carries the signer's
//! certificate and the intermediate ones.

mod der;
#[cfg(windows)]
mod windows;

pub use der::signing_certificate_v2;

use std::fmt;

/// A certificate the user can sign with: in the personal store, with a
/// private key, valid now and allowed to make digital signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
    /// SHA-1 hash of the certificate (its thumbprint): how write jobs name
    /// it.
    pub thumbprint: [u8; 20],
    /// The subject's name for display (its common name, usually).
    pub subject: String,
    pub issuer: String,
    /// Last day of validity, `YYYY-MM-DD` (UTC).
    pub valid_until: String,
}

impl Certificate {
    /// The thumbprint in hex, as write jobs carry it.
    pub fn thumbprint_hex(&self) -> String {
        to_hex(&self.thumbprint)
    }
}

/// Who signs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signer {
    /// A certificate of the personal store, by thumbprint. `window` (a raw
    /// window handle, 0 for none) owns the PIN dialog.
    Store { thumbprint: [u8; 20], window: isize },
    /// A key and self-signed certificate made for this one signature and
    /// never stored anywhere: tests and development scripts.
    Ephemeral,
}

/// Why signing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignError {
    /// The certificate is not in the store (any more).
    CertificateMissing,
    /// No card in the reader, or it was taken out.
    CardMissing,
    /// The PIN dialog was cancelled.
    Cancelled,
    /// The card refused the PIN.
    WrongPin,
    /// This platform cannot sign.
    Unavailable,
    /// A Windows call failed.
    Failed { what: &'static str, code: u32 },
}

impl fmt::Display for SignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CertificateMissing => write!(f, "the certificate is not in the store"),
            Self::CardMissing => write!(f, "the smart card is not in the reader"),
            Self::Cancelled => write!(f, "signing was cancelled"),
            Self::WrongPin => write!(f, "the card refused the PIN"),
            Self::Unavailable => write!(f, "signing is not available on this platform"),
            Self::Failed { what, code } => write!(f, "{what} failed (error 0x{code:08X})"),
        }
    }
}

impl std::error::Error for SignError {}

/// The certificates the user can sign with, by subject. Empty where there
/// is no Windows certificate store.
pub fn certificates() -> Vec<Certificate> {
    #[cfg(windows)]
    {
        windows::certificates()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// A detached CMS signature (DER `ContentInfo`) over `parts`, one after the
/// other. The signature is checked before it is returned.
pub fn sign(signer: &Signer, parts: &[&[u8]]) -> Result<Vec<u8>, SignError> {
    #[cfg(windows)]
    {
        windows::sign(signer, parts)
    }
    #[cfg(not(windows))]
    {
        let _ = (signer, parts);
        Err(SignError::Unavailable)
    }
}

/// Parses a thumbprint written in hex (40 digits).
pub fn parse_thumbprint(hex: &str) -> Option<[u8; 20]> {
    if hex.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbprints_round_trip_through_hex() {
        let cert = Certificate {
            thumbprint: [0xAB; 20],
            subject: "a".into(),
            issuer: "b".into(),
            valid_until: "2030-01-01".into(),
        };
        let hex = cert.thumbprint_hex();
        assert_eq!(hex.len(), 40);
        assert_eq!(parse_thumbprint(&hex), Some([0xAB; 20]));
        assert_eq!(parse_thumbprint(&hex.to_lowercase()), Some([0xAB; 20]));
        assert_eq!(parse_thumbprint("AB"), None);
        assert_eq!(parse_thumbprint(&"G".repeat(40)), None);
    }
}
