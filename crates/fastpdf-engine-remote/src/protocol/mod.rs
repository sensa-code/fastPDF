//! Wire protocol between FastPDF and its render host (ADR 0008 §1.3).
//!
//! Frame: `u32 length` (little endian) followed by `length` payload bytes.
//! Every payload starts with `u16 protocol version` and `u8 kind`, then the
//! kind's fields, little endian, with no padding. Strings are `u32 byte
//! length` + UTF-8; collections are `u32 count` + items; optional values are
//! a `0`/`1` presence byte + value.
//!
//! The format is hand-written with `std` only (no serde) so that the decoder
//! can treat every byte as hostile: lengths are capped and checked against
//! the remaining bytes before anything is allocated, enum tags and booleans
//! must be canonical, floats must be finite, strings must be valid UTF-8,
//! the outline is decoded iteratively with a depth cap, and a frame must be
//! consumed exactly. The parent treats a host whose frame fails to decode as
//! compromised and kills it; the host exits on a bad command.
//!
//! Encoding is the mirror image. Engine output that the decoder would reject
//! is made representable on the host side instead of failing the whole
//! reply: non-finite geometry becomes `0` (or `None`), over-long strings are
//! clipped at a char boundary, and collections beyond their caps are cut.
//! Only pathological documents ever hit those caps.

mod wire;

#[cfg(test)]
mod tests;

use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Duration;

use fastpdf_engine_api::{
    ColorMode, Destination, DestinationView, DocumentMetadata, EngineCapabilities, EngineError,
    LimitKind, Link, LinkTarget, MemoryPressure, OutlineItem, PageIndex, PageInfo, PageRect,
    PageSize, PixelFormat, PixelRect, RenderOutcome, RenderRequest, RenderScale, ResourceLimits,
    Rgba8, Rotation, TextLayer, TextSpan,
};

pub(crate) use wire::ProtocolError;
use wire::{Decoder, Encoder, Result, clip};

/// Bumped on every incompatible change. Each frame carries it.
pub(crate) const PROTOCOL_VERSION: u16 = 1;

/// Identifies the code on both ends; parent and host must match exactly
/// (same executable in production, ADR 0008 §1.2).
pub(crate) const BUILD_ID: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    "/protocol-1"
);

/// Largest command frame (parent → host); the biggest command is `Open`
/// with a path and a password.
pub(crate) const MAX_COMMAND_FRAME: usize = 1 << 20;
/// Largest reply frame (host → parent).
pub(crate) const MAX_REPLY_FRAME: usize = 64 << 20;

pub(crate) const MAX_NAME: usize = 64;
pub(crate) const MAX_BUILD_ID: usize = 256;
pub(crate) const MAX_PASSWORD: usize = 4096;
pub(crate) const MAX_PATH_UNITS: usize = 32_768;
/// Error messages.
pub(crate) const MAX_MESSAGE: usize = 4096;
/// Metadata values, outline titles, URIs.
pub(crate) const MAX_STRING: usize = 64 * 1024;
pub(crate) const MAX_SPAN_TEXT: usize = 1 << 20;
pub(crate) const MAX_SPANS: usize = 1 << 20;
pub(crate) const MAX_CHAR_BOXES: usize = 1 << 22;
pub(crate) const MAX_LINKS: usize = 1 << 16;
/// Pages per geometry batch (`PageInfos`).
pub(crate) const MAX_GEOMETRY_BATCH: usize = 4096;
pub(crate) const MAX_OUTLINE_ITEMS: usize = 100_000;
pub(crate) const MAX_OUTLINE_DEPTH: usize = 64;
pub(crate) const MAX_WORKERS: u32 = 64;
pub(crate) const MAX_SLOTS: u32 = 1024;
pub(crate) const MAX_SLOT_BYTES: u64 = 256 << 20;
pub(crate) const MAX_SECTION_BYTES: u64 = 1 << 40;
/// Bytes of one slot's control block in the slot channel
/// (`win::channel`).
pub(crate) const SLOT_CONTROL_BYTES: u64 = 4096;

// Message kinds. Commands and replies use disjoint ranges so a frame sent
// the wrong way is rejected outright.
const CMD_INIT: u8 = 0x01;
const CMD_OPEN: u8 = 0x02;
const CMD_PAGE_INFO: u8 = 0x03;
const CMD_METADATA: u8 = 0x04;
const CMD_RENDER: u8 = 0x05;
const CMD_TEXT_LAYER: u8 = 0x06;
const CMD_OUTLINE: u8 = 0x07;
const CMD_LINKS: u8 = 0x08;
const CMD_MEMORY_USAGE: u8 = 0x09;
const CMD_TRIM: u8 = 0x0A;
const CMD_CANCEL: u8 = 0x0B;
const CMD_PAGE_INFOS: u8 = 0x0C;
const REPLY_READY: u8 = 0x81;
const REPLY_INIT_FAILED: u8 = 0x82;
const REPLY_DONE: u8 = 0x83;

// Payload tags inside a successful `Done`.
const P_OPENED: u8 = 1;
const P_PAGE_INFO: u8 = 2;
const P_METADATA: u8 = 3;
const P_RENDERED: u8 = 4;
const P_TEXT_LAYER: u8 = 5;
const P_OUTLINE: u8 = 6;
const P_LINKS: u8 = 7;
const P_MEMORY_USAGE: u8 = 8;
const P_TRIMMED: u8 = 9;
const P_PAGE_INFOS: u8 = 10;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Parent → host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Command {
    /// First command on a new connection.
    Init(Init),
    Open(Open),
    PageInfo {
        id: u64,
        page: PageIndex,
    },
    /// Geometry of `count` pages starting at `first` (page-geometry lane).
    PageInfos {
        id: u64,
        first: PageIndex,
        count: u32,
    },
    Metadata {
        id: u64,
    },
    Render(Render),
    TextLayer {
        id: u64,
        page: PageIndex,
    },
    Outline {
        id: u64,
    },
    Links {
        id: u64,
        page: PageIndex,
    },
    MemoryUsage {
        id: u64,
    },
    Trim {
        id: u64,
        pressure: MemoryPressure,
    },
    /// Cancels request `id`; that request still gets its own terminal reply.
    Cancel {
        id: u64,
    },
}

