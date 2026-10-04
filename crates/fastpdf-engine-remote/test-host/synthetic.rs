//! Synthetic PDF engine for the render-host tests: no parsing, just
//! deterministic pixels, text, links and outlines, plus pages that misbehave
//! on purpose (panic, abort, stack overflow, hang, allocation bomb, exit).
//!
//! Shared by the test host binary and the integration tests (which render
//! the same documents in-process to compare).

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fastpdf_engine_api::{
    CancelToken, Destination, DestinationView, DocumentMetadata, DocumentSource,
    EngineCapabilities, EngineDocument, EngineError, EngineInfo, Link, LinkTarget, MemoryPressure,
    OpenOptions, OutlineItem, PageIndex, PageInfo, PageRect, PageSize, PdfEngine, PixmapMut,
    RenderOutcome, RenderRequest, Rgba8, Rotation, TextLayer, TextSpan,
};

pub(crate) const NAME: &str = "synthetic";

/// Engine factory for `run_host`.
pub(crate) fn factory(name: &str) -> Option<Box<dyn PdfEngine>> {
    (name == NAME).then(|| Box::new(SyntheticEngine) as Box<dyn PdfEngine>)
}

/// What a page (or `open`) does when asked to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Behavior {
    Normal,
    /// Renders normally after 300 ms (keeps requests in flight).
    Delay,
    Panic,
    Abort,
    StackOverflow,
    /// Never returns and ignores cancellation.
    Hang,
    /// Polls its cancel token for up to 30 s.
    Slow,
    /// Allocates until the job's memory limit ends the process.
    Bomb,
    /// `std::process::exit(3)`.
    Exit,
    /// The first render (per marker file) aborts; later ones take 1.5 s
    /// and ignore cancellation.
    AbortThenStubborn,
}

impl Behavior {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Delay => "delay",
            Self::Panic => "panic",
            Self::Abort => "abort",
            Self::StackOverflow => "overflow",
            Self::Hang => "hang",
            Self::Slow => "slow",
            Self::Bomb => "bomb",
            Self::Exit => "exit",
            Self::AbortThenStubborn => "abort-then-stubborn",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [
            Self::Normal,
            Self::Delay,
            Self::Panic,
            Self::Abort,
            Self::StackOverflow,
            Self::Hang,
            Self::Slow,
            Self::Bomb,
            Self::Exit,
            Self::AbortThenStubborn,
        ]
        .into_iter()
        .find(|b| b.name() == s)
    }
}

/// A synthetic document file.
#[derive(Debug, Clone, Default)]
pub(crate) struct Spec {
    pub(crate) pages: u32,
    pub(crate) behaviors: Vec<(u32, Behavior)>,
    pub(crate) open: Option<Behavior>,
    pub(crate) password: Option<String>,
    pub(crate) outline_depth: Option<u32>,
    /// Remembers across host processes that `AbortThenStubborn` aborted.
    pub(crate) marker: Option<std::path::PathBuf>,
    /// `page_info` of this page never returns.
    pub(crate) geometry_hang: Option<u32>,
}

impl Spec {
    pub(crate) fn new(pages: u32) -> Self {
        Self {
            pages,
            ..Self::default()
        }
    }

    pub(crate) fn page(mut self, page: u32, behavior: Behavior) -> Self {
        self.behaviors.push((page, behavior));
        self
    }

    pub(crate) fn on_open(mut self, behavior: Behavior) -> Self {
        self.open = Some(behavior);
        self
    }

    pub(crate) fn password(mut self, password: &str) -> Self {
        self.password = Some(password.to_owned());
        self
    }

    pub(crate) fn marker(mut self, path: &std::path::Path) -> Self {
        self.marker = Some(path.to_path_buf());
        self
    }

    pub(crate) fn geometry_hang(mut self, page: u32) -> Self {
        self.geometry_hang = Some(page);
        self
    }

    pub(crate) fn deep_outline(mut self, depth: u32) -> Self {
        self.outline_depth = Some(depth);
        self
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut text = format!("synthetic\npages {}\n", self.pages);
        for (page, b) in &self.behaviors {
            text.push_str(&format!("page {page} {}\n", b.name()));
        }
        if let Some(b) = self.open {
            text.push_str(&format!("open {}\n", b.name()));
        }
        if let Some(pw) = &self.password {
            text.push_str(&format!("password {pw}\n"));
        }
        if let Some(depth) = self.outline_depth {
            text.push_str(&format!("outline {depth}\n"));
        }
        if let Some(page) = self.geometry_hang {
            text.push_str(&format!("geometry-hang {page}\n"));
        }
        if let Some(path) = &self.marker {
            text.push_str(&format!("marker {}\n", path.display()));
        }
        text.into_bytes()
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        if lines.next()? != "synthetic" {
            return None;
        }
        let mut spec = Self::default();
        for line in lines {
            if line == "end" {
                // Padding follows (large-file tests).
                break;
            }
            if let Some(path) = line.strip_prefix("marker ") {
                spec.marker = Some(std::path::PathBuf::from(path));
                continue;
            }
            let words: Vec<&str> = line.split(' ').collect();
            match words.as_slice() {
                ["pages", n] => spec.pages = n.parse().ok()?,
                ["page", p, b] => spec.behaviors.push((p.parse().ok()?, Behavior::parse(b)?)),
                ["open", b] => spec.open = Some(Behavior::parse(b)?),
                ["password", pw] => spec.password = Some((*pw).to_owned()),
                ["outline", d] => spec.outline_depth = Some(d.parse().ok()?),
                ["geometry-hang", p] => spec.geometry_hang = Some(p.parse().ok()?),
                [""] => {}
                _ => return None,
            }
        }
        Some(spec)
    }
}

