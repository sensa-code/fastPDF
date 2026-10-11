//! Signing (ADR 0013 §5, `docs/digital-signatures.md`): a document's bytes,
//! unchanged, followed by an incremental update that adds a signature field
//! and its signature dictionary (PAdES B-B, `/ETSI.CAdES.detached`).
//!
//! The update holds the signature dictionary, the field (merged with its
//! widget annotation; with an image as its appearance when the signature
//! is visible), new versions of the catalog (or its form) and of the page
//! that gets the widget, and a cross-reference section of the same kind as
//! the document's last one (a table or a stream) chained to it with
//! `/Prev`. `/Contents` is reserved as a fixed run of hex digits; once
//! the update is complete, `/ByteRange` (the whole file except those
//! digits) is filled in, the signer signs those bytes, and the signature
//! goes into the reserved room. Existing signatures stay valid: no byte of
//! the document changes.

use std::time::{SystemTime, UNIX_EPOCH};

use fastpdf_engine_api::Rotation;
use fastpdf_engine_api::write::{SignFn, SignProblem, SignatureSpec, WriteError};
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::object::{Array, Dict, ObjectIdentifier};

use super::image::{self, ImageObject};
use super::syntax::{self, RawRef, real};
use crate::geometry::PageGeom;

/// Room for the CMS signature, in bytes (twice as many hex digits): a few
/// certificates and a 4096-bit signature fit many times over.
const SIGNATURE_ROOM: usize = 32 * 1024;
/// Width of each `/ByteRange` number: room for files up to 9.9 GB.
const RANGE_DIGITS: usize = 10;
/// How far from the end `startxref` is looked for.
const TAIL: usize = 4096;

/// Why a document cannot take an incremental update as it is.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The last cross-reference section cannot be found or read: the
    /// document must be rewritten first.
    NeedsRewrite,
    Error(WriteError),
}

impl From<WriteError> for Refusal {
    fn from(e: WriteError) -> Self {
        Self::Error(e)
    }
}

/// The kind of the document's last cross-reference section.
#[derive(Debug, Clone, Copy, PartialEq)]
enum XrefKind {
    Table,
    Stream,
}

/// What the update continues from.
struct Tail {
    kind: XrefKind,
    /// Where the last cross-reference section starts (`/Prev`).
    start: usize,
    size: u32,
    root: RawRef,
    info: Option<RawRef>,
    /// The first file identifier, as written (a string).
    id: Option<Vec<u8>>,
}

