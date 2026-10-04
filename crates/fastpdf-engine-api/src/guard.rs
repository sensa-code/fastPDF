//! Fault isolation around engine calls (spec §24, §25; ADR 0002).
//!
//! PDF engines parse hostile input. Every call into an engine goes through
//! [`GuardedDocument`], which validates requests before they reach the
//! engine, enforces generic resource limits, and converts panics into
//! [`EngineError::Panicked`] so one broken page cannot crash the reader.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{
    CancelToken, ColorMode, DocumentMetadata, DocumentSource, EngineDocument, EngineError,
    EngineInfo, HostStatus, Link, MemoryPressure, OpenOptions, OutlineItem, PageIndex, PageInfo,
    PdfEngine, PixmapMut, RenderOutcome, RenderRequest, ResourceLimits, TextLayer,
};

/// Upper bound for [`EngineDocument::render_queue_depth`]: callers start one
/// thread per request in flight.
const MAX_RENDER_QUEUE_DEPTH: usize = 4;

/// Panics after which a document is considered degraded; the reader core
/// should reopen it rather than keep using possibly inconsistent state.
const DEGRADED_AFTER_PANICS: u32 = 3;

/// Opens a document through `engine` with panic isolation and limit checks.
pub fn open_guarded(
    engine: &dyn PdfEngine,
    source: DocumentSource,
    options: &OpenOptions,
) -> Result<GuardedDocument, EngineError> {
    let info = engine.info();
    let inner = contain(|| engine.open(source, options))?;
    let page_count = contain(|| Ok(inner.page_count()))?;
    options.limits.check_page_count(page_count)?;
    if page_count == 0 {
        return Err(EngineError::Malformed("document has no pages".into()));
    }
    Ok(GuardedDocument {
        inner,
        info,
        limits: options.limits.clone(),
        page_count,
        page_info: RwLock::new(HashMap::new()),
        panics: AtomicU32::new(0),
    })
}

/// An [`EngineDocument`] wrapper that validates, limits and contains panics.
pub struct GuardedDocument {
    inner: Box<dyn EngineDocument>,
    info: EngineInfo,
    limits: ResourceLimits,
    page_count: u32,
    page_info: RwLock<HashMap<u32, PageInfo>>,
    panics: AtomicU32,
}

impl fmt::Debug for GuardedDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedDocument")
            .field("engine", &self.info.name)
            .field("page_count", &self.page_count)
            .field("panics", &self.panic_count())
            .finish_non_exhaustive()
    }
}

