//! Links on the pages: the pointer over a link shows a hand and where the
//! link goes (`crate::link_prompt` draws the chip), and a click follows it.
//! A click is a press and a release on the same link within
//! [`CLICK_SLOP`](fastpdf_core::links::CLICK_SLOP); dragging from a link
//! still selects text.
//!
//! Nothing happens at open: a page's links are read (off the UI thread,
//! through the guarded document, so with the render host by default) the
//! first time the pointer moves over the page, at most two pages at a time,
//! and kept in the budgeted [`LinkCache`]. While the view scrolls or zooms
//! under a still pointer, the hover is recomputed in prepaint from the
//! cache only; the pointer's next move reads a page that is not there yet.
//!
//! Internal links jump through `ReaderView::jump_to_destination` (Back
//! returns). External addresses are checked with
//! `fastpdf_shell::check_external_uri` when the hover changes; everything
//! else (launching programs, other files, scripts) is never followed.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fastpdf_core::links::{LinkAction, LinkCache, NotFollowed, PageLinks, is_click};
use fastpdf_engine_api::{Destination, EngineError, Link, PageId, PageIndex};
use fastpdf_shell::{ExternalUri, UriKind, UriRejection, check_external_uri};
use gpui::{Bounds, Context, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point};

use crate::reader::ReaderView;

/// Link reads running at once; one more waits (the newest).
const MAX_IN_FLIGHT: usize = 2;
/// Pages whose links could not be read, remembered per document.
const MAX_FAILED: usize = 1024;
/// Visible pages whose links the development overlay reads.
const MAX_PREFETCH: u32 = 16;
/// After a transient failure (the render host restarting), the page is
/// not asked for again before this.
const RETRY_AFTER: Duration = Duration::from_secs(1);

/// The links of the open document, the hover and a press on a link.
pub(crate) struct LinkLayer {
    pub cache: LinkCache,
    loading: HashSet<PageId>,
    failed: HashSet<PageId>,
    queued: Option<PageId>,
    /// The last page that failed transiently, and when to ask again.
    retry: Option<(PageId, Instant)>,
    /// The pointer over the document area (window coordinates).
    pointer: Option<Point<Pixels>>,
    /// The view the hover was last computed in.
    hover_view: Option<ViewKey>,
    /// The page looked at last, without the cache's lock.
    memo: Option<(PageId, Arc<PageLinks>)>,
    pub hover: Option<LinkHover>,
    press: Option<LinkPress>,
}

impl std::fmt::Debug for LinkLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkLayer")
            .field("cache", &self.cache)
            .field("loading", &self.loading.len())
            .finish_non_exhaustive()
    }
}

/// The link under the pointer.
pub(crate) struct LinkHover {
    pub page: PageIndex,
    pub index: usize,
    links: Arc<PageLinks>,
    pub target: HoverTarget,
}

/// What following a link would do; worked out once per hover change.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HoverTarget {
    Page(Destination),
    External(ExternalUri),
    Blocked(LinkBlock),
}

/// Why a link is not followed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LinkBlock {
    /// Launch, another file, a script, or a destination that does not
    /// resolve.
    Action,
    MissingPage,
    /// The address is refused (`fastpdf_shell::check_external_uri`).
    Uri(UriRejection),
}

impl LinkBlock {
    /// English, for logs.
    pub(crate) fn log_reason(&self) -> String {
        match self {
            Self::Action => "an action FastPDF does not run".into(),
            Self::MissingPage => "the target page does not exist".into(),
            Self::Uri(UriRejection::Scheme(s)) => format!("scheme {s}"),
            Self::Uri(why) => why.to_string(),
        }
    }
}

impl HoverTarget {
    fn of(action: &LinkAction) -> Self {
        match action {
            LinkAction::GoTo(d) => Self::Page(d.clone()),
            LinkAction::Uri(uri) => match check_external_uri(uri) {
                Ok(uri) => Self::External(uri),
                Err(why) => Self::Blocked(LinkBlock::Uri(why)),
            },
            LinkAction::NotFollowed(NotFollowed::Action) => Self::Blocked(LinkBlock::Action),
            LinkAction::NotFollowed(NotFollowed::MissingPage) => {
                Self::Blocked(LinkBlock::MissingPage)
            }
            LinkAction::NotFollowed(NotFollowed::UriTooLong) => {
                Self::Blocked(LinkBlock::Uri(UriRejection::TooLong))
            }
        }
    }