#[derive(Debug)]
pub(crate) struct SyntheticEngine;

impl PdfEngine for SyntheticEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: NAME,
            version: "1",
            capabilities: EngineCapabilities {
                region_render: true,
                parallel_render: true,
                cooperative_cancel: true,
                text_extraction: true,
                outline: true,
                links: true,
                encryption: true,
                gpu: false,
            },
        }
    }

    fn open(
        &self,
        source: DocumentSource,
        options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        let spec = Spec::parse(source.data.as_slice())
            .ok_or_else(|| EngineError::Malformed("not a synthetic document".into()))?;
        if let Some(b) = spec.open {
            misbehave(b, &CancelToken::new(), &spec)?;
        }
        if let Some(expected) = &spec.password {
            match &options.password {
                None => return Err(EngineError::PasswordRequired),
                Some(given) if given != expected => return Err(EngineError::InvalidPassword),
                Some(_) => {}
            }
        }
        Ok(Box::new(SyntheticDoc {
            behaviors: spec.behaviors.iter().copied().collect(),
            spec,
            memory: AtomicU64::new(1 << 20),
        }))
    }
}

struct SyntheticDoc {
    spec: Spec,
    behaviors: HashMap<u32, Behavior>,
    /// Pretend cache: grows with every render, shrinks on trim.
    memory: AtomicU64,
}

/// Runs a misbehavior; `Ok` means "carry on rendering".
fn misbehave(behavior: Behavior, cancel: &CancelToken, spec: &Spec) -> Result<(), EngineError> {
    match behavior {
        Behavior::Normal => Ok(()),
        Behavior::Delay => {
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        }
        Behavior::Panic => panic!("synthetic panic"),
        Behavior::Abort => std::process::abort(),
        Behavior::StackOverflow => {
            let mut seed = [0u8; 1024];
            let n = recurse(0, &mut seed);
            Err(EngineError::Internal(format!("recursion ended at {n}")))
        }
        Behavior::Hang => loop {
            std::thread::sleep(Duration::from_millis(10));
        },
        Behavior::Slow => {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(30) {
                cancel.check()?;
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(())
        }
        Behavior::Bomb => {
            let mut hoard: Vec<Vec<u8>> = Vec::new();
            loop {
                // Every page of the chunk is written, so it is committed.
                hoard.push(vec![1u8; 32 << 20]);
                std::hint::black_box(&mut hoard);
            }
        }
        Behavior::Exit => std::process::exit(3),
        Behavior::AbortThenStubborn => {
            let marker = spec
                .marker
                .as_ref()
                .ok_or_else(|| EngineError::Internal("no marker file".into()))?;
            if !marker.exists() {
                std::fs::write(marker, b"aborted")
                    .map_err(|e| EngineError::Internal(e.to_string()))?;
                std::process::abort();
            }
            // Deliberately ignores `cancel`.
            std::thread::sleep(Duration::from_millis(1500));
            Ok(())
        }
    }
}

#[inline(never)]
fn recurse(depth: u64, parent: &mut [u8; 1024]) -> u64 {
    let mut frame = [0u8; 1024];
    frame[(depth % 1024) as usize] = parent[0].wrapping_add(1);
    std::hint::black_box(&mut frame);
    if std::hint::black_box(depth) == u64::MAX {
        return 0;
    }
    recurse(depth + 1, &mut frame).wrapping_add(u64::from(frame[0]))
}

fn page_info(page: u32) -> PageInfo {
    PageInfo {
        size: PageSize::new(200.0 + 10.0 * page as f32, 300.0),
        rotation: if page % 4 == 3 {
            Rotation::R90
        } else {
            Rotation::R0
        },
    }
}

/// Deterministic pattern pixel (straight RGBA) at absolute page pixel x, y.
fn pattern(page: u32, x: u32, y: u32, salt: u32) -> [u8; 4] {
    let v = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503) ^ page.wrapping_mul(97) ^ salt;
    [v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]
}

