//! Document properties (`EngineDocument::properties`), read only when the
//! properties panel asks, never while a document opens: hayro's metadata
//! completed from the trailer (`trailer.rs`) with the dates as they are
//! written and the encryption dictionary, the form (`forms.rs`), whether
//! the document is tagged and whether it is linearized.
//!
//! hayro exposes neither the trailer nor the encryption dictionary, so the
//! last trailer is found in the file's bytes; everything it names is then
//! resolved through hayro's cross-reference table (decrypted where needed).
//! Only fixed-size windows of the file are scanned; the form walk has its
//! own budgets.

mod forms;
mod trailer;

use fastpdf_engine_api::display::clean_display;
use fastpdf_engine_api::{
    DocumentMetadata, DocumentProperties, EncryptionMethod, Permissions, Security,
};
use hayro::hayro_syntax::object::{
    Dict, FromBytes, MaybeRef, Name, ObjectIdentifier, String as PdfString,
};
use hayro::hayro_syntax::xref::XRef;

use crate::document::{DocInner, Generation};
use crate::nav::{self, info_text};

/// Where a linearization dictionary must be (ISO 32000-2 Annex F).
const LINEARIZATION_WINDOW: usize = 1024;

pub(crate) fn properties(inner: &DocInner, generation: &Generation) -> DocumentProperties {
    let pdf = &generation.pdf;
    let xref = pdf.xref();
    let bytes = inner.bytes.as_slice();
    let trailer = trailer::last_trailer(bytes);
    let mut metadata = nav::metadata(pdf, inner.encrypted);
    let mut security = None;
    let info = trailer
        .as_ref()
        .and_then(|trailer| resolve_dict(xref, trailer, b"Info"));
    match &info {
        Some(info) => complete(&mut metadata, info),
        None => {
            // hayro's dates: an offset of zero hours may be one the file
            // never names (hayro stores none as 0) or a lost minus sign.
            for date in [&mut metadata.creation_date, &mut metadata.modification_date] {
                if let Some(d) = date.as_mut() {
                    drop_unknown_offset(d);
                }
            }
        }
    }
    if let Some(trailer) = &trailer {
        security = resolve_dict(xref, trailer, b"Encrypt")
            .map(|dict| read_security(&dict, !inner.password.is_empty()));
    }
    metadata.encrypted |= security.is_some();
    let catalog = xref.get::<Dict<'_>>(xref.root_id());
    let forms = catalog
        .as_ref()
        .map(|catalog| forms::summary(xref, catalog))
        .unwrap_or_default();
    let tagged = catalog
        .as_ref()
        .and_then(|c| c.get::<Dict<'_>>(b"MarkInfo"))
        .and_then(|m| m.get::<bool>(b"Marked"))
        .unwrap_or(false);
    DocumentProperties {
        metadata,
        security,
        forms: Some(forms),
        tagged: Some(tagged),
        linearized: Some(linearized(bytes)),
    }
}

/// The dictionary `key` of the trailer names: written there, or an
/// indirect object (a stream answers with its dictionary).
fn resolve_dict<'p>(xref: &'p XRef, trailer: &Dict<'p>, key: &[u8]) -> Option<Dict<'p>> {
    match trailer.get_raw::<Dict<'p>>(key)? {
        MaybeRef::Ref(r) => xref.get::<Dict<'p>>(ObjectIdentifier::from(r)),
        MaybeRef::NotRef(dict) => Some(dict),
    }
}

/// hayro's metadata, completed from the information dictionary: the dates
/// as written (hayro drops dates without `D:` and the sign of offsets under
/// an hour), and text it does not have (a repaired file).
fn complete(metadata: &mut DocumentMetadata, info: &Dict<'_>) {
    let text = |key: &[u8]| {
        info.get::<PdfString<'_>>(key)
            .and_then(|s| info_text(s.as_bytes()))
    };
    for (slot, key) in [
        (&mut metadata.creation_date, b"CreationDate".as_slice()),
        (&mut metadata.modification_date, b"ModDate"),
    ] {
        if let Some(date) = text(key) {
            *slot = Some(date);
        }
    }
    for (slot, key) in [
        (&mut metadata.title, b"Title".as_slice()),
        (&mut metadata.author, b"Author"),
        (&mut metadata.subject, b"Subject"),
        (&mut metadata.keywords, b"Keywords"),
        (&mut metadata.creator, b"Creator"),
        (&mut metadata.producer, b"Producer"),
    ] {
        if slot.is_none() {
            *slot = text(key);
        }
    }
}