impl GuardedDocument {
    pub fn engine(&self) -> &EngineInfo {
        &self.info
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    pub fn panic_count(&self) -> u32 {
        self.panics.load(Ordering::Relaxed)
    }

    /// True once the engine panicked repeatedly for this document.
    pub fn is_degraded(&self) -> bool {
        self.panic_count() >= DEGRADED_AFTER_PANICS
    }

    fn check_page(&self, page: PageIndex) -> Result<(), EngineError> {
        if page.get() < self.page_count {
            Ok(())
        } else {
            Err(EngineError::PageOutOfRange {
                page,
                page_count: self.page_count,
            })
        }
    }

    fn call<T>(&self, f: impl FnOnce() -> Result<T, EngineError>) -> Result<T, EngineError> {
        let result = contain(f);
        if matches!(result, Err(EngineError::Panicked(_))) {
            self.panics.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
}

impl EngineDocument for GuardedDocument {
    fn page_count(&self) -> u32 {
        self.page_count
    }

    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError> {
        self.check_page(page)?;
        if let Some(info) = read_lock(&self.page_info).get(&page.get()) {
            return Ok(*info);
        }
        // Only answers are cached: errors, transient ones in particular
        // (`EngineError::is_transient`), are asked again next time.
        let info = self.call(|| self.inner.page_info(page))?;
        self.limits.check_page_size(info.size)?;
        write_lock(&self.page_info).insert(page.get(), info);
        Ok(info)
    }

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        self.call(|| self.inner.metadata())
    }

    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        cancel.check()?;
        let info = self.page_info(request.page)?;
        let bounds = request
            .scale
            .page_pixels(info.size, info.rotation.then(request.rotation))
            .bounds();
        if request.region.is_empty() || !request.region.is_within(bounds) {
            return Err(EngineError::InvalidRequest(format!(
                "region {:?} outside page bounds {:?}",
                request.region, bounds
            )));
        }
        if target.size() != request.region.size() {
            return Err(EngineError::InvalidRequest(
                "target size does not match region size".into(),
            ));
        }
        self.limits.check_bitmap(target.size())?;
        // Engines always render normal colors; derived color modes are
        // applied here, uniformly for every engine.
        let normal;
        let engine_request = if request.color_mode == ColorMode::Normal {
            request
        } else {
            normal = RenderRequest {
                color_mode: ColorMode::Normal,
                ..request.clone()
            };
            &normal
        };
        let outcome = self.call(|| self.inner.render(engine_request, target, cancel))?;
        apply_color_mode(request.color_mode, target);
        // A result that arrives after cancellation is stale; drop it.
        cancel.check()?;
        Ok(outcome)
    }

    fn text_layer(&self, page: PageIndex, cancel: &CancelToken) -> Result<TextLayer, EngineError> {
        self.check_page(page)?;
        cancel.check()?;
        self.call(|| self.inner.text_layer(page, cancel))
    }

    fn outline(&self) -> Result<Vec<OutlineItem>, EngineError> {
        self.call(|| self.inner.outline())
    }

    fn links(&self, page: PageIndex) -> Result<Vec<Link>, EngineError> {
        self.check_page(page)?;
        self.call(|| self.inner.links(page))
    }

    fn memory_usage(&self) -> Option<u64> {
        self.call(|| Ok(self.inner.memory_usage())).ok().flatten()
    }

    fn trim_memory(&self, pressure: MemoryPressure) {
        let _ = self.call(|| {
            self.inner.trim_memory(pressure);
            Ok(())
        });
    }

    fn host_status(&self) -> Option<HostStatus> {
        self.call(|| Ok(self.inner.host_status())).ok().flatten()
    }

    fn render_queue_depth(&self) -> usize {
        self.call(|| Ok(self.inner.render_queue_depth()))
            .unwrap_or(1)
            .clamp(1, MAX_RENDER_QUEUE_DEPTH)
    }
}

/// Post-processes a normally rendered bitmap into `mode`.
fn apply_color_mode(mode: ColorMode, target: &mut PixmapMut<'_>) {
    if mode == ColorMode::Inverted {
        // Premultiplied inversion: straight 255 - c becomes a - c, in
        // either channel order.
        let (pixels, _) = target.data_mut().as_chunks_mut::<4>();
        for px in pixels {
            let a = px[3];
            *px = [a - px[0].min(a), a - px[1].min(a), a - px[2].min(a), a];
        }
    }
}

fn contain<T>(f: impl FnOnce() -> Result<T, EngineError>) -> Result<T, EngineError> {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|payload| Err(EngineError::Panicked(panic_message(payload.as_ref()))))
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

// Lock poisoning only means another thread panicked while holding the lock;
// the page-info map stays valid, so recover the guard instead of panicking.
fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EngineCapabilities, PageSize, PixelFormat, PixelRect, PixelSize, Pixmap, RenderScale,
        Rgba8, Rotation, SharedBytes,
    };

    /// Test engine: every page is Letter-sized; page 1 panics on render.
    struct FakeEngine {
        pages: u32,
    }

    struct FakeDoc {
        pages: u32,
    }

    impl PdfEngine for FakeEngine {
        fn info(&self) -> EngineInfo {
            EngineInfo {
                name: "fake",
                version: "0",
                capabilities: EngineCapabilities::default(),
            }
        }

        fn open(
            &self,
            source: DocumentSource,
            _options: &OpenOptions,
        ) -> Result<Box<dyn EngineDocument>, EngineError> {
            if source.data.as_slice() == b"boom" {
                panic!("parser exploded");
            }
            Ok(Box::new(FakeDoc { pages: self.pages }))
        }
    }

    impl EngineDocument for FakeDoc {
        fn page_count(&self) -> u32 {
            self.pages
        }

        fn page_info(&self, _page: PageIndex) -> Result<PageInfo, EngineError> {
            Ok(PageInfo {
                size: PageSize::LETTER,
                rotation: Rotation::R90,
            })
        }

        fn render(
            &self,
            request: &RenderRequest,
            target: &mut PixmapMut<'_>,
            _cancel: &CancelToken,
        ) -> Result<RenderOutcome, EngineError> {
            if request.page == PageIndex::new(1) {
                panic!("bad content stream");
            }
            target.fill(request.background);
            Ok(RenderOutcome::default())
        }
    }

    fn open(pages: u32, bytes: &[u8]) -> Result<GuardedDocument, EngineError> {
        open_guarded(
            &FakeEngine { pages },
            DocumentSource::from_bytes(SharedBytes::from_vec(bytes.to_vec())),
            &OpenOptions::default(),
        )
    }

    fn request(doc: &GuardedDocument, page: u32) -> RenderRequest {
        let info = doc.page_info(PageIndex::new(page)).unwrap();
        RenderRequest::full_page(
            PageIndex::new(page),
            info.size,
            info.rotation,
            Rotation::R0,
            RenderScale::new(0.25).unwrap(),
        )
    }

    #[test]
    fn panics_during_open_are_contained() {
        assert!(
            matches!(open(3, b"boom"), Err(EngineError::Panicked(m)) if m.contains("exploded"))
        );
    }

    #[test]
    fn empty_documents_are_rejected() {
        assert!(matches!(open(0, b"ok"), Err(EngineError::Malformed(_))));
    }

    #[test]
    fn render_panics_are_contained_and_counted() {
        let doc = open(3, b"ok").unwrap();
        let req = request(&doc, 1);
        let mut pm = Pixmap::new(req.region.size(), PixelFormat::default(), doc.limits()).unwrap();
        let cancel = CancelToken::new();
        for _ in 0..3 {
            let r = doc.render(&req, &mut pm.as_mut(), &cancel);
            assert!(matches!(r, Err(EngineError::Panicked(_))));
        }
        assert!(doc.is_degraded());
        // Other pages keep working.
        let ok = request(&doc, 0);
        let mut pm = Pixmap::new(ok.region.size(), PixelFormat::default(), doc.limits()).unwrap();
        assert!(doc.render(&ok, &mut pm.as_mut(), &cancel).is_ok());
        assert_eq!(
            &pm.data()[..4],
            &Rgba8::WHITE.premultiplied_bytes(PixelFormat::default())
        );
    }

    #[test]
    fn requests_are_validated_before_reaching_the_engine() {
        let doc = open(3, b"ok").unwrap();
        let cancel = CancelToken::new();
        let req = request(&doc, 0);
        // Rotated Letter at 0.25 => 198 x 153 pixels.
        assert_eq!(req.region, PixelRect::new(0, 0, 198, 153));

        let outside = req.clone().with_region(PixelRect::new(190, 0, 16, 16));
        let mut pm =
            Pixmap::new(outside.region.size(), PixelFormat::default(), doc.limits()).unwrap();
        assert!(matches!(
            doc.render(&outside, &mut pm.as_mut(), &cancel),
            Err(EngineError::InvalidRequest(_))
        ));

        let mut wrong =
            Pixmap::new(PixelSize::new(8, 8), PixelFormat::default(), doc.limits()).unwrap();
        assert!(matches!(
            doc.render(&req, &mut wrong.as_mut(), &cancel),
            Err(EngineError::InvalidRequest(_))
        ));

        assert!(matches!(
            doc.page_info(PageIndex::new(3)),
            Err(EngineError::PageOutOfRange { .. })
        ));
    }

    #[test]
    fn inverted_mode_turns_paper_black_for_any_engine() {
        let doc = open(3, b"ok").unwrap();
        let mut req = request(&doc, 0);
        req.color_mode = ColorMode::Inverted;
        let mut pm = Pixmap::new(req.region.size(), PixelFormat::default(), doc.limits()).unwrap();
        doc.render(&req, &mut pm.as_mut(), &CancelToken::new())
            .unwrap();
        assert_eq!(&pm.data()[..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn transient_page_info_errors_are_not_cached() {
        use std::sync::atomic::AtomicU32;

        /// Unavailable the first time, then a Letter page.
        struct Flaky {
            calls: AtomicU32,
        }
        impl EngineDocument for Flaky {
            fn page_count(&self) -> u32 {
                1
            }
            fn page_info(&self, _page: PageIndex) -> Result<PageInfo, EngineError> {
                if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    Err(EngineError::Unavailable("host restarting".into()))
                } else {
                    Ok(PageInfo {
                        size: PageSize::LETTER,
                        rotation: Rotation::R0,
                    })
                }
            }
            fn render(
                &self,
                _request: &RenderRequest,
                _target: &mut PixmapMut<'_>,
                _cancel: &CancelToken,
            ) -> Result<RenderOutcome, EngineError> {
                Ok(RenderOutcome::default())
            }
        }
        struct FlakyEngine;
        impl PdfEngine for FlakyEngine {
            fn info(&self) -> EngineInfo {
                EngineInfo {
                    name: "flaky",
                    version: "0",
                    capabilities: EngineCapabilities::default(),
                }
            }
            fn open(
                &self,
                _source: DocumentSource,
                _options: &OpenOptions,
            ) -> Result<Box<dyn EngineDocument>, EngineError> {
                Ok(Box::new(Flaky {
                    calls: AtomicU32::new(0),
                }))
            }
        }
        let doc = open_guarded(
            &FlakyEngine,
            DocumentSource::from_bytes(SharedBytes::from_vec(b"x".to_vec())),
            &OpenOptions::default(),
        )
        .unwrap();
        let first = doc.page_info(PageIndex::FIRST);
        assert!(first.as_ref().is_err_and(EngineError::is_transient));
        assert_eq!(
            doc.page_info(PageIndex::FIRST).map(|i| i.size),
            Ok(PageSize::LETTER)
        );
        assert_eq!(doc.host_status(), None);
    }

    #[test]
    fn cancelled_requests_never_render() {
        let doc = open(3, b"ok").unwrap();
        let req = request(&doc, 1); // would panic if it reached the engine
        let mut pm = Pixmap::new(req.region.size(), PixelFormat::default(), doc.limits()).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(
            doc.render(&req, &mut pm.as_mut(), &cancel),
            Err(EngineError::Cancelled)
        );
        assert_eq!(doc.panic_count(), 0);
    }
}
