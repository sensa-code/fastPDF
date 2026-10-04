//! Static pre-render scan of everything a page can reach (spec §25).
//!
//! hayro resolves color spaces, functions, patterns and XObjects
//! recursively and inflates streams without a size limit. Before a page is
//! interpreted for the first time the adapter walks the page's resources
//! itself — iteratively, with an explicit stack, so the scan cannot overflow
//! — and rejects pages that
//! * nest objects deeper than `ResourceLimits::max_nesting_depth` (chains
//!   of indirect color spaces / functions overflow hayro's stack; the depth
//!   counted here is the longest path, not the DFS discovery depth),
//! * declare images larger than `max_decoded_image_pixels` (hayro and its
//!   decoders allocate the declared size up front),
//! * contain Flate streams that inflate beyond `max_object_bytes` (images:
//!   beyond twice their declared size) — decompression bombs.
//!
//! Results are memoized per object across pages, so shared resources are
//! scanned once per document.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::rc::Rc;

use fastpdf_engine_api::{CancelToken, LimitKind, ResourceLimits};
use hayro::hayro_syntax::Filter;
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, ObjectIdentifier, Stream};
use hayro::hayro_syntax::page::Page;

/// Objects visited for one page before the scan gives up. A page whose
/// resources reach more objects than this is refused rather than rendered
/// unchecked (padding the resources must not disable the guardrail).
const MAX_SCAN_NODES: usize = 200_000;
/// Memo entries kept per document before the memo is reset.
const MAX_MEMO_ENTRIES: usize = 262_144;
/// Read buffer for the bounded inflate.
const INFLATE_CHUNK: usize = 64 * 1024;
/// Upper bound on Deflate's expansion ratio (1032:1); streams that cannot
/// exceed the cap even at that ratio are not inflated.
const MAX_DEFLATE_RATIO: u64 = 1032;

/// Why a scan stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanError {
    Limit(LimitKind),
    Cancelled,
}

/// What the scan learned about a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ScanSummary {
    /// The page reaches content that hayro interprets recursively (Form
    /// XObjects incl. soft-mask groups and annotation appearances, tiling
    /// patterns, Type3 fonts). Only such content can fan out exponentially.
    pub(crate) nested: bool,
}

/// Memoized result for one object.
#[derive(Debug, Clone, Copy)]
struct Seen {
    /// Height of the object graph below (and including) the object.
    height: u32,
    nested: bool,
}

/// Per-document memo shared by all page scans.
#[derive(Debug, Default)]
pub(crate) struct ScanMemo {
    seen: HashMap<ObjectIdentifier, Seen>,
}

impl ScanMemo {
    pub(crate) fn clear(&mut self) {
        self.seen = HashMap::new();
    }
}

enum Child<'a> {
    Ref(ObjectIdentifier),
    Direct(Object<'a>),
}

struct Frame<'a> {
    children: Vec<Child<'a>>,
    next: usize,
    max_child: u32,
    nested: bool,
    id: Option<ObjectIdentifier>,
}

/// Objects hayro interprets as nested content streams.
fn is_nested_content(obj: &Object<'_>) -> bool {
    let dict = match obj {
        Object::Dict(d) => d,
        Object::Stream(s) => s.dict(),
        _ => return false,
    };
    dict.get::<Name<'_>>(b"Subtype")
        .is_some_and(|n| matches!(&*n, b"Form" | b"Type3"))
        || dict.get::<u8>(b"PatternType") == Some(1)
}

/// Keys that point back up the document structure or at data hayro never
/// interprets while rendering a page.
fn skip_key(name: &[u8]) -> bool {
    matches!(
        name,
        b"Parent"
            | b"P"
            | b"Prev"
            | b"Next"
            | b"First"
            | b"Last"
            | b"Kids"
            | b"Dest"
            | b"Dests"
            | b"A"
            | b"AA"
            | b"PA"
            | b"IRT"
            | b"Popup"
            | b"StructParent"
            | b"StructParents"
            | b"PieceInfo"
            | b"Metadata"
            | b"Thumb"
            | b"OPI"
            | b"AF"
            | b"Measure"
            | b"PtData"
            | b"Names"
            | b"Outlines"
            | b"Threads"
            | b"B"
            | b"Root"
            | b"Info"
    )
}

fn is_container(obj: &Object<'_>) -> bool {
    matches!(obj, Object::Dict(_) | Object::Array(_) | Object::Stream(_))
}