impl Command {
    /// Request id, for every command except `Init`.
    pub(crate) fn id(&self) -> Option<u64> {
        match self {
            Self::Init(_) => None,
            Self::Open(o) => Some(o.id),
            Self::Render(r) => Some(r.id),
            Self::PageInfo { id, .. }
            | Self::PageInfos { id, .. }
            | Self::Metadata { id }
            | Self::TextLayer { id, .. }
            | Self::Outline { id }
            | Self::Links { id, .. }
            | Self::MemoryUsage { id }
            | Self::Trim { id, .. }
            | Self::Cancel { id } => Some(*id),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Init {
    pub(crate) build_id: String,
    /// Engine name passed to the application's factory.
    pub(crate) engine: String,
    /// Worker threads the host runs requests other than renders on.
    pub(crate) workers: u32,
    /// Renders the host runs at a time.
    pub(crate) render_threads: u32,
    /// Tile slot section, already duplicated into the host.
    pub(crate) slots: Option<SlotSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotSpec {
    pub(crate) section: SectionRef,
    pub(crate) count: u32,
    pub(crate) slot_bytes: u64,
    /// The slot channel's control blocks, `count * SLOT_CONTROL_BYTES`.
    pub(crate) control: SectionRef,
    /// Semaphore released once per slot render posted in the channel.
    pub(crate) requests: u64,
    /// One completion event per slot.
    pub(crate) done: Vec<u64>,
}

/// A section (shared memory) handle that is valid in the host process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SectionRef {
    pub(crate) handle: u64,
    pub(crate) len: u64,
}

/// Where the host finds the document's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentRef {
    /// A read-only section holding a copy (sources without a file).
    Section(SectionRef),
    /// A read-only handle to the file itself, duplicated into the host
    /// (ADR 0008 §1.5): the host reads it, or maps it when it is large and
    /// not on a network drive.
    File {
        handle: u64,
        len: u64,
        network: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Open {
    pub(crate) id: u64,
    /// Document bytes; `None` for an empty source.
    pub(crate) document: Option<DocumentRef>,
    /// Diagnostics only (`DocumentSource::path`).
    pub(crate) path: Option<PathBuf>,
    pub(crate) password: Option<String>,
    pub(crate) limits: ResourceLimits,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Render {
    pub(crate) id: u64,
    pub(crate) request: RenderRequest,
    pub(crate) format: PixelFormat,
    pub(crate) target: RenderTarget,
}

/// Where the host writes the pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderTarget {
    /// One of the fixed-size tile slots.
    Slot(u32),
    /// A section made for this request (larger than a slot); the host takes
    /// ownership of the handle and closes it when done.
    Section(SectionRef),
}

/// Host → parent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reply {
    Ready {
        build_id: String,
        engine: WireEngineInfo,
    },
    InitFailed(EngineError),
    /// Terminal reply of request `id`.
    Done {
        id: u64,
        result: std::result::Result<Payload, EngineError>,
    },
}

/// `EngineInfo` with owned strings (the API type holds `&'static str`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireEngineInfo {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) capabilities: EngineCapabilities,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Payload {
    Opened {
        page_count: u32,
    },
    PageInfo(PageInfo),
    /// One result per page, starting at `first`.
    PageInfos {
        first: PageIndex,
        pages: Vec<std::result::Result<PageInfo, EngineError>>,
    },
    Metadata(DocumentMetadata),
    Rendered(RenderOutcome),
    TextLayer(TextLayer),
    Outline(Vec<OutlineItem>),
    Links(Vec<Link>),
    MemoryUsage(Option<u64>),
    Trimmed,
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum FrameError {
    Io(io::Error),
    Protocol(ProtocolError),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "pipe error: {e}"),
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
        }
    }
}

