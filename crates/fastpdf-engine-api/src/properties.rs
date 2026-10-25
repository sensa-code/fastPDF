//! Document properties: what the properties panel shows that only the
//! engine knows besides the metadata (spec §20 minimal chrome: the panel
//! opens on request). How the document is encrypted and what its owner
//! password allows, its form and signature fields, whether it is tagged and
//! linearized. Nothing here is enforced: FastPDF shows the permissions, it
//! does not apply them.

use crate::DocumentMetadata;

/// The answer of [`crate::EngineDocument::properties`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocumentProperties {
    pub metadata: DocumentMetadata,
    /// How the document is encrypted, when its encryption dictionary could
    /// be read. `None` with `metadata.encrypted` set means "encrypted,
    /// details unknown".
    pub security: Option<Security>,
    /// `None` when the engine cannot tell (not "no form").
    pub forms: Option<FormSummary>,
    /// The catalog's `/MarkInfo /Marked` is true (a tagged PDF); `None`
    /// when the engine cannot tell.
    pub tagged: Option<bool>,
    /// A linearization dictionary within the first 1024 bytes names the
    /// file's length ("Fast Web View"). Its hint tables are not checked.
    /// `None` when the engine cannot tell.
    pub linearized: Option<bool>,
}

impl DocumentProperties {
    /// Properties of an engine that knows only the metadata: everything
    /// else is unknown.
    pub fn from_metadata(metadata: DocumentMetadata) -> Self {
        Self {
            metadata,
            ..Self::default()
        }
    }
}

/// The encryption dictionary (ISO 32000-2 §7.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Security {
    /// `/Filter`, the security handler (`Standard` for passwords).
    pub handler: Option<String>,
    pub method: EncryptionMethod,
    /// `/V`.
    pub version: Option<u8>,
    /// `/R`, the revision of the standard security handler.
    pub revision: Option<u8>,
    /// `/P`; `None` when the dictionary has none (other handlers).
    pub permissions: Option<Permissions>,
    /// `/EncryptMetadata` (true when absent).
    pub encrypt_metadata: bool,
    /// The document was opened with a password the reader typed; false
    /// when it opens without one. Not necessarily the user password: the
    /// engine may accept the owner password as well (hayro does for
    /// revisions 5 and 6).
    pub user_password: bool,
}

impl Security {
    /// Longest `/Filter` name kept, in bytes.
    pub const MAX_HANDLER_BYTES: usize = 64;
}

/// The cipher strings and streams are encrypted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionMethod {
    Rc4 {
        key_bits: u16,
    },
    Aes128,
    Aes256,
    /// The crypt filter `/Identity`: nothing is actually encrypted.
    Identity,
    Unknown,
}

/// What the owner password allows (`/P`, ISO 32000-2 Table 22). Bits are
/// numbered from 1; revision 2 knows bits 3 to 6 only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    pub bits: u32,
    /// `/R` of the security handler, which decides how bits are read.
    pub revision: u8,
}

/// Printing as [`Permissions`] allow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintPermission {
    NotAllowed,
    /// Revision 3 and later without bit 12: a degraded (low resolution)
    /// print only.
    LowResolution,
    HighResolution,
}

impl Permissions {
    /// `/P` as written: a signed 32-bit number, sometimes written unsigned
    /// (4294963392 is -3904); any other value is reduced modulo 2^32.
    pub fn from_p(p: i64, revision: u8) -> Self {
        let bits = u32::try_from(p.rem_euclid(1 << 32)).unwrap_or(0);
        Self { bits, revision }
    }

    fn bit(self, n: u32) -> bool {
        self.bits & (1 << (n - 1)) != 0
    }

    /// Revision 3 or later: bits 9 to 12 count.
    fn extended(self) -> bool {
        self.revision >= 3
    }

    pub fn printing(self) -> PrintPermission {
        if !self.bit(3) {
            PrintPermission::NotAllowed
        } else if self.extended() && !self.bit(12) {
            PrintPermission::LowResolution
        } else {
            PrintPermission::HighResolution
        }
    }

    /// Changing the document (bit 4).
    pub fn modify(self) -> bool {
        self.bit(4)
    }

    /// Copying text and graphics (bit 5).
    pub fn copy(self) -> bool {
        self.bit(5)
    }

    /// Copying for accessibility (bit 10 from revision 3 on, deprecated in
    /// PDF 2.0; implied by bit 5).
    pub fn copy_for_accessibility(self) -> bool {
        self.copy() || (self.extended() && self.bit(10))
    }

    /// Adding or changing annotations, filling in forms (bit 6).
    pub fn annotate(self) -> bool {
        self.bit(6)
    }

    /// Filling in form fields, signing included (bit 9, or bit 6).
    pub fn fill_forms(self) -> bool {
        self.annotate() || (self.extended() && self.bit(9))
    }

    /// Inserting, rotating and deleting pages (bit 11, or bit 4).
    pub fn assemble(self) -> bool {
        self.modify() || (self.extended() && self.bit(11))
    }

    pub fn all_allowed(self) -> bool {
        self.printing() == PrintPermission::HighResolution
            && self.modify()
            && self.copy()
            && self.copy_for_accessibility()
            && self.annotate()
            && self.fill_forms()
            && self.assemble()
    }
}

/// The interactive form (`/AcroForm`): its terminal fields, how many are
/// signature fields and how many of those are signed. Counts only; no
/// signature is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FormSummary {
    pub fields: u32,
    pub signature_fields: u32,
    /// Signature fields with a signature (`/V`).
    pub signatures: u32,
    /// The form also has an XFA part.
    pub xfa: bool,
    /// The field tree was too large or malformed to count completely: the
    /// counts are lower bounds.
    pub truncated: bool,
}

#[cfg(test)]
mod tests;