fn push_value<'a>(out: &mut Vec<Child<'a>>, value: MaybeRef<Object<'a>>) {
    match value {
        MaybeRef::Ref(r) => out.push(Child::Ref(r.into())),
        MaybeRef::NotRef(obj) if is_container(&obj) => out.push(Child::Direct(obj)),
        MaybeRef::NotRef(_) => {}
    }
}

fn dict_children<'a>(dict: &Dict<'a>, out: &mut Vec<Child<'a>>) {
    for (key, value) in dict.entries() {
        if !skip_key(&key) {
            push_value(out, value);
        }
    }
}

fn children<'a>(obj: &Object<'a>) -> Vec<Child<'a>> {
    let mut out = Vec::new();
    match obj {
        Object::Dict(d) => dict_children(d, &mut out),
        Object::Stream(s) => dict_children(s.dict(), &mut out),
        Object::Array(a) => {
            for item in a.raw_iter() {
                push_value(&mut out, item);
            }
        }
        _ => {}
    }
    out
}

/// Root objects of a page: its (inherited) resources, content streams and
/// annotation appearance streams.
fn page_roots<'a>(page: &Page<'a>) -> Vec<Child<'a>> {
    let res = page.resources();
    let mut roots: Vec<Child<'a>> = [
        &res.ext_g_states,
        &res.fonts,
        &res.properties,
        &res.color_spaces,
        &res.x_objects,
        &res.patterns,
        &res.shadings,
    ]
    .into_iter()
    .filter(|d| !d.is_empty())
    .map(|d| Child::Direct(Object::Dict(d.clone())))
    .collect();
    let raw = page.raw();
    if let Some(contents) = raw.get_raw::<Object<'a>>(b"Contents") {
        push_value(&mut roots, contents);
    }
    if let Some(annots) = raw.get::<Array<'a>>(b"Annots") {
        for annot in annots.iter::<Dict<'a>>() {
            if let Some(ap) = annot.get_raw::<Object<'a>>(b"AP") {
                push_value(&mut roots, ap);
            }
        }
    }
    roots
}

/// Scans everything reachable from `page`. `memo` is shared per document.
pub(crate) fn scan_page(
    page: &Page<'_>,
    limits: &ResourceLimits,
    memo: &mut ScanMemo,
    cancel: Option<&CancelToken>,
) -> Result<ScanSummary, ScanError> {
    if memo.seen.len() > MAX_MEMO_ENTRIES {
        memo.clear();
    }
    let xref = page.xref();
    let max_depth = limits.max_nesting_depth.max(8);
    let mut on_stack: HashSet<ObjectIdentifier> = HashSet::new();
    let mut visited = 0usize;
    let mut stack: Vec<Frame<'_>> = vec![Frame {
        children: page_roots(page),
        next: 0,
        max_child: 0,
        nested: false,
        id: None,
    }];
    let mut summary = ScanSummary::default();

    loop {
        let depth = stack.len() as u32;
        let Some(top) = stack.last_mut() else { break };
        if top.next >= top.children.len() {
            let Some(frame) = stack.pop() else { break };
            let height = frame.max_child.saturating_add(1);
            if let Some(id) = frame.id {
                on_stack.remove(&id);
                memo.seen.insert(
                    id,
                    Seen {
                        height,
                        nested: frame.nested,
                    },
                );
            }
            match stack.last_mut() {
                Some(parent) => {
                    parent.max_child = parent.max_child.max(height);
                    parent.nested |= frame.nested;
                }
                None => summary.nested = frame.nested,
            }
            continue;
        }
        let child = match &top.children[top.next] {
            Child::Ref(id) => Child::Ref(*id),
            Child::Direct(obj) => Child::Direct(obj.clone()),
        };
        top.next += 1;
        let (id, obj) = match child {
            Child::Ref(id) => {
                if let Some(seen) = memo.seen.get(&id) {
                    if depth + seen.height > max_depth {
                        return Err(ScanError::Limit(LimitKind::Nesting));
                    }
                    top.max_child = top.max_child.max(seen.height);
                    top.nested |= seen.nested;
                    continue;
                }
                if on_stack.contains(&id) {
                    // Cycle: hayro's own cycle detection stops there.
                    continue;
                }
                visited += 1;
                if visited > MAX_SCAN_NODES {
                    return Err(ScanError::Limit(LimitKind::ObjectSize));
                }
                if visited.is_multiple_of(4096) && cancel.is_some_and(CancelToken::is_cancelled) {
                    return Err(ScanError::Cancelled);
                }
                let Some(obj) = xref.get::<Object<'_>>(id) else {
                    memo.seen.insert(
                        id,
                        Seen {
                            height: 0,
                            nested: false,
                        },
                    );
                    continue;
                };
                (Some(id), obj)
            }
            Child::Direct(obj) => (None, obj),
        };
        if let Object::Stream(s) = &obj {
            check_stream(s, limits)?;
        }
        if depth + 1 > max_depth {
            return Err(ScanError::Limit(LimitKind::Nesting));
        }
        if let Some(id) = id {
            on_stack.insert(id);
        }
        stack.push(Frame {
            children: children(&obj),
            next: 0,
            max_child: 0,
            nested: is_nested_content(&obj),
            id,
        });
    }
    Ok(summary)
}

