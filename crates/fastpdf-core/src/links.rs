//! The links of a page as the reader uses them: validated, capped and hit
//! tested in page space, and a byte-budgeted cache of them.
//!
//! Link data comes from the engine (with the default engine, from the
//! render host), so it is checked again here: destinations must name a
//! page of the document, rectangles must be finite with an area, and a
//! page keeps at most [`MAX_PAGE_LINKS`] links and [`MAX_PAGE_URI_BYTES`]
//! of addresses. Which external addresses may be opened is decided by
//! `fastpdf_shell::check_external_uri`; this module only keeps them.

use std::sync::Arc;

use fastpdf_cache::{BudgetedCache, CacheStats, SharedCache, retention};
use fastpdf_engine_api::{
    Destination, DestinationView, DocumentId, Link, LinkTarget, PageId, PageRect,
};

/// Links kept per page; later ones are dropped (and the page is
/// [`PageLinks::truncated`]).
pub const MAX_PAGE_LINKS: usize = 1024;
/// Address bytes kept per page; later addresses are
/// [`NotFollowed::UriTooLong`].
pub const MAX_PAGE_URI_BYTES: usize = 64 * 1024;
/// Shorter sides (in points) make no clickable area.
pub const MIN_LINK_SIDE: f32 = 0.5;
/// How far (logical pixels) the mouse may move between press and release
/// for a click, not a drag: the Windows drag threshold (`SM_CXDRAG`).
pub const CLICK_SLOP: f32 = 4.0;

/// What following a link does.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkAction {
    /// Show another place of the document.
    GoTo(Destination),
    /// An external address, still to be checked before anything opens it.
    Uri(Arc<str>),
    /// Nothing: FastPDF does not follow it.
    NotFollowed(NotFollowed),
}

/// Why a link is not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFollowed {
    /// An action FastPDF never runs (launch a program, open another file,
    /// a script, ...) or a destination that does not resolve.
    Action,
    /// The destination page is not in the document.
    MissingPage,
    /// The page's addresses are longer than [`MAX_PAGE_URI_BYTES`].
    UriTooLong,
}

/// One clickable area of a page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLink {
    /// Page space: points, unrotated, top-left origin.
    pub bounds: PageRect,
    pub action: LinkAction,
}

/// The links of one page in the document's order: the last one is on top.
#[derive(Debug, Default, PartialEq)]
pub struct PageLinks {
    links: Vec<PageLink>,
    truncated: bool,
}

impl PageLinks {
    /// Checks the engine's links of a page of a `page_count`-page document.
    pub fn from_engine(links: Vec<Link>, page_count: u32) -> Self {
        let mut out = Vec::with_capacity(links.len().min(MAX_PAGE_LINKS));
        let mut uri_bytes = 0usize;
        let mut truncated = false;
        for link in links {
            let b = link.bounds;
            let finite = [b.x0, b.y0, b.x1, b.y1].iter().all(|v| v.is_finite());
            if !finite || b.width() < MIN_LINK_SIDE || b.height() < MIN_LINK_SIDE {
                continue;
            }
            if out.len() == MAX_PAGE_LINKS {
                truncated = true;
                break;
            }
            let action = match link.target {
                LinkTarget::Internal(dest) if dest.page.get() < page_count => {
                    LinkAction::GoTo(sanitized(dest))
                }
                LinkTarget::Internal(_) => LinkAction::NotFollowed(NotFollowed::MissingPage),
                LinkTarget::Uri(uri) => {
                    uri_bytes = uri_bytes.saturating_add(uri.len());
                    if uri_bytes <= MAX_PAGE_URI_BYTES {
                        LinkAction::Uri(uri.into())
                    } else {
                        LinkAction::NotFollowed(NotFollowed::UriTooLong)
                    }
                }
                // Unsupported, and whatever a later engine API adds.
                _ => LinkAction::NotFollowed(NotFollowed::Action),
            };
            out.push(PageLink { bounds: b, action });
        }
        out.shrink_to_fit();
        Self {
            links: out,
            truncated,
        }
    }