/// The offset `startxref` gives.
fn startxref(bytes: &[u8]) -> Option<usize> {
    let from = bytes.len().saturating_sub(TAIL);
    let tail = &bytes[from..];
    let at = tail.windows(9).rposition(|w| w == b"startxref")?;
    let digits: Vec<u8> = tail[at + 9..]
        .iter()
        .skip_while(|b| syntax::is_whitespace(**b))
        .take_while(|b| b.is_ascii_digit())
        .copied()
        .collect();
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

fn integer(value: &[u8]) -> Option<i64> {
    std::str::from_utf8(value.trim_ascii()).ok()?.parse().ok()
}

/// The whitespace-separated tokens of `bytes`.
fn tokens(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(|b| syntax::is_whitespace(*b))
        .filter(|t| !t.is_empty())
}

/// Whether `n g obj` starts at `offset`.
fn object_at(bytes: &[u8], offset: usize, number: u32, generation: u16) -> bool {
    let Some(rest) = bytes.get(offset..) else {
        return false;
    };
    let head: Vec<&[u8]> = tokens(&rest[..rest.len().min(64)]).take(3).collect();
    let text = |t: &[u8]| {
        std::str::from_utf8(t)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
    };
    matches!(head[..], [n, g, obj] if text(n) == Some(u64::from(number))
        && text(g) == Some(u64::from(generation))
        && obj.starts_with(b"obj"))
}

/// Whether every entry in use of the cross-reference table at `start`, and
/// of the tables before it (`/Prev`), points at its object. Sections that
/// are streams are not looked into (hayro has read them).
fn tables_hold(bytes: &[u8], start: usize) -> bool {
    let mut next = Some(start);
    for _ in 0..64 {
        let Some(offset) = next else {
            return true;
        };
        let Some(section) = bytes.get(offset..) else {
            return false;
        };
        let skipped = section
            .iter()
            .take_while(|b| syntax::is_whitespace(**b))
            .count();
        let section = &section[skipped..];
        if !section.starts_with(b"xref") {
            return true;
        }
        let Some(end) = section.windows(7).position(|w| w == b"trailer") else {
            return false;
        };
        let mut words = tokens(&section[4..end]);
        let number =
            |t: Option<&[u8]>| -> Option<u64> { std::str::from_utf8(t?).ok()?.parse().ok() };
        while let Some(first) = words.next() {
            let (Some(first), Some(count)) = (number(Some(first)), number(words.next())) else {
                return false;
            };
            for i in 0..count {
                let (Some(at), Some(generation), Some(kind)) =
                    (number(words.next()), number(words.next()), words.next())
                else {
                    return false;
                };
                let (Ok(n), Ok(g), Ok(at)) = (
                    u32::try_from(first + i),
                    u16::try_from(generation),
                    usize::try_from(at),
                ) else {
                    return false;
                };
                if kind == b"n" && !object_at(bytes, at, n, g) {
                    return false;
                }
            }
        }
        let trailer = &section[end + 7..];
        let Some(open) = trailer.windows(2).position(|w| w == b"<<") else {
            return false;
        };
        next = syntax::dict_value(&trailer[open..], b"Prev")
            .and_then(integer)
            .and_then(|p| usize::try_from(p).ok());
    }
    false
}

/// The last cross-reference section and its trailer (or stream
/// dictionary), when the sections hold.
fn read_tail(bytes: &[u8]) -> Option<Tail> {
    let start = startxref(bytes)?;
    if !tables_hold(bytes, start) {
        return None;
    }
    let section = bytes.get(start..)?;
    let skipped = section
        .iter()
        .take_while(|b| syntax::is_whitespace(**b))
        .count();
    let section = &section[skipped..];
    let (kind, dict) = if section.starts_with(b"xref") {
        // The trailer follows the table.
        let at = section.windows(7).position(|w| w == b"trailer")?;
        let rest = &section[at + 7..];
        let open = rest.windows(2).position(|w| w == b"<<")?;
        (XrefKind::Table, &rest[open..])
    } else {
        // "n g obj << /Type /XRef ... >> stream"
        let at = section.windows(3).position(|w| w == b"obj")?;
        let head = std::str::from_utf8(&section[..at]).ok()?;
        let mut numbers = head.split_ascii_whitespace();
        numbers.next()?.parse::<u32>().ok()?;
        numbers.next()?.parse::<u16>().ok()?;
        let rest = &section[at + 3..];
        let open = rest.windows(2).position(|w| w == b"<<")?;
        let dict = &rest[open..];
        let kind = syntax::dict_value(dict, b"Type")?.trim_ascii();
        if kind != b"/XRef" {
            return None;
        }
        (XrefKind::Stream, dict)
    };
    let size = u32::try_from(integer(syntax::dict_value(dict, b"Size")?)?).ok()?;
    let root = syntax::single_reference(syntax::dict_value(dict, b"Root")?)?;
    let info = syntax::dict_value(dict, b"Info").and_then(syntax::single_reference);
    let id = syntax::dict_value(dict, b"ID").and_then(|ids| {
        let elements = syntax::array_elements(ids)?;
        Some(ids[elements.first()?.clone()].to_vec())
    });
    Some(Tail {
        kind,
        start,
        size,
        root,
        info,
        id,
    })
}

fn object_id((number, generation): RawRef) -> ObjectIdentifier {
    ObjectIdentifier::new(
        i32::try_from(number).unwrap_or(i32::MAX),
        i32::from(generation),
    )
}

fn raw_ref(id: ObjectIdentifier) -> Option<RawRef> {
    Some((
        u32::try_from(id.obj_number).ok()?,
        u16::try_from(id.gen_number).ok()?,
    ))
}

/// A text string: literal when it is plain ASCII, otherwise UTF-16BE in
/// hex.
fn text_string(text: &str) -> Vec<u8> {
    if text
        .bytes()
        .all(|b| (b' '..=b'~').contains(&b) && !matches!(b, b'(' | b')' | b'\\'))
    {
        return format!("({text})").into_bytes();
    }
    let mut out = b"<FEFF".to_vec();
    for unit in text.encode_utf16() {
        out.extend_from_slice(format!("{unit:04X}").as_bytes());
    }
    out.push(b'>');
    out
}

/// A PDF date for `time` (UTC).
fn pdf_date(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The update being written after the document.
struct Update {
    out: Vec<u8>,
    /// Objects written: number, generation, offset.
    entries: Vec<(u32, u16, usize)>,
    next: u32,
}

impl Update {
    fn object(&mut self, (number, generation): RawRef, body: &[u8]) {
        self.entries.push((number, generation, self.out.len()));
        self.out
            .extend_from_slice(format!("{number} {generation} obj\n").as_bytes());
        self.out.extend_from_slice(body);
        self.out.extend_from_slice(b"\nendobj\n");
    }

    fn stream(&mut self, number: u32, dict: &str, data: &[u8]) {
        self.entries.push((number, 0, self.out.len()));
        self.out.extend_from_slice(
            format!("{number} 0 obj\n<<{dict}/Length {}>>\nstream\n", data.len()).as_bytes(),
        );
        self.out.extend_from_slice(data);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    fn alloc(&mut self) -> u32 {
        let n = self.next;
        self.next += 1;
        n
    }
}

/// `base` signed as `spec` says, by `signer`, at `now`.
pub(crate) fn sign(
    base: &[u8],
    spec: &SignatureSpec,
    signer: SignFn<'_>,
    now: SystemTime,
) -> Result<Vec<u8>, Refusal> {
    let tail = read_tail(base).ok_or(Refusal::NeedsRewrite)?;
    let unreadable = |what: &str| {
        Refusal::Error(WriteError::Sign(SignProblem::Failed(format!(
            "cannot read {what}"
        ))))
    };
    let pdf = Pdf::new(base.to_vec()).map_err(|_| Refusal::NeedsRewrite)?;
    let catalog_id = pdf.xref().root_id();
    if raw_ref(catalog_id) != Some(tail.root) {
        return Err(Refusal::NeedsRewrite);
    }
    let catalog = pdf
        .xref()
        .get::<Dict<'_>>(catalog_id)
        .ok_or_else(|| unreadable("the catalog"))?;
    let page_index = spec.appearance.as_ref().map_or(0, |a| a.page.as_usize());
    let pages = pdf.pages();
    let page = pages
        .get(page_index)
        .ok_or(Refusal::Error(WriteError::NoSuchPage {
            source: 0,
            page: fastpdf_engine_api::PageIndex::new(page_index as u32),
        }))?;
    let page_ref = page
        .raw()
        .obj_id()
        .and_then(raw_ref)
        .ok_or_else(|| unreadable("the page"))?;

    let mut update = Update {
        out: base.to_vec(),
        entries: Vec::new(),
        next: tail.size,
    };
    if !update.out.ends_with(b"\n") {
        update.out.push(b'\n');
    }
    let signature = update.alloc();
    let field = update.alloc();

    // The signature dictionary, with room for the signature.
    let mut dict = b"<</Type /Sig/Filter /Adobe.PPKLite/SubFilter /ETSI.CAdES.detached".to_vec();
    dict.extend_from_slice(format!("/M ({})", pdf_date(now)).as_bytes());
    if !spec.reason.is_empty() {
        dict.extend_from_slice(b"/Reason ");
        dict.extend_from_slice(&text_string(&spec.reason));
    }
    if !spec.location.is_empty() {
        dict.extend_from_slice(b"/Location ");
        dict.extend_from_slice(&text_string(&spec.location));
    }
    dict.extend_from_slice(b"/ByteRange [0 ");
    let range_at = update.out.len() + format!("{signature} 0 obj\n").len() + dict.len();
    dict.extend_from_slice(" ".repeat(3 * RANGE_DIGITS + 2).as_bytes());
    dict.extend_from_slice(b"]/Contents ");
    let contents_at = update.out.len() + format!("{signature} 0 obj\n").len() + dict.len();
    dict.push(b'<');
    dict.extend(std::iter::repeat_n(b'0', 2 * SIGNATURE_ROOM));
    dict.push(b'>');
    let contents_end = contents_at + 2 * SIGNATURE_ROOM + 2;
    dict.extend_from_slice(b">>");
    update.object((signature, 0), &dict);

    // The field and its widget.
    let name = field_name(&catalog);
    let mut widget = format!(
        "<</Type /Annot/Subtype /Widget/FT /Sig/T {}/V {signature} 0 R/F 132/P {} {} R",
        String::from_utf8_lossy(&text_string(&name)),
        page_ref.0,
        page_ref.1
    )
    .into_bytes();
    match &spec.appearance {
        None => widget.extend_from_slice(b"/Rect [0 0 0 0]"),
        Some(appearance) => {
            let geom = PageGeom::from_page(page);
            let u = if geom.user_unit > 0.0 {
                geom.user_unit
            } else {
                1.0
            };
            let r = appearance.stamp.rect;
            let (x0, x1) = (r.x0.min(r.x1), r.x0.max(r.x1));
            let (y0, y1) = (r.y0.min(r.y1), r.y0.max(r.y1));
            let x = geom.crop_x0 + f64::from(x0) / u;
            let y = geom.crop_y0 + geom.base_h - f64::from(y1) / u;
            let w = f64::from(x1 - x0) / u;
            let h = f64::from(y1 - y0) / u;
            let form = appearance_form(
                &mut update,
                &appearance.stamp.image,
                appearance.stamp.rotate,
                w,
                h,
            )
            .map_err(Refusal::Error)?;
            widget.extend_from_slice(
                format!(
                    "/Rect [{} {} {} {}]/AP <</N {form} 0 R>>",
                    real(x),
                    real(y),
                    real(x + w),
                    real(y + h)
                )
                .as_bytes(),
            );
        }
    }
    widget.extend_from_slice(b">>");
    update.object((field, 0), &widget);

    // The page lists the widget.
    let mut annots = existing_array(&pdf, syntax::dict_value(page.raw().data(), b"Annots"));
    annots.push(format!("{field} 0 R").into_bytes());
    let page_dict = syntax::rebuild_dict(
        page.raw().data(),
        &[],
        &[(b"Annots", array(&annots))],
        <[u8]>::to_vec,
    )
    .ok_or_else(|| unreadable("the page"))?;
    update.object(page_ref, &page_dict);

    // The form lists the field.
    let form_raw = syntax::dict_value(catalog.data(), b"AcroForm");
    let form_ref = form_raw.and_then(syntax::single_reference);
    let form_dict: Vec<u8> = match (form_raw, form_ref) {
        (_, Some(r)) => pdf
            .xref()
            .get::<Dict<'_>>(object_id(r))
            .map_or_else(|| b"<<>>".to_vec(), |d| d.data().to_vec()),
        (Some(raw), None) if syntax::dict_entries(raw).is_some() => raw.to_vec(),
        _ => b"<<>>".to_vec(),
    };
    let mut fields = existing_array(&pdf, syntax::dict_value(&form_dict, b"Fields"));
    fields.push(format!("{field} 0 R").into_bytes());
    let form = syntax::rebuild_dict(
        &form_dict,
        &[],
        &[(b"Fields", array(&fields)), (b"SigFlags", b"3".to_vec())],
        <[u8]>::to_vec,
    )
    .ok_or_else(|| unreadable("the form"))?;
    match form_ref {
        Some(r) => update.object(r, &form),
        None => {
            let catalog_dict =
                syntax::rebuild_dict(catalog.data(), &[], &[(b"AcroForm", form)], <[u8]>::to_vec)
                    .ok_or_else(|| unreadable("the catalog"))?;
            update.object(tail.root, &catalog_dict);
        }
    }

    finish(&mut update, &tail, now);

    // Fill in the byte range and sign.
    let total = update.out.len();
    let ranges = [contents_at, contents_end, total - contents_end];
    let mut text = String::new();
    for value in ranges {
        text.push_str(&format!("{value:>width$} ", width = RANGE_DIGITS));
    }
    let text = text.trim_end();
    update.out[range_at..range_at + text.len()].copy_from_slice(text.as_bytes());
    let cms = {
        let parts: [&[u8]; 2] = [&update.out[..contents_at], &update.out[contents_end..]];
        signer(spec, &parts).map_err(|p| Refusal::Error(WriteError::Sign(p)))?
    };
    if cms.len() > SIGNATURE_ROOM {
        return Err(Refusal::Error(WriteError::Sign(SignProblem::TooLarge)));
    }
    for (i, b) in cms.iter().enumerate() {
        let hex = format!("{b:02X}");
        let at = contents_at + 1 + 2 * i;
        update.out[at..at + 2].copy_from_slice(hex.as_bytes());
    }
    Ok(update.out)
}

/// An array value's elements (direct, or behind a reference), as written.
fn existing_array(pdf: &Pdf, raw: Option<&[u8]>) -> Vec<Vec<u8>> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let data: Vec<u8> = match syntax::single_reference(raw) {
        Some(r) => match pdf.xref().get::<Array<'_>>(object_id(r)) {
            Some(a) => a.data().to_vec(),
            None => return Vec::new(),
        },
        None => raw.to_vec(),
    };
    syntax::array_elements(&data)
        .unwrap_or_default()
        .into_iter()
        .map(|r| data[r].to_vec())
        .collect()
}

fn array(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"[".to_vec();
    out.extend_from_slice(&items.join(&b' '));
    out.push(b']');
    out
}

/// A field name no top-level field of the form has: `Signature1`, ...
fn field_name(catalog: &Dict<'_>) -> String {
    let mut taken: Vec<Vec<u8>> = Vec::new();
    if let Some(fields) = catalog
        .get::<Dict<'_>>(b"AcroForm")
        .and_then(|f| f.get::<Array<'_>>(b"Fields"))
    {
        for field in fields.iter::<Dict<'_>>() {
            if let Some(t) = field.get::<hayro::hayro_syntax::object::String<'_>>(b"T") {
                taken.push(t.as_bytes().to_vec());
            }
        }
    }
    (1..)
        .map(|n| format!("Signature{n}"))
        .find(|name| !taken.iter().any(|t| t == name.as_bytes()))
        .unwrap_or_default()
}