/// Cuts the offset off a date hayro wrote back (`D:YYYYMMDDHHmmSS+00'mm'`)
/// when its hours are zero: hayro writes `+00'00'` for a date without an
/// offset, and drops the minus sign of `-00'30'`.
fn drop_unknown_offset(date: &mut String) {
    const DIGITS: usize = "D:YYYYMMDDHHmmSS".len();
    if date.get(DIGITS + 1..DIGITS + 3) == Some("00") {
        date.truncate(DIGITS);
    }
}

/// What the encryption dictionary says; its strings (`/O`, `/U`) are never
/// read.
fn read_security(dict: &Dict<'_>, user_password: bool) -> Security {
    let small = |key: &[u8]| dict.get::<i64>(key).and_then(|v| u8::try_from(v).ok());
    let version = small(b"V");
    let revision = small(b"R");
    let handler = dict.get::<Name<'_>>(b"Filter").map(|name| {
        clean_display(
            &String::from_utf8_lossy(name.as_ref()),
            Security::MAX_HANDLER_BYTES,
        )
    });
    Security {
        handler,
        method: method(dict, version),
        version,
        revision,
        permissions: dict
            .get::<i64>(b"P")
            .map(|p| Permissions::from_p(p, revision.unwrap_or(2))),
        encrypt_metadata: dict.get::<bool>(b"EncryptMetadata").unwrap_or(true),
        user_password,
    }
}

/// The cipher: by `/V` up to 2, by the crypt filter of streams (or of
/// strings when streams are not encrypted) from 4 on.
fn method(dict: &Dict<'_>, version: Option<u8>) -> EncryptionMethod {
    match version {
        Some(1) => EncryptionMethod::Rc4 { key_bits: 40 },
        Some(2) => {
            let bits = dict
                .get::<i64>(b"Length")
                .and_then(|l| u16::try_from(l).ok())
                .filter(|l| (40..=128).contains(l) && l % 8 == 0)
                .unwrap_or(40);
            EncryptionMethod::Rc4 { key_bits: bits }
        }
        Some(4..=6) => {
            let streams = crypt_filter(dict, b"StmF");
            if streams == EncryptionMethod::Identity {
                crypt_filter(dict, b"StrF")
            } else {
                streams
            }
        }
        _ => EncryptionMethod::Unknown,
    }
}

/// The method of the crypt filter named by `key` (`/Identity` when none).
fn crypt_filter(dict: &Dict<'_>, key: &[u8]) -> EncryptionMethod {
    let Some(name) = dict.get::<Name<'_>>(key) else {
        return EncryptionMethod::Identity;
    };
    if name.as_ref() == b"Identity" {
        return EncryptionMethod::Identity;
    }
    let filter = dict
        .get::<Dict<'_>>(b"CF")
        .and_then(|filters| filters.get::<Dict<'_>>(name.as_ref()));
    let cfm = filter
        .as_ref()
        .and_then(|filter| filter.get::<Name<'_>>(b"CFM"));
    match cfm.as_ref().map(|n| n.as_ref()) {
        Some(b"V2") => EncryptionMethod::Rc4 {
            key_bits: rc4_filter_bits(filter.as_ref(), dict),
        },
        Some(b"AESV2") => EncryptionMethod::Aes128,
        Some(b"AESV3") => EncryptionMethod::Aes256,
        Some(b"None") => EncryptionMethod::Identity,
        _ => EncryptionMethod::Unknown,
    }
}

/// The key length of an RC4 crypt filter: its `/Length`, which the
/// standard security handler gives in bytes (5 to 16) and some writers in
/// bits, else the dictionary's `/Length` (bits), else 128.
fn rc4_filter_bits(filter: Option<&Dict<'_>>, dict: &Dict<'_>) -> u16 {
    let length = |d: &Dict<'_>| d.get::<i64>(b"Length");
    let bits = |l: i64| match l {
        5..=16 => Some(l * 8),
        40..=128 if l % 8 == 0 => Some(l),
        _ => None,
    };
    filter
        .and_then(length)
        .and_then(bits)
        .or_else(|| length(dict).and_then(bits))
        .and_then(|b| u16::try_from(b).ok())
        .unwrap_or(128)
}

/// A linearization dictionary within the first 1024 bytes whose `/L` is
/// the file's length ("Fast Web View"; the hint tables are not checked).
/// An incremental update changes the length, so the answer turns false.
fn linearized(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(LINEARIZATION_WINDOW)];
    let Some(key) = head.windows(11).position(|w| w == b"/Linearized") else {
        return false;
    };
    let Some(open) = head[..key].windows(2).rposition(|w| w == b"<<") else {
        return false;
    };
    let Some(dict) = Dict::from_bytes(&head[open..]) else {
        return false;
    };
    dict.contains_key(b"Linearized")
        && dict.get::<i64>(b"L").and_then(|l| usize::try_from(l).ok()) == Some(bytes.len())
}

#[cfg(test)]
mod tests;
