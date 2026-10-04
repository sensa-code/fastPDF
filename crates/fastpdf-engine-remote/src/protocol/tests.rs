//! Round trips for every message, plus fuzz-style tests: a fixed-seed PRNG
//! throws random, mutated and truncated frames at both decoders, which must
//! return an error (never panic, never over-allocate) and, when they do
//! accept a frame, must re-encode it to exactly the same bytes.
//!
//! `FASTPDF_REMOTE_FUZZ_ITERS` raises the iteration count for longer runs.

use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

use fastpdf_engine_api::{
    ColorMode, Destination, DestinationView, DocumentMetadata, EngineCapabilities, EngineError,
    LimitKind, Link, LinkTarget, MemoryPressure, OutlineItem, PageIndex, PageInfo, PageRect,
    PageSize, PixelFormat, PixelRect, RenderOutcome, RenderRequest, RenderScale, ResourceLimits,
    Rgba8, Rotation, TextLayer, TextSpan,
};

use super::*;

/// SplitMix64: tiny, fast, and deterministic for a given seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }

    fn f32(&mut self) -> f32 {
        const SPECIAL: [f32; 8] = [0.0, -0.0, 1.0, -1.0, 0.5, 612.0, 1e-30, 3.0e38];
        if self.chance(4) {
            return SPECIAL[self.below(SPECIAL.len())];
        }
        loop {
            let v = f32::from_bits(self.next() as u32);
            if v.is_finite() {
                return v;
            }
        }
    }

    fn positive_f32(&mut self) -> f32 {
        loop {
            let v = self.f32().abs();
            if v > 0.0 {
                return v;
            }
        }
    }

    fn opt_f32(&mut self) -> Option<f32> {
        self.chance(2).then(|| self.f32())
    }

    fn string(&mut self, max_chars: usize) -> String {
        const POOL: [char; 12] = [
            'a', 'Z', '0', ' ', '\n', '\\', '"', 'ü', '測', '試', '😀', '\u{0}',
        ];
        let n = self.below(max_chars + 1);
        (0..n).map(|_| POOL[self.below(POOL.len())]).collect()
    }

    fn opt_string(&mut self, max_chars: usize) -> Option<String> {
        self.chance(2).then(|| self.string(max_chars))
    }

    fn handle(&mut self) -> u64 {
        (1 + self.below(0x3FFF_FFFF) as u64) * 4
    }

    fn section(&mut self) -> SectionRef {
        SectionRef {
            handle: self.handle(),
            len: 1 + self.next() % MAX_SECTION_BYTES,
        }
    }

    fn rect(&mut self) -> PageRect {
        PageRect {
            x0: self.f32(),
            y0: self.f32(),
            x1: self.f32(),
            y1: self.f32(),
        }
    }

    fn rotation(&mut self) -> Rotation {
        [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270][self.below(4)]
    }

    fn destination(&mut self) -> Destination {
        let view = match self.below(5) {
            0 => DestinationView::Xyz {
                left: self.opt_f32(),
                top: self.opt_f32(),
                zoom: self.opt_f32(),
            },
            1 => DestinationView::Fit,
            2 => DestinationView::FitWidth {
                top: self.opt_f32(),
            },
            3 => DestinationView::FitHeight {
                left: self.opt_f32(),
            },
            _ => DestinationView::FitRect(self.rect()),
        };
        Destination {
            page: PageIndex::new(self.next() as u32),
            view,
        }
    }

    fn outline(&mut self, depth: usize) -> Vec<OutlineItem> {
        let n = if depth > 4 { 0 } else { self.below(4) };
        (0..n)
            .map(|_| OutlineItem {
                title: self.string(12),
                destination: self.chance(2).then(|| self.destination()),
                uri: self.opt_string(8),
                open: self.chance(2),
                children: self.outline(depth + 1),
            })
            .collect()
    }

    fn error(&mut self) -> EngineError {
        const KINDS: [LimitKind; 9] = [
            LimitKind::BitmapBytes,
            LimitKind::BitmapDimension,
            LimitKind::PageDimension,
            LimitKind::PageCount,
            LimitKind::DecodedImage,
            LimitKind::Nesting,
            LimitKind::Recursion,
            LimitKind::ObjectSize,
            LimitKind::RenderTime,
        ];
        match self.below(10) {
            0 => EngineError::PasswordRequired,
            1 => EngineError::InvalidPassword,
            2 => EngineError::Malformed(self.string(20)),
            3 => EngineError::Unsupported(self.string(20)),
            4 => EngineError::PageOutOfRange {
                page: PageIndex::new(self.next() as u32),
                page_count: self.next() as u32,
            },
            5 => EngineError::InvalidRequest(self.string(20)),
            6 => EngineError::LimitExceeded(KINDS[self.below(KINDS.len())]),
            7 => EngineError::Cancelled,
            8 => EngineError::Panicked(self.string(20)),
            _ => EngineError::Internal(self.string(20)),
        }
    }

    fn limits(&mut self) -> ResourceLimits {
        ResourceLimits {
            max_bitmap_bytes: self.next(),
            max_bitmap_dimension: self.next() as u32,
            max_page_dimension_pt: self.positive_f32(),
            max_page_count: self.next() as u32,
            max_decoded_image_pixels: self.next(),
            max_nesting_depth: self.next() as u32,
            max_recursion_depth: self.next() as u32,
            max_object_bytes: self.next(),
            max_render_time: self
                .chance(2)
                .then(|| Duration::new(self.next(), (self.next() % 1_000_000_000) as u32)),
        }
    }

    fn render_request(&mut self) -> RenderRequest {
        let scale = RenderScale::MIN + (self.below(10_000) as f32 / 10_000.0) * 60.0;
        RenderRequest {
            page: PageIndex::new(self.next() as u32),
            scale: RenderScale::new(scale).unwrap_or(RenderScale::IDENTITY),
            rotation: self.rotation(),
            region: PixelRect::new(
                self.next() as u32,
                self.next() as u32,
                1 + self.below(u32::MAX as usize - 1) as u32,
                1 + self.below(u32::MAX as usize - 1) as u32,
            ),
            background: Rgba8::new(self.byte(), self.byte(), self.byte(), self.byte()),
            color_mode: if self.chance(2) {
                ColorMode::Normal
            } else {
                ColorMode::Inverted
            },
            annotations: self.chance(2),
        }
    }

    fn command(&mut self) -> Command {
        let id = self.next();
        let page = PageIndex::new(self.next() as u32);
        match self.below(12) {
            11 => Command::PageInfos {
                id,
                first: page,
                count: 1 + self.below(MAX_GEOMETRY_BATCH) as u32,
            },
            0 => Command::Init(Init {
                build_id: self.string(30),
                engine: self.string(10),
                workers: 1 + self.below(MAX_WORKERS as usize) as u32,
                render_threads: 1 + self.below(MAX_WORKERS as usize) as u32,
                slots: self.chance(2).then(|| {
                    let count = 1 + self.below(MAX_SLOTS as usize) as u32;
                    let slot_bytes = 1 + self.next() % MAX_SLOT_BYTES;
                    SlotSpec {
                        section: SectionRef {
                            handle: self.handle(),
                            len: u64::from(count) * slot_bytes,
                        },
                        count,
                        slot_bytes,
                    }
                }),
            }),
            1 => Command::Open(Open {
                id,
                document: match self.below(4) {
                    0 => None,
                    1 => Some(DocumentRef::Section(self.section())),
                    _ => {
                        let SectionRef { handle, len } = self.section();
                        Some(DocumentRef::File {
                            handle,
                            len,
                            network: self.chance(2),
                        })
                    }
                },
                path: self.opt_string(30).map(PathBuf::from),
                password: self.opt_string(20),
                limits: self.limits(),
            }),
            2 => Command::PageInfo { id, page },
            3 => Command::Metadata { id },
            4 => Command::Render(Render {
                id,
                request: self.render_request(),
                format: if self.chance(2) {
                    PixelFormat::Rgba8Premultiplied
                } else {
                    PixelFormat::Bgra8Premultiplied
                },
                target: if self.chance(2) {
                    RenderTarget::Slot(self.next() as u32)
                } else {
                    RenderTarget::Section(self.section())
                },
            }),
            5 => Command::TextLayer { id, page },
            6 => Command::Outline { id },
            7 => Command::Links { id, page },
            8 => Command::MemoryUsage { id },
            9 => Command::Trim {
                id,
                pressure: [
                    MemoryPressure::Normal,
                    MemoryPressure::Soft,
                    MemoryPressure::Hard,
                ][self.below(3)],
            },
            _ => Command::Cancel { id },
        }
    }

    fn page_info(&mut self) -> PageInfo {
        PageInfo {
            size: PageSize::new(self.positive_f32(), self.positive_f32()),
            rotation: self.rotation(),
        }
    }

    fn payload(&mut self) -> Payload {
        match self.below(10) {
            9 => Payload::PageInfos {
                first: PageIndex::new(self.next() as u32),
                pages: (0..self.below(20))
                    .map(|_| {
                        if self.chance(4) {
                            Err(self.error())
                        } else {
                            Ok(self.page_info())
                        }
                    })
                    .collect(),
            },
            0 => Payload::Opened {
                page_count: self.next() as u32,
            },
            1 => Payload::PageInfo(self.page_info()),
            2 => Payload::Metadata(DocumentMetadata {
                title: self.opt_string(20),
                author: self.opt_string(20),
                subject: self.opt_string(20),
                keywords: self.opt_string(20),
                creator: self.opt_string(20),
                producer: self.opt_string(20),
                creation_date: self.opt_string(20),
                modification_date: self.opt_string(20),
                pdf_version: self.opt_string(4),
                encrypted: self.chance(2),
            }),
            3 => Payload::Rendered(RenderOutcome {
                partial: self.chance(2),
            }),
            4 => {
                let spans = (0..self.below(5))
                    .map(|_| TextSpan {
                        text: self.string(16),
                        bounds: self.rect(),
                        char_bounds: (0..self.below(6)).map(|_| self.rect()).collect(),
                    })
                    .collect();
                Payload::TextLayer(TextLayer {
                    page: PageIndex::new(self.next() as u32),
                    spans,
                })
            }
            5 => Payload::Outline(self.outline(0)),
            6 => Payload::Links(
                (0..self.below(5))
                    .map(|_| Link {
                        bounds: self.rect(),
                        target: match self.below(3) {
                            0 => LinkTarget::Internal(self.destination()),
                            1 => LinkTarget::Uri(self.string(20)),
                            _ => LinkTarget::Unsupported,
                        },
                    })
                    .collect(),
            ),
            7 => Payload::MemoryUsage(self.chance(2).then(|| self.next())),
            _ => Payload::Trimmed,
        }
    }

    fn reply(&mut self) -> Reply {
        match self.below(6) {
            0 => Reply::Ready {
                build_id: self.string(30),
                engine: WireEngineInfo {
                    name: self.string(10),
                    version: self.string(10),
                    capabilities: capabilities_from_bits(self.byte()),
                },
            },
            1 => Reply::InitFailed(self.error()),
            2 => Reply::Done {
                id: self.next(),
                result: Err(self.error()),
            },
            _ => Reply::Done {
                id: self.next(),
                result: Ok(self.payload()),
            },
        }
    }
}