/// Reads one frame payload (without the length prefix). `Ok(None)` is a
/// clean end of stream at a frame boundary.
pub(crate) fn read_frame(
    r: &mut impl Read,
    max: usize,
) -> std::result::Result<Option<Vec<u8>>, FrameError> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Protocol(ProtocolError::Truncated)),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let len = u32::from_le_bytes(len);
    let n = usize::try_from(len).unwrap_or(usize::MAX);
    if n > max {
        return Err(FrameError::Protocol(ProtocolError::FrameTooLarge(
            u64::from(len),
        )));
    }
    if n < 3 {
        // Shorter than the version and the kind.
        return Err(FrameError::Protocol(ProtocolError::Truncated));
    }
    let mut payload = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        match r.read(&mut payload[filled..]) {
            Ok(0) => return Err(FrameError::Protocol(ProtocolError::Truncated)),
            Ok(k) => filled += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    Ok(Some(payload))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Encodes a command frame. Fails for values the protocol cannot carry (an
/// unknown future color mode, an over-long password or path).
pub(crate) fn encode_command(cmd: &Command) -> Result<Vec<u8>> {
    let enc = match cmd {
        Command::Init(init) => {
            let mut e = Encoder::new(CMD_INIT);
            put_bounded_str(&mut e, &init.build_id, MAX_BUILD_ID, "build id")?;
            put_bounded_str(&mut e, &init.engine, MAX_NAME, "engine name")?;
            e.u32(init.workers);
            e.u32(init.render_threads);
            match &init.slots {
                Some(s) => {
                    e.u8(1);
                    put_section(&mut e, s.section);
                    e.u32(s.count);
                    e.u64(s.slot_bytes);
                    put_section(&mut e, s.control);
                    e.u64(s.requests);
                    e.u32(u32::try_from(s.done.len()).unwrap_or(u32::MAX));
                    for &event in &s.done {
                        e.u64(event);
                    }
                }
                None => e.u8(0),
            }
            e
        }
        Command::Open(open) => {
            let mut e = Encoder::new(CMD_OPEN);
            e.u64(open.id);
            match open.document {
                Some(DocumentRef::Section(s)) => {
                    e.u8(1);
                    put_section(&mut e, s);
                }
                Some(DocumentRef::File {
                    handle,
                    len,
                    network,
                }) => {
                    e.u8(2);
                    put_section(&mut e, SectionRef { handle, len });
                    e.bool(network);
                }
                None => e.u8(0),
            }
            match &open.path {
                Some(p) => {
                    let units = path_units(p);
                    if units.len() > MAX_PATH_UNITS {
                        return Err(ProtocolError::Unencodable("path"));
                    }
                    e.u8(1);
                    e.count(units.len());
                    for u in units {
                        e.u16(u);
                    }
                }
                None => e.u8(0),
            }
            match &open.password {
                Some(p) => {
                    e.u8(1);
                    put_bounded_str(&mut e, p, MAX_PASSWORD, "password")?;
                }
                None => e.u8(0),
            }
            put_limits(&mut e, &open.limits)?;
            e
        }
        Command::PageInfo { id, page } => id_page(CMD_PAGE_INFO, *id, *page),
        Command::PageInfos { id, first, count } => {
            let mut e = id_page(CMD_PAGE_INFOS, *id, *first);
            e.u32(*count);
            e
        }
        Command::Metadata { id } => id_only(CMD_METADATA, *id),
        Command::Render(r) => {
            let mut e = Encoder::new(CMD_RENDER);
            e.u64(r.id);
            put_render_request(&mut e, &r.request)?;
            e.u8(match r.format {
                PixelFormat::Rgba8Premultiplied => 0,
                PixelFormat::Bgra8Premultiplied => 1,
            });
            match r.target {
                RenderTarget::Slot(i) => {
                    e.u8(0);
                    e.u32(i);
                }
                RenderTarget::Section(s) => {
                    e.u8(1);
                    put_section(&mut e, s);
                }
            }
            e
        }
        Command::TextLayer { id, page } => id_page(CMD_TEXT_LAYER, *id, *page),
        Command::Outline { id } => id_only(CMD_OUTLINE, *id),
        Command::Links { id, page } => id_page(CMD_LINKS, *id, *page),
        Command::MemoryUsage { id } => id_only(CMD_MEMORY_USAGE, *id),
        Command::Trim { id, pressure } => {
            let mut e = Encoder::new(CMD_TRIM);
            e.u64(*id);
            e.u8(match pressure {
                MemoryPressure::Normal => 0,
                MemoryPressure::Soft => 1,
                MemoryPressure::Hard => 2,
            });
            e
        }
        Command::Cancel { id } => id_only(CMD_CANCEL, *id),
    };
    if enc.payload_len() > MAX_COMMAND_FRAME {
        return Err(ProtocolError::Unencodable("command larger than a frame"));
    }
    Ok(enc.finish())
}

/// Decodes and validates a command payload.
pub(crate) fn decode_command(payload: &[u8]) -> Result<Command> {
    let (mut d, kind) = Decoder::start(payload)?;
    let cmd = match kind {
        CMD_INIT => {
            let build_id = d.str("build id", MAX_BUILD_ID)?;
            let engine = d.str("engine name", MAX_NAME)?;
            let workers = d.u32()?;
            if workers == 0 || workers > MAX_WORKERS {
                return Err(ProtocolError::BadValue("worker count"));
            }
            let render_threads = d.u32()?;
            if render_threads == 0 || render_threads > MAX_WORKERS {
                return Err(ProtocolError::BadValue("render thread count"));
            }
            let slots = if d.present("slots")? {
                let section = get_section(&mut d)?;
                let count = d.u32()?;
                let slot_bytes = d.u64()?;
                if count == 0 || count > MAX_SLOTS {
                    return Err(ProtocolError::BadValue("slot count"));
                }
                if slot_bytes == 0 || slot_bytes > MAX_SLOT_BYTES {
                    return Err(ProtocolError::BadValue("slot size"));
                }
                if u64::from(count).checked_mul(slot_bytes) != Some(section.len) {
                    return Err(ProtocolError::BadValue("slot section size"));
                }
                let control = get_section(&mut d)?;
                if u64::from(count).checked_mul(SLOT_CONTROL_BYTES) != Some(control.len) {
                    return Err(ProtocolError::BadValue("slot control size"));
                }
                let requests = get_handle(&mut d)?;
                if d.u32()? != count {
                    return Err(ProtocolError::BadValue("slot event count"));
                }
                let done = (0..count)
                    .map(|_| get_handle(&mut d))
                    .collect::<Result<Vec<_>>>()?;
                Some(SlotSpec {
                    section,
                    count,
                    slot_bytes,
                    control,
                    requests,
                    done,
                })
            } else {
                None
            };
            Command::Init(Init {
                build_id,
                engine,
                workers,
                render_threads,
                slots,
            })
        }
        CMD_OPEN => {
            let id = d.u64()?;
            let document = match d.u8()? {
                0 => None,
                1 => Some(DocumentRef::Section(get_section(&mut d)?)),
                2 => {
                    let SectionRef { handle, len } = get_section(&mut d)?;
                    Some(DocumentRef::File {
                        handle,
                        len,
                        network: d.bool("network")?,
                    })
                }
                t => return Err(ProtocolError::BadTag("document", t)),
            };
            let path = if d.present("path")? {
                let n = d.count("path", MAX_PATH_UNITS, 2)?;
                let mut units = Vec::with_capacity(n);
                for _ in 0..n {
                    units.push(d.u16()?);
                }
                Some(path_from_units(units)?)
            } else {
                None
            };
            let password = d.opt_str("password", MAX_PASSWORD)?;
            let limits = get_limits(&mut d)?;
            Command::Open(Open {
                id,
                document,
                path,
                password,
                limits,
            })
        }
        CMD_PAGE_INFOS => {
            let id = d.u64()?;
            let first = PageIndex::new(d.u32()?);
            let count = d.u32()?;
            if count == 0 || count as usize > MAX_GEOMETRY_BATCH {
                return Err(ProtocolError::BadValue("geometry batch size"));
            }
            Command::PageInfos { id, first, count }
        }
        CMD_PAGE_INFO => Command::PageInfo {
            id: d.u64()?,
            page: PageIndex::new(d.u32()?),
        },
        CMD_METADATA => Command::Metadata { id: d.u64()? },
        CMD_RENDER => {
            let id = d.u64()?;
            let request = get_render_request(&mut d)?;
            let format = match d.u8()? {
                0 => PixelFormat::Rgba8Premultiplied,
                1 => PixelFormat::Bgra8Premultiplied,
                t => return Err(ProtocolError::BadTag("pixel format", t)),
            };
            let target = match d.u8()? {
                0 => RenderTarget::Slot(d.u32()?),
                1 => RenderTarget::Section(get_section(&mut d)?),
                t => return Err(ProtocolError::BadTag("render target", t)),
            };
            Command::Render(Render {
                id,
                request,
                format,
                target,
            })
        }
        CMD_TEXT_LAYER => Command::TextLayer {
            id: d.u64()?,
            page: PageIndex::new(d.u32()?),
        },
        CMD_OUTLINE => Command::Outline { id: d.u64()? },
        CMD_LINKS => Command::Links {
            id: d.u64()?,
            page: PageIndex::new(d.u32()?),
        },
        CMD_MEMORY_USAGE => Command::MemoryUsage { id: d.u64()? },
        CMD_TRIM => {
            let id = d.u64()?;
            let pressure = match d.u8()? {
                0 => MemoryPressure::Normal,
                1 => MemoryPressure::Soft,
                2 => MemoryPressure::Hard,
                t => return Err(ProtocolError::BadTag("memory pressure", t)),
            };
            Command::Trim { id, pressure }
        }
        CMD_CANCEL => Command::Cancel { id: d.u64()? },
        k => return Err(ProtocolError::UnknownKind(k)),
    };
    d.finish()?;
    Ok(cmd)
}

fn id_only(kind: u8, id: u64) -> Encoder {
    let mut e = Encoder::new(kind);
    e.u64(id);
    e
}

fn id_page(kind: u8, id: u64, page: PageIndex) -> Encoder {
    let mut e = id_only(kind, id);
    e.u32(page.get());
    e
}

fn put_bounded_str(e: &mut Encoder, s: &str, max: usize, what: &'static str) -> Result<()> {
    if s.len() > max {
        return Err(ProtocolError::Unencodable(what));
    }
    e.str(s);
    Ok(())
}

fn put_section(e: &mut Encoder, s: SectionRef) {
    e.u64(s.handle);
    e.u64(s.len);
}

/// A kernel handle value: a non-zero multiple of four that fits in 32 bits;
/// pseudo handles (-1, -2) and garbage fail here.
fn get_handle(d: &mut Decoder<'_>) -> Result<u64> {
    let handle = d.u64()?;
    if handle == 0 || handle > u64::from(u32::MAX) || !handle.is_multiple_of(4) {
        return Err(ProtocolError::BadValue("handle"));
    }
    Ok(handle)
}

fn get_section(d: &mut Decoder<'_>) -> Result<SectionRef> {
    let handle = get_handle(d)?;
    let len = d.u64()?;
    if len == 0 || len > MAX_SECTION_BYTES {
        return Err(ProtocolError::BadValue("section size"));
    }
    Ok(SectionRef { handle, len })
}

fn put_limits(e: &mut Encoder, l: &ResourceLimits) -> Result<()> {
    if !(l.max_page_dimension_pt.is_finite() && l.max_page_dimension_pt > 0.0) {
        return Err(ProtocolError::Unencodable("page dimension limit"));
    }
    e.u64(l.max_bitmap_bytes);
    e.u32(l.max_bitmap_dimension);
    e.f32(l.max_page_dimension_pt);
    e.u32(l.max_page_count);
    e.u64(l.max_decoded_image_pixels);
    e.u32(l.max_nesting_depth);
    e.u32(l.max_recursion_depth);
    e.u64(l.max_object_bytes);
    match l.max_render_time {
        Some(t) => {
            e.u8(1);
            e.u64(t.as_secs());
            e.u32(t.subsec_nanos());
        }
        None => e.u8(0),
    }
    Ok(())
}

fn get_limits(d: &mut Decoder<'_>) -> Result<ResourceLimits> {
    let max_bitmap_bytes = d.u64()?;
    let max_bitmap_dimension = d.u32()?;
    let max_page_dimension_pt = d.f32("page dimension limit")?;
    if max_page_dimension_pt <= 0.0 {
        return Err(ProtocolError::BadValue("page dimension limit"));
    }
    let max_page_count = d.u32()?;
    let max_decoded_image_pixels = d.u64()?;
    let max_nesting_depth = d.u32()?;
    let max_recursion_depth = d.u32()?;
    let max_object_bytes = d.u64()?;
    let max_render_time = if d.present("render time limit")? {
        let secs = d.u64()?;
        let nanos = d.u32()?;
        if nanos >= 1_000_000_000 {
            return Err(ProtocolError::BadValue("render time limit"));
        }
        Some(Duration::new(secs, nanos))
    } else {
        None
    };
    Ok(ResourceLimits {
        max_bitmap_bytes,
        max_bitmap_dimension,
        max_page_dimension_pt,
        max_page_count,
        max_decoded_image_pixels,
        max_nesting_depth,
        max_recursion_depth,
        max_object_bytes,
        max_render_time,
    })
}

fn put_render_request(e: &mut Encoder, r: &RenderRequest) -> Result<()> {
    let color = match r.color_mode {
        ColorMode::Normal => 0,
        ColorMode::Inverted => 1,
        _ => return Err(ProtocolError::Unencodable("color mode")),
    };
    e.u32(r.page.get());
    e.f32(r.scale.get());
    e.u8(r.rotation.quarter_turns());
    e.u32(r.region.x);
    e.u32(r.region.y);
    e.u32(r.region.width);
    e.u32(r.region.height);
    e.u8(r.background.r);
    e.u8(r.background.g);
    e.u8(r.background.b);
    e.u8(r.background.a);
    e.u8(color);
    e.bool(r.annotations);
    Ok(())
}

fn get_render_request(d: &mut Decoder<'_>) -> Result<RenderRequest> {
    let page = PageIndex::new(d.u32()?);
    let scale = RenderScale::new(d.f32("scale")?).ok_or(ProtocolError::BadValue("scale"))?;
    let rotation = get_rotation(d)?;
    let region = PixelRect::new(d.u32()?, d.u32()?, d.u32()?, d.u32()?);
    if region.is_empty() {
        return Err(ProtocolError::BadValue("region"));
    }
    let background = Rgba8::new(d.u8()?, d.u8()?, d.u8()?, d.u8()?);
    let color_mode = match d.u8()? {
        0 => ColorMode::Normal,
        1 => ColorMode::Inverted,
        t => return Err(ProtocolError::BadTag("color mode", t)),
    };
    let annotations = d.bool("annotations")?;
    Ok(RenderRequest {
        page,
        scale,
        rotation,
        region,
        background,
        color_mode,
        annotations,
    })
}

fn get_rotation(d: &mut Decoder<'_>) -> Result<Rotation> {
    Ok(match d.u8()? {
        0 => Rotation::R0,
        1 => Rotation::R90,
        2 => Rotation::R180,
        3 => Rotation::R270,
        t => return Err(ProtocolError::BadTag("rotation", t)),
    })
}

#[cfg(windows)]
fn path_units(p: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().collect()
}

#[cfg(not(windows))]
fn path_units(p: &std::path::Path) -> Vec<u16> {
    p.to_string_lossy().encode_utf16().collect()
}

#[cfg(windows)]
fn path_from_units(units: Vec<u16>) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    // Windows paths are arbitrary UTF-16 (unpaired surrogates included),
    // so every unit sequence is a valid path.
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&units)))
}