    /// English, for logs: page numbers, and only the scheme and host of an
    /// address (full addresses are logged at debug level only).
    pub(crate) fn log_text(&self) -> String {
        match self {
            Self::Page(d) => format!("page {}", d.page.display_number()),
            Self::External(uri) => match (uri.kind(), uri.host()) {
                (UriKind::Web, Some(host)) => format!("{} link to {host}", uri.scheme()),
                _ => format!("{} link", uri.scheme()),
            },
            Self::Blocked(why) => format!("not followed ({})", why.log_reason()),
        }
    }
}

/// A press on a link, waiting for its release.
#[derive(Debug, Clone, Copy)]
struct LinkPress {
    page: PageIndex,
    index: usize,
    origin: Point<Pixels>,
}

/// What the hover depends on besides the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ViewKey {
    bounds: Option<Bounds<Pixels>>,
    /// Page mode turns pages without changing the scroll position.
    page: PageIndex,
    scroll: (f64, f64),
    zoom: f32,
    rotation: fastpdf_engine_api::Rotation,
    layout: fastpdf_core::LayoutMode,
    size: (f64, f64),
}

impl LinkLayer {
    pub(crate) fn new() -> Self {
        Self {
            cache: LinkCache::new(LinkCache::DEFAULT_BUDGET),
            loading: HashSet::new(),
            failed: HashSet::new(),
            queued: None,
            retry: None,
            pointer: None,
            hover_view: None,
            memo: None,
            hover: None,
            press: None,
        }
    }

    /// Everything but the cache (a document closed).
    fn clear(&mut self) {
        self.loading.clear();
        self.failed.clear();
        self.queued = None;
        self.retry = None;
        self.pointer = None;
        self.memo = None;
        self.forget_hover();
    }

    /// The view moved under the pointer (a jump): the hover is stale.
    pub(crate) fn forget_hover(&mut self) {
        self.hover = None;
        self.press = None;
        self.hover_view = None;
    }
}

/// A link under a point: its page, its index and the page's links.
type Hit = (PageIndex, usize, Arc<PageLinks>);

impl ReaderView {
    /// The pointer moved over the document area.
    pub(crate) fn link_hover(&mut self, position: Point<Pixels>, cx: &mut Context<'_, Self>) {
        self.links.pointer = Some(position);
        self.links.hover_view = self.view_key();
        let hit = if self.links_suspended() {
            None
        } else {
            self.link_at(position, true, cx)
        };
        if self.replace_hover(hit) {
            cx.notify();
        }
    }

    /// The pointer left the document area.
    pub(crate) fn clear_link_hover(&mut self, cx: &mut Context<'_, Self>) {
        self.links.pointer = None;
        if self.links.hover.take().is_some() {
            cx.notify();
        }
    }

    /// Canvas prepaint: the view moved under a still pointer (scrolling,
    /// zooming, pages laid out): the hover again, from the cache only.
    /// True when it changed.
    pub(crate) fn refresh_link_hover(&mut self) -> bool {
        let Some(pointer) = self.links.pointer else {
            return false;
        };
        let key = self.view_key();
        if key == self.links.hover_view {
            return false;
        }
        self.links.hover_view = key;
        let hit = if self.links_suspended() {
            None
        } else {
            self.link_at_cached(pointer)
        };
        self.replace_hover(hit)
    }

    /// A press on the document area, before the text selection starts: a
    /// press on a link may become a click on it.
    pub(crate) fn link_press(&mut self, event: &MouseDownEvent) {
        self.links.press = None;
        // The second press of a double-click lands where the first click
        // took the view: it must not follow another link.
        if event.button != MouseButton::Left || event.click_count > 1 || self.links_suspended() {
            return;
        }
        if let Some((page, index, _)) = self.link_at_cached(event.position) {
            self.links.press = Some(LinkPress {
                page,
                index,
                origin: event.position,
            });
        }
    }

    /// The selection drag moved: once it went farther than a click may, the
    /// press is a drag, wherever it ends.
    pub(crate) fn link_drag(&mut self, position: Point<Pixels>) {
        if let Some(press) = self.links.press {
            let moved = position - press.origin;
            if !is_click(f32::from(moved.x), f32::from(moved.y)) {
                self.links.press = None;
            }
        }
    }