impl EngineDocument for SyntheticDoc {
    fn page_count(&self) -> u32 {
        self.spec.pages
    }

    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        if self.spec.geometry_hang == Some(page.get()) {
            loop {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(page_info(page.get()))
    }

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        Ok(DocumentMetadata {
            title: Some(format!("Synthetic {}", self.spec.pages)),
            producer: Some("fastpdf-engine-remote tests".into()),
            pdf_version: Some("1.7".into()),
            encrypted: self.spec.password.is_some(),
            ..DocumentMetadata::default()
        })
    }

    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        let page = request.page.get();
        if let Some(b) = self.behaviors.get(&page) {
            misbehave(*b, cancel, &self.spec)?;
        }
        let info = page_info(page);
        let full = request
            .scale
            .page_pixels(info.size, info.rotation.then(request.rotation));
        let salt =
            (request.scale.get() * 1000.0) as u32 ^ u32::from(request.rotation.quarter_turns());
        let format = target.format();
        let width = request.region.width as usize;
        let background = request.background.premultiplied_bytes(format);
        let data = target.data_mut();
        for (row, line) in data.chunks_exact_mut(width * 4).enumerate() {
            let y = request.region.y + row as u32;
            for (col, px) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = request.region.x + col as u32;
                let inside = x >= 4 && y >= 4 && x + 4 < full.width && y + 4 < full.height;
                let rgba = if inside {
                    let [r, g, b, a] = pattern(page, x, y, salt);
                    Rgba8::new(r, g, b, a).premultiplied_bytes(format)
                } else {
                    background
                };
                px.copy_from_slice(&rgba);
            }
        }
        self.memory.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(RenderOutcome {
            partial: page % 5 == 4,
        })
    }

    fn text_layer(&self, page: PageIndex, cancel: &CancelToken) -> Result<TextLayer, EngineError> {
        cancel.check()?;
        let label = format!("Page {}", page.display_number());
        let char_bounds = (0..label.chars().count())
            .map(|i| {
                let x = 10.0 + 8.0 * i as f32;
                PageRect::new(x, 10.0, x + 8.0, 30.0)
            })
            .collect();
        Ok(TextLayer {
            page,
            spans: vec![
                TextSpan {
                    text: label,
                    bounds: PageRect::new(10.0, 10.0, 80.0, 30.0),
                    char_bounds,
                },
                TextSpan {
                    text: "測試 ü".into(),
                    bounds: PageRect::new(10.0, 40.0, 80.0, 60.0),
                    char_bounds: Vec::new(),
                },
            ],
        })
    }

    fn outline(&self) -> Result<Vec<OutlineItem>, EngineError> {
        if let Some(depth) = self.spec.outline_depth {
            let mut items = Vec::new();
            for level in (0..depth).rev() {
                items = vec![OutlineItem {
                    title: format!("level {level}"),
                    children: items,
                    ..OutlineItem::default()
                }];
            }
            return Ok(items);
        }
        let dest = |page: u32| Destination {
            page: PageIndex::new(page % self.spec.pages.max(1)),
            view: DestinationView::Xyz {
                left: Some(0.0),
                top: Some(12.5),
                zoom: None,
            },
        };
        Ok((0..3)
            .map(|i| OutlineItem {
                title: format!("Chapter {i}"),
                destination: Some(dest(i)),
                uri: None,
                open: i == 0,
                children: (0..2)
                    .map(|j| OutlineItem {
                        title: format!("Section {i}.{j}"),
                        destination: Some(Destination {
                            page: PageIndex::new(i),
                            view: DestinationView::FitRect(PageRect::new(1.0, 2.0, 3.0, 4.0)),
                        }),
                        uri: (j == 1).then(|| format!("https://example.com/{i}/{j}")),
                        open: false,
                        children: Vec::new(),
                    })
                    .collect(),
            })
            .collect())
    }

    fn links(&self, page: PageIndex) -> Result<Vec<Link>, EngineError> {
        let next = PageIndex::new((page.get() + 1) % self.spec.pages.max(1));
        Ok(vec![
            Link {
                bounds: PageRect::new(10.0, 100.0, 60.0, 120.0),
                target: LinkTarget::Internal(Destination {
                    page: next,
                    view: DestinationView::FitWidth { top: Some(5.0) },
                }),
            },
            Link {
                bounds: PageRect::new(10.0, 130.0, 60.0, 150.0),
                target: LinkTarget::Uri(format!("https://example.com/{}", page.get())),
            },
            Link {
                bounds: PageRect::new(10.0, 160.0, 60.0, 180.0),
                target: LinkTarget::Unsupported,
            },
        ])
    }

    fn trim_memory(&self, pressure: MemoryPressure) {
        match pressure {
            MemoryPressure::Normal => {}
            MemoryPressure::Soft => {
                let half = self.memory.load(Ordering::Relaxed) / 2;
                self.memory.store(half, Ordering::Relaxed);
            }
            MemoryPressure::Hard => self.memory.store(0, Ordering::Relaxed),
        }
    }

    fn memory_usage(&self) -> Option<u64> {
        Some(self.memory.load(Ordering::Relaxed))
    }
}