#[cfg(not(windows))]
fn path_from_units(units: Vec<u16>) -> Result<PathBuf> {
    String::from_utf16(&units)
        .map(PathBuf::from)
        .map_err(|_| ProtocolError::BadText("path"))
}

// ---------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------

/// Encodes a reply frame. Never fails: engine output is sanitized, and a
/// result that would not fit in one frame becomes an error reply.
pub(crate) fn encode_reply(reply: &Reply) -> Vec<u8> {
    let enc = match reply {
        Reply::Ready { build_id, engine } => {
            let mut e = Encoder::new(REPLY_READY);
            e.str(clip(build_id, MAX_BUILD_ID));
            e.str(clip(&engine.name, MAX_NAME));
            e.str(clip(&engine.version, MAX_NAME));
            e.u8(capability_bits(engine.capabilities));
            e
        }
        Reply::InitFailed(err) => {
            let mut e = Encoder::new(REPLY_INIT_FAILED);
            put_error(&mut e, err);
            e
        }
        Reply::Done { id, result } => {
            let mut e = Encoder::new(REPLY_DONE);
            e.u64(*id);
            match result {
                Ok(payload) => {
                    e.u8(0);
                    put_payload(&mut e, payload);
                }
                Err(err) => {
                    e.u8(1);
                    put_error(&mut e, err);
                }
            }
            if e.payload_len() > MAX_REPLY_FRAME {
                let mut small = Encoder::new(REPLY_DONE);
                small.u64(*id);
                small.u8(1);
                put_error(
                    &mut small,
                    &EngineError::LimitExceeded(LimitKind::ObjectSize),
                );
                small
            } else {
                e
            }
        }
    };
    enc.finish()
}