/// Image size and bomb checks for one stream.
fn check_stream(stream: &Stream<'_>, limits: &ResourceLimits) -> Result<(), ScanError> {
    let dict = stream.dict();
    let is_image = dict
        .get::<Name<'_>>(b"Subtype")
        .is_some_and(|n| &*n == b"Image");
    let mut cap = limits.max_object_bytes;
    if is_image {
        let w = u64::from(dict.get::<u32>(b"Width").unwrap_or(0));
        let h = u64::from(dict.get::<u32>(b"Height").unwrap_or(0));
        if w.saturating_mul(h) > limits.max_decoded_image_pixels {
            return Err(ScanError::Limit(LimitKind::DecodedImage));
        }
        check_ccitt_params(dict, h, limits)?;
        if let Some(expected) = expected_image_bytes(dict, w, h) {
            cap = cap.min(expected.saturating_mul(2).saturating_add(1024 * 1024));
        }
    }
    if inflates_beyond(stream, cap) {
        return Err(ScanError::Limit(LimitKind::ObjectSize));
    }
    Ok(())
}

/// CCITT decoding allocates `Columns x max(Rows, Height)` bytes up front.
fn check_ccitt_params(
    dict: &Dict<'_>,
    height: u64,
    limits: &ResourceLimits,
) -> Result<(), ScanError> {
    let params: Vec<Dict<'_>> = if let Some(d) = dict.get::<Dict<'_>>(b"DecodeParms") {
        vec![d]
    } else if let Some(a) = dict.get::<Array<'_>>(b"DecodeParms") {
        a.iter::<Dict<'_>>().collect()
    } else {
        Vec::new()
    };
    for p in params {
        let columns = p.get::<usize>(b"Columns").map_or(1728, |c| c as u64);
        let rows = p.get::<usize>(b"Rows").map_or(0, |r| r as u64).max(height);
        if columns.saturating_mul(rows) > limits.max_decoded_image_pixels {
            return Err(ScanError::Limit(LimitKind::DecodedImage));
        }
    }
    Ok(())
}

/// Bytes a raw (Flate-only) image decodes to, when that is knowable.
fn expected_image_bytes(dict: &Dict<'_>, w: u64, h: u64) -> Option<u64> {
    let mask = dict.get::<bool>(b"ImageMask").unwrap_or(false);
    let (components, bpc) = if mask {
        (1, 1)
    } else {
        let bpc = u64::from(
            dict.get::<u8>(b"BitsPerComponent")
                .unwrap_or(8)
                .clamp(1, 16),
        );
        (color_components(dict)?, bpc)
    };
    let row = (w.saturating_mul(components).saturating_mul(bpc)).div_ceil(8);
    // + one predictor byte per row.
    Some(row.saturating_add(1).saturating_mul(h))
}

fn color_components(dict: &Dict<'_>) -> Option<u64> {
    let cs = dict.get::<Object<'_>>(b"ColorSpace")?;
    let name_components = |n: &[u8]| -> Option<u64> {
        Some(match n {
            b"DeviceGray" | b"G" | b"CalGray" | b"Indexed" | b"I" | b"Separation" => 1,
            b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => 3,
            b"DeviceCMYK" | b"CMYK" => 4,
            _ => return None,
        })
    };
    match cs {
        Object::Name(n) => name_components(&n),
        Object::Array(a) => {
            let mut items = a.iter::<Object<'_>>();
            let Some(Object::Name(family)) = items.next() else {
                return None;
            };
            match &*family {
                b"ICCBased" => match items.next() {
                    Some(Object::Stream(s)) => s
                        .dict()
                        .get::<usize>(b"N")
                        .map(|n| n as u64)
                        .filter(|n| *n <= 32),
                    _ => None,
                },
                b"DeviceN" => match items.next() {
                    Some(Object::Array(names)) => {
                        Some(names.raw_iter().count() as u64).filter(|n| *n <= 32)
                    }
                    _ => None,
                },
                other => name_components(other),
            }
        }
        _ => None,
    }
}