/// The appearance of a visible signature: a form XObject `w` x `h` that
/// draws the image, turned clockwise by `rotate`.
fn appearance_form(
    update: &mut Update,
    path: &std::path::Path,
    rotate: Rotation,
    w: f64,
    h: f64,
) -> Result<u32, WriteError> {
    let ImageObject {
        dict, data, smask, ..
    } = image::load(path).map_err(|message| WriteError::Image {
        path: path.to_path_buf(),
        message,
    })?;
    let mask = smask.map(|(mask_dict, mask_data)| {
        let n = update.alloc();
        update.stream(n, &mask_dict, &mask_data);
        n
    });
    let picture = update.alloc();
    let mut full = dict;
    if let Some(mask) = mask {
        full.push_str(&format!("/SMask {mask} 0 R"));
    }
    update.stream(picture, &full, &data);
    let m = match rotate {
        Rotation::R0 => [w, 0.0, 0.0, h, 0.0, 0.0],
        Rotation::R90 => [0.0, -h, w, 0.0, 0.0, h],
        Rotation::R180 => [-w, 0.0, 0.0, -h, w, h],
        Rotation::R270 => [0.0, h, -w, 0.0, w, 0.0],
    };
    let content = format!("q {} cm /Seal Do Q", m.map(real).join(" "));
    let form = update.alloc();
    update.stream(
        form,
        &format!(
            "/Type /XObject/Subtype /Form/BBox [0 0 {} {}]/Resources <</XObject <</Seal {picture} 0 R>>>>",
            real(w),
            real(h)
        ),
        content.as_bytes(),
    );
    Ok(form)
}