    /// The release that ends a press (and the text selection): a click on
    /// the pressed link follows it. True when it did.
    pub(crate) fn link_release(
        &mut self,
        event: &MouseUpEvent,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(press) = self.links.press.take() else {
            return false;
        };
        let moved = event.position - press.origin;
        if !is_click(f32::from(moved.x), f32::from(moved.y)) {
            return false;
        }
        let Some((page, index, links)) = self.link_at_cached(event.position) else {
            return false;
        };
        if (page, index) != (press.page, press.index) {
            return false;
        }
        let Some(link) = links.get(index) else {
            return false;
        };
        self.follow_link(HoverTarget::of(&link.action), cx);
        true
    }

    fn follow_link(&mut self, target: HoverTarget, cx: &mut Context<'_, Self>) {
        match target {
            HoverTarget::Page(destination) => {
                log::info!("link followed: page {}", destination.page.display_number());
                self.jump_to_destination(&destination);
            }
            HoverTarget::External(uri) => {
                log::info!(
                    "link not opened yet: {}",
                    HoverTarget::External(uri).log_text()
                );
            }
            HoverTarget::Blocked(why) => {
                log::info!("link not followed: {}", why.log_reason());
            }
        }
        cx.notify();
    }

    /// The development overlay outlines the links of the visible pages, so
    /// they are read while it shows.
    pub(crate) fn prefetch_visible_links(&mut self, cx: &mut Context<'_, Self>) {
        let Some(session) = self.session() else {
            return;
        };
        let visible = session.viewport().visible_rect();
        let pages = session
            .layout()
            .pages_intersecting(visible.y, visible.y + visible.height);
        let first = pages.start;
        let end = pages.end.min(first.saturating_add(MAX_PREFETCH));
        for page in first..end {
            self.want_links(PageIndex::new(page), cx);
        }
    }

    /// The links of `page` of the open document, if read already (the
    /// development overlay's outlines).
    pub(crate) fn cached_links(&self, page: PageIndex) -> Option<Arc<PageLinks>> {
        let key = PageId::new(self.session()?.id(), page);
        match &self.links.memo {
            Some((memo, links)) if *memo == key => Some(Arc::clone(links)),
            _ => self.links.cache.get(key),
        }
    }

    /// The links of the open document's `page` are being read or known.
    pub(crate) fn links_loaded_for(&self, page: PageIndex) -> bool {
        self.session().is_some_and(|s| {
            let key = PageId::new(s.id(), page);
            self.links.cache.contains(key) || self.links.failed.contains(&key)
        })
    }

    /// The document closes: its links go (the cache is shared by all
    /// documents of the window).
    pub(crate) fn reset_links(&mut self) {
        if let Some(document) = self.session().map(|s| s.id()) {
            self.links.cache.remove_document(document);
        }
        self.links.clear();
    }

    /// Hover and clicks on links wait while another tool owns the pointer
    /// (text edits, stamps, a selection or scroll-bar drag) or a prompt
    /// shows.
    fn links_suspended(&self) -> bool {
        self.text_edits.open
            || self.stamps.armed.is_some()
            || self.selection.dragging
            || self.scrollbars.drag.is_some()
            || self.scrollbars.hover.is_some()
            || self.active_prompt().is_some()
    }

    fn view_key(&self) -> Option<ViewKey> {
        let session = self.session()?;
        let vp = session.viewport();
        Some(ViewKey {
            bounds: self.viewport_bounds,
            page: session.current_page(),
            scroll: (vp.scroll_x, vp.scroll_y),
            zoom: vp.zoom.get(),
            rotation: session.rotation(),
            layout: session.layout_mode(),
            size: session.layout().total_size(),
        })
    }

    /// The link under a window position; reads the page when `fetch` and
    /// its links are not known yet.
    fn link_at(
        &mut self,
        position: Point<Pixels>,
        fetch: bool,
        cx: &mut Context<'_, Self>,
    ) -> Option<Hit> {
        let point = self.page_point(position)?;
        let key = PageId::new(self.session()?.id(), point.page);
        let Some(links) = self.page_links(key) else {
            if fetch {
                self.want_links(point.page, cx);
            }
            return None;
        };
        let index = links.hit(point.x, point.y)?;
        Some((point.page, index, links))
    }