/// True when the stream's leading Flate filter(s) inflate to more than `cap`
/// bytes. Only Flate is checked; LZW and ASCII-prefixed chains are not.
fn inflates_beyond(stream: &Stream<'_>, cap: u64) -> bool {
    let filters = stream.filters();
    let flate_layers = filters
        .iter()
        .take_while(|f| **f == Filter::FlateDecode)
        .count();
    if flate_layers == 0 {
        return false;
    }
    let data = stream.raw_data();
    if flate_layers == 1 && (data.len() as u64).saturating_mul(MAX_DEFLATE_RATIO) <= cap {
        return false;
    }
    match count_inflated(&data, flate_layers, true, cap) {
        Some(over) => over,
        // Not a zlib stream: hayro retries as raw deflate, so do we.
        None => count_inflated(&data, flate_layers, false, cap).unwrap_or(false),
    }
}

/// Counts the inflated size of `layers` nested Flate layers without
/// materializing them. Every intermediate layer is held to `cap` too, since
/// hayro decodes each filter into memory. `None` when the outer layer fails
/// before producing any output.
fn count_inflated(data: &[u8], layers: usize, zlib: bool, cap: u64) -> Option<bool> {
    let exceeded = Rc::new(Cell::new(false));
    let produced = Rc::new(Cell::new(0u64));
    let outer: Box<dyn Read + '_> = if zlib {
        Box::new(flate2::read::ZlibDecoder::new(data))
    } else {
        Box::new(flate2::read::DeflateDecoder::new(data))
    };
    let mut reader: Box<dyn Read + '_> = Box::new(Capped {
        inner: outer,
        total: 0,
        cap,
        exceeded: Rc::clone(&exceeded),
        produced: Some(Rc::clone(&produced)),
    });
    for _ in 1..layers {
        reader = Box::new(Capped {
            inner: Box::new(flate2::read::ZlibDecoder::new(reader)),
            total: 0,
            cap,
            exceeded: Rc::clone(&exceeded),
            produced: None,
        });
    }
    let mut buf = vec![0u8; INFLATE_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Some(exceeded.get()),
            Ok(_) => {}
            Err(_) => {
                if exceeded.get() {
                    return Some(true);
                }
                return if produced.get() == 0 {
                    None
                } else {
                    Some(false)
                };
            }
        }
    }
}

/// A reader that fails once more than `cap` bytes went through it.
struct Capped<'a> {
    inner: Box<dyn Read + 'a>,
    total: u64,
    cap: u64,
    exceeded: Rc<Cell<bool>>,
    produced: Option<Rc<Cell<u64>>>,
}

impl Read for Capped<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.total = self.total.saturating_add(n as u64);
        if let Some(p) = &self.produced {
            p.set(self.total);
        }
        if self.total > self.cap {
            self.exceeded.set(true);
            return Err(std::io::Error::other("inflate cap exceeded"));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn bounded_inflate_detects_bombs() {
        let bomb = zlib(&vec![0u8; 8 * 1024 * 1024]);
        assert_eq!(count_inflated(&bomb, 1, true, 1024 * 1024), Some(true));
        assert_eq!(
            count_inflated(&bomb, 1, true, 16 * 1024 * 1024),
            Some(false)
        );
        // Nested: the intermediate layer is small, the final one is not.
        let nested = zlib(&bomb);
        assert_eq!(count_inflated(&nested, 2, true, 1024 * 1024), Some(true));
        // Garbage is not a bomb.
        assert_eq!(count_inflated(b"not zlib at all", 1, true, 10), None);
    }

    #[test]
    fn skip_list_covers_back_pointers() {
        assert!(skip_key(b"Parent"));
        assert!(skip_key(b"P"));
        assert!(!skip_key(b"Resources"));
        assert!(!skip_key(b"ColorSpace"));
    }
}