    /// The topmost link containing the page-space point.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.links.iter().rposition(|l| l.bounds.contains(x, y))
    }

    pub fn get(&self, index: usize) -> Option<&PageLink> {
        self.links.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PageLink> {
        self.links.iter()
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// More links than [`MAX_PAGE_LINKS`] were offered.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Bytes kept on the heap: the cache's weight of the page.
    pub fn heap_bytes(&self) -> usize {
        let uris: usize = self
            .links
            .iter()
            .map(|l| match &l.action {
                LinkAction::Uri(u) => u.len(),
                _ => 0,
            })
            .sum();
        self.links.capacity() * std::mem::size_of::<PageLink>() + uris
    }
}

/// Non-finite coordinates of a destination are dropped (the view keeps its
/// place on that axis).
fn sanitized(mut dest: Destination) -> Destination {
    let keep = |v: Option<f32>| v.filter(|v| v.is_finite());
    dest.view = match dest.view {
        DestinationView::Xyz { left, top, zoom } => DestinationView::Xyz {
            left: keep(left),
            top: keep(top),
            zoom: keep(zoom),
        },
        DestinationView::FitWidth { top } => DestinationView::FitWidth { top: keep(top) },
        DestinationView::FitHeight { left } => DestinationView::FitHeight { left: keep(left) },
        DestinationView::FitRect(r) if [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) => {
            DestinationView::FitRect(r)
        }
        DestinationView::FitRect(_) | DestinationView::Fit => DestinationView::Fit,
    };
    dest
}

/// A click, not a drag: the mouse moved at most [`CLICK_SLOP`] logical
/// pixels between press and release.
pub fn is_click(dx: f32, dy: f32) -> bool {
    dx * dx + dy * dy <= CLICK_SLOP * CLICK_SLOP
}

/// The links of the pages the pointer visited, by page, within a byte
/// budget (registered with the memory budget manager as "links").
pub struct LinkCache {
    cache: Arc<SharedCache<PageId, Arc<PageLinks>>>,
}

impl std::fmt::Debug for LinkCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkCache")
            .field("stats", &self.cache.stats())
            .finish()
    }
}

impl LinkCache {
    /// A page weighs at most about 120 KiB (1024 links and 64 KiB of
    /// addresses), so no page is ever larger than the whole budget.
    pub const DEFAULT_BUDGET: usize = 2 * 1024 * 1024;

    pub fn new(budget: usize) -> Self {
        Self {
            cache: Arc::new(SharedCache::new("links", budget, retention::LINKS)),
        }
    }

    /// Handle for [`fastpdf_cache::MemoryBudgetManager::register`].
    pub fn budgeted(&self) -> Arc<dyn BudgetedCache> {
        self.cache.clone()
    }

    pub fn get(&self, key: PageId) -> Option<Arc<PageLinks>> {
        self.cache.with(&key, Arc::clone)
    }

    pub fn contains(&self, key: PageId) -> bool {
        self.cache.contains(&key)
    }

    pub fn insert(&self, key: PageId, links: Arc<PageLinks>) {
        let weight = links.heap_bytes() + std::mem::size_of::<PageLinks>();
        self.cache.insert(key, links, weight);
    }

    /// Forgets every page of `document` (closed).
    pub fn remove_document(&self, document: DocumentId) {
        self.cache.retain(|key, _| key.document != document);
    }

    pub fn stats(&self) -> CacheStats {
        self.cache.stats()
    }
}

#[cfg(test)]
mod tests {
    use fastpdf_engine_api::PageIndex;

    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> PageRect {
        PageRect::new(x0, y0, x1, y1)
    }

    fn goto(page: u32) -> LinkTarget {
        LinkTarget::Internal(Destination {
            page: PageIndex::new(page),
            view: DestinationView::Fit,
        })
    }

    fn link(bounds: PageRect, target: LinkTarget) -> Link {
        Link { bounds, target }
    }

    #[test]
    fn engine_links_are_checked_and_kept_in_order() {
        let links = PageLinks::from_engine(
            vec![
                link(rect(0.0, 0.0, 100.0, 100.0), goto(1)),
                // Zero area, a hair line, non-finite: no clickable area.
                link(rect(5.0, 5.0, 5.0, 50.0), goto(1)),
                link(rect(5.0, 5.0, 50.0, 5.3), goto(1)),
                link(rect(0.0, 0.0, f32::INFINITY, 10.0), goto(1)),
                link(rect(50.0, 50.0, 150.0, 150.0), goto(7)),
                link(rect(60.0, 60.0, 70.0, 70.0), LinkTarget::Unsupported),
                link(
                    rect(0.0, 200.0, 10.0, 210.0),
                    LinkTarget::Uri("https://example.invalid/".into()),
                ),
            ],
            3,
        );
        assert_eq!(links.len(), 4);
        assert!(matches!(&links.get(0).unwrap().action, LinkAction::GoTo(d) if d.page.get() == 1));
        assert_eq!(
            links.get(1).unwrap().action,
            LinkAction::NotFollowed(NotFollowed::MissingPage)
        );
        assert_eq!(
            links.get(2).unwrap().action,
            LinkAction::NotFollowed(NotFollowed::Action)
        );
        assert_eq!(
            links.get(3).unwrap().action,
            LinkAction::Uri("https://example.invalid/".into())
        );
        // The topmost (last) link wins where links overlap.
        assert_eq!(links.hit(65.0, 65.0), Some(2));
        assert_eq!(links.hit(55.0, 55.0), Some(1));
        assert_eq!(links.hit(10.0, 10.0), Some(0));
        assert_eq!(links.hit(300.0, 300.0), None);
        assert!(!links.truncated());
    }