/// Decodes and validates a reply payload.
pub(crate) fn decode_reply(payload: &[u8]) -> Result<Reply> {
    let (mut d, kind) = Decoder::start(payload)?;
    let reply = match kind {
        REPLY_READY => {
            let build_id = d.str("build id", MAX_BUILD_ID)?;
            let name = d.str("engine name", MAX_NAME)?;
            let version = d.str("engine version", MAX_NAME)?;
            let capabilities = capabilities_from_bits(d.u8()?);
            Reply::Ready {
                build_id,
                engine: WireEngineInfo {
                    name,
                    version,
                    capabilities,
                },
            }
        }
        REPLY_INIT_FAILED => Reply::InitFailed(get_error(&mut d)?),
        REPLY_DONE => {
            let id = d.u64()?;
            let result = match d.u8()? {
                0 => Ok(get_payload(&mut d)?),
                1 => Err(get_error(&mut d)?),
                t => return Err(ProtocolError::BadTag("result", t)),
            };
            Reply::Done { id, result }
        }
        k => return Err(ProtocolError::UnknownKind(k)),
    };
    d.finish()?;
    Ok(reply)
}

fn capability_bits(c: EngineCapabilities) -> u8 {
    [
        c.region_render,
        c.parallel_render,
        c.cooperative_cancel,
        c.text_extraction,
        c.outline,
        c.links,
        c.encryption,
        c.gpu,
    ]
    .iter()
    .enumerate()
    .fold(0, |bits, (i, on)| bits | (u8::from(*on) << i))
}

