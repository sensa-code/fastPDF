use std::sync::{Arc, Mutex};

use fastpdf_engine_api::{
    CancelToken, Destination, DestinationView, DocumentSource, EngineDocument, EngineError,
    EngineInfo, OpenOptions, PageIndex, PageInfo, PageSize, PdfEngine, PixmapMut, RenderOutcome,
    RenderRequest, Rotation, SharedBytes, open_guarded,
};
use fastpdf_render::{Spread, ZoomLevel};

use crate::history::Jump;
use crate::session::tests::{mode, session, settle};
use crate::session::{DocumentSession, ScrollAxis, SessionConfig, ZoomMode};
use crate::view_state::{RestoreScope, SavedView, SavedZoom};

fn scroll(s: &DocumentSession<Arc<fastpdf_engine_api::Pixmap>>) -> (f64, f64) {
    (s.viewport().scroll_x, s.viewport().scroll_y)
}

fn close(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
}

#[test]
fn back_and_forward_restore_the_exact_view() {
    let mut s = session();
    s.set_zoom(ZoomLevel::new(2.0), None);
    s.scroll_by(150.0, 2500.0);
    let start = scroll(&s);
    assert!(start.0 > 0.0 && start.1 > 0.0);
    assert!(s.jump(Jump::Direct, |s| s.go_to_page(PageIndex::new(30))));
    assert_eq!(s.current_page(), PageIndex::new(30));
    s.scroll_by(-40.0, 300.0);
    let there = scroll(&s);
    assert!(s.can_go_back() && !s.can_go_forward());

    assert!(s.go_back());
    assert!(close(scroll(&s), start), "{:?} vs {start:?}", scroll(&s));
    assert_eq!(s.history_depth(), (0, 1));
    assert!(!s.go_back());
    assert!(s.go_forward());
    assert!(close(scroll(&s), there), "{:?} vs {there:?}", scroll(&s));
    assert_eq!(s.history_depth(), (1, 0));
    assert!(!s.go_forward());
}

#[test]
fn ordinary_navigation_is_not_recorded() {
    let mut s = session();
    assert_eq!(s.history_depth(), (0, 0), "fresh sessions have no history");
    s.next_page();
    s.page_down();
    s.scroll_by(0.0, 500.0);
    s.zoom_in(None);
    s.scroll_to(ScrollAxis::Vertical, 0.5);
    s.fit_page();
    s.set_layout_mode(mode(Spread::Double, true));
    s.rotate_clockwise();
    s.first_page();
    s.last_page();
    assert!(!s.can_go_back() && !s.can_go_forward());
    assert_eq!(s.history_depth(), (0, 0));
}

#[test]
fn back_restores_zoom_and_zoom_mode() {
    let mut s = session();
    s.set_zoom(ZoomLevel::new(2.0), None);
    s.jump(Jump::Direct, |s| s.go_to_page(PageIndex::new(20)));
    s.fit_width();
    let fitted = s.zoom();
    assert!(s.go_back());
    assert_eq!(s.zoom_mode(), ZoomMode::Custom);
    assert_eq!(s.zoom(), ZoomLevel::new(2.0));
    assert!(s.go_forward());
    assert_eq!(s.zoom_mode(), ZoomMode::FitWidth);
    assert_eq!(s.zoom(), fitted);
    assert_eq!(s.current_page(), PageIndex::new(20));

    // Fit page is recomputed for the page shown.
    s.fit_page();
    s.jump(Jump::Direct, |s| s.go_to_page(PageIndex::new(2)));
    s.actual_size();
    assert!(s.go_back());
    assert_eq!(s.zoom_mode(), ZoomMode::FitPage);
    assert_eq!(s.current_page(), PageIndex::new(20));
}

