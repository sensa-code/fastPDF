use crate::{
    CancelToken, DocumentMetadata, DocumentSource, EngineError, Link, OutlineItem, PageIndex,
    PageSize, PixmapMut, RenderOutcome, RenderRequest, ResourceLimits, Rotation, TextLayer,
};

/// Static description of an engine adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInfo {
    /// Short identifier used on the command line (`--engine hayro`).
    pub name: &'static str,
    /// Version of the underlying engine library.
    pub version: &'static str,
    pub capabilities: EngineCapabilities,
}

/// What an adapter can do natively. The scheduler and UI adapt to these
/// instead of assuming every engine behaves the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineCapabilities {
    /// Renders a sub-rectangle without rasterizing the whole page. Without
    /// it, tiles are still correct but cost a full-page rasterization each.
    pub region_render: bool,
    /// One document can render on several threads at once.
    pub parallel_render: bool,
    /// Long renders poll the [`CancelToken`] and stop early.
    pub cooperative_cancel: bool,
    pub text_extraction: bool,
    pub outline: bool,
    pub links: bool,
    pub encryption: bool,
    /// Renders on the GPU.
    pub gpu: bool,
}

/// Options for [`PdfEngine::open`].
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    pub password: Option<String>,
    pub limits: ResourceLimits,
}

/// Geometry of one page, as stored in the PDF.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageInfo {
    /// Visible box (CropBox, falling back to MediaBox) before rotation.
    pub size: PageSize,
    /// Intrinsic `/Rotate` of the page.
    pub rotation: Rotation,
}

impl PageInfo {
    /// Size as displayed, after the intrinsic and the user's rotation.
    pub fn display_size(&self, user_rotation: Rotation) -> PageSize {
        self.size.rotated(self.rotation.then(user_rotation))
    }
}

/// Memory pressure level broadcast by the memory budget manager (spec §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum MemoryPressure {
    #[default]
    Normal,
    /// Evict aggressively.
    Soft,
    /// Drop everything that is not needed for the current view.
    Hard,
}

/// A PDF engine: a factory for documents.
///
/// The trait is object-safe on purpose so engines can be chosen at runtime
/// (command line, per-document fallback) as well as at compile time
/// (cargo features). See ADR 0002.
pub trait PdfEngine: Send + Sync {
    fn info(&self) -> EngineInfo;

    /// Opens a document. Implementations must do the minimum work needed to
    /// answer `page_count` (spec §11): no full parse, no text extraction, no
    /// rendering.
    fn open(
        &self,
        source: DocumentSource,
        options: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError>;
}

/// An opened document.
///
/// Implementations must be shareable across render worker threads. Engines
/// whose documents are not thread-safe serialize internally and report
/// `parallel_render: false`.
pub trait EngineDocument: Send + Sync {
    fn page_count(&self) -> u32;

    /// Size and intrinsic rotation of one page; must be cheap after the
    /// first call for that page.
    fn page_info(&self, page: PageIndex) -> Result<PageInfo, EngineError>;

    fn metadata(&self) -> Result<DocumentMetadata, EngineError> {
        Ok(DocumentMetadata::default())
    }

    /// Renders `request.region` of a page into `target`, whose size equals
    /// the region size. The engine paints `request.background` first.
    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        cancel: &CancelToken,
    ) -> Result<RenderOutcome, EngineError>;

    fn text_layer(&self, page: PageIndex, cancel: &CancelToken) -> Result<TextLayer, EngineError> {
        let _ = (page, cancel);
        Err(EngineError::Unsupported("text extraction".into()))
    }

    fn outline(&self) -> Result<Vec<OutlineItem>, EngineError> {
        Err(EngineError::Unsupported("outline".into()))
    }

    fn links(&self, page: PageIndex) -> Result<Vec<Link>, EngineError> {
        let _ = page;
        Err(EngineError::Unsupported("links".into()))
    }

    /// Lets the engine drop internal caches (fonts, decoded images) when the
    /// memory budget manager reports pressure.
    fn trim_memory(&self, pressure: MemoryPressure) {
        let _ = pressure;
    }

    /// Approximate bytes held by the engine's own caches for this document
    /// (decoded streams, fonts, images, render blocks), for the memory
    /// overlay and the budget manager. `None` when the engine cannot tell.
    fn memory_usage(&self) -> Option<u64> {
        None
    }

    /// For engines that run the document in a separate process (the render
    /// host, ADR 0008): that process and its supervision state. `None` for
    /// in-process engines. Must not block.
    fn host_status(&self) -> Option<HostStatus> {
        None
    }

    /// Renders a caller should keep in flight for each page it wants
    /// rendered at a time. 1 (the default) for engines that render on the
    /// calling thread or a pool of their own. Documents whose renders travel
    /// to another process (ADR 0008) answer 2: while one reply is on its way
    /// back, the next request already waits there, so the engine never idles
    /// on a round trip. The number of pages rendered at once stays the
    /// engine's business (the render host renders as many as the caller has
    /// workers, not as many as it has requests in flight).
    fn render_queue_depth(&self) -> usize {
        1
    }
}

/// A document's render host (ADR 0008), for diagnostics, the memory budget
/// (the host's memory is not in FastPDF's own process) and the UI's
/// document-level notices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostStatus {
    /// Process id of the running host; `None` while none runs.
    pub pid: Option<u32>,
    /// Committed private bytes of the host process.
    pub private_bytes: Option<u64>,
    /// Hosts started after the first one.
    pub restarts: u32,
    /// Host crashes, deadline kills included.
    pub crashes: u32,
    pub last_crash: Option<String>,
    /// Pages that brought hosts down repeatedly and are not rendered any
    /// more.
    pub failed_pages: Vec<PageIndex>,
    /// True once the document stopped restarting hosts (crash storm): it
    /// renders nothing more until it is reopened.
    pub restarts_disabled: bool,
}