    fn link_at_cached(&mut self, position: Point<Pixels>) -> Option<Hit> {
        let point = self.page_point(position)?;
        let key = PageId::new(self.session()?.id(), point.page);
        let links = self.page_links(key)?;
        let index = links.hit(point.x, point.y)?;
        Some((point.page, index, links))
    }

    /// The links of a page from the memo or the cache.
    fn page_links(&mut self, key: PageId) -> Option<Arc<PageLinks>> {
        if let Some((memo, links)) = &self.links.memo
            && *memo == key
        {
            return Some(Arc::clone(links));
        }
        let links = self.links.cache.get(key)?;
        self.links.memo = Some((key, Arc::clone(&links)));
        Some(links)
    }

    /// Sets the hover; true when it changed.
    fn replace_hover(&mut self, hit: Option<Hit>) -> bool {
        let same = match (&self.links.hover, &hit) {
            (None, None) => true,
            (Some(h), Some((page, index, links))) => {
                h.page == *page && h.index == *index && Arc::ptr_eq(&h.links, links)
            }
            _ => false,
        };
        if same {
            return false;
        }
        self.links.hover = hit.and_then(|(page, index, links)| {
            let target = HoverTarget::of(&links.get(index)?.action);
            log::debug!(
                "link hover: page {} #{index} -> {}",
                page.display_number(),
                target.log_text()
            );
            Some(LinkHover {
                page,
                index,
                links,
                target,
            })
        });
        true
    }

    /// Reads the links of `page` in the background unless they are known,
    /// being read or failed; with two reads running, `page` waits (and
    /// replaces any page that waited before it).
    pub(crate) fn want_links(&mut self, page: PageIndex, cx: &mut Context<'_, Self>) {
        let Some(document) = self.session().map(|s| s.id()) else {
            return;
        };
        let key = PageId::new(document, page);
        let layer = &mut self.links;
        if layer.loading.contains(&key)
            || layer.failed.contains(&key)
            || layer.memo.as_ref().is_some_and(|(k, _)| *k == key)
            || layer.cache.contains(key)
            || layer
                .retry
                .is_some_and(|(k, at)| k == key && Instant::now() < at)
        {
            return;
        }
        if layer.loading.len() >= MAX_IN_FLIGHT {
            layer.queued = Some(key);
            return;
        }
        layer.loading.insert(key);
        let Some(doc) = self.session().map(|s| Arc::clone(s.document())) else {
            return;
        };
        let work = cx.background_executor().spawn(async move {
            use fastpdf_engine_api::EngineDocument;
            doc.links(page)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| this.links_loaded(key, result, cx));
        })
        .detach();
    }

    fn links_loaded(
        &mut self,
        key: PageId,
        result: Result<Vec<Link>, EngineError>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(session) = self.session() else {
            return;
        };
        // An answer for a document that was closed meanwhile.
        if session.id() != key.document {
            return;
        }
        let page_count = session.page_count();
        self.links.loading.remove(&key);
        match result {
            Ok(links) => {
                let links = PageLinks::from_engine(links, page_count);
                if links.truncated() {
                    log::debug!(
                        "page {}: only the first {} links are used",
                        key.page.display_number(),
                        links.len()
                    );
                }
                self.links.cache.insert(key, Arc::new(links));
            }
            // An engine without links: the page has none.
            Err(EngineError::Unsupported(_)) => {
                self.links.cache.insert(key, Arc::new(PageLinks::default()));
            }
            // Asked again on a hover a moment later.
            Err(e) if e.is_transient() => {
                log::debug!("links of page {}: {e}", key.page.display_number());
                self.links.retry = Some((key, Instant::now() + RETRY_AFTER));
            }
            Err(e) => {
                log::debug!("links of page {}: {e}", key.page.display_number());
                if self.links.failed.len() < MAX_FAILED {
                    self.links.failed.insert(key);
                }
            }
        }
        if let Some(next) = self.links.queued.take()
            && next.document == key.document
        {
            self.want_links(next.page, cx);
        }
        // The development overlay outlines what arrived (and reads the
        // next visible pages on that frame).
        if self.overlay.visible {
            cx.notify();
        }
        // The pointer may be over the page that arrived.
        if let Some(pointer) = self.links.pointer
            && !self.links_suspended()
        {
            let hit = self.link_at_cached(pointer);
            if self.replace_hover(hit) {
                cx.notify();
            }
        }
    }
}