#[test]
fn layout_change_falls_back_to_the_recorded_page() {
    let mut s = session();
    s.go_to_page(PageIndex::new(30));
    s.scroll_by(0.0, 200.0);
    let recorded = s.current_page();
    s.jump(Jump::Direct, |s| s.go_to_page(PageIndex::new(10)));
    s.set_layout_mode(mode(Spread::Double, false));
    assert!(s.go_back());
    let row = s.layout().row_of(recorded);
    assert_eq!(s.layout().row_of(s.current_page()), row);
    assert_eq!(s.layout().focus_row(), row);
    let frame = settle(&mut s);
    assert!(frame.pages.iter().any(|p| p.page == recorded));

    // Same after a rotation.
    let mut s = session();
    s.go_to_page(PageIndex::new(30));
    s.jump(Jump::Direct, |s| s.go_to_page(PageIndex::new(10)));
    s.rotate_clockwise();
    assert!(s.go_back());
    assert_eq!(s.current_page(), PageIndex::new(30));
    assert_eq!(
        s.rotation(),
        Rotation::R90,
        "Back does not undo the rotation"
    );
}

#[test]
fn broken_views_fall_back_to_their_page() {
    let mut s = session();
    let mut view = s.view_state();
    view.anchor.fraction = f64::NAN;
    view.scroll_x = f64::INFINITY;
    view.page = PageIndex::new(12);
    s.restore_view(&view);
    assert_eq!(s.current_page(), PageIndex::new(12));
    assert!(s.viewport().scroll_x.is_finite() && s.viewport().scroll_y.is_finite());
}

#[test]
fn destinations_are_jumps() {
    let mut s = session();
    let dest = Destination {
        page: PageIndex::new(12),
        view: DestinationView::Xyz {
            left: None,
            top: Some(300.0),
            zoom: None,
        },
    };
    assert!(s.jump(Jump::Direct, |s| s.go_to_destination(&dest)));
    assert_eq!(s.current_page(), PageIndex::new(12));
    assert_eq!(s.history_depth(), (1, 0));
    assert!(s.go_back());
    assert_eq!(s.current_page(), PageIndex::FIRST);
    // Going where the view already is records nothing and keeps Forward.
    assert!(!s.jump(Jump::Direct, |s| s.first_page()));
    assert_eq!(s.history_depth(), (0, 1));
}

#[test]
fn saved_view_round_trips() {
    let layouts = [
        mode(Spread::Single, true),
        mode(Spread::Single, false),
        mode(Spread::Double, true),
        mode(Spread::DoubleCover, true),
    ];
    for layout in layouts {
        let mut s = session();
        s.set_layout_mode(layout);
        s.rotate_clockwise();
        s.set_zoom(ZoomLevel::new(2.5), None);
        s.go_to_page(PageIndex::new(30));
        s.scroll_by(120.0, 400.0);
        let before = s.view_state();
        let saved = s.saved_view();
        assert_eq!(saved.zoom, SavedZoom::Custom(2.5));
        assert_eq!(saved.rotation, Rotation::R90);

        // A new session of the same document, in the same layout.
        let mut t = session();
        t.set_layout_mode(layout);
        t.restore_saved(&saved, RestoreScope::Exact);
        let after = t.view_state();
        assert_eq!(after.anchor.page, before.anchor.page, "{layout:?}");
        assert!(
            (after.anchor.fraction - before.anchor.fraction).abs() < 1e-6,
            "{layout:?}: {after:?} vs {before:?}"
        );
        assert!(
            (after.scroll_x - before.scroll_x).abs() < 1e-3,
            "{layout:?}"
        );
        assert_eq!(
            (after.zoom, after.zoom_mode, after.rotation),
            (before.zoom, before.zoom_mode, before.rotation)
        );
        assert_eq!(after.page, before.page, "{layout:?}");
    }
}