/// A new file identifier: different for every signed file.
fn new_id(update: &Update, now: SystemTime) -> String {
    let nanos = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut hash: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for b in nanos
        .to_le_bytes()
        .iter()
        .chain(&(update.out.len() as u64).to_le_bytes())
    {
        hash ^= u128::from(*b);
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013B);
    }
    format!("<{hash:032X}>")
}

/// The cross-reference section and trailer of the update.
fn finish(update: &mut Update, tail: &Tail, now: SystemTime) {
    let id = format!(
        "[{} {}]",
        tail.id.as_ref().map_or_else(
            || new_id(update, now),
            |id| String::from_utf8_lossy(id).into_owned()
        ),
        new_id(update, now)
    );
    let info = tail
        .info
        .map_or_else(String::new, |(n, g)| format!("/Info {n} {g} R"));
    let root = format!("/Root {} {} R", tail.root.0, tail.root.1);
    match tail.kind {
        XrefKind::Table => {
            let mut entries = update.entries.clone();
            entries.sort_by_key(|(n, _, _)| *n);
            let start = update.out.len();
            let mut table = String::from("xref\n");
            for run in runs(&entries) {
                table.push_str(&format!("{} {}\n", run[0].0, run.len()));
                for (_, generation, offset) in run {
                    table.push_str(&format!("{offset:010} {generation:05} n \n"));
                }
            }
            table.push_str(&format!(
                "trailer\n<</Size {}{root}{info}/ID {id}/Prev {}>>\nstartxref\n{start}\n%%EOF\n",
                update.next.max(tail.size),
                tail.start
            ));
            update.out.extend_from_slice(table.as_bytes());
        }
        XrefKind::Stream => {
            let number = update.alloc();
            let start = update.out.len();
            let mut entries = update.entries.clone();
            entries.push((number, 0, start));
            entries.sort_by_key(|(n, _, _)| *n);
            let mut index = Vec::new();
            let mut data = Vec::with_capacity(entries.len() * 11);
            for run in runs(&entries) {
                index.push(format!("{} {}", run[0].0, run.len()));
                for (_, generation, offset) in run {
                    data.push(1u8);
                    data.extend_from_slice(&(*offset as u64).to_be_bytes());
                    data.extend_from_slice(&generation.to_be_bytes());
                }
            }
            let dict = format!(
                "/Type /XRef/Size {}{root}{info}/ID {id}/Prev {}/Index [{}]/W [1 8 2]",
                update.next.max(tail.size),
                tail.start,
                index.join(" ")
            );
            update.entries.push((number, 0, start));
            update.out.extend_from_slice(
                format!("{number} 0 obj\n<<{dict}/Length {}>>\nstream\n", data.len()).as_bytes(),
            );
            update.out.extend_from_slice(&data);
            update.out.extend_from_slice(
                format!("\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes(),
            );
        }
    }
}

/// Entries grouped into runs of consecutive object numbers.
fn runs(entries: &[(u32, u16, usize)]) -> Vec<&[(u32, u16, usize)]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=entries.len() {
        if i == entries.len() || entries[i].0 != entries[i - 1].0 + 1 {
            out.push(&entries[start..i]);
            start = i;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_pdf_dates() {
        let at = UNIX_EPOCH + std::time::Duration::from_secs(1_791_703_800);
        assert_eq!(pdf_date(at), "D:20261011073000Z");
        let leap = UNIX_EPOCH + std::time::Duration::from_secs(951_868_799);
        assert_eq!(pdf_date(leap), "D:20000229235959Z");
        assert_eq!(pdf_date(UNIX_EPOCH), "D:19700101000000Z");
    }

    #[test]
    fn texts_are_literal_or_utf16() {
        assert_eq!(text_string("Approve"), b"(Approve)");
        assert_eq!(text_string("a(b"), b"<FEFF006100280062>");
        assert_eq!(text_string("同意"), b"<FEFF540C610F>");
    }

    #[test]
    fn startxref_is_found_at_the_end() {
        assert_eq!(startxref(b"...\nstartxref\n1234\n%%EOF\n"), Some(1234));
        assert_eq!(startxref(b"startxref\r\n 77 %%EOF"), Some(77));
        assert_eq!(startxref(b"no tail here"), None);
    }

    #[test]
    fn broken_tables_are_found() {
        let good = b"1 0 obj
<<>>
endobj
xref
0 2
0000000000 65535 f 
0000000000 00000 n 
trailer
<</Size 2>>
";
        let start = good.windows(4).position(|w| w == b"xref").unwrap();
        assert!(tables_hold(good, start));
        let bad = String::from_utf8_lossy(good).replace("0000000000 00000 n", "0000000005 00000 n");
        assert!(!tables_hold(bad.as_bytes(), start));
        assert!(!tables_hold(good, good.len() + 5));
    }

    #[test]
    fn runs_split_at_gaps() {
        let entries = [(3, 0, 0), (4, 0, 0), (9, 0, 0), (10, 1, 0), (12, 0, 0)];
        let lens: Vec<usize> = runs(&entries).iter().map(|r| r.len()).collect();
        assert_eq!(lens, [2, 2, 1]);
    }
}