fn capabilities_from_bits(bits: u8) -> EngineCapabilities {
    let bit = |i: u8| bits & (1 << i) != 0;
    EngineCapabilities {
        region_render: bit(0),
        parallel_render: bit(1),
        cooperative_cancel: bit(2),
        text_extraction: bit(3),
        outline: bit(4),
        links: bit(5),
        encryption: bit(6),
        gpu: bit(7),
    }
}

fn put_error(e: &mut Encoder, err: &EngineError) {
    let msg = |e: &mut Encoder, tag: u8, m: &str| {
        e.u8(tag);
        e.str(clip(m, MAX_MESSAGE));
    };
    match err {
        EngineError::PasswordRequired => e.u8(0),
        EngineError::InvalidPassword => e.u8(1),
        EngineError::Malformed(m) => msg(e, 2, m),
        EngineError::Unsupported(m) => msg(e, 3, m),
        EngineError::PageOutOfRange { page, page_count } => {
            e.u8(4);
            e.u32(page.get());
            e.u32(*page_count);
        }
        EngineError::InvalidRequest(m) => msg(e, 5, m),
        EngineError::LimitExceeded(kind) => match limit_tag(*kind) {
            Some(tag) => {
                e.u8(6);
                e.u8(tag);
            }
            None => msg(e, 9, &err.to_string()),
        },
        EngineError::Cancelled => e.u8(7),
        EngineError::Panicked(m) => msg(e, 8, m),
        EngineError::Internal(m) => msg(e, 9, m),
        // A variant added to the API after this protocol version.
        other => msg(e, 9, &other.to_string()),
    }
}

fn get_error(d: &mut Decoder<'_>) -> Result<EngineError> {
    Ok(match d.u8()? {
        0 => EngineError::PasswordRequired,
        1 => EngineError::InvalidPassword,
        2 => EngineError::Malformed(d.str("error message", MAX_MESSAGE)?),
        3 => EngineError::Unsupported(d.str("error message", MAX_MESSAGE)?),
        4 => EngineError::PageOutOfRange {
            page: PageIndex::new(d.u32()?),
            page_count: d.u32()?,
        },
        5 => EngineError::InvalidRequest(d.str("error message", MAX_MESSAGE)?),
        6 => EngineError::LimitExceeded(limit_from_tag(d.u8()?)?),
        7 => EngineError::Cancelled,
        8 => EngineError::Panicked(d.str("error message", MAX_MESSAGE)?),
        9 => EngineError::Internal(d.str("error message", MAX_MESSAGE)?),
        t => return Err(ProtocolError::BadTag("error", t)),
    })
}

fn limit_tag(kind: LimitKind) -> Option<u8> {
    Some(match kind {
        LimitKind::BitmapBytes => 0,
        LimitKind::BitmapDimension => 1,
        LimitKind::PageDimension => 2,
        LimitKind::PageCount => 3,
        LimitKind::DecodedImage => 4,
        LimitKind::Nesting => 5,
        LimitKind::Recursion => 6,
        LimitKind::ObjectSize => 7,
        LimitKind::RenderTime => 8,
        _ => return None,
    })
}

fn limit_from_tag(tag: u8) -> Result<LimitKind> {
    Ok(match tag {
        0 => LimitKind::BitmapBytes,
        1 => LimitKind::BitmapDimension,
        2 => LimitKind::PageDimension,
        3 => LimitKind::PageCount,
        4 => LimitKind::DecodedImage,
        5 => LimitKind::Nesting,
        6 => LimitKind::Recursion,
        7 => LimitKind::ObjectSize,
        8 => LimitKind::RenderTime,
        t => return Err(ProtocolError::BadTag("limit kind", t)),
    })
}