#[test]
fn place_only_keeps_zoom_and_rotation() {
    let mut s = session();
    s.rotate_clockwise();
    s.set_zoom(ZoomLevel::new(1.5), None);
    let saved = SavedView {
        page: PageIndex::new(20),
        offset: 0.5,
        x: 0.3,
        zoom: SavedZoom::Custom(3.0),
        rotation: Rotation::R180,
    };
    s.restore_saved(&saved, RestoreScope::PlaceOnly);
    assert_eq!(s.rotation(), Rotation::R90);
    assert_eq!(
        (s.zoom_mode(), s.zoom()),
        (ZoomMode::Custom, ZoomLevel::new(1.5))
    );
    let anchor = s.view_state().anchor;
    assert_eq!(anchor.page, PageIndex::new(20));
    assert!((anchor.fraction - 0.5).abs() < 1e-6);

    s.restore_saved(&saved, RestoreScope::Exact);
    assert_eq!(s.rotation(), Rotation::R180);
    assert_eq!(
        (s.zoom_mode(), s.zoom()),
        (ZoomMode::Custom, ZoomLevel::new(3.0))
    );
    let (doc_width, _) = s.layout().total_size();
    assert!((s.viewport().scroll_x / doc_width - 0.3).abs() < 1e-6);

    // Hostile values: past the end, not finite.
    let wild = SavedView {
        page: PageIndex::new(9_999),
        offset: f32::NAN,
        x: f32::NEG_INFINITY,
        zoom: SavedZoom::FitPage,
        rotation: Rotation::R0,
    };
    s.restore_saved(&wild, RestoreScope::Exact);
    assert_eq!(s.current_page(), PageIndex::new(49));
    assert_eq!(s.zoom_mode(), ZoomMode::FitPage);
    assert!(s.viewport().scroll_x.is_finite() && s.viewport().scroll_y.is_finite());
}

/// Letter pages; records which pages were rendered.
struct RecordingEngine(Arc<Mutex<Vec<u32>>>);
struct RecordingDoc(Arc<Mutex<Vec<u32>>>);

impl PdfEngine for RecordingEngine {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "recording",
            version: "0",
            capabilities: Default::default(),
        }
    }
    fn open(
        &self,
        _: DocumentSource,
        _: &OpenOptions,
    ) -> Result<Box<dyn EngineDocument>, EngineError> {
        Ok(Box::new(RecordingDoc(Arc::clone(&self.0))))
    }
}

impl EngineDocument for RecordingDoc {
    fn page_count(&self) -> u32 {
        50
    }
    fn page_info(&self, _: PageIndex) -> Result<PageInfo, EngineError> {
        Ok(PageInfo {
            size: PageSize::LETTER,
            rotation: Rotation::R0,
        })
    }
    fn render(
        &self,
        request: &RenderRequest,
        target: &mut PixmapMut<'_>,
        _: &CancelToken,
    ) -> Result<RenderOutcome, EngineError> {
        if let Ok(mut pages) = self.0.lock() {
            pages.push(request.page.get());
        }
        target.fill(request.background);
        Ok(RenderOutcome::default())
    }
}

#[test]
fn restore_saved_plans_the_saved_page_first() {
    let rendered = Arc::new(Mutex::new(Vec::new()));
    let doc = open_guarded(
        &RecordingEngine(Arc::clone(&rendered)),
        DocumentSource::from_bytes(SharedBytes::from_vec(vec![0])),
        &OpenOptions::default(),
    )
    .unwrap();
    let mut s = DocumentSession::new(
        Arc::new(doc),
        SessionConfig::default(),
        (1280.0, 720.0),
        1.0,
        Arc::new,
        || {},
        |_| {},
    );
    let saved = SavedView {
        page: PageIndex::new(30),
        offset: 0.25,
        x: 0.0,
        zoom: SavedZoom::FitWidth,
        rotation: Rotation::R0,
    };
    s.restore_saved(&saved, RestoreScope::Exact);
    let first = s.frame();
    let shown: Vec<u32> = first.pages.iter().map(|p| p.page.get()).collect();
    assert!(shown.contains(&30), "{shown:?}");
    assert!(shown.iter().all(|p| (29..=31).contains(p)), "{shown:?}");
    let frame = settle(&mut s);
    assert_eq!(frame.pending, 0);
    let rendered = rendered.lock().unwrap();
    assert!(!rendered.is_empty());
    assert!(
        rendered.iter().all(|p| (29..=31).contains(p)),
        "page 1 must not render first: {rendered:?}"
    );
}
