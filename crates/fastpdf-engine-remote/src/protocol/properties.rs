//! Document metadata and properties on the wire (`Payload::Metadata`,
//! `Payload::Properties`; the properties start with the metadata).
//!
//! Metadata: nine optional strings (title, author, subject, keywords,
//! creator, producer, creation and modification dates, PDF version), then
//! `encrypted`. Properties: the metadata; the security block behind a
//! presence byte (handler, method tag `0` RC4 + `u16` key bits, `1`
//! AES-128, `2` AES-256, `3` Identity, `4` unknown; optional `/V`, `/R` and
//! permissions `u32` bits + `u8` revision; `/EncryptMetadata`; whether a
//! password was typed); the form counts and flags; tagged; linearized.
//!
//! The host side clips strings and makes the form counts consistent
//! (signed ≤ signature fields ≤ fields); the decoder rejects unknown
//! tags, non-canonical booleans and presence bytes, over-long strings and
//! inconsistent counts, since a host that sends them is broken or
//! compromised.

use fastpdf_engine_api::{
    DocumentMetadata, DocumentProperties, EncryptionMethod, FormSummary, Permissions, Security,
};

use super::MAX_STRING;
use super::wire::{Decoder, Encoder, ProtocolError, Result, clip};

/// Bytes of the security handler's name at most.
pub(crate) const MAX_HANDLER: usize = Security::MAX_HANDLER_BYTES;

pub(super) fn put_metadata(e: &mut Encoder, m: &DocumentMetadata) {
    for value in [
        &m.title,
        &m.author,
        &m.subject,
        &m.keywords,
        &m.creator,
        &m.producer,
        &m.creation_date,
        &m.modification_date,
        &m.pdf_version,
    ] {
        e.opt_str(value.as_deref().map(|s| clip(s, MAX_STRING)));
    }
    e.bool(m.encrypted);
}

pub(super) fn get_metadata(d: &mut Decoder<'_>) -> Result<DocumentMetadata> {
    let mut values: [Option<String>; 9] = Default::default();
    for v in &mut values {
        *v = d.opt_str("metadata", MAX_STRING)?;
    }
    let [
        title,
        author,
        subject,
        keywords,
        creator,
        producer,
        creation_date,
        modification_date,
        pdf_version,
    ] = values;
    Ok(DocumentMetadata {
        title,
        author,
        subject,
        keywords,
        creator,
        producer,
        creation_date,
        modification_date,
        pdf_version,
        encrypted: d.bool("encrypted")?,
    })
}

pub(super) fn put_properties(e: &mut Encoder, p: &DocumentProperties) {
    put_metadata(e, &p.metadata);
    match &p.security {
        Some(s) => {
            e.u8(1);
            put_security(e, s);
        }
        None => e.u8(0),
    }
    // Consistent counts, whatever the engine said.
    let fields = p.forms.fields;
    let signature_fields = p.forms.signature_fields.min(fields);
    let signatures = p.forms.signatures.min(signature_fields);
    e.u32(fields);
    e.u32(signature_fields);
    e.u32(signatures);
    e.bool(p.forms.xfa);
    e.bool(p.forms.truncated);
    e.bool(p.tagged);
    e.bool(p.linearized);
}

pub(super) fn get_properties(d: &mut Decoder<'_>) -> Result<DocumentProperties> {
    let metadata = get_metadata(d)?;
    let security = if d.present("security")? {
        Some(get_security(d)?)
    } else {
        None
    };
    let forms = FormSummary {
        fields: d.u32()?,
        signature_fields: d.u32()?,
        signatures: d.u32()?,
        xfa: d.bool("xfa")?,
        truncated: d.bool("form truncated")?,
    };
    if forms.signatures > forms.signature_fields || forms.signature_fields > forms.fields {
        return Err(ProtocolError::BadValue("form counts"));
    }
    Ok(DocumentProperties {
        metadata,
        security,
        forms,
        tagged: d.bool("tagged")?,
        linearized: d.bool("linearized")?,
    })
}

fn put_security(e: &mut Encoder, s: &Security) {
    e.opt_str(s.handler.as_deref().map(|h| clip(h, MAX_HANDLER)));
    match s.method {
        EncryptionMethod::Rc4 { key_bits } => {
            e.u8(0);
            e.u16(key_bits);
        }
        EncryptionMethod::Aes128 => e.u8(1),
        EncryptionMethod::Aes256 => e.u8(2),
        EncryptionMethod::Identity => e.u8(3),
        EncryptionMethod::Unknown => e.u8(4),
    }
    for value in [s.version, s.revision] {
        match value {
            Some(v) => {
                e.u8(1);
                e.u8(v);
            }
            None => e.u8(0),
        }
    }
    match s.permissions {
        Some(p) => {
            e.u8(1);
            e.u32(p.bits);
            e.u8(p.revision);
        }
        None => e.u8(0),
    }
    e.bool(s.encrypt_metadata);
    e.bool(s.user_password);
}

fn get_security(d: &mut Decoder<'_>) -> Result<Security> {
    let handler = d.opt_str("security handler", MAX_HANDLER)?;
    let method = match d.u8()? {
        0 => EncryptionMethod::Rc4 { key_bits: d.u16()? },
        1 => EncryptionMethod::Aes128,
        2 => EncryptionMethod::Aes256,
        3 => EncryptionMethod::Identity,
        4 => EncryptionMethod::Unknown,
        t => return Err(ProtocolError::BadTag("encryption method", t)),
    };
    let version = if d.present("encryption version")? {
        Some(d.u8()?)
    } else {
        None
    };
    let revision = if d.present("encryption revision")? {
        Some(d.u8()?)
    } else {
        None
    };
    let permissions = if d.present("permissions")? {
        Some(Permissions {
            bits: d.u32()?,
            revision: d.u8()?,
        })
    } else {
        None
    };
    Ok(Security {
        handler,
        method,
        version,
        revision,
        permissions,
        encrypt_metadata: d.bool("encrypt metadata")?,
        user_password: d.bool("user password")?,
    })
}

#[cfg(test)]
mod tests;