    #[test]
    fn destinations_lose_non_finite_coordinates() {
        let links = PageLinks::from_engine(
            vec![link(
                rect(0.0, 0.0, 10.0, 10.0),
                LinkTarget::Internal(Destination {
                    page: PageIndex::FIRST,
                    view: DestinationView::Xyz {
                        left: Some(f32::NAN),
                        top: Some(12.0),
                        zoom: Some(f32::INFINITY),
                    },
                }),
            )],
            1,
        );
        assert!(matches!(
            &links.get(0).unwrap().action,
            LinkAction::GoTo(d) if d.view == DestinationView::Xyz { left: None, top: Some(12.0), zoom: None }
        ));
    }

    #[test]
    fn pages_are_capped_in_links_and_address_bytes() {
        let many: Vec<Link> = (0..MAX_PAGE_LINKS + 5)
            .map(|i| link(rect(0.0, i as f32, 10.0, i as f32 + 1.0), goto(0)))
            .collect();
        let links = PageLinks::from_engine(many, 1);
        assert_eq!(links.len(), MAX_PAGE_LINKS);
        assert!(links.truncated());

        let uri = format!("https://example.invalid/{}", "a".repeat(30 * 1024));
        let three = (0..3)
            .map(|i| {
                link(
                    rect(0.0, i as f32 * 10.0, 10.0, i as f32 * 10.0 + 5.0),
                    LinkTarget::Uri(uri.clone()),
                )
            })
            .collect();
        let links = PageLinks::from_engine(three, 1);
        assert!(matches!(links.get(1).unwrap().action, LinkAction::Uri(_)));
        assert_eq!(
            links.get(2).unwrap().action,
            LinkAction::NotFollowed(NotFollowed::UriTooLong)
        );
    }

    #[test]
    fn the_worst_page_fits_the_cache_budget() {
        let uri = "a".repeat(MAX_PAGE_URI_BYTES / MAX_PAGE_LINKS);
        let worst: Vec<Link> = (0..MAX_PAGE_LINKS)
            .map(|i| {
                link(
                    rect(0.0, i as f32, 10.0, i as f32 + 1.0),
                    LinkTarget::Uri(uri.clone()),
                )
            })
            .collect();
        let links = PageLinks::from_engine(worst, 1);
        assert!(
            links.heap_bytes() < LinkCache::DEFAULT_BUDGET / 8,
            "{}",
            links.heap_bytes()
        );
    }

    #[test]
    fn clicks_allow_a_few_pixels_of_movement() {
        assert!(is_click(0.0, 0.0));
        assert!(is_click(4.0, 0.0));
        assert!(is_click(-2.8, 2.8));
        assert!(!is_click(4.1, 0.0));
        assert!(!is_click(3.0, 3.0));
    }

    #[test]
    fn the_cache_forgets_closed_documents_only() {
        let cache = LinkCache::new(LinkCache::DEFAULT_BUDGET);
        let (a, b) = (DocumentId::next(), DocumentId::next());
        let page = |doc, n| PageId::new(doc, PageIndex::new(n));
        let empty = Arc::new(PageLinks::default());
        cache.insert(page(a, 0), Arc::clone(&empty));
        cache.insert(page(a, 1), Arc::clone(&empty));
        cache.insert(page(b, 0), Arc::clone(&empty));
        assert!(cache.get(page(a, 1)).is_some_and(|l| l.is_empty()));
        cache.remove_document(a);
        assert!(cache.get(page(a, 0)).is_none() && !cache.contains(page(a, 1)));
        assert!(cache.contains(page(b, 0)));
        assert_eq!(cache.stats().entries, 1);
    }
}