fn put_payload(e: &mut Encoder, p: &Payload) {
    match p {
        Payload::Opened { page_count } => {
            e.u8(P_OPENED);
            e.u32(*page_count);
        }
        Payload::PageInfo(info) => {
            e.u8(P_PAGE_INFO);
            put_page_info(e, info);
        }
        Payload::PageInfos { first, pages } => {
            e.u8(P_PAGE_INFOS);
            e.u32(first.get());
            let pages = &pages[..pages.len().min(MAX_GEOMETRY_BATCH)];
            e.count(pages.len());
            for page in pages {
                match page {
                    Ok(info) => {
                        e.u8(0);
                        put_page_info(e, info);
                    }
                    Err(err) => {
                        e.u8(1);
                        put_error(e, err);
                    }
                }
            }
        }
        Payload::Metadata(m) => {
            e.u8(P_METADATA);
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
        Payload::Rendered(outcome) => {
            e.u8(P_RENDERED);
            e.bool(outcome.partial);
        }
        Payload::TextLayer(layer) => {
            e.u8(P_TEXT_LAYER);
            put_text_layer(e, layer);
        }
        Payload::Outline(items) => {
            e.u8(P_OUTLINE);
            put_outline(e, items);
        }
        Payload::Links(links) => {
            e.u8(P_LINKS);
            let links = &links[..links.len().min(MAX_LINKS)];
            e.count(links.len());
            for link in links {
                put_rect(e, link.bounds);
                match &link.target {
                    LinkTarget::Internal(dest) => {
                        e.u8(0);
                        put_destination(e, dest);
                    }
                    LinkTarget::Uri(uri) if uri.len() <= MAX_STRING => {
                        e.u8(1);
                        e.str(uri);
                    }
                    // An over-long URI is not clipped (that would change
                    // where it points); the link degrades to unsupported.
                    _ => e.u8(2),
                }
            }
        }
        Payload::MemoryUsage(bytes) => {
            e.u8(P_MEMORY_USAGE);
            match bytes {
                Some(b) => {
                    e.u8(1);
                    e.u64(*b);
                }
                None => e.u8(0),
            }
        }
        Payload::Trimmed => e.u8(P_TRIMMED),
    }
}

fn get_payload(d: &mut Decoder<'_>) -> Result<Payload> {
    Ok(match d.u8()? {
        P_OPENED => Payload::Opened {
            page_count: d.u32()?,
        },
        P_PAGE_INFO => Payload::PageInfo(get_page_info(d)?),
        P_PAGE_INFOS => {
            let first = PageIndex::new(d.u32()?);
            // tag (1) + error tag (1) at least
            let n = d.count("page infos", MAX_GEOMETRY_BATCH, 2)?;
            let mut pages = Vec::with_capacity(n);
            for _ in 0..n {
                pages.push(match d.u8()? {
                    0 => Ok(get_page_info(d)?),
                    1 => Err(get_error(d)?),
                    t => return Err(ProtocolError::BadTag("page info", t)),
                });
            }
            Payload::PageInfos { first, pages }
        }
        P_METADATA => {
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
            Payload::Metadata(DocumentMetadata {
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
        P_RENDERED => Payload::Rendered(RenderOutcome {
            partial: d.bool("partial")?,
        }),
        P_TEXT_LAYER => Payload::TextLayer(get_text_layer(d)?),
        P_OUTLINE => Payload::Outline(get_outline(d)?),
        P_LINKS => {
            // bounds (16) + target tag (1)
            let n = d.count("links", MAX_LINKS, 17)?;
            let mut links = Vec::with_capacity(n);
            for _ in 0..n {
                let bounds = get_rect(d)?;
                let target = match d.u8()? {
                    0 => LinkTarget::Internal(get_destination(d)?),
                    1 => LinkTarget::Uri(d.str("uri", MAX_STRING)?),
                    2 => LinkTarget::Unsupported,
                    t => return Err(ProtocolError::BadTag("link target", t)),
                };
                links.push(Link { bounds, target });
            }
            Payload::Links(links)
        }
        P_MEMORY_USAGE => Payload::MemoryUsage(if d.present("memory usage")? {
            Some(d.u64()?)
        } else {
            None
        }),
        P_TRIMMED => Payload::Trimmed,
        t => return Err(ProtocolError::BadTag("payload", t)),
    })
}

fn put_page_info(e: &mut Encoder, info: &PageInfo) {
    // The host's guard only lets sane sizes through; anything else becomes
    // a 1 pt page rather than an undecodable reply.
    let side = |v: f32| if v.is_finite() && v > 0.0 { v } else { 1.0 };
    e.f32(side(info.size.width));
    e.f32(side(info.size.height));
    e.u8(info.rotation.quarter_turns());
}

fn get_page_info(d: &mut Decoder<'_>) -> Result<PageInfo> {
    let width = d.f32("page width")?;
    let height = d.f32("page height")?;
    if width <= 0.0 || height <= 0.0 {
        return Err(ProtocolError::BadValue("page size"));
    }
    Ok(PageInfo {
        size: PageSize::new(width, height),
        rotation: get_rotation(d)?,
    })
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn put_rect(e: &mut Encoder, r: PageRect) {
    e.f32(finite(r.x0));
    e.f32(finite(r.y0));
    e.f32(finite(r.x1));
    e.f32(finite(r.y1));
}

fn get_rect(d: &mut Decoder<'_>) -> Result<PageRect> {
    // Fields are read as stored (not re-normalized) so a round trip is exact.
    Ok(PageRect {
        x0: d.f32("rectangle")?,
        y0: d.f32("rectangle")?,
        x1: d.f32("rectangle")?,
        y1: d.f32("rectangle")?,
    })
}

fn put_opt_f32(e: &mut Encoder, v: Option<f32>) {
    match v.filter(|v| v.is_finite()) {
        Some(v) => {
            e.u8(1);
            e.f32(v);
        }
        None => e.u8(0),
    }
}

fn get_opt_f32(d: &mut Decoder<'_>, what: &'static str) -> Result<Option<f32>> {
    if d.present(what)? {
        d.f32(what).map(Some)
    } else {
        Ok(None)
    }
}

fn put_destination(e: &mut Encoder, dest: &Destination) {
    e.u32(dest.page.get());
    match &dest.view {
        DestinationView::Xyz { left, top, zoom } => {
            e.u8(0);
            put_opt_f32(e, *left);
            put_opt_f32(e, *top);
            put_opt_f32(e, *zoom);
        }
        DestinationView::Fit => e.u8(1),
        DestinationView::FitWidth { top } => {
            e.u8(2);
            put_opt_f32(e, *top);
        }
        DestinationView::FitHeight { left } => {
            e.u8(3);
            put_opt_f32(e, *left);
        }
        DestinationView::FitRect(r) => {
            e.u8(4);
            put_rect(e, *r);
        }
    }
}

fn get_destination(d: &mut Decoder<'_>) -> Result<Destination> {
    let page = PageIndex::new(d.u32()?);
    let view = match d.u8()? {
        0 => DestinationView::Xyz {
            left: get_opt_f32(d, "destination")?,
            top: get_opt_f32(d, "destination")?,
            zoom: get_opt_f32(d, "destination")?,
        },
        1 => DestinationView::Fit,
        2 => DestinationView::FitWidth {
            top: get_opt_f32(d, "destination")?,
        },
        3 => DestinationView::FitHeight {
            left: get_opt_f32(d, "destination")?,
        },
        4 => DestinationView::FitRect(get_rect(d)?),
        t => return Err(ProtocolError::BadTag("destination view", t)),
    };
    Ok(Destination { page, view })
}

fn put_text_layer(e: &mut Encoder, layer: &TextLayer) {
    e.u32(layer.page.get());
    let spans = &layer.spans[..layer.spans.len().min(MAX_SPANS)];
    e.count(spans.len());
    for span in spans {
        e.str(clip(&span.text, MAX_SPAN_TEXT));
        put_rect(e, span.bounds);
        let boxes = &span.char_bounds[..span.char_bounds.len().min(MAX_CHAR_BOXES)];
        e.count(boxes.len());
        for r in boxes {
            put_rect(e, *r);
        }
    }
}

fn get_text_layer(d: &mut Decoder<'_>) -> Result<TextLayer> {
    let page = PageIndex::new(d.u32()?);
    // text length (4) + bounds (16) + box count (4)
    let n = d.count("text spans", MAX_SPANS, 24)?;
    let mut spans = Vec::with_capacity(n);
    for _ in 0..n {
        let text = d.str("span text", MAX_SPAN_TEXT)?;
        let bounds = get_rect(d)?;
        let boxes = d.count("character boxes", MAX_CHAR_BOXES, 16)?;
        let mut char_bounds = Vec::with_capacity(boxes);
        for _ in 0..boxes {
            char_bounds.push(get_rect(d)?);
        }
        spans.push(TextSpan {
            text,
            bounds,
            char_bounds,
        });
    }
    Ok(TextLayer { page, spans })
}

/// Pre-order, iteratively (a hostile outline must not overflow the host's
/// stack either). Each item is followed by its child count, patched in once
/// the children have been written; items past the depth or count caps are
/// left out.
fn put_outline(e: &mut Encoder, items: &[OutlineItem]) {
    struct Level<'a> {
        items: &'a [OutlineItem],
        next: usize,
        count_at: usize,
        written: u32,
    }
    let mut budget = MAX_OUTLINE_ITEMS;
    let top = e.placeholder_u32();
    let mut stack = vec![Level {
        items,
        next: 0,
        count_at: top,
        written: 0,
    }];
    loop {
        let depth = stack.len();
        let Some(level) = stack.last_mut() else {
            break;
        };
        let siblings: &[OutlineItem] = level.items;
        let next = if budget > 0 {
            siblings.get(level.next)
        } else {
            None
        };
        let Some(item) = next else {
            let (at, written) = (level.count_at, level.written);
            stack.pop();
            e.patch_u32(at, written);
            continue;
        };
        level.next += 1;
        level.written += 1;
        budget -= 1;
        e.str(clip(&item.title, MAX_STRING));
        match &item.destination {
            Some(dest) => {
                e.u8(1);
                put_destination(e, dest);
            }
            None => e.u8(0),
        }
        e.opt_str(item.uri.as_deref().filter(|u| u.len() <= MAX_STRING));
        e.bool(item.open);
        let children_at = e.placeholder_u32();
        if depth < MAX_OUTLINE_DEPTH && !item.children.is_empty() {
            stack.push(Level {
                items: &item.children,
                next: 0,
                count_at: children_at,
                written: 0,
            });
        }
    }
}

fn get_outline(d: &mut Decoder<'_>) -> Result<Vec<OutlineItem>> {
    struct Level {
        items: Vec<OutlineItem>,
        remaining: usize,
        parent: Option<OutlineItem>,
    }
    // title length (4) + destination flag + uri flag + open + child count (4)
    const MIN_ITEM: usize = 11;
    let top = d.count("outline", MAX_OUTLINE_ITEMS, MIN_ITEM)?;
    let mut total = 0usize;
    let mut stack = vec![Level {
        items: Vec::with_capacity(top.min(1024)),
        remaining: top,
        parent: None,
    }];
    loop {
        let depth = stack.len();
        let Some(level) = stack.last_mut() else {
            return Err(ProtocolError::BadValue("outline"));
        };
        if level.remaining == 0 {
            let Some(done) = stack.pop() else {
                return Err(ProtocolError::BadValue("outline"));
            };
            match (done.parent, stack.last_mut()) {
                (None, _) => return Ok(done.items),
                (Some(mut parent), Some(up)) => {
                    parent.children = done.items;
                    up.items.push(parent);
                }
                (Some(_), None) => return Err(ProtocolError::BadValue("outline")),
            }
            continue;
        }
        level.remaining -= 1;
        total += 1;
        if total > MAX_OUTLINE_ITEMS {
            return Err(ProtocolError::TooLong("outline"));
        }
        let title = d.str("outline title", MAX_STRING)?;
        let destination = if d.present("outline destination")? {
            Some(get_destination(d)?)
        } else {
            None
        };
        let uri = d.opt_str("outline uri", MAX_STRING)?;
        let open = d.bool("outline open")?;
        let children = d.count("outline", MAX_OUTLINE_ITEMS, MIN_ITEM)?;
        let item = OutlineItem {
            title,
            destination,
            uri,
            open,
            children: Vec::new(),
        };
        if children == 0 {
            level.items.push(item);
        } else {
            if depth >= MAX_OUTLINE_DEPTH {
                return Err(ProtocolError::TooDeep);
            }
            stack.push(Level {
                items: Vec::with_capacity(children.min(1024)),
                remaining: children,
                parent: Some(item),
            });
        }
    }
}