fn iterations(default: usize) -> usize {
    std::env::var("FASTPDF_REMOTE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Runs both decoders; accepted frames must re-encode byte for byte.
fn check_decoders(payload: &[u8]) {
    if let Ok(cmd) = decode_command(payload) {
        let again = encode_command(&cmd).expect("a decoded command re-encodes");
        assert_eq!(&again[4..], payload, "command encoding is not canonical");
    }
    if let Ok(reply) = decode_reply(payload) {
        let again = encode_reply(&reply);
        assert_eq!(&again[4..], payload, "reply encoding is not canonical");
    }
}

fn frame_payload(frame: &[u8]) -> &[u8] {
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    assert_eq!(len, frame.len() - 4, "length prefix");
    &frame[4..]
}

#[test]
fn every_command_round_trips() {
    let mut rng = Rng(1);
    for _ in 0..iterations(3_000) {
        let cmd = rng.command();
        let frame = encode_command(&cmd).expect("valid commands encode");
        let decoded = decode_command(frame_payload(&frame)).expect("valid commands decode");
        assert_eq!(decoded, cmd);
        // And through the frame reader.
        let mut cursor = Cursor::new(frame.clone());
        let payload = read_frame(&mut cursor, MAX_COMMAND_FRAME)
            .expect("frame reads")
            .expect("not at end of stream");
        assert_eq!(payload, frame[4..]);
        assert!(matches!(
            read_frame(&mut cursor, MAX_COMMAND_FRAME),
            Ok(None)
        ));
    }
}

#[test]
fn every_reply_round_trips() {
    let mut rng = Rng(2);
    for _ in 0..iterations(3_000) {
        let reply = rng.reply();
        let frame = encode_reply(&reply);
        let decoded = decode_reply(frame_payload(&frame)).expect("valid replies decode");
        assert_eq!(decoded, reply);
    }
}

#[test]
fn hand_picked_messages_round_trip() {
    let request = RenderRequest::full_page(
        PageIndex::new(3),
        PageSize::LETTER,
        Rotation::R90,
        Rotation::R180,
        RenderScale::new(96.0 / 72.0).unwrap(),
    );
    let commands = [
        Command::Init(Init {
            build_id: BUILD_ID.into(),
            engine: "hayro".into(),
            workers: 4,
            render_threads: 2,
            slots: Some(SlotSpec {
                section: SectionRef {
                    handle: 0x1A4,
                    len: 10 * 1_114_112,
                },
                count: 10,
                slot_bytes: 1_114_112,
            }),
        }),
        Command::Open(Open {
            id: 1,
            document: Some(DocumentRef::Section(SectionRef {
                handle: 0x200,
                len: 12_345,
            })),
            path: Some(PathBuf::from(r"C:\文件\a b.pdf")),
            password: Some("pässwörd".into()),
            limits: ResourceLimits::default(),
        }),
        Command::Open(Open {
            id: 3,
            document: Some(DocumentRef::File {
                handle: 0x3c4,
                len: 900 << 20,
                network: true,
            }),
            path: Some(PathBuf::from(r"\\nas\scans\big.pdf")),
            password: None,
            limits: ResourceLimits::default(),
        }),
        Command::Open(Open {
            id: 2,
            document: None,
            path: None,
            password: None,
            limits: ResourceLimits {
                max_render_time: None,
                ..ResourceLimits::default()
            },
        }),
        Command::Render(Render {
            id: u64::MAX,
            request,
            format: PixelFormat::Bgra8Premultiplied,
            target: RenderTarget::Slot(9),
        }),
        Command::Cancel { id: 0 },
    ];
    for cmd in commands {
        let frame = encode_command(&cmd).unwrap();
        assert_eq!(decode_command(frame_payload(&frame)).unwrap(), cmd);
    }
}

#[test]
fn decoders_survive_random_bytes() {
    let kinds = [
        CMD_INIT,
        CMD_OPEN,
        CMD_PAGE_INFO,
        CMD_METADATA,
        CMD_RENDER,
        CMD_TEXT_LAYER,
        CMD_OUTLINE,
        CMD_LINKS,
        CMD_MEMORY_USAGE,
        CMD_TRIM,
        CMD_CANCEL,
        CMD_PAGE_INFOS,
        REPLY_READY,
        REPLY_INIT_FAILED,
        REPLY_DONE,
    ];
    let mut rng = Rng(0x00F0_0D5E_ED00);
    for _ in 0..iterations(40_000) {
        let len = rng.below(200);
        let mut payload: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        // Half the inputs get a valid header so the field decoders run.
        if payload.len() >= 3 && rng.chance(2) {
            payload[..2].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
            payload[2] = kinds[rng.below(kinds.len())];
            if payload.len() >= 4 && payload[2] == REPLY_DONE && rng.chance(2) {
                // Steer `Done` replies towards the payload decoders.
                payload.resize(payload.len().max(13), 0);
                payload[11] = 0;
                payload[12] = 1 + rng.below(10) as u8;
            }
        }
        check_decoders(&payload);
    }
}

#[test]
fn decoders_survive_mutated_messages() {
    let mut rng = Rng(0xBAD_F00D);
    for _ in 0..iterations(40_000) {
        let frame = if rng.chance(2) {
            encode_command(&rng.command()).unwrap()
        } else {
            encode_reply(&rng.reply())
        };
        let mut payload = frame[4..].to_vec();
        for _ in 0..1 + rng.below(4) {
            if payload.is_empty() {
                break;
            }
            let at = rng.below(payload.len());
            match rng.below(6) {
                0 => payload[at] ^= 1 << rng.below(8),
                1 => payload[at] = rng.byte(),
                2 => payload.insert(at, rng.byte()),
                3 => {
                    payload.remove(at);
                }
                4 => payload.truncate(at),
                _ => {
                    // Corrupt what may be a length or count field.
                    let big = [u32::MAX, 0x7FFF_FFFF, 0x0100_0000, 65_536, 4_096];
                    let v = big[rng.below(big.len())].to_le_bytes();
                    for (i, b) in v.iter().enumerate() {
                        if let Some(slot) = payload.get_mut(at + i) {
                            *slot = *b;
                        }
                    }
                }
            }
        }
        check_decoders(&payload);
    }
}

#[test]
fn every_truncation_is_rejected() {
    let mut rng = Rng(7);
    for _ in 0..iterations(300) {
        let (frame, command) = if rng.chance(2) {
            (encode_command(&rng.command()).unwrap(), true)
        } else {
            (encode_reply(&rng.reply()), false)
        };
        let payload = &frame[4..];
        for cut in 0..payload.len() {
            let part = &payload[..cut];
            if command {
                assert!(decode_command(part).is_err(), "prefix {cut} accepted");
            } else {
                assert!(decode_reply(part).is_err(), "prefix {cut} accepted");
            }
        }
        // A byte too many is rejected as well.
        let mut long = payload.to_vec();
        long.push(0);
        assert!(decode_command(&long).is_err() && decode_reply(&long).is_err());
    }
}

#[test]
fn frame_reader_rejects_bad_lengths_without_allocating() {
    let mut huge = Cursor::new(u32::MAX.to_le_bytes().to_vec());
    assert!(matches!(
        read_frame(&mut huge, MAX_REPLY_FRAME),
        Err(FrameError::Protocol(ProtocolError::FrameTooLarge(_)))
    ));
    let over = (MAX_COMMAND_FRAME as u32 + 1).to_le_bytes().to_vec();
    assert!(matches!(
        read_frame(&mut Cursor::new(over), MAX_COMMAND_FRAME),
        Err(FrameError::Protocol(ProtocolError::FrameTooLarge(_)))
    ));
    // Length says 100 bytes, stream ends after 10.
    let mut short = 100u32.to_le_bytes().to_vec();
    short.extend_from_slice(&[0; 10]);
    assert!(matches!(
        read_frame(&mut Cursor::new(short), MAX_COMMAND_FRAME),
        Err(FrameError::Protocol(ProtocolError::Truncated))
    ));
    // Stream ends inside the length prefix.
    assert!(matches!(
        read_frame(&mut Cursor::new(vec![1, 0]), MAX_COMMAND_FRAME),
        Err(FrameError::Protocol(ProtocolError::Truncated))
    ));
    // Too short for version + kind.
    let mut tiny = 2u32.to_le_bytes().to_vec();
    tiny.extend_from_slice(&[1, 0]);
    assert!(read_frame(&mut Cursor::new(tiny), MAX_COMMAND_FRAME).is_err());
    // Random streams never panic.
    let mut rng = Rng(11);
    for _ in 0..iterations(5_000) {
        let bytes: Vec<u8> = (0..rng.below(64)).map(|_| rng.byte()).collect();
        let mut cursor = Cursor::new(bytes);
        while let Ok(Some(payload)) = read_frame(&mut cursor, 4096) {
            check_decoders(&payload);
        }
    }
}

#[test]
fn messages_are_rejected_in_the_wrong_direction() {
    let frame = encode_command(&Command::Metadata { id: 5 }).unwrap();
    assert_eq!(
        decode_reply(&frame[4..]),
        Err(ProtocolError::UnknownKind(CMD_METADATA))
    );
    let frame = encode_reply(&Reply::Done {
        id: 5,
        result: Ok(Payload::Trimmed),
    });
    assert_eq!(
        decode_command(&frame[4..]),
        Err(ProtocolError::UnknownKind(REPLY_DONE))
    );
}

#[test]
fn invalid_field_values_are_rejected() {
    // Scale outside RenderScale's range.
    let mut frame = encode_command(&Command::Render(Render {
        id: 1,
        request: RenderRequest::full_page(
            PageIndex::FIRST,
            PageSize::LETTER,
            Rotation::R0,
            Rotation::R0,
            RenderScale::IDENTITY,
        ),
        format: PixelFormat::Rgba8Premultiplied,
        target: RenderTarget::Slot(0),
    }))
    .unwrap();
    // payload: version(2) kind(1) id(8) page(4) scale(4)
    let scale_at = 4 + 3 + 8 + 4;
    frame[scale_at..scale_at + 4].copy_from_slice(&1000.0f32.to_bits().to_le_bytes());
    assert_eq!(
        decode_command(&frame[4..]),
        Err(ProtocolError::BadValue("scale"))
    );
    frame[scale_at..scale_at + 4].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
    assert_eq!(
        decode_command(&frame[4..]),
        Err(ProtocolError::BadValue("scale"))
    );

    // A handle that cannot be a kernel handle.
    let mut frame = encode_command(&Command::Render(Render {
        id: 1,
        request: RenderRequest::full_page(
            PageIndex::FIRST,
            PageSize::LETTER,
            Rotation::R0,
            Rotation::R0,
            RenderScale::IDENTITY,
        ),
        format: PixelFormat::Rgba8Premultiplied,
        target: RenderTarget::Section(SectionRef {
            handle: 0x40,
            len: 64,
        }),
    }))
    .unwrap();
    let handle_at = frame.len() - 16;
    frame[handle_at..handle_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        decode_command(&frame[4..]),
        Err(ProtocolError::BadValue("handle"))
    );

    // Page sizes must be positive.
    let frame = encode_reply(&Reply::Done {
        id: 1,
        result: Ok(Payload::PageInfo(PageInfo {
            size: PageSize::new(10.0, 10.0),
            rotation: Rotation::R0,
        })),
    });
    let mut bad = frame[4..].to_vec();
    let width_at = 2 + 1 + 8 + 1 + 1;
    bad[width_at..width_at + 4].copy_from_slice(&(-5.0f32).to_bits().to_le_bytes());
    assert_eq!(
        decode_reply(&bad),
        Err(ProtocolError::BadValue("page size"))
    );

    // Slot section size must match count x slot size.
    let init = Command::Init(Init {
        build_id: "b".into(),
        engine: "e".into(),
        workers: 1,
        render_threads: 1,
        slots: Some(SlotSpec {
            section: SectionRef { handle: 8, len: 20 },
            count: 2,
            slot_bytes: 10,
        }),
    });
    let mut frame = encode_command(&init).unwrap();
    let len_at = frame.len() - 12 - 8;
    frame[len_at..len_at + 8].copy_from_slice(&21u64.to_le_bytes());
    assert_eq!(
        decode_command(&frame[4..]),
        Err(ProtocolError::BadValue("slot section size"))
    );
}

#[test]
fn unencodable_commands_fail_instead_of_truncating() {
    let open = Command::Open(Open {
        id: 1,
        document: None,
        path: None,
        password: Some("x".repeat(MAX_PASSWORD + 1)),
        limits: ResourceLimits::default(),
    });
    assert_eq!(
        encode_command(&open),
        Err(ProtocolError::Unencodable("password"))
    );
    let limits = ResourceLimits {
        max_page_dimension_pt: f32::INFINITY,
        ..ResourceLimits::default()
    };
    let open = Command::Open(Open {
        id: 1,
        document: None,
        path: None,
        password: None,
        limits,
    });
    assert!(encode_command(&open).is_err());
}

#[test]
fn engine_output_is_sanitized_not_rejected() {
    let nan = PageRect {
        x0: f32::NAN,
        y0: 1.0,
        x1: f32::INFINITY,
        y1: 2.0,
    };
    let long = "測".repeat(MAX_SPAN_TEXT); // three bytes per char
    let layer = TextLayer {
        page: PageIndex::new(2),
        spans: vec![TextSpan {
            text: long,
            bounds: nan,
            char_bounds: vec![nan],
        }],
    };
    let frame = encode_reply(&Reply::Done {
        id: 9,
        result: Ok(Payload::TextLayer(layer)),
    });
    let Ok(Reply::Done {
        result: Ok(Payload::TextLayer(back)),
        ..
    }) = decode_reply(&frame[4..])
    else {
        panic!("text layer did not decode");
    };
    let span = &back.spans[0];
    assert!(span.text.len() <= MAX_SPAN_TEXT && span.text.chars().all(|c| c == '測'));
    assert_eq!(
        span.bounds,
        PageRect {
            x0: 0.0,
            y0: 1.0,
            x1: 0.0,
            y1: 2.0
        }
    );
    assert_eq!(span.char_bounds[0], span.bounds);

    // Over-long URIs degrade to unsupported links instead of being cut.
    let links = vec![Link {
        bounds: PageRect::default(),
        target: LinkTarget::Uri("u".repeat(MAX_STRING + 1)),
    }];
    let frame = encode_reply(&Reply::Done {
        id: 1,
        result: Ok(Payload::Links(links)),
    });
    let Ok(Reply::Done {
        result: Ok(Payload::Links(back)),
        ..
    }) = decode_reply(&frame[4..])
    else {
        panic!("links did not decode");
    };
    assert_eq!(back[0].target, LinkTarget::Unsupported);

    // Non-finite destination coordinates become "unspecified".
    let dest = Destination {
        page: PageIndex::FIRST,
        view: DestinationView::Xyz {
            left: Some(f32::NAN),
            top: Some(5.0),
            zoom: Some(f32::NEG_INFINITY),
        },
    };
    let items = vec![OutlineItem {
        title: "t".into(),
        destination: Some(dest),
        ..OutlineItem::default()
    }];
    let frame = encode_reply(&Reply::Done {
        id: 1,
        result: Ok(Payload::Outline(items)),
    });
    let Ok(Reply::Done {
        result: Ok(Payload::Outline(back)),
        ..
    }) = decode_reply(&frame[4..])
    else {
        panic!("outline did not decode");
    };
    assert_eq!(
        back[0].destination.as_ref().map(|d| d.view.clone()),
        Some(DestinationView::Xyz {
            left: None,
            top: Some(5.0),
            zoom: None
        })
    );
}

/// Builds a chain `depth` levels deep without recursion.
fn chain(depth: usize) -> Vec<OutlineItem> {
    let mut items = Vec::new();
    for level in (0..depth).rev() {
        items = vec![OutlineItem {
            title: format!("level {level}"),
            children: items,
            ..OutlineItem::default()
        }];
    }
    items
}

fn depth_of(items: &[OutlineItem]) -> usize {
    let mut depth = 0;
    let mut level = items;
    while let Some(first) = level.first() {
        depth += 1;
        level = &first.children;
    }
    depth
}

#[test]
fn outlines_are_capped_in_depth_and_count() {
    // Deeper than the cap: the host keeps the first MAX_OUTLINE_DEPTH levels.
    let frame = encode_reply(&Reply::Done {
        id: 1,
        result: Ok(Payload::Outline(chain(500))),
    });
    let Ok(Reply::Done {
        result: Ok(Payload::Outline(back)),
        ..
    }) = decode_reply(&frame[4..])
    else {
        panic!("outline did not decode");
    };
    assert_eq!(depth_of(&back), MAX_OUTLINE_DEPTH);
    assert_eq!(back[0].title, "level 0");

    // A hostile frame that claims a deeper tree is rejected without
    // recursing: every level announces one child.
    let mut e = Encoder::new(REPLY_DONE);
    e.u64(1);
    e.u8(0);
    e.u8(P_OUTLINE);
    e.u32(1);
    for _ in 0..MAX_OUTLINE_DEPTH + 10 {
        e.str("x");
        e.u8(0);
        e.u8(0);
        e.bool(false);
        e.u32(1);
    }
    let frame = e.finish();
    assert_eq!(decode_reply(&frame[4..]), Err(ProtocolError::TooDeep));

    // Too many items: the host cuts, the decoder enforces.
    let flat: Vec<OutlineItem> = (0..MAX_OUTLINE_ITEMS + 5)
        .map(|_| OutlineItem::default())
        .collect();
    let frame = encode_reply(&Reply::Done {
        id: 1,
        result: Ok(Payload::Outline(flat)),
    });
    let Ok(Reply::Done {
        result: Ok(Payload::Outline(back)),
        ..
    }) = decode_reply(&frame[4..])
    else {
        panic!("outline did not decode");
    };
    assert_eq!(back.len(), MAX_OUTLINE_ITEMS);
}

#[test]
fn oversized_replies_become_errors() {
    let span = TextSpan {
        text: "x".repeat(MAX_SPAN_TEXT),
        ..TextSpan::default()
    };
    let layer = TextLayer {
        page: PageIndex::FIRST,
        spans: vec![span; MAX_REPLY_FRAME / MAX_SPAN_TEXT + 1],
    };
    let frame = encode_reply(&Reply::Done {
        id: 3,
        result: Ok(Payload::TextLayer(layer)),
    });
    assert!(frame.len() < 64);
    assert_eq!(
        decode_reply(&frame[4..]),
        Ok(Reply::Done {
            id: 3,
            result: Err(EngineError::LimitExceeded(LimitKind::ObjectSize)),
        })
    );
}

#[test]
fn capabilities_survive_every_bit_pattern() {
    for bits in 0..=u8::MAX {
        assert_eq!(capability_bits(capabilities_from_bits(bits)), bits);
    }
    let all = EngineCapabilities {
        region_render: true,
        parallel_render: true,
        cooperative_cancel: true,
        text_extraction: true,
        outline: true,
        links: true,
        encryption: true,
        gpu: true,
    };
    assert_eq!(capability_bits(all), 0xFF);
}

#[test]
fn geometry_batches_are_capped() {
    let ok = Command::PageInfos {
        id: 1,
        first: PageIndex::new(10),
        count: MAX_GEOMETRY_BATCH as u32,
    };
    let frame = encode_command(&ok).unwrap();
    assert_eq!(decode_command(&frame[4..]).unwrap(), ok);
    for bad in [0, MAX_GEOMETRY_BATCH as u32 + 1] {
        let cmd = Command::PageInfos {
            id: 1,
            first: PageIndex::new(10),
            count: bad,
        };
        let frame = encode_command(&cmd).unwrap();
        assert_eq!(
            decode_command(&frame[4..]),
            Err(ProtocolError::BadValue("geometry batch size"))
        );
    }
    // A reply with more entries than a batch holds is cut by the host and
    // refused by the parent.
    let pages = vec![
        Ok(PageInfo {
            size: PageSize::LETTER,
            rotation: Rotation::R0,
        });
        MAX_GEOMETRY_BATCH + 3
    ];
    let frame = encode_reply(&Reply::Done {
        id: 2,
        result: Ok(Payload::PageInfos {
            first: PageIndex::FIRST,
            pages,
        }),
    });
    let Ok(Reply::Done {
        result: Ok(Payload::PageInfos { pages, .. }),
        ..
    }) = decode_reply(&frame[4..])
    else {
        panic!("batch did not decode");
    };
    assert_eq!(pages.len(), MAX_GEOMETRY_BATCH);
}
